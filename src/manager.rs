use std::sync::Arc;
use futures_util::StreamExt;
use tokio::task::JoinSet;
use crate::resources::calculate_optimal_workers;
use crate::segment::{download_worker, Chunk};

pub struct Metadata {
    pub url: String,
    pub size: u64,
    pub chunks: Vec<Arc<Chunk>>
}

pub struct Manager {
    metadata: Arc<Metadata>,
    cancel_token: tokio_util::sync::CancellationToken,
    target_path: std::path::PathBuf,
}

impl Manager {
    pub async fn run(&mut self) -> Result<(), anyhow::Error> {
        // 1. Determine the optimal worker count
        let stats = calculate_optimal_workers();
        println!("System CPU Usage: {}%. Spawning {} workers for stability.",
                 stats.cpu_usage, stats.suggested_workers);

        // 2. Start the heartbeat task
        let hb_metadata = Arc::clone(&self.metadata);
        let hb_token = self.cancel_token.clone();
        let hb_path = self.target_path.with_extension("warp");

        tokio::spawn(async move {
            if let Err(e) = crate::beat::start_heartbeat(hb_metadata, hb_token, &self.target_path).await {
                eprintln!("Heartbeat failed: {}", e);
            };
        });

        // 3. Launch workers in a JoinSet
        let mut workers = JoinSet::new();
        for chunk in &self.metadata.chunks {
            let chunk_ptr = Arc::clone(chunk);
            // We'd pass the client and URL here too
            workers.spawn(download_worker(chunk_ptr))
        }

        // 4. Orchestration completion
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => continue, // Worker finished successfully
                Ok(Err(e)) => {
                    // If one worker fails critically, we stop everything
                    self.cancel_token.cancel();
                    return Err(anyhow::anyhow!("Worker failed: {}", e));
                }
                Err(e) => return Err(e.into()), // Task panicked
            }
        }

        // 5. Finalize
        self.cancel_token.cancel();
        let _ = tokio::fs::remove_file(hb_path).await;
        println!("Download complete. Cleaned up .warp file.");

        Ok(())
    }
}

