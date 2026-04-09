use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

/// The default timeout for network requests and stream reads.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The minimum size in bytes for a chunk to be considered eligible for splitting.
/// Smaller chunks are not split to avoid excessive HTTP overhead.
pub const MIN_SPLIT_SIZE: u64 = 1024 * 1024 * 10; // 10MB minimum for splitting a chunk

/// Represents a range of bytes to download within a file.
///
/// Chunks are designed to be thread-safe and dynamic. They can be shared across
/// multiple tasks to track progress and can be split into smaller chunks to
/// balance work between idle workers.
pub struct Chunk {
    /// The absolute byte range (inclusive) within the target file.
    /// Protected by a Mutex because it can be modified during a split operation
    /// while a worker is actively downloading it.
    pub chunk_limits: tokio::sync::Mutex<std::ops::RangeInclusive<u64>>,
    /// The number of bytes successfully written to the file for this chunk.
    /// Uses AtomicU64 to allow safe, lock-free progress tracking from the worker
    /// while other tasks (like the heartbeat) read it.
    pub progress: AtomicU64,
}

impl Chunk {
    /// Creates a new Chunk with the specified range and initial progress.
    pub fn new(range: std::ops::RangeInclusive<u64>, progress: u64) -> Self {
        Self {
            chunk_limits: tokio::sync::Mutex::new(range),
            progress: AtomicU64::new(progress),
        }
    }

    /// Returns the number of bytes remaining to download in this chunk.
    ///
    /// This calculation is dynamic and reflects both current progress and
    /// any range adjustments caused by splitting.
    pub async fn remaining_bytes(&self) -> u64 {
        let limits = self.chunk_limits.lock().await;
        let total_size = (*limits.end() - *limits.start()) + 1;
        let p = self.progress.load(Ordering::SeqCst);
        if p >= total_size { 0 } else { total_size - p }
    }

    /// Attempts to split the remaining work of this chunk into two separate chunks.
    ///
    /// This is the core of the **Work Stealing** mechanism. If a chunk is large enough,
    /// it can be halved. The current chunk's range is reduced, and a new `Arc<Chunk>`
    /// is returned representing the latter half.
    ///
    /// Returns `None` if the remaining work is smaller than `MIN_SPLIT_SIZE * 2`.
    pub async fn split(self: &Arc<Self>) -> Option<Arc<Self>> {
        let mut limits = self.chunk_limits.lock().await;
        let current_start = *limits.start();
        let current_end = *limits.end();
        let total_size = (current_end - current_start) + 1;
        let current_progress = self.progress.load(Ordering::SeqCst);
        
        let remaining = if current_progress >= total_size { 0 } else { total_size - current_progress };
        
        if remaining < MIN_SPLIT_SIZE * 2 {
            return None;
        }

        // Calculate the absolute midpoint of the REMAINING work
        let split_offset = current_progress + (remaining / 2);
        let absolute_split_point = current_start + split_offset;

        // Create a new chunk for the latter half, starting with 0 progress
        let new_chunk = Arc::new(Chunk::new(absolute_split_point..=current_end, 0));

        // Shrink the current chunk's end limit to just before the split point
        *limits = current_start..=(absolute_split_point - 1);
        
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
) -> Result<(), anyhow::Error> {
    // Open a private handle for this worker to allow concurrent writes without locking
    let mut handle = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&target_path)
        .await?;

    loop {
        tokio::select! {
            // Respect global cancellation from the Manager
            _ = cancel_token.cancelled() => return Ok(()),
            // Execute the actual download loop
            res = perform_download(&client, &mut handle, &chunk, &url) => {
                return match res {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        // In this implementation, errors cause the worker to fail, 
                        // letting the manager decide whether to re-assign or stop.
                        Err(e)
                    }
                }
            }
        }
    }
}

