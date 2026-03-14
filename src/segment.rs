use std::sync::Arc;
use futures_util::stream::StreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

pub struct Chunk {
    chunk_limits: std::ops::RangeInclusive<u64>,
    progress: AtomicU64
}

// An owned handle since we will need a mutex if we had to use Arc
// A shared chunk so that the manager can track the progress
pub async fn download_worker(
    client: Arc<reqwest::Client>,
    mut handle: tokio::fs::File,
    chunk: Arc<Chunk>,
    url: String
) -> Result<(), anyhow::Error> {
    let start_offset = chunk.chunk_limits.start() + chunk.progress.load(Ordering::SeqCst);
    handle.seek(SeekFrom::Start(start_offset)).await?;

    let response = client
        .get(url)
        .header("Range", format!("bytes={}-{}", start_offset, chunk.chunk_limits.end()))
        .send()
        .await?;

    let mut stream = response.bytes_stream();

    while let Some(packet_result) = stream.next().await {
        match packet_result {
            Ok(packet) => {
                handle.write_all(&packet).await?;
                chunk.progress.fetch_add(packet.len() as u64, Ordering::SeqCst);
            }
            Err(_) => {}
        }
    }

    Ok(())
}
