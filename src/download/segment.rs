use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter, SeekFrom};

use super::rate_limit::{acquire_composed, RunLimits};

/// The default timeout for network requests and stream reads.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Buffered writes — fewer syscalls under high concurrency.
const WRITE_BUF_BYTES: usize = 256 * 1024;

/// The minimum size in bytes for a chunk to be considered eligible for splitting.
/// Smaller chunks are not split to avoid excessive HTTP overhead.
pub const MIN_SPLIT_SIZE: u64 = 1024 * 1024; // 1MB minimum for splitting a chunk

/// Represents a range of bytes to download within a file.
///
/// Chunks are designed to be thread-safe and dynamic. They can be shared across
/// multiple tasks to track progress and can be split into smaller chunks to
/// balance work between idle workers.
pub struct Chunk {
    /// Start byte — immutable after creation.
    pub start: u64,
    /// End byte (inclusive) — Atomic so the hot-path per-packet check avoids
    /// a Mutex acquisition.  Only shrunk during a split.
    pub end: AtomicU64,
    /// The number of bytes successfully written to the file for this chunk.
    /// Uses AtomicU64 to allow safe, lock-free progress tracking from the worker
    /// while other tasks (like the heartbeat) read it.
    pub progress: AtomicU64,
}

impl Chunk {
    /// Creates a new Chunk with the specified range and initial progress.
    pub fn new(range: std::ops::RangeInclusive<u64>, progress: u64) -> Self {
        Self {
            start: *range.start(),
            end: AtomicU64::new(*range.end()),
            progress: AtomicU64::new(progress),
        }
    }

    /// Returns the number of bytes remaining to download in this chunk.
    ///
    /// This calculation is dynamic and reflects both current progress and
    /// any range adjustments caused by splitting.
    pub fn remaining_bytes(&self) -> u64 {
        let total_size = (self.end.load(Ordering::Relaxed) - self.start) + 1;
        total_size.saturating_sub(self.progress.load(Ordering::Relaxed))
    }

    /// Attempts to split the remaining work of this chunk into two separate chunks.
    ///
    /// This is the core of the **Work Stealing** mechanism. If a chunk is large enough,
    /// it can be halved. The current chunk's range is reduced, and a new `Arc<Chunk>`
    /// is returned representing the latter half.
    ///
    /// Returns `None` if the remaining work is smaller than `MIN_SPLIT_SIZE * 2`.
    pub fn split(self: &Arc<Self>) -> Option<Arc<Self>> {
        let current_end = self.end.load(Ordering::Acquire);
        let total_size = (current_end - self.start) + 1;
        let current_progress = self.progress.load(Ordering::Acquire);

        let remaining = total_size.saturating_sub(current_progress);

        if remaining < MIN_SPLIT_SIZE * 2 {
            return None;
        }

        // Calculate the absolute midpoint of the REMAINING work
        let absolute_split_point = self.start + current_progress + (remaining / 2);

        // Create a new chunk for the latter half, starting with 0 progress
        let new_chunk = Arc::new(Chunk::new(absolute_split_point..=current_end, 0));

        // Shrink the current chunk's end limit to just before the split point
        self.end.store(absolute_split_point - 1, Ordering::Release);

        Some(new_chunk)
    }
}

/// The core worker task responsible for downloading a single byte range ([`Chunk`]).
///
/// It manages its own network connection and file handle to ensure high concurrency.
/// It also handles its own internal retry logic and listens for cancellation signals.
///
/// # Arguments
/// * `client` - Shared HTTP client.
/// * `target_path` - Path to the file being downloaded.
/// * `chunk` - The shared chunk metadata this worker is responsible for.
/// * `url` - The source URL.
/// * `cancel_token` - Token to signal task termination.
pub async fn download_worker(
    client: Arc<reqwest::Client>,
    target_path: std::path::PathBuf,
    chunk: Arc<Chunk>,
    url: String,
    cancel_token: CancellationToken,
    limits: RunLimits,
    use_range: bool,
) -> Result<(), anyhow::Error> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&target_path)
        .await?;
    let mut handle = BufWriter::with_capacity(WRITE_BUF_BYTES, file);

    let result = tokio::select! {
        _ = cancel_token.cancelled() => Ok(()),
        res = perform_download(&client, &mut handle, &chunk, &url, limits, use_range) => res,
    };
    handle.flush().await?;
    result
}

