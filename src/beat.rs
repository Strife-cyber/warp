use std::sync::Arc;
use std::path::Path;
use std::time::Duration;
use crate::segment::Chunk;
use crate::manager::Metadata;
use tokio_util::sync::CancellationToken;
use std::sync::atomic::{Ordering};

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

    let mut chunks = std::collections::VecDeque::new();
    for c in snapshot.chunks {
        // Reconstruct each chunk with its previously saved progress
        chunks.push_back(Arc::new(Chunk::new(c.start..=c.end, c.progress)));
    }

    Ok(Metadata {
        url: snapshot.url,
        size: snapshot.size,
        chunks: tokio::sync::Mutex::new(chunks),
    })
}

/// Reads and deserializes the `.warp` file from disk.
async fn load_warp_file(target_path: &Path) -> Result<MetadataSnapshot, anyhow::Error> {
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

/// Captures a point-in-time snapshot of all chunks from the live Metadata.
async fn create_snapshot_sync(metadata: &Metadata) -> MetadataSnapshot {
    let chunks_guard = metadata.chunks.lock().await;
    let mut chunks = Vec::new();
    
    for chunk_arc in chunks_guard.iter() {
        // We must lock each chunk's limits as they might be changing during a split
        let limits = chunk_arc.chunk_limits.lock().await;
        chunks.push(ChunkSnapshot {
            start: *limits.start(),
            end: *limits.end(),
            progress: chunk_arc.progress.load(Ordering::Relaxed),
        });
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
        let metadata = Metadata::new("http://test.com".to_string(), 1000);
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

        let chunks = loaded_metadata.chunks.lock().await;
        assert_eq!(chunks.len(), 1, "Should have loaded exactly one chunk");
        assert_eq!(chunks[0].progress.load(Ordering::SeqCst), 500, "Loaded progress should match saved progress");
        
        let limits = chunks[0].chunk_limits.lock().await;
        assert_eq!(*limits.start(), 0);
        assert_eq!(*limits.end(), 999);
    }

    #[tokio::test]
    async fn test_create_snapshot_sync() {
        let metadata = Metadata::new("url".to_string(), 1000);
        let snapshot = create_snapshot_sync(&metadata).await;
        
        assert_eq!(snapshot.url, "url");
        assert_eq!(snapshot.chunks.len(), 1);
        assert_eq!(snapshot.chunks[0].start, 0);
        assert_eq!(snapshot.chunks[0].end, 999);
    }
}
