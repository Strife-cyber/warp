use std::sync::Arc;
use tokio::task::JoinSet;
use indicatif::ProgressBar;
use super::utils::HumanBytes;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use tokio::sync::{Mutex, Semaphore};
use super::segment::{download_worker, Chunk};

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
    /// A list of chunks that have COMPLETELY finished downloading.
    pub completed_chunks: Mutex<Vec<Arc<Chunk>>>,
    /// Optional limit on download speed (bytes per second)
    pub max_speed_bytes: Option<u64>,
}

impl Metadata {
    /// Creates fresh metadata for a new download.
    pub fn new(url: String, size: u64, max_speed_bytes: Option<u64>) -> Self {
        let mut chunks = VecDeque::new();
        chunks.push_back(Arc::new(Chunk::new(0..=(size - 1), 0)));

        Self {
            url,
            size,
            chunks: Mutex::new(chunks),
            active_chunks: Mutex::new(Vec::new()),
            completed_chunks: Mutex::new(Vec::new()),
            max_speed_bytes,
        }
    }

    /// Calculates the total bytes downloaded across waiting, active, and completed chunks.
    pub async fn total_progress(&self) -> u64 {
        let mut total = 0;

        // Count progress from waiting chunks (usually 0 or partial if re-queued)
        let queue = self.chunks.lock().await;
        for c in queue.iter() {
            total += c.progress.load(Ordering::Relaxed);
        }
        drop(queue);

        // Count progress from active chunks (currently being downloaded)
        let active = self.active_chunks.lock().await;
        for c in active.iter() {
            total += c.progress.load(Ordering::Relaxed);
        }
        drop(active);

        // Count progress from completed chunks
        let completed = self.completed_chunks.lock().await;
        for c in completed.iter() {
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
    pub cancel_token: tokio_util::sync::CancellationToken,
    /// Local path where the file will be saved.
    pub target_path: std::path::PathBuf,
    /// Progress bar for this download.
    pb: Option<ProgressBar>,
}

impl Manager {
    /// Fetches the Content-Length of a remote resource using a HEAD request.
    async fn fetch_content_length(client: &reqwest::Client, url: &str) -> Result<u64, anyhow::Error> {
        let response = client.head(url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to fetch content length for {}: Status {}", url, response.status()));
        }
        let size = response.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|val| val.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| anyhow::anyhow!("Content-Length header missing or invalid for {}", url))?;
        Ok(size)
    }

    /// Initializes a Manager from a DownloadEntry, automatically attempting to resume.
    pub async fn from_entry(entry: &super::registry::DownloadEntry) -> Result<Self, anyhow::Error> {
        let warp_path = entry.target_path.with_extension("warp");
        
        let mut client_builder = reqwest::Client::builder();
        if let Some(ref proxy_url) = entry.proxy {
            if let Ok(proxy) = reqwest::Proxy::all(proxy_url) {
                client_builder = client_builder.proxy(proxy);
            }
        }
        let client = Arc::new(client_builder.build()?);

        let metadata = if warp_path.exists() {
            match super::beat::load_snapshot(&warp_path).await {
                Ok(mut m) => {
                    m.max_speed_bytes = entry.max_speed_bytes;
                    m
                },
                Err(e) => {
                    eprintln!("Failed to load snapshot for {}: {}. Starting fresh.", entry.target_path.display(), e);
                    let size = Self::fetch_content_length(&client, &entry.url).await?;
                    Metadata::new(entry.url.clone(), size, entry.max_speed_bytes) 
                }
            }
        } else {
            let size = Self::fetch_content_length(&client, &entry.url).await?;
            Metadata::new(entry.url.clone(), size, entry.max_speed_bytes) 
        };

        Ok(Self::new(metadata, entry.target_path.clone(), client))
    }

    pub fn new(metadata: Metadata, target_path: std::path::PathBuf, client: Arc<reqwest::Client>) -> Self {
        Self {
            metadata: Arc::new(metadata),
            client,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            target_path,
            pb: None,
        }
    }

    pub fn set_progress_bar(&mut self, pb: ProgressBar) {
        self.pb = Some(pb);
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
            if let Err(e) = super::beat::start_heartbeat_sync(hb_metadata, hb_token, &hb_path_clone).await {
                eprintln!("Heartbeat failed: {}", e);
            }
        });

        // Progress logging task
        let log_metadata = Arc::clone(&self.metadata);
        let log_token = self.cancel_token.clone();
        let pb = self.pb.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            let mut last_progress = log_metadata.total_progress().await;
            
