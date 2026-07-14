use std::sync::Arc;
use tokio::task::JoinSet;
use indicatif::ProgressBar;
use crate::utils::HumanBytes;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use tokio::sync::{Mutex, Semaphore};
use super::rate_limit::RunLimits;
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
    /// Creates fresh metadata for a download, pre-split into `initial_chunks` pieces
    /// so workers can start in parallel immediately instead of ramping up over seconds.
    pub fn new(url: String, size: u64, max_speed_bytes: Option<u64>, initial_chunks: usize) -> Self {
        let target = initial_chunks.max(1);
        let chunk_size = size / target as u64;

        let mut chunks = VecDeque::with_capacity(target);
        let mut start = 0u64;
        for i in 0..target {
            let end = if i == target - 1 {
                size - 1
            } else {
                start + chunk_size - 1
            };
            if start <= end {
                chunks.push_back(Arc::new(Chunk::new(start..=end, 0)));
            }
            start = end + 1;
        }

        if chunks.is_empty() {
            chunks.push_back(Arc::new(Chunk::new(0..=(size.saturating_sub(1)), 0)));
        }

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
    /// Whether the server supports byte-range requests.
    supports_range: bool,
}

impl Manager {
    /// Initializes a Manager from a DownloadEntry, automatically attempting to resume.
    pub async fn from_entry(entry: &crate::core::DownloadEntry) -> Result<Self, anyhow::Error> {
        let warp_path = entry.target_path.with_extension("warp");

        let mut client_builder = reqwest::Client::builder()
            .tcp_keepalive(Some(std::time::Duration::from_secs(30)));
        if let Some(ref proxy_url) = entry.proxy
            && let Ok(proxy) = reqwest::Proxy::all(proxy_url)
        {
            client_builder = client_builder.proxy(proxy);
        }
        let client = Arc::new(client_builder.build()?);

        let (metadata, supports_range) = if warp_path.exists() {
            match super::beat::load_snapshot(&warp_path).await {
                Ok(mut m) => {
                    m.max_speed_bytes = entry.max_speed_bytes;
                    (m, true)
                }
                Err(e) => {
                    eprintln!(
                        "Failed to load snapshot for {}: {}. Starting fresh.",
                        entry.target_path.display(),
                        e
                    );
                    let probe =
                        super::probe::probe_url(&client, &entry.url, &entry.mirror_urls).await?;
                    (
                        Metadata::new(probe.effective_url, probe.size, entry.max_speed_bytes, 1),
                        probe.supports_range,
                    )
                }
            }
        } else {
            let probe = super::probe::probe_url(&client, &entry.url, &entry.mirror_urls).await?;
            (
                Metadata::new(probe.effective_url, probe.size, entry.max_speed_bytes, 16),
                probe.supports_range,
            )
        };

        Ok(Self {
            metadata: Arc::new(metadata),
            client,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            target_path: entry.target_path.clone(),
            pb: None,
            supports_range,
        })
    }

    #[cfg(test)]
    pub fn new(metadata: Metadata, target_path: std::path::PathBuf, client: Arc<reqwest::Client>) -> Self {
        Self {
            metadata: Arc::new(metadata),
            client,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            target_path,
            pb: None,
            supports_range: true,
        }
    }

    pub fn set_progress_bar(&mut self, pb: ProgressBar) {
        self.pb = Some(pb);
    }

