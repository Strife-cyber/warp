//! # Warp - High Performance Download Accelerator
//!
//! Warp is a multithreaded download manager designed to use system resources
//! efficiently while ensuring download integrity through atomic progress tracking
//! and a heartbeat-based snapshot system.

pub mod ui;
mod cli;
mod engine;
mod manager;
mod segment;
mod beat;
mod download_registry;
mod resources;
mod utils;
mod hls;
mod registry;

use clap::Parser;
use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    let registry = download_registry::Registry::open().await?;

    match cli.command {
        Commands::Add { url, output, speed_limit, proxy, checksum, priority } => {
            cli::handle_add(url, output, speed_limit, proxy, checksum, priority, &registry).await?;
        }
        Commands::List => {
            cli::handle_list(&registry).await?;
        }
        Commands::Remove { id } => {
            cli::handle_remove(id, &registry).await?;
        }
        Commands::Run => {
            engine::run_all(&registry).await?;
        }
        Commands::Inspect { id } => {
            cli::handle_inspect(id, &registry).await?;
        }
        Commands::Pause { id } => {
            cli::handle_pause(id, &registry).await?;
        }
        Commands::Resume { id } => {
            cli::handle_resume(id, &registry).await?;
        }
        Commands::Retry { id } => {
            cli::handle_retry(id, &registry).await?;
        }
        Commands::Clean => {
            cli::handle_clean(&registry).await?;
        }
        Commands::Tui => {
            ui::tui::run(registry)?;
        }
        Commands::M3u8 { url, output, quality, concurrent } => {
            hls::download_hls(&url, &output, quality, concurrent).await?;
        }
    }

    Ok(())
}