/// The inner download loop that performs the actual HTTP Range request and streaming.
///
/// It handles "split-aware" writes, ensuring that if the chunk is split by another worker
/// while this one is downloading, it stops exactly at the new boundary.
async fn perform_download(
    client: &reqwest::Client,
    handle: &mut BufWriter<tokio::fs::File>,
    chunk: &Chunk,
    url: &str,
    limits: RunLimits,
    use_range: bool,
) -> Result<(), anyhow::Error> {
    let mut retry_count = 0;
    let max_retries = 5;

    loop {
        // 1. Fetch current limits and progress. This handle resumes.
        let (start_offset, end_offset, current_progress) = {
            let cp = chunk.progress.load(Ordering::Acquire);
            (chunk.start, chunk.end.load(Ordering::Acquire), cp)
        };

        let absolute_start = start_offset + current_progress;
        if absolute_start > end_offset {
            return Ok(());
        }

        // 2. Prepare the Range header for the request
        handle.seek(SeekFrom::Start(absolute_start)).await?;

        let mut request = client.get(url);
        if use_range {
            request = request.header("Range", format!("bytes={absolute_start}-{end_offset}"));
        }
        let response_res = timeout(TIMEOUT, request.send()).await;

        let response = match response_res {
            Ok(Ok(resp)) => resp,
            _ => {
                if retry_count < max_retries {
                    retry_count += 1;
                    let delay = Duration::from_secs(2u64.pow(retry_count));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                match response_res {
                    Ok(Err(e)) => return Err(anyhow::anyhow!("Network error for {}: {}", url, e)),
                    Err(_) => return Err(anyhow::anyhow!("Connection timeout for {}: {}", url, TIMEOUT.as_secs())),
                    _ => unreachable!(),
                }
            }
        };

        if !response.status().is_success() {
            if response.status().is_server_error() && retry_count < max_retries {
                retry_count += 1;
                tokio::time::sleep(Duration::from_secs(2u64.pow(retry_count))).await;
                continue;
            }
            return Err(anyhow::anyhow!("Server rejected Range request for {} (Status: {}). Bytes: {}-{}",
                url, response.status(), absolute_start, end_offset));
        }

        // 3. Process the byte stream
        let mut stream = response.bytes_stream();
        let mut bytes_since_flush = 0u64;

        while let Some(item) = stream.next().await {
            let packet = match item {
                Ok(p) => p,
                Err(e) => {
                    if retry_count < max_retries {
                        retry_count += 1;
                        tokio::time::sleep(Duration::from_secs(2u64.pow(retry_count))).await;
                        break; // Break stream loop to retry full request
                    }
                    return Err(e.into());
                }
            };

            // Reset retry count on successful packet
            retry_count = 0;

            // 4. Re-check limits in case of a split during the stream.
            //    Uses atomic loads — no Mutex in the hot path.
            let current_end = chunk.end.load(Ordering::Acquire);
            let current_abs_start = chunk.start + chunk.progress.load(Ordering::Acquire);

            if current_abs_start > current_end {
                return Ok(());
            }

            let bytes_to_write = if current_abs_start + packet.len() as u64 > current_end + 1 {
                (current_end + 1 - current_abs_start) as usize
            } else {
                packet.len()
            };

            handle.write_all(&packet[..bytes_to_write]).await?;
            chunk.progress.fetch_add(bytes_to_write as u64, Ordering::Release);

            // Periodically flush so a crash can't lose more than ~1 MB of
            // recently-received data per worker.  Without this the BufWriter
            // only flushes when its 256 KB buffer is full or at worker exit —
            // fine for throughput, but bad for resume accuracy since the
            // heartbeat snapshot counts bytes written before they hit disk.
            bytes_since_flush += bytes_to_write as u64;
            if bytes_since_flush >= 1024 * 1024 {
                handle.flush().await?;
                bytes_since_flush = 0;
            }

            acquire_composed(
                limits.global.as_ref(),
                limits.local.as_ref(),
                bytes_to_write as u64,
            )
            .await;

            if bytes_to_write < packet.len() {
                return Ok(());
            }
        }

        let final_progress = chunk.progress.load(Ordering::Acquire);
        let final_end = chunk.end.load(Ordering::Acquire);
        if final_progress > (final_end - chunk.start) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_chunk_initialization() {
        // Goal: Ensure a Chunk is correctly initialized with the provided range and progress.
        let chunk = Chunk::new(0..=99, 10);

        assert_eq!(chunk.progress.load(Ordering::Relaxed), 10);
        assert_eq!(chunk.start, 0);
        assert_eq!(chunk.end.load(Ordering::Relaxed), 99);
    }

    #[tokio::test]
    async fn test_chunk_single_byte_range() {
        // Edge case: range where start == end (single byte).
        let chunk = Chunk::new(42..=42, 0);
        assert_eq!(chunk.remaining_bytes(), 1);
        assert_eq!(chunk.start, 42);
        assert_eq!(chunk.end.load(Ordering::Relaxed), 42);

        // Mark as complete
        chunk.progress.store(1, Ordering::Relaxed);
        assert_eq!(chunk.remaining_bytes(), 0);
    }

    #[tokio::test]
    async fn test_remaining_bytes() {
        let chunk = Chunk::new(0..=99, 0);

        // Case 1: Fresh chunk should have full range remaining.
        assert_eq!(chunk.remaining_bytes(), 100);

        // Case 2: Partial progress.
        chunk.progress.store(50, Ordering::Relaxed);
        assert_eq!(chunk.remaining_bytes(), 50);

        // Case 3: Fully completed chunk.
        chunk.progress.store(100, Ordering::Relaxed);
        assert_eq!(chunk.remaining_bytes(), 0);

        // Case 4: Progress exceeding range (should be clamped to 0).
        chunk.progress.store(150, Ordering::Relaxed);
        assert_eq!(chunk.remaining_bytes(), 0);
    }

    #[tokio::test]
    async fn test_remaining_bytes_after_atomic_shrink() {
        // Simulates: chunk is partially downloaded, then a split shrinks `end`.
        // remaining_bytes() should reflect the new range, not the original.
        let chunk = Arc::new(Chunk::new(0..=99, 20));

        // Manually shrink end (as split() would).
        chunk.end.store(49, Ordering::Release);

        // 30 remaining: (49 - 0 + 1) - 20 = 30
        assert_eq!(chunk.remaining_bytes(), 30);

        // Progress past new end.
        chunk.progress.store(60, Ordering::Relaxed);
        assert_eq!(chunk.remaining_bytes(), 0);
    }

    #[tokio::test]
    async fn test_chunk_split_too_small() {
        // Chunks below MIN_SPLIT_SIZE × 2 must NOT be split.
        let chunk = Arc::new(Chunk::new(0..=(1024 * 1024 - 1), 0));
        let new_chunk = chunk.split();

        assert!(new_chunk.is_none(), "Chunk should not split if below size threshold");
    }

    #[tokio::test]
    async fn test_chunk_split_at_exact_boundary() {
        // Exactly at MIN_SPLIT_SIZE × 2 — should split.
        let size = 2 * MIN_SPLIT_SIZE;
        let chunk = Arc::new(Chunk::new(0..=(size - 1), 0));
        let new_chunk = chunk.split();

        assert!(new_chunk.is_some(), "Chunk at exact boundary should split");
        let new_chunk = new_chunk.unwrap();

        // Split point should be at MIN_SPLIT_SIZE.
        assert_eq!(chunk.end.load(Ordering::Relaxed), MIN_SPLIT_SIZE - 1);
        assert_eq!(new_chunk.start, MIN_SPLIT_SIZE);
        assert_eq!(new_chunk.end.load(Ordering::Relaxed), size - 1);
    }

    #[tokio::test]
    async fn test_chunk_split_success() {
        let total_size = 30 * 1024 * 1024;
        let chunk = Arc::new(Chunk::new(0..=(total_size - 1), 0));

        let new_chunk = chunk.split().expect("Should split 30MB chunk");

        let expected_split_point = 15 * 1024 * 1024;

        assert_eq!(chunk.start, 0);
        assert_eq!(chunk.end.load(Ordering::Relaxed), (expected_split_point - 1) as u64);
        assert_eq!(new_chunk.start, expected_split_point as u64);
        assert_eq!(new_chunk.end.load(Ordering::Relaxed), total_size - 1);
        assert_eq!(new_chunk.progress.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_chunk_split_with_progress() {
        // 30 MB range, 10 MB already written → split the remaining 20 MB.
        let total_size = 30 * 1024 * 1024;
        let progress = 10 * 1024 * 1024;
        let chunk = Arc::new(Chunk::new(0..=(total_size - 1), progress));

        let new_chunk = chunk.split().expect("Should split chunk with remaining 20MB");

        // Split point = 10 MB (progress) + 10 MB (half of 20 MB remaining) = 20 MB.
        let expected_split_point = 20 * 1024 * 1024;

        assert_eq!(chunk.end.load(Ordering::Relaxed), (expected_split_point - 1) as u64);
        assert_eq!(new_chunk.start, expected_split_point as u64);
    }

    #[tokio::test]
    async fn test_chunk_split_preserves_start_immutable() {
        // After split, `start` must never change — only `end` shrinks.
        let chunk = Arc::new(Chunk::new(500..=999, 0));
        chunk.split();

        assert_eq!(chunk.start, 500, "start must never change after split");

        // Split again from the new (smaller) chunk.
        chunk.split();
        assert_eq!(chunk.start, 500, "start must never change after second split");
    }

    #[tokio::test]
    async fn test_chunk_multiple_sequential_splits() {
        // Split a large chunk repeatedly until it refuses to split further.
        let size = 20 * MIN_SPLIT_SIZE; // 20 MB
        let chunk = Arc::new(Chunk::new(0..=(size - 1), 0));

        let mut splits = 0;
        let mut current = Arc::clone(&chunk);
        while let Some(new_chunk) = current.split() {
            splits += 1;
            // The original (left) chunk's end should never overlap with new (right).
            assert!(current.end.load(Ordering::Relaxed) < new_chunk.start,
                    "Split chunks must not overlap: left end {} < right start {}",
                    current.end.load(Ordering::Relaxed), new_chunk.start);
            // The two ranges must cover a contiguous region with no gap.
            assert_eq!(current.end.load(Ordering::Relaxed) + 1, new_chunk.start,
                    "Split chunks must be contiguous with no gap");
            current = new_chunk;
        }

        // At 20 MB with 1 MB minimum, we should get ~10 splits (log₂).
        assert!(splits >= 3, "Should get multiple splits, got {splits}");
        // All splits combined must cover exactly the original range.
        // Last chunk covers from its start to `size - 1`. First chunk starts at 0.
        assert_eq!(chunk.start, 0);
        assert_eq!(current.end.load(Ordering::Relaxed), size - 1);
    }

    #[tokio::test]
    async fn test_chunk_split_with_large_progress_resume_margin_scenario() {
        // Simulates the scenario after a resume margin has been applied:
        // progress = 1 GB, range = 2 GB, remaining = 1 GB — split should work.
        let total_size = 2u64 * 1024 * 1024 * 1024; // 2 GB
        let progress = 1u64 * 1024 * 1024 * 1024;   // 1 GB
        let chunk = Arc::new(Chunk::new(0..=(total_size - 1), progress));

        let new_chunk = chunk.split().expect("Should split 2 GB chunk with 50% progress");

        // Split point = progress (1 GB) + half-remaining (512 MB) = 1.5 GB
        let expected = 1u64 * 1024 * 1024 * 1024 + 512 * 1024 * 1024;
        assert_eq!(chunk.end.load(Ordering::Relaxed), expected - 1);
        assert_eq!(new_chunk.start, expected);
        assert_eq!(new_chunk.end.load(Ordering::Relaxed), total_size - 1);
    }
}
