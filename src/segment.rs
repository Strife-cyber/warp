use std::sync::Arc;
use futures_util::stream::StreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Chunk {
    pub chunk_limits: std::ops::RangeInclusive<u64>,
    pub progress: AtomicU64
}

// An owned handle since we will need a mutex if we had to use Arc
// A shared chunk so that the manager can track the progress
pub async fn download_worker(
    client: Arc<reqwest::Client>,
    mut handle: tokio::fs::File,
    chunk: Arc<Chunk>,
    url: &str
) -> Result<(), anyhow::Error> {
    // 1. The Persistence Layer: Try the whole operation up to 3 times
    for _ in 0..3 {
        // 2. Refresh state: Get the latest progress to set the Range header
        let start_offset = chunk.chunk_limits.start() + chunk.progress.load(Ordering::SeqCst);

        // 3. Set the handle to the current offset to avoid reseeking
        handle.seek(SeekFrom::Start(start_offset)).await?;

        //4. The Connection: Wrap the 'send()' in a 30s timeout
        let response = match timeout(TIMEOUT, client
            .get(url)
            .header("Range", format!("bytes={}-{}", start_offset, chunk.chunk_limits.end()))
            .send()).await {
            Ok(res) => res?,
            Err(_) => continue // Timeout! Jump to the next iteration
        };

        // 5. Collect the byte stream from the response into a stream
        let mut stream = response.bytes_stream();

        // 6. The Streaming Layer: Process packets
        while let Ok(Some(item_result)) = timeout(TIMEOUT, stream.next()).await {
            let packet = item_result?;
            handle.write_all(&packet).await?;
            chunk.progress.fetch_add(packet.len() as u64, Ordering::SeqCst);
        }

        // 7. The checkout layer just in case we timed out
        if chunk.progress.load(Ordering::SeqCst) != *chunk.chunk_limits.end() {
            continue;
        }

        return Ok(());
    }

    Err(anyhow::anyhow!("Worker failed to complete chunk after 3 attempts"))
}