/// The inner download loop that performs the actual HTTP Range request and streaming.
///
/// It handles "split-aware" writes, ensuring that if the chunk is split by another worker
/// while this one is downloading, it stops exactly at the new boundary.
async fn perform_download(
    client: &reqwest::Client,
    handle: &mut tokio::fs::File,
    chunk: &Chunk,
    url: &str,
) -> Result<(), anyhow::Error> {
    let mut retry_count = 0;
    let max_retries = 5;

    loop {
        // 1. Fetch current limits and progress. This handle resumes.
        let (start_offset, end_offset, current_progress) = {
            let limits = chunk.chunk_limits.lock().await;
            let cp = chunk.progress.load(Ordering::SeqCst);
            (*limits.start(), *limits.end(), cp)
        };

        let absolute_start = start_offset + current_progress;
        if absolute_start > end_offset {
            return Ok(());
        }

        // 2. Prepare the Range header for the request
        handle.seek(SeekFrom::Start(absolute_start)).await?;

        let response_res = timeout(TIMEOUT, client
            .get(url)
            .header("Range", format!("bytes={}-{}", absolute_start, end_offset))
            .send()).await;

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
            
            // 4. Re-check limits in case of a split during the stream
            let limits = chunk.chunk_limits.lock().await;
            let current_end = *limits.end();
            let current_abs_start = *limits.start() + chunk.progress.load(Ordering::SeqCst);
            
            if current_abs_start > current_end {
                return Ok(());
            }
            
            let bytes_to_write = if current_abs_start + packet.len() as u64 > current_end + 1 {
                (current_end + 1 - current_abs_start) as usize
            } else {
                packet.len()
            };

            handle.write_all(&packet[..bytes_to_write]).await?;
            chunk.progress.fetch_add(bytes_to_write as u64, Ordering::SeqCst);
            
            if bytes_to_write < packet.len() {
                return Ok(());
            }
        }
        
        let final_progress = chunk.progress.load(Ordering::SeqCst);
        let final_limits = chunk.chunk_limits.lock().await;
        if final_progress >= (*final_limits.end() - *final_limits.start()) + 1 {
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
        
        // Verify initial progress is stored correctly.
        assert_eq!(chunk.progress.load(Ordering::SeqCst), 10);
        
        // Verify the byte range limits are set accurately.
        let limits = chunk.chunk_limits.lock().await;
        assert_eq!(*limits.start(), 0);
        assert_eq!(*limits.end(), 99);
    }

    #[tokio::test]
    async fn test_remaining_bytes() {
        // Goal: Verify the dynamic calculation of remaining bytes based on progress and range.
        let chunk = Chunk::new(0..=99, 0);
        
        // Case 1: Fresh chunk (0 progress) should have full range remaining.
        assert_eq!(chunk.remaining_bytes().await, 100);

        // Case 2: Partial progress.
        chunk.progress.store(50, Ordering::SeqCst);
        assert_eq!(chunk.remaining_bytes().await, 50);

        // Case 3: Fully completed chunk.
        chunk.progress.store(100, Ordering::SeqCst);
        assert_eq!(chunk.remaining_bytes().await, 0);

        // Case 4: Progress exceeding range (should be clamped to 0 remaining).
        chunk.progress.store(150, Ordering::SeqCst);
        assert_eq!(chunk.remaining_bytes().await, 0);
    }

    #[tokio::test]
    async fn test_chunk_split_too_small() {
        // Goal: Ensure chunks below the MIN_SPLIT_SIZE threshold are NOT split.
        // Range is 1MB, which is significantly less than the 20MB required for a split.
        let chunk = Arc::new(Chunk::new(0..=(1024 * 1024 - 1), 0));
        let new_chunk = chunk.split().await;
        
        assert!(new_chunk.is_none(), "Chunk should not split if below size threshold");
    }

    #[tokio::test]
    async fn test_chunk_split_success() {
        // Goal: Verify a successful split of a large chunk into two halves.
        // Range is 30MB (sufficient for a split).
        let total_size = 30 * 1024 * 1024;
        let chunk = Arc::new(Chunk::new(0..=(total_size - 1), 0));
        
        let new_chunk = chunk.split().await.expect("Should split 30MB chunk");
        
        let original_limits = chunk.chunk_limits.lock().await;
        let new_limits = new_chunk.chunk_limits.lock().await;

        // The split point should be at exactly 15MB (midpoint).
        let expected_split_point = 15 * 1024 * 1024;
        
        // Original chunk should now cover the first 15MB.
        assert_eq!(*original_limits.start(), 0);
        assert_eq!(*original_limits.end(), (expected_split_point - 1) as u64);
        
        // New chunk should cover the remaining 15MB.
        assert_eq!(*new_limits.start(), expected_split_point as u64);
        assert_eq!(*new_limits.end(), total_size - 1);
        
        // New chunk must start with 0 progress.
        assert_eq!(new_chunk.progress.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_chunk_split_with_progress() {
        // Goal: Verify that a split correctly accounts for current progress.
        // Range 0..=30MB, but 10MB already downloaded. 20MB remaining.
        let total_size = 30 * 1024 * 1024;
        let progress = 10 * 1024 * 1024;
        let chunk = Arc::new(Chunk::new(0..=(total_size - 1), progress));

        let new_chunk = chunk.split().await.expect("Should split chunk with remaining 20MB");
        
        let original_limits = chunk.chunk_limits.lock().await;
        let new_limits = new_chunk.chunk_limits.lock().await;

        // The split should happen in the MIDDLE of the REMAINING 20MB.
        // Split point = progress (10MB) + half-remaining (10MB) = 20MB.
        let expected_split_point = 20 * 1024 * 1024;

        assert_eq!(*original_limits.end(), (expected_split_point - 1) as u64);
        assert_eq!(*new_limits.start(), expected_split_point as u64);
    }
}