            if let Some(ref pbar) = pb {
                pbar.set_position(last_progress);
            }

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let prog = log_metadata.total_progress().await;
                        if let Some(ref pbar) = pb {
                            pbar.set_position(prog);
                            let delta = prog.saturating_sub(last_progress);
                            let speed = delta * 2; // Since we tick every 500ms
                            pbar.set_message(format!("{} /s", HumanBytes(speed)));
                        }
                        last_progress = prog;
                    }
                    _ = log_token.cancelled() => break,
                }
            }
        });

        let mut workers = JoinSet::new();

        loop {
            let mut chunk_to_spawn = None;
            let mut permit = None;

            {
                let mut queue = self.metadata.chunks.lock().await;

                // Cleanup phase
                while let Some(front) = queue.front() {
                    if front.remaining_bytes().await == 0 {
                        let chunk = queue.pop_front().unwrap();
                        self.metadata.completed_chunks.lock().await.push(chunk);
                    } else {
                        break;
                    }
                }

                if queue.is_empty() {
                    if let Some(new_chunk) = self.try_steal_work().await {
                        queue.push_back(new_chunk);
                    } else if workers.is_empty() {
                        break; 
                    }
                }

                if !queue.is_empty() {
                    if let Ok(p) = semaphore.clone().try_acquire_owned() {
                        chunk_to_spawn = queue.pop_front();
                        permit = Some(p);
                    }
                }
            } // Lock is explicitly dropped here

            if let Some(chunk) = chunk_to_spawn {
                let _permit = permit.unwrap();
                let client = Arc::clone(&self.client);
                let path = self.target_path.clone();
                let url = self.metadata.url.clone();
                let token = self.cancel_token.clone();
                let meta = Arc::clone(&self.metadata);
                let chunk_clone = Arc::clone(&chunk);

                // Track as active
                meta.active_chunks.lock().await.push(Arc::clone(&chunk));

                workers.spawn(async move {
                    let _permit = _permit;
                    let speed = meta.max_speed_bytes;
                    let res = download_worker(client, path, chunk, url, token, speed).await;

                    // Transition chunk based on result
                    let c_opt = {
                        let mut active = meta.active_chunks.lock().await;
                        if let Some(pos) = active.iter().position(|c| Arc::ptr_eq(c, &chunk_clone)) {
                            Some(active.remove(pos))
                        } else {
                            None
                        }
                    };

                    if let Some(c) = c_opt {
                        if res.is_ok() {
                            meta.completed_chunks.lock().await.push(c);
                        } else {
                            meta.chunks.lock().await.push_back(c);
                        }
                    }

                    res
                });
                
                // Continue loop immediately to spawn more workers if work and permits exist
                continue;
            }

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
        if let Some(ref pbar) = self.pb {
            pbar.finish_with_message("Done");
        }

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

    /// Attempts to steal work from one of the currently active worker chunks by splitting it.
    async fn try_steal_work(&self) -> Option<Arc<Chunk>> {
        let active = self.metadata.active_chunks.lock().await;
        
        // Find the active chunk with the most remaining bytes to split.
        let mut best_target: Option<Arc<Chunk>> = None;
        let mut max_remaining = 0;

        for chunk in active.iter() {
            let rem = chunk.remaining_bytes().await;
            if rem > max_remaining {
                max_remaining = rem;
                best_target = Some(Arc::clone(chunk));
            }
        }

        if let Some(target) = best_target {
            target.split().await
        } else {
            None
        }
    }
}

use tokio::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_metadata_new() {
        let url = "http://example.com".to_string();
        let size = 1000;
        let metadata = Metadata::new(url.clone(), size, None);
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

        let metadata = Metadata::new("http://test.com".to_string(), 5000, None);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(2500, Ordering::SeqCst);
        }
        super::super::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let mut registry = crate::downloader::registry::Registry::default();
        let id = registry.add("http://test.com".to_string(), target_path.clone());
        let entry = registry.downloads.get(&id).unwrap();

        let manager = Manager::from_entry(entry).await.unwrap();
        assert_eq!(manager.metadata.total_progress().await, 2500);
    }

    #[tokio::test]
    async fn test_total_progress_with_completed_chunks() {
        let size = 1000;
        let metadata = Metadata::new("url".to_string(), size, None);
        
        // 1. Initial progress is 0
        assert_eq!(metadata.total_progress().await, 0);

        // 2. Add progress to a waiting chunk
        {
            let queue = metadata.chunks.lock().await;
            queue[0].progress.store(100, Ordering::SeqCst);
        }
        assert_eq!(metadata.total_progress().await, 100);

        // 3. Move chunk to active
        let chunk = {
            let mut queue = metadata.chunks.lock().await;
            queue.pop_front().unwrap()
        };
        metadata.active_chunks.lock().await.push(Arc::clone(&chunk));
        assert_eq!(metadata.total_progress().await, 100);

        // 4. Move chunk to completed
        {
            let mut active = metadata.active_chunks.lock().await;
            active.pop();
        }
        metadata.completed_chunks.lock().await.push(chunk);
        
        // Progress should STILL be 100
        assert_eq!(metadata.total_progress().await, 100);
    }

    #[tokio::test]
    async fn test_worker_failure_requeue() {
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("test.bin");
        let metadata = Metadata::new("url".to_string(), 1000, None);
        let manager = Manager::new(metadata, target_path, Arc::new(reqwest::Client::new()));
        
        let chunk = {
            let mut queue = manager.metadata.chunks.lock().await;
            queue.pop_front().unwrap()
        };
        let chunk_clone = Arc::clone(&chunk);
        
        // Simulate worker starting
        manager.metadata.active_chunks.lock().await.push(Arc::clone(&chunk));
        
        // Simulate worker failing
        {
            let mut active = manager.metadata.active_chunks.lock().await;
            let pos = active.iter().position(|c| Arc::ptr_eq(c, &chunk_clone)).unwrap();
            let c = active.remove(pos);
            // Re-queue
            manager.metadata.chunks.lock().await.push_back(c);
        }
        
        // Verify it's back in the queue
        let queue = manager.metadata.chunks.lock().await;
        assert_eq!(queue.len(), 1);
        assert!(Arc::ptr_eq(&queue[0], &chunk_clone));
        
        // Verify active is empty
        let active = manager.metadata.active_chunks.lock().await;
        assert!(active.is_empty());
    }
}

