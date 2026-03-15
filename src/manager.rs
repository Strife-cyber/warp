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
    /// A queue of byte chunks that are WAITING to be picked up by a worker.
    pub chunks: Mutex<VecDeque<Arc<Chunk>>>,
    /// A list of chunks that are CURRENTLY being processed by workers.
    pub active_chunks: Mutex<Vec<Arc<Chunk>>>,
}

impl Metadata {
    /// Creates fresh metadata for a new download.
    pub fn new(url: String, size: u64) -> Self {
        let mut chunks = VecDeque::new();
        chunks.push_back(Arc::new(Chunk::new(0..=(size - 1), 0)));

        Self {
            url,
            size,
            chunks: Mutex::new(chunks),
            active_chunks: Mutex::new(Vec::new()),
        }
    }

    /// Calculates the total bytes downloaded across both waiting and active chunks.
    pub async fn total_progress(&self) -> u64 {
        let mut total = 0;

        // Count progress from waiting chunks (usually 0 or partial if re-queued)
        let queue = self.chunks.lock().await;
        for c in queue.iter() {
            total += c.progress.load(Ordering::Relaxed);
        }
        drop(queue);

        // Count progress from active chunks (the most important part)
        let active = self.active_chunks.lock().await;
        for c in active.iter() {
            total += c.progress.load(Ordering::Relaxed);
        }

        total
    }
}

/// The Orchestrator for the entire download process.
pub struct Manager {
    /// Shared state across all workers and the heartbeat.
    pub metadata: Arc<Metadata>,
    /// Reusable HTTP client for all worker tasks.
    client: Arc<reqwest::Client>,
    /// Master switch to stop all operations (workers and heartbeat).
    cancel_token: tokio_util::sync::CancellationToken,
    /// Local path where the file will be saved.
    pub target_path: std::path::PathBuf,
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
                    Metadata::new(url, 1024 * 1024 * 100) 
                }
            }
        } else {
            println!("No .warp file found for {}, starting fresh download.", target_path.display());
            Metadata::new(url, 1024 * 1024 * 100) 
        };

        Ok(Self::new(metadata, target_path))
    }

    pub fn new(metadata: Metadata, target_path: std::path::PathBuf) -> Self {
        Self {
            metadata: Arc::new(metadata),
            client: Arc::new(reqwest::Client::new()),
            cancel_token: tokio_util::sync::CancellationToken::new(),
            target_path,
        }
    }

    /// Starts and manages the download lifecycle.
    pub async fn run(&mut self, suggested_workers: usize, semaphore: Arc<Semaphore>) -> Result<(), anyhow::Error> {
        if !self.target_path.exists() {
            let file = tokio::fs::File::create(&self.target_path).await?;
            file.set_len(self.metadata.size).await?;
        }

        self.reconcile_chunks(suggested_workers).await;

        let hb_metadata = Arc::clone(&self.metadata);
        let hb_token = self.cancel_token.clone();
        let hb_path = self.target_path.with_extension("warp");
        let hb_path_clone = hb_path.clone();

        tokio::spawn(async move {
            if let Err(e) = crate::beat::start_heartbeat_sync(hb_metadata, hb_token, &hb_path_clone).await {
                eprintln!("Heartbeat failed: {}", e);
            }
        });

        // Progress logging task
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

        let mut workers = JoinSet::new();

        loop {
            // CRITICAL: We only lock the queue to decide what to do next, then drop it immediately.
            let mut queue = self.metadata.chunks.lock().await;

            // Cleanup phase
            while let Some(front) = queue.front() {
                if front.remaining_bytes().await == 0 {
                    queue.pop_front();
                } else {
                    break;
                }
            }

            if queue.is_empty() {
                if let Some(new_chunk) = self.try_steal_work(&mut queue).await {
                    queue.push_back(new_chunk);
                } else if workers.is_empty() {
                    break; 
                }
            }

            if let Some(chunk) = queue.pop_front() {
                let permit = semaphore.clone().acquire_owned().await?;
                let client = Arc::clone(&self.client);
                let path = self.target_path.clone();
                let url = self.metadata.url.clone();
                let token = self.cancel_token.clone();
                let meta = Arc::clone(&self.metadata);
                let chunk_clone = Arc::clone(&chunk);

                // Track as active
                meta.active_chunks.lock().await.push(Arc::clone(&chunk));

                workers.spawn(async move {
                    let _permit = permit;
                    let res = download_worker(client, path, chunk, url, token).await;

                    // Remove from active once done
                    let mut active = meta.active_chunks.lock().await;
                    active.retain(|c| !Arc::ptr_eq(c, &chunk_clone));

                    res
                });
            }

            // Drop the lock explicitly before select! to avoid starvation
            drop(queue);

            tokio::select! {
                result = workers.join_next(), if !workers.is_empty() => {
                    if let Some(res) = result {
                        match res {
                            Ok(Ok(())) => {},
                            Ok(Err(e)) => {
                                eprintln!("Worker error on {}: {}", self.target_path.display(), e);
                            }
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }

        self.cancel_token.cancel();
        let _ = tokio::fs::remove_file(hb_path).await;
        println!("Download complete: {}", self.target_path.display());

        Ok(())
    }

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
                    break;
                }
            } else {
                break;
            }
        }
    }

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
        let url = "http://example.com".to_string();
        let size = 1000;
        let metadata = Metadata::new(url.clone(), size);
        assert_eq!(metadata.url, url);
        assert_eq!(metadata.size, size);
        let chunks = metadata.chunks.lock().await;
        assert_eq!(chunks.len(), 1);
    }

    #[tokio::test]
    async fn test_manager_resume_logic() {
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("download.zip");
        let warp_path = target_path.with_extension("warp");

        let metadata = Metadata::new("http://test.com".to_string(), 5000);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(2500, Ordering::SeqCst);
        }
        crate::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let manager = Manager::from_url("http://test.com".to_string(), target_path).await.unwrap();
        assert_eq!(manager.metadata.total_progress().await, 2500);
    }
}

