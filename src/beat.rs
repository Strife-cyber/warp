use std::sync::Arc;
use std::path::Path;
use std::time::Duration;
use crate::segment::Chunk;
use crate::manager::Metadata;
use tokio_util::sync::CancellationToken;
use std::sync::atomic::{AtomicU64, Ordering};

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

pub async fn load_snapshot(target_path: &Path) -> Result<Metadata, anyhow::Error> {
    let snapshot = load_warp_file(target_path).await?;

    let chunks = snapshot.chunks.into_iter().map(|c| {
        Arc::new(Chunk {
            chunk_limits: c.start..=c.end,
            progress: AtomicU64::new(c.progress)
        })
    }).collect();

    Ok(Metadata {
        url: snapshot.url,
        size: snapshot.size,
        chunks,
    })
}

async fn load_warp_file(target_path: &Path) -> Result<MetadataSnapshot, anyhow::Error> {
    let bytes = tokio::fs::read(target_path).await?;
    Ok(bincode::deserialize(&bytes)?)
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

