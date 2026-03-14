use std::sync::Arc;
use std::path::Path;
use std::time::Duration;
use crate::segment::Chunk;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

// This is what actually gets turned into binary and saved to the .warp file
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ChunkSnapshot {
    pub start: u64,
    pub end: u64,
    pub progress: u64, // Just a plain number now!
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct MetadataSnapshot {
    pub url: String,
    pub size: u64,
    pub chunks: Vec<ChunkSnapshot>,
}

pub struct Metadata {
    url: String,
    size: u64,
    chunks: Vec<Arc<Chunk>>
}

pub async fn start_heartbeat(metadata: Arc<Metadata>, token: CancellationToken, target_path: &Path) -> Result<(), anyhow::Error> {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        tokio::select! {
            // Option A: The timer goes off
            _ = interval.tick() => {
                save_snapshot(&metadata, target_path).await?;
            },
            // Option B: The Manager flips the switch
            _ = token.cancelled() => {
                // Perform one final save before exiting!
                save_snapshot(&metadata, target_path).await?;
                break;
            }
        }
    }

    Ok(())
}

async fn save_snapshot(metadata: &Metadata, target_path: &Path) -> Result<(), anyhow::Error> {
    let snapshot = create_snapshot(&metadata);
    let bytes = bincode::serialize(&snapshot)?;

    // 1. Create a temporary path
    let mut tmp_path = target_path.to_path_buf();
    let file_name = target_path.file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid target file name"))?;
    let tmp_file_name = format!("{}.warp.tmp", file_name);
    tmp_path.set_file_name(tmp_file_name);

    // 2. Write the data to the temporary file
    tokio::fs::write(&tmp_path, &bytes).await?;

    // 3. Atomically rename the temp file to the target file
    // On most OSs, this "overwrites" the target path in one step
    tokio::fs::rename(&tmp_path, target_path).await?;

    Ok(())
}

fn create_snapshot(metadata: &Metadata) -> MetadataSnapshot {
    MetadataSnapshot {
        url: metadata.url.clone(),
        size: metadata.size,
        chunks: metadata.chunks.iter().map(|chunk_arc| {
            ChunkSnapshot {
                start: *chunk_arc.chunk_limits.start(),
                end: *chunk_arc.chunk_limits.end(),
                // This is the most important part: capturing the current number
                progress: chunk_arc.progress.load(Ordering::Relaxed),
            }
        }).collect()
    }
}
