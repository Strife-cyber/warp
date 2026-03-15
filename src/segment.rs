use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio_util::sync::CancellationToken;
use futures_util::StreamExt;
use tokio::time::timeout;
use std::time::Duration;

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

        let response = timeout(TIMEOUT, client
            .get(url)
            .header("Range", format!("bytes={}-{}", absolute_start, end_offset))
            .send()).await??;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Server returned {}: {}", response.status(), url));
        }

        // 3. Process the byte stream
        let mut stream = response.bytes_stream();
        while let Some(item) = stream.next().await {
            let packet = item?;
            
            // 4. Re-check limits in case of a split during the stream
            // If the chunk was split, the new 'end' might be before what we are currently writing.
            let limits = chunk.chunk_limits.lock().await;
            let current_end = *limits.end();
            let current_abs_start = *limits.start() + chunk.progress.load(Ordering::SeqCst);
            
            if current_abs_start > current_end {
                // The chunk was split, and we've already exceeded the new boundary.
                // Stop here; the newly created chunk will handle the rest.
                return Ok(());
            }
            
            // Calculate how many bytes from the packet are still within our current range
            let bytes_to_write = if current_abs_start + packet.len() as u64 > current_end + 1 {
                (current_end + 1 - current_abs_start) as usize
            } else {
                packet.len()
            };

            handle.write_all(&packet[..bytes_to_write]).await?;
            chunk.progress.fetch_add(bytes_to_write as u64, Ordering::SeqCst);
            
            if bytes_to_write < packet.len() {
                // We reached the new limit imposed by a split
                return Ok(());
            }
        }
        
        // Final verification: If we completed the stream, check if we actually finished the chunk.
        let final_progress = chunk.progress.load(Ordering::SeqCst);
        let final_limits = chunk.chunk_limits.lock().await;
        if final_progress >= (*final_limits.end() - *final_limits.start()) + 1 {
            return Ok(());
        }
    }
}
