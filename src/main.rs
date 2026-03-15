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
use crate::manager::Manager;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let url = "http://uf3qp40g0f.y9a8ua48mhss5ye.cyou/rlink_t/432ca7c011673093e49aa5bfc7f33306/c28d75b4142b6e057fa613cb532c59cc/9cd23ba0cc4bdf9074a55b24b567f6fc/White_Collar_-_S2_-_E3_62ac2e31b27ab5b98680e35d1bba43f4.mp4".to_string();
    let target_path = PathBuf::from("White_Collar_-_S2_-_E3_62ac2e31b27ab5b98680e35d1bba43f4.mp4");

    // Initialize and run the manager
    let mut manager = Manager::from_url(url, target_path).await?;
    manager.run().await?;

    Ok(())
}