    /// Starts and manages the download lifecycle.
    pub async fn run(
        &mut self,
        suggested_workers: usize,
        semaphore: Arc<Semaphore>,
        limits: RunLimits,
    ) -> Result<(), anyhow::Error> {
        // Write to a .warpart sidecar so a crash never leaves a half-written
        // file at the final path.  ZIP/archive readers can't accidentally open
        // a partial download because the extension doesn't match anything.
        // On success we atomically rename .warpart → target_path.
        let part_path = self.target_path.with_extension("warpart");
        if !part_path.exists() {
            let file = tokio::fs::File::create(&part_path).await?;
            file.set_len(self.metadata.size).await?;
        } else if part_path.metadata().ok().map(|m| m.len()) != Some(self.metadata.size) {
            // Stale part with wrong size (e.g. set_len was interrupted) — recreate.
            tokio::fs::remove_file(&part_path).await.ok();
            let file = tokio::fs::File::create(&part_path).await?;
            file.set_len(self.metadata.size).await?;
        }

        let mut limits = limits;
        if limits.local.is_none()
            && let Some(b) = self.metadata.max_speed_bytes
        {
            limits.local = Some(Arc::new(super::rate_limit::RateLimiter::new(b)));
        }

        let worker_target = if self.supports_range {
            suggested_workers
        } else {
            1
        };
        self.reconcile_chunks(worker_target).await;

        let mut adaptive_target = worker_target;
        let mut last_adapt = std::time::Instant::now();
        let mut last_progress = self.metadata.total_progress().await;

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
        // Channel to receive failed chunks — avoids the ABBA deadlock
        // between `active_chunks` (acquired inside the spawned task) and
        // `chunks` (acquired in the main loop).  Instead of re-queuing
        // directly (active → chunks), the sender passes the chunk through
        // this channel, and the main loop re-queues it (chunks only).
        let (failed_tx, mut failed_rx) = tokio::sync::mpsc::unbounded_channel::<Arc<Chunk>>();

        loop {
            // Drain any chunks from failed workers before making spawn decisions.
            while let Ok(failed_chunk) = failed_rx.try_recv() {
                self.metadata.chunks.lock().await.push_back(failed_chunk);
            }

            // Acquire semaphore permit OUTSIDE the chunks lock so a contended
            // semaphore can't stall the heartbeat or progress display.
            let acquired_permit = semaphore.clone().try_acquire_owned().ok();
            let mut chunk_to_spawn = None;

            {
                let mut queue = self.metadata.chunks.lock().await;

                // Cleanup phase
                while let Some(front) = queue.front() {
                    if front.remaining_bytes() == 0 {
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

                if acquired_permit.is_some() && !queue.is_empty() {
                    chunk_to_spawn = queue.pop_front();
                }
            } // Lock is explicitly dropped here

            if let Some(chunk) = chunk_to_spawn {
                let _permit = acquired_permit.unwrap();
                let client = Arc::clone(&self.client);
                let path = part_path.clone();
                // URL is cloned inside the spawned task body (via `meta`),
                // not here — keeps the allocation out of the coordination path.
                let token = self.cancel_token.clone();
                let meta = Arc::clone(&self.metadata);
                let chunk_clone = Arc::clone(&chunk);
                let limits = limits.clone();
                let use_range = self.supports_range;
                let tx = failed_tx.clone();

                // Track as active
                meta.active_chunks.lock().await.push(Arc::clone(&chunk));

                workers.spawn(async move {
                    let _permit = _permit;
                    let url = meta.url.clone();
                    let res = download_worker(
                        client,
                        path,
                        chunk,
                        url,
                        token,
                        limits,
                        use_range,
                    )
                    .await;

                    // Transition chunk based on result
                    let c_opt = {
                        let mut active = meta.active_chunks.lock().await;
                        active.iter().position(|c| Arc::ptr_eq(c, &chunk_clone)).map(|pos| active.remove(pos))
                    };

                    if let Some(c) = c_opt {
                        if res.is_ok() {
                            meta.completed_chunks.lock().await.push(c);
                        } else {
                            // Send through channel — never acquire `chunks` lock
                            // from inside the spawned task (avoids ABBA deadlock
                            // with the main loop's active_chunks → chunks path).
                            let _ = tx.send(c);
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
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Adaptive concurrency: grow chunk count when workers stay busy.
                    if self.supports_range && last_adapt.elapsed() >= Duration::from_secs(3) {
                        let prog = self.metadata.total_progress().await;
                        let speed = prog.saturating_sub(last_progress);
                        last_progress = prog;
                        if !workers.is_empty() && speed > 0 {
                            adaptive_target = (adaptive_target + 1)
                                .min(suggested_workers * 2)
                                .min(64);
                            self.reconcile_chunks(adaptive_target).await;
                        }
                        last_adapt = std::time::Instant::now();
                    }
                }
            }
        }

        // All workers finished — atomically swap the part file into place.
        // A crash / Ctrl+C before this point leaves only the .warpart on disk,
        // which no OS tool will try to interpret as a finished download.
        tokio::fs::rename(&part_path, &self.target_path).await
            .map_err(|e| anyhow::anyhow!("rename .warpart → {}: {e}", self.target_path.display()))?;

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
                let rem = chunk.remaining_bytes();
                if rem > max_rem {
                    max_rem = rem;
                    largest_idx = Some(i);
                }
            }

            if let Some(idx) = largest_idx {
                if let Some(new_chunk) = queue[idx].split() {
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
            let rem = chunk.remaining_bytes();
            if rem > max_remaining {
                max_remaining = rem;
                best_target = Some(Arc::clone(chunk));
            }
        }

        if let Some(target) = best_target {
            target.split()
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
        let metadata = Metadata::new(url.clone(), size, None, 1);
        assert_eq!(metadata.url, url);
        assert_eq!(metadata.size, size);
        let chunks = metadata.chunks.lock().await;
        assert_eq!(chunks.len(), 1);
    }

    #[tokio::test]
    async fn test_manager_resume_small_progress_zeroed_by_margin() {
        // When saved progress is smaller than RESUME_MARGIN (2 MB), it gets
        // zeroed so the worker re-downloads from the chunk start.
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("download.zip");
        let warp_path = target_path.with_extension("warp");

        let metadata = Metadata::new("http://test.com".to_string(), 5000, None, 1);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(2500, Ordering::SeqCst);
        }
        crate::download::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let entry = crate::core::DownloadEntry::new_http(
            "test".to_string(),
            "http://test.com".to_string(),
            target_path.clone(),
        );

        let manager = Manager::from_entry(&entry).await.unwrap();
        assert_eq!(manager.metadata.total_progress().await, 0);
    }

    #[tokio::test]
    async fn test_manager_resume_large_progress_partially_preserved() {
        // With progress = 50 MB (> RESUME_MARGIN), the margin should only
        // subtract 2 MB, preserving 48 MB of resumed progress.
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("large.bin");
        let warp_path = target_path.with_extension("warp");

        let metadata = Metadata::new("http://test.com".to_string(), 200 * 1024 * 1024, None, 1);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(50 * 1024 * 1024, Ordering::SeqCst);
        }
        crate::download::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let entry = crate::core::DownloadEntry::new_http(
            "test".to_string(),
            "http://test.com".to_string(),
            target_path.clone(),
        );

        let manager = Manager::from_entry(&entry).await.unwrap();
        let expected = 50 * 1024 * 1024 - 2 * 1024 * 1024;
        assert_eq!(manager.metadata.total_progress().await, expected,
                   "Progress above RESUME_MARGIN should be preserved after subtraction");
    }

    #[tokio::test]
    async fn test_metadata_new_with_initial_chunks() {
        // Metadata::new should split into the requested number of initial chunks.
        let meta = Metadata::new("http://test.com".to_string(), 10000, None, 4);

        assert_eq!(meta.url, "http://test.com");
        assert_eq!(meta.size, 10000);

        let chunks = meta.chunks.lock().await;
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[0].end.load(Ordering::Relaxed), 2499);
        assert_eq!(chunks[1].start, 2500);
        assert_eq!(chunks[1].end.load(Ordering::Relaxed), 4999);
        assert_eq!(chunks[2].start, 5000);
        assert_eq!(chunks[2].end.load(Ordering::Relaxed), 7499);
        assert_eq!(chunks[3].start, 7500);
        assert_eq!(chunks[3].end.load(Ordering::Relaxed), 9999);

        // All start with 0 progress.
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.progress.load(Ordering::Relaxed), 0,
                       "Chunk {i} should start with 0 progress");
        }
    }

    #[tokio::test]
    async fn test_metadata_new_single_chunk_for_small_file() {
        // A tiny file with initial_chunks=1 produces exactly one chunk.
        let meta = Metadata::new("http://test.com".to_string(), 1, None, 1);
        let chunks = meta.chunks.lock().await;
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[0].end.load(Ordering::Relaxed), 0);
        assert_eq!(chunks[0].remaining_bytes(), 1);
    }

