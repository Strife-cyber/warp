use std::sync::Arc;
use std::collections::VecDeque;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use std::sync::atomic::Ordering;
use crate::segment::{download_worker, Chunk};

/// Holds the essential metadata for a download session.
pub struct Metadata {
    /// The source URL of the file.
    pub url: String,
    /// The total size of the file in bytes.
    pub size: u64,
    /// A queue of byte chunks that need to be downloaded.
    /// Chunks are wrapped in a Mutex-protected VecDeque to allow the Manager
    /// to dynamically add, remove, or split them during orchestration.
    pub chunks: Mutex<VecDeque<Arc<Chunk>>>,
}

impl Metadata {
    /// Creates fresh metadata for a new download.
    /// The Manager will automatically split the initial chunk based on CPU cores.
    pub fn new(url: String, size: u64) -> Self {
        let mut chunks = VecDeque::new();
        chunks.push_back(Arc::new(Chunk::new(0..=(size - 1), 0)));

        Self {
            url,
            size,
            chunks: Mutex::new(chunks),
        }
    }

    /// Calculates the total bytes downloaded across all chunks.
    pub async fn total_progress(&self) -> u64 {
        let chunks = self.chunks.lock().await;
        let mut total = 0;
        for c in chunks.iter() {
            total += c.progress.load(Ordering::Relaxed);
        }
        total
    }
}

/// The Orchestrator for the entire download process.
///
/// The `Manager` is responsible for:
/// 1.  **Work Distribution:** Managing a shared queue of chunks.
/// 2.  **Resilience:** Starting the heartbeat task to persist progress snapshots.
/// 3.  **Dynamic Adaptation:** Splitting chunks if there are fewer chunks than available workers.
pub struct Manager {
    /// Shared state across all workers and the heartbeat.
    metadata: Arc<Metadata>,
    /// Reusable HTTP client for all worker tasks.
    client: Arc<reqwest::Client>,
    /// Master switch to stop all operations (workers and heartbeat).
    cancel_token: tokio_util::sync::CancellationToken,
    /// Local path where the file will be saved.
    target_path: std::path::PathBuf,
}

impl Manager {
    /// Initializes a Manager, automatically attempting to resume from a .warp file if it exists.
    pub async fn from_url(url: String, target_path: std::path::PathBuf) -> Result<Self, anyhow::Error> {
        let warp_path = target_path.with_extension("warp");

        let metadata = if warp_path.exists() {
            println!("Found .warp file, attempting to resume {}...", target_path.display());
            match crate::beat::load_snapshot(&warp_path).await {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Failed to load snapshot for {}: {}. Starting fresh.", target_path.display(), e);
                    // In a real app, you'd fetch the file size via a HEAD request first
                    Metadata::new(url, 1024 * 1024 * 100) 
                }
            }
        } else {
            println!("No .warp file found for {}, starting fresh download.", target_path.display());
            // In a real app, you'd fetch the file size via a HEAD request first
            Metadata::new(url, 1024 * 1024 * 100) 
        };

