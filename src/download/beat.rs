use std::sync::Arc;
use std::path::Path;
use std::time::Duration;
use super::segment::Chunk;
use tokio::sync::MutexGuard;
use super::manager::Metadata;
use std::collections::VecDeque;
use std::sync::atomic::{Ordering};
use tokio_util::sync::CancellationToken;

/// A serializable snapshot of a single chunk's progress.
/// Used to save and resume downloads from a `.warp` file.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct ChunkSnapshot {
    /// Start byte of the range.
    pub start: u64,
    /// End byte of the range (inclusive).
    pub end: u64,
    /// Number of bytes downloaded within this range.
    pub progress: u64,
}

/// A serializable snapshot of the entire download state.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct MetadataSnapshot {
    /// Source URL.
    pub url: String,
    /// Total expected file size.
    pub size: u64,
    /// List of all chunk snapshots.
    pub chunks: Vec<ChunkSnapshot>,
}

/// Periodically saves the download state to a persistence file.
///
/// The heartbeat ensures that if the process crashes, the download can be resumed
/// from the last saved state with minimal data loss.
pub async fn start_heartbeat_sync(
    metadata: Arc<Metadata>,
    token: CancellationToken,
    target_path: &Path
) -> Result<(), anyhow::Error> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            // Periodic snapshot every second
            _ = interval.tick() => {
                if let Err(e) = save_snapshot_sync(&metadata, target_path).await {
                    eprintln!("Failed to save heartbeat snapshot for {}: {}", target_path.display(), e);
                }
            },
            // Final snapshot upon manager cancellation/completion
            _ = token.cancelled() => {
                let _ = save_snapshot_sync(&metadata, target_path).await;
                break;
            }
        }
    }

    Ok(())
}

/// Loads a previous download state from a `.warp` file.
///
/// This is the entry point for resuming a download. It reconstructs the
/// [`Metadata`] structure, including the thread-safe chunk queue, from the disk snapshot.
pub async fn load_snapshot(target_path: &Path) -> Result<Metadata, anyhow::Error> {
    let snapshot = load_warp_file(target_path).await?;

    // Safety margin: on crash, the last ~1 MB per worker may have been
    // counted in progress (heartbeat snapshot) but still sat in the
    // BufWriter buffer, never reaching disk.  Pull progress back by
    // RESUME_MARGIN bytes so we re-download a small overlap instead of
    // leaving a gap of zeros.  Re-writing the same bytes is idempotent.
    const RESUME_MARGIN: u64 = 2 * 1024 * 1024;

    let mut chunks = VecDeque::new();
    for c in snapshot.chunks {
        let safe_progress = c.progress.saturating_sub(RESUME_MARGIN);
        chunks.push_back(Arc::new(Chunk::new(c.start..=c.end, safe_progress)));
    }

    Ok(Metadata {
        url: snapshot.url,
        size: snapshot.size,
        chunks: tokio::sync::Mutex::new(chunks),
        active_chunks: tokio::sync::Mutex::new(Vec::new()),
        completed_chunks: tokio::sync::Mutex::new(Vec::new()),
        max_speed_bytes: None,
    })
}

/// Reads and deserializes the `.warp` file from disk.
pub async fn load_warp_file(target_path: &Path) -> Result<MetadataSnapshot, anyhow::Error> {
    let bytes = tokio::fs::read(target_path).await
        .map_err(|e| anyhow::anyhow!("Failed to read .warp file {}: {}", target_path.display(), e))?;
    Ok(bincode::deserialize(&bytes)?)
}

/// Atomically saves the current metadata state to a file.
///
/// To prevent file corruption during a crash, this function:
/// 1.  Serializes the state to a memory buffer.
/// 2.  Writes the buffer to a temporary file (`.warp.tmp`).
/// 3.  Atomically renames the temporary file to the final destination.
pub async fn save_snapshot_sync(metadata: &Metadata, target_path: &Path) -> Result<(), anyhow::Error> {
    let snapshot = create_snapshot_sync(metadata).await;
    let bytes = bincode::serialize(&snapshot)?;

    let mut tmp_path = target_path.to_path_buf();
    let file_name = target_path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid target file name for snapshot"))?;
    let tmp_file_name = format!("{}.warp.tmp", file_name);
    tmp_path.set_file_name(tmp_file_name);

    tokio::fs::write(&tmp_path, &bytes).await?;
    tokio::fs::rename(&tmp_path, target_path).await?;

    Ok(())
}