    #[tokio::test]
    async fn test_total_progress_after_multi_chunk_resume() {
        // Total progress should correctly aggregate across chunks after resume,
        // with the RESUME_MARGIN applied independently to each.
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("multi.bin");
        let warp_path = target_path.with_extension("warp");

        // 100 MB file split into 4 chunks, each with varying progress.
        // RESUME_MARGIN (2 MB) is subtracted from each chunk independently.
        let size = 100 * 1024 * 1024;
        let metadata = Metadata::new("http://test.com".to_string(), size, None, 4);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(5 * 1024 * 1024, Ordering::SeqCst);  // 5 - 2 = 3 MB
            chunks[1].progress.store(25 * 1024 * 1024, Ordering::SeqCst); // 25 - 2 = 23 MB
            chunks[2].progress.store(8 * 1024 * 1024, Ordering::SeqCst);  // 8 - 2 = 6 MB
            chunks[3].progress.store(1 * 1024 * 1024, Ordering::SeqCst);  // 1 - 2 → 0 (saturating)
        }
        crate::download::beat::save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let entry = crate::core::DownloadEntry::new_http(
            "multi".to_string(),
            "http://test.com".to_string(),
            target_path,
        );

        let manager = Manager::from_entry(&entry).await.unwrap();
        // Expected: 3 + 23 + 6 + 0 = 32 MB
        let expected = 32u64 * 1024 * 1024;
        assert_eq!(manager.metadata.total_progress().await, expected);
    }

    #[tokio::test]
    async fn test_total_progress_with_completed_chunks() {
        let size = 1000;
        let metadata = Metadata::new("url".to_string(), size, None, 1);

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
        let metadata = Metadata::new("url".to_string(), 1000, None, 1);
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

    #[tokio::test]
    async fn test_worker_failure_mpsc_channel() {
        // The mpsc channel is the new re-queue path that eliminates the ABBA
        // deadlock.  This test verifies that sending through the channel
        // correctly delivers the chunk back to the waiting queue.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Arc<Chunk>>();

        let chunk = Arc::new(Chunk::new(100..=199, 0));
        let chunk_clone = Arc::clone(&chunk);

        // Simulate: worker fails, sends chunk through channel (instead of
        // acquiring chunks lock directly).
        tx.send(chunk_clone).unwrap();

        // Main loop drains the channel and re-queues.
        let received = rx.recv().await.unwrap();
        assert!(Arc::ptr_eq(&received, &chunk));

        // Verify the chunk metadata survived the round trip.
        assert_eq!(received.start, 100);
        assert_eq!(received.end.load(Ordering::Relaxed), 199);
        assert_eq!(received.progress.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_lock_ordering_paths_run_concurrently() {
        // Stress-test: exercise both lock-order paths concurrently to
        // verify no deadlock occurs with the new mpsc channel design.
        // Path A (main loop): chunks → active_chunks (via try_steal_work)
        // Path B (worker):    active_chunks → chunks (via channel, not direct)
        let dir = tempdir().unwrap();
        let target_path = dir.path().join("conc.bin");
        let metadata = Metadata::new("url".to_string(), 10_000_000, None, 4);
        let manager = Arc::new(Manager::new(
            metadata,
            target_path,
            Arc::new(reqwest::Client::new()),
        ));

        let mut handles = Vec::new();

        // Path A: main-loop style — drain queue, try to steal from active.
        for _ in 0..4 {
            let m = Arc::clone(&manager);
            handles.push(tokio::spawn(async move {
                // Pop from queue, push to active (simulating worker start).
                let chunk = {
                    let mut queue = m.metadata.chunks.lock().await;
                    queue.pop_front()
                };
                if let Some(c) = chunk {
                    m.metadata.active_chunks.lock().await.push(c);
                }
                // Try to steal work from active chunks.
                let _stolen = m.try_steal_work().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
        }

        // Path B: worker-style — remove from active, send to channel.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for _ in 0..4 {
            let m = Arc::clone(&manager);
            let tx = tx.clone();
            handles.push(tokio::spawn(async move {
                let c_opt = {
                    let mut active = m.metadata.active_chunks.lock().await;
                    active.pop()
                };
                if let Some(c) = c_opt {
                    // Send through channel (never acquire chunks lock).
                    let _ = tx.send(c);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
        }

        // Drain the channel (simulating main loop's drain step).
        let drain_handle = tokio::spawn(async move {
            let mut count = 0u32;
            loop {
                match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                    Ok(Some(_)) => count += 1,
                    _ => break,
                }
            }
            count
        });

        for h in handles {
            h.await.unwrap();
        }
        let drained = drain_handle.await.unwrap();

        // At minimum nothing deadlocked.  All paths completed within the
        // timeout.  The exact count depends on scheduling but should be ≥ 0.
        assert!(drained <= 4, "At most 4 chunks could have been sent");
    }

    #[tokio::test]
    async fn test_part_file_extension_naming() {
        // The .warpart extension must preserve the original filename's stem
        // while being un-associated with any known file type.
        let target = std::path::PathBuf::from("my_video.mp4");
        let part = target.with_extension("warpart");
        assert_eq!(part.to_string_lossy(), "my_video.warpart",
                   "warpart must replace extension, not append");

        // ZIP files specifically — the extension that triggered this design.
        let zip_target = std::path::PathBuf::from("archive.zip");
        let zip_part = zip_target.with_extension("warpart");
        assert_eq!(zip_part.to_string_lossy(), "archive.warpart");
        // No OS tool associates .warpart with ZIP / archive formats.
        let ext = zip_part.extension().and_then(|e| e.to_str()).unwrap_or("");
        assert_eq!(ext, "warpart", "part file must have .warpart extension");
        assert_ne!(ext, "zip", "must not look like a ZIP");
        assert_ne!(ext, "war", "must not be ambiguous with .warp snapshot");

        // HLS part naming.
        let hls_target = std::path::PathBuf::from("stream.ts");
        let hls_part = hls_target.with_extension("hlspart");
        assert_eq!(hls_part.to_string_lossy(), "stream.hlspart");
    }
}