        Ok(Self::new(metadata, target_path))
    }

    /// Creates a new Manager instance from existing metadata.
    pub fn new(
        metadata: Metadata,
        target_path: std::path::PathBuf,
    ) -> Self {
        Self {
            metadata: Arc::new(metadata),
            client: Arc::new(reqwest::Client::new()),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            target_path,
        }
    }

    /// Starts and manages the download lifecycle, sharing a global worker semaphore.
    pub async fn run(&mut self, suggested_workers: usize, semaphore: Arc<Semaphore>) -> Result<(), anyhow::Error> {
        // 1. Pre-allocate or open the target file to ensure we have disk space
        if !self.target_path.exists() {
            let file = tokio::fs::File::create(&self.target_path).await?;
            file.set_len(self.metadata.size).await?;
        }

        // 2. Resource Reconciliation: Split chunks if we have more workers than work.
        self.reconcile_chunks(suggested_workers).await;

        // 3. Start heartbeat (snapshot persistence)
        let hb_metadata = Arc::clone(&self.metadata);
        let hb_token = self.cancel_token.clone();
        let hb_path = self.target_path.with_extension("warp");
        let hb_target = self.target_path.clone();

        tokio::spawn(async move {
            if let Err(e) = crate::beat::start_heartbeat_sync(hb_metadata, hb_token, &hb_target).await {
                eprintln!("Heartbeat failed: {}", e);
            }
        });

        // 4. Start progress logging
        let log_metadata = Arc::clone(&self.metadata);
        let log_token = self.cancel_token.clone();
        let log_target = self.target_path.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let prog = log_metadata.total_progress().await;
                        let percent = (prog as f64 / log_metadata.size as f64) * 100.0;
                        println!("[{}] Progress: {:.2}% ({}/{} bytes)", 
                            log_target.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"),
                            percent, prog, log_metadata.size);
                    }
                    _ = log_token.cancelled() => break,
                }
            }
        });

        // 5. Worker Pool Loop using shared global semaphore
        let mut workers = JoinSet::new();

        loop {
            let mut queue = self.metadata.chunks.lock().await;

            // Cleanup phase: remove fully completed chunks from the queue
            while let Some(front) = queue.front() {
                if front.remaining_bytes().await == 0 {
                    queue.pop_front();
                } else {
                    break;
                }
            }

            // Check if we are done or need to steal work
            if queue.is_empty() {
                if let Some(new_chunk) = self.try_steal_work(&mut queue).await {
                    queue.push_back(new_chunk);
                } else if workers.is_empty() {
                    break; // No work left in the queue and no active workers
                }
            }

            // Spawning phase: if we have work and a free worker slot (permit)
            if let Some(chunk) = queue.pop_front() {
                let permit = semaphore.clone().acquire_owned().await?;
                let client = Arc::clone(&self.client);
                let path = self.target_path.clone();
                let url = self.metadata.url.clone();
                let token = self.cancel_token.clone();

                workers.spawn(async move {
                    let _permit = permit; // Permit is held until this future resolves
                    download_worker(client, path, chunk, url, token).await
                });
            } else if workers.is_empty() {
                break;
            }

            // Orchestration phase: wait for events or check for work periodically
            tokio::select! {
                result = workers.join_next(), if !workers.is_empty() => {
                    if let Some(res) = result {
                        match res {
                            Ok(Ok(())) => {}, // Worker finished its current chunk successfully
                            Ok(Err(e)) => {
                                eprintln!("Worker error on {}: {}", self.target_path.display(), e);
                            }
                            Err(e) => return Err(e.into()), // Task panicked
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Check for work-stealing opportunities
                }
            }
        }

        // 6. Cleanup: Stop heartbeat and remove the temporary .warp file
        self.cancel_token.cancel();
        let _ = tokio::fs::remove_file(hb_path).await;
        println!("Download complete: {}", self.target_path.display());

        Ok(())
    }

    /// Splits large chunks until the number of available chunks is at least equal
    /// to the target worker count. This is used during startup to maximize concurrency.
    async fn reconcile_chunks(&self, target_count: usize) {
        let mut queue = self.metadata.chunks.lock().await;
        while queue.len() < target_count {
            let mut largest_idx = None;
            let mut max_rem = 0;

            for (i, chunk) in queue.iter().enumerate() {
                let rem = chunk.remaining_bytes().await;
                if rem > max_rem {
                    max_rem = rem;
                    largest_idx = Some(i);
                }
            }

            if let Some(idx) = largest_idx {
                if let Some(new_chunk) = queue[idx].split().await {
                    queue.push_back(new_chunk);
                } else {
                    break; // No chunks are large enough to be split further
                }
            } else {
                break;
            }
        }
    }

    /// Placeholder for active work-stealing from currently running workers.
    /// In the current implementation, reconciliation happens at the start and
    /// between task completions.
    async fn try_steal_work(&self, _queue: &mut VecDeque<Arc<Chunk>>) -> Option<Arc<Chunk>> {
        None
    }
}

use tokio::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_metadata_new() {
        // Goal: Ensure fresh Metadata is created with a single chunk covering the entire range.
        let url = "http://example.com".to_string();
        let size = 1000;
        let metadata = Metadata::new(url.clone(), size);

        // Verify core properties.
        assert_eq!(metadata.url, url);
        assert_eq!(metadata.size, size);

        // Verify the initial chunk structure.
        let chunks = metadata.chunks.lock().await;
        assert_eq!(chunks.len(), 1, "Metadata should start with exactly one chunk");

        let chunk_limits = chunks[0].chunk_limits.lock().await;
        assert_eq!(*chunk_limits.start(), 0);
        assert_eq!(*chunk_limits.end(), 999);
    }

    #[tokio::test]
    async fn test_manager_new() {
        // Goal: Ensure the Manager is correctly initialized with provided metadata and path.
        let url = "http://example.com".to_string();
        let metadata = Metadata::new(url, 1000);
        let target_path = PathBuf::from("test.mp4");
        let manager = Manager::new(metadata, target_path.clone());

        // Verify state initialization.
        assert_eq!(manager.target_path, target_path);
        assert!(!manager.cancel_token.is_cancelled(), "Manager should start in an active state");
    }

    #[tokio::test]
    async fn test_reconcile_chunks() {
        // Goal: Verify that the Manager can automatically split an initial large chunk 
        // to match a target worker count (Resource Reconciliation).

        // Create 100MB metadata (starts as 1 chunk).
        let metadata = Metadata::new("url".to_string(), 100 * 1024 * 1024);
        let manager = Manager::new(metadata, PathBuf::from("test"));

        // Scenario: We have 4 available worker slots. 
        // reconcile_chunks should split the single 100MB chunk until at least 4 chunks exist.
        manager.reconcile_chunks(4).await;

        let chunks = manager.metadata.chunks.lock().await;
        assert!(chunks.len() >= 4, "Reconciliation should have increased chunk count to at least 4");

        // Ensure no data loss: total size of all chunks should still be 100MB.
        let mut total_range_sum = 0;
        for c in chunks.iter() {
            let limits = c.chunk_limits.lock().await;
            total_range_sum += (*limits.end() - *limits.start()) + 1;
        }
        assert_eq!(total_range_sum, 100 * 1024 * 1024);
    }

    #[tokio::test]
    async fn test_manager_resume_logic() {
        // Goal: Ensure Manager::from_url correctly loads a .warp file if it exists.
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("download.zip");
        let warp_path = target_path.with_extension("warp");

        // 1. Manually create a .warp file.
        let metadata = Metadata::new("http://test.com".to_string(), 5000);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(2500, Ordering::SeqCst);
        }
        crate::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        // 2. Load manager using from_url.
        let manager = Manager::from_url("http://test.com".to_string(), target_path).await.unwrap();

        // 3. Verify progress was restored.
        assert_eq!(manager.metadata.total_progress().await, 2500);
    }
}