/// Captures a point-in-time snapshot of all chunks (waiting, active, and completed) from the live Metadata.
async fn create_snapshot_sync(metadata: &Metadata) -> MetadataSnapshot {
    let mut chunks = Vec::new();

    // 1. Capture waiting chunks
    {
        let chunks_guard: MutexGuard<VecDeque<Arc<Chunk>>> = metadata.chunks.lock().await;
        for chunk_arc in chunks_guard.iter() {
            chunks.push(ChunkSnapshot {
                start: chunk_arc.start,
                end: chunk_arc.end.load(Ordering::Relaxed),
                progress: chunk_arc.progress.load(Ordering::Relaxed),
            });
        }
    }

    // 2. Capture active chunks (currently with workers)
    {
        let active_guard: MutexGuard<Vec<Arc<Chunk>>> = metadata.active_chunks.lock().await;
        for chunk_arc in active_guard.iter() {
            chunks.push(ChunkSnapshot {
                start: chunk_arc.start,
                end: chunk_arc.end.load(Ordering::Relaxed),
                progress: chunk_arc.progress.load(Ordering::Relaxed),
            });
        }
    }

    // 3. Capture completed chunks
    {
        let completed_guard: MutexGuard<Vec<Arc<Chunk>>> = metadata.completed_chunks.lock().await;
        for chunk_arc in completed_guard.iter() {
            chunks.push(ChunkSnapshot {
                start: chunk_arc.start,
                end: chunk_arc.end.load(Ordering::Relaxed),
                progress: chunk_arc.progress.load(Ordering::Relaxed),
            });
        }
    }

    MetadataSnapshot {
        url: metadata.url.clone(),
        size: metadata.size,
        chunks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_snapshot_serialization_deserialization() {
        // Goal: Verify the full lifecycle of a persistence snapshot (save -> load).

        let dir = tempdir().unwrap();
        let warp_path = dir.path().join("test.warp");

        // 1. Create initial metadata and simulate some progress.
        let metadata = Metadata::new("http://test.com".to_string(), 1000, None, 1);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(500, Ordering::SeqCst);
        }

        // 2. Save the state to a temporary .warp file.
        save_snapshot_sync(&metadata, &warp_path).await.unwrap();
        assert!(warp_path.exists(), "Snapshot file should be created on disk");

        // 3. Load the state back from the file.
        let loaded_metadata = load_snapshot(&warp_path).await.expect("Failed to load snapshot from disk");

        // 4. Verify the loaded state matches the original simulated state.
        assert_eq!(loaded_metadata.url, "http://test.com");
        assert_eq!(loaded_metadata.size, 1000);

        let chunks: MutexGuard<VecDeque<Arc<Chunk>>> = loaded_metadata.chunks.lock().await;
        assert_eq!(chunks.len(), 1, "Should have loaded exactly one chunk");
        // Progress was adjusted by RESUME_MARGIN (2 MB) to guard against
        // unflushed BufWriter data — the small test value gets zeroed.
        assert_eq!(chunks[0].progress.load(Ordering::SeqCst), 0, "Progress regressed by resume margin");

        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[0].end.load(Ordering::Relaxed), 999);
    }

    #[tokio::test]
    async fn test_create_snapshot_sync() {
        let metadata = Metadata::new("url".to_string(), 1000, None, 1);
        let snapshot = create_snapshot_sync(&metadata).await;

        assert_eq!(snapshot.url, "url");
        assert_eq!(snapshot.chunks.len(), 1);
        assert_eq!(snapshot.chunks[0].start, 0);
        assert_eq!(snapshot.chunks[0].end, 999);
    }

    #[tokio::test]
    async fn test_resume_margin_preserves_large_progress() {
        // When saved progress exceeds RESUME_MARGIN the margin should only
        // pull it back by 2 MB, not zero it out.
        let dir = tempdir().unwrap();
        let warp_path = dir.path().join("test.warp");
        let large_progress = 50 * 1024 * 1024; // 50 MB

        let metadata = Metadata::new("http://test.com".to_string(), 200 * 1024 * 1024, None, 1);
        {
            let chunks = metadata.chunks.lock().await;
            chunks[0].progress.store(large_progress, Ordering::SeqCst);
        }
        save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let loaded = load_snapshot(&warp_path).await.unwrap();
        let chunks = loaded.chunks.lock().await;

        // 50 MB - 2 MB margin = 48 MB
        let expected = large_progress - 2 * 1024 * 1024;
        assert_eq!(chunks[0].progress.load(Ordering::SeqCst), expected,
                   "Large progress should only be reduced by RESUME_MARGIN");
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[0].end.load(Ordering::Relaxed), 200 * 1024 * 1024 - 1);
    }

    #[tokio::test]
    async fn test_resume_margin_with_multiple_chunks() {
        // Each chunk in a multi-chunk download should independently have the
        // RESUME_MARGIN applied.
        let dir = tempdir().unwrap();
        let warp_path = dir.path().join("multi.warp");

        // Create metadata with 4 initial chunks (simulating a pre-split download).
        // Each chunk is 25 MB.  RESUME_MARGIN (2 MB) is subtracted independently.
        let metadata = Metadata::new("http://test.com".to_string(), 100 * 1024 * 1024, None, 4);
        {
            let chunks = metadata.chunks.lock().await;
            assert_eq!(chunks.len(), 4);
            chunks[0].progress.store(5 * 1024 * 1024, Ordering::SeqCst);  // 5 - 2 = 3 MB
            chunks[1].progress.store(10 * 1024 * 1024, Ordering::SeqCst); // 10 - 2 = 8 MB
            chunks[2].progress.store(3 * 1024 * 1024, Ordering::SeqCst);  // 3 - 2 = 1 MB
            chunks[3].progress.store(0, Ordering::SeqCst);                // 0 - 2 → 0 (saturating)
        }
        save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        let loaded = load_snapshot(&warp_path).await.unwrap();
        let chunks = loaded.chunks.lock().await;
        assert_eq!(chunks.len(), 4);

        let mb = |n: u64| n * 1024 * 1024;
        // Chunk 0: 5 MB - 2 MB = 3 MB
        assert_eq!(chunks[0].progress.load(Ordering::SeqCst), mb(3));
        // Chunk 1: 10 MB - 2 MB = 8 MB
        assert_eq!(chunks[1].progress.load(Ordering::SeqCst), mb(8));
        // Chunk 2: 3 MB - 2 MB = 1 MB
        assert_eq!(chunks[2].progress.load(Ordering::SeqCst), mb(1));
        // Chunk 3: 0 → 0 (saturating)
        assert_eq!(chunks[3].progress.load(Ordering::SeqCst), 0);

        // Ranges must be preserved exactly.
        let total_size_4 = (100 * 1024 * 1024) / 4;
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[1].start, total_size_4);
        assert_eq!(chunks[2].start, 2 * total_size_4);
        assert_eq!(chunks[3].start, 3 * total_size_4);
    }

    #[tokio::test]
    async fn test_snapshot_with_mixed_chunk_states() {
        // Snapshot must correctly capture chunks across waiting, active, and
        // completed lists — not just the waiting queue.
        let dir = tempdir().unwrap();
        let warp_path = dir.path().join("mixed.warp");

        let metadata = Metadata::new("url".to_string(), 3000, None, 3);

        // 1. Move middle chunk to active with partial progress.
        // 2. Move last chunk to completed with full progress.
        {
            let mut queue = metadata.chunks.lock().await;
            assert_eq!(queue.len(), 3);

            // Pop the last two chunks.
            let _c1 = queue.pop_back(); // chunk index 2 (range 2000..=2999)
            let c2 = queue.pop_back().unwrap(); // chunk index 1 (range 1000..=1999)

            // c2 goes to active with partial progress.
            c2.progress.store(500, Ordering::SeqCst);
            metadata.active_chunks.lock().await.push(c2);

            // chunk index 2 goes to completed.
            let c3 = Chunk::new(2000..=2999, 1000);
            metadata.completed_chunks.lock().await.push(Arc::new(c3));
        }

        save_snapshot_sync(&metadata, &warp_path).await.unwrap();

        // Load back — all three chunks must be reconstructed in the waiting queue.
        let loaded = load_snapshot(&warp_path).await.unwrap();
        let chunks = loaded.chunks.lock().await;
        assert_eq!(chunks.len(), 3, "All chunks must be restored to waiting queue");

        // Verify each chunk: range + (margin-adjusted) progress.
        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks[0].end.load(Ordering::Relaxed), 999);
        assert_eq!(chunks[0].progress.load(Ordering::SeqCst), 0);

        assert_eq!(chunks[1].start, 1000);
        assert_eq!(chunks[1].end.load(Ordering::Relaxed), 1999);
        // progress was 500, but < 2MB margin → zeroed
        assert_eq!(chunks[1].progress.load(Ordering::SeqCst), 0);

        assert_eq!(chunks[2].start, 2000);
        assert_eq!(chunks[2].end.load(Ordering::Relaxed), 2999);
        // progress was 1000, but < 2MB margin → zeroed
        assert_eq!(chunks[2].progress.load(Ordering::SeqCst), 0);

        // Total size preserved.
        assert_eq!(loaded.size, 3000);
        assert_eq!(loaded.url, "url");
    }
}
