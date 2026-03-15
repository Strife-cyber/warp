//! # Warp - High Performance Download Accelerator
//!
//! Warp is a multi-threaded download manager designed to utilize system resources
//! efficiently while ensuring download integrity through atomic progress tracking
//! and a heartbeat-based snapshot system.

mod segment;
pub mod manager;
mod beat;
mod resources;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::VecDeque;
use crate::manager::{Manager, Metadata};
use crate::segment::Chunk;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let url = "http://uf3qp40g0f.y9a8ua48mhss5ye.cyou/rlink_t/432ca7c011673093e49aa5bfc7f33306/c28d75b4142b6e057fa613cb532c59cc/9cd23ba0cc4bdf9074a55b24b567f6fc/White_Collar_-_S2_-_E3_62ac2e31b27ab5b98680e35d1bba43f4.mp4".to_string();
    let target_path = PathBuf::from("White_Collar_-_S2_-_E3_62ac2e31b27ab5b98680e35d1bba43f4.mp4");
    let warp_path = target_path.with_extension("warp");

    // 1. Check if we can resume an existing download
    let metadata = if warp_path.exists() {
        println!("Found .warp file, attempting to resume...");
        match beat::load_snapshot(&warp_path).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Failed to load snapshot: {}. Starting fresh.", e);
                create_fresh_metadata(url, 1024 * 1024 * 100) // Example 100MB
            }
        }
    } else {
        println!("No .warp file found, starting fresh download.");
        // In a real app, you'd fetch the file size via a HEAD request first
        create_fresh_metadata(url, 1024 * 1024 * 100) 
    };

    // 2. Initialize and run the manager
    let mut manager = Manager::new(metadata, target_path);
    manager.run().await?;

    Ok(())
}

/// Helper to create a single initial chunk for a fresh download.
/// The Manager will automatically split this into more chunks based on CPU cores.
fn create_fresh_metadata(url: String, size: u64) -> Metadata {
    let mut chunks = VecDeque::new();
    chunks.push_back(Arc::new(Chunk::new(0..=(size - 1), 0)));
    
    Metadata {
        url,
        size,
        chunks: Mutex::new(chunks),
    }
}
