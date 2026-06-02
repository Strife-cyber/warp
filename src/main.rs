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
use registry::Registry;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    // Load the global download_registry
    let mut registry = Registry::load()?;

    match cli.command {
        Commands::Add { url, output, speed_limit, proxy, checksum, priority } => {
            cli::handle_add(url, output, speed_limit, proxy, checksum, priority, &mut registry).await?;
        }
        Commands::List => {
            cli::handle_list(&registry);
        }
        Commands::Remove { id } => {
            cli::handle_remove(id, &mut registry)?;
        }
        Commands::Run => {
            engine::run_all(&mut registry).await?;
        }
        Commands::Inspect { id } => {
            cli::handle_inspect(id, &registry).await?;
        }
        Commands::Pause { id } => {
            cli::handle_pause(id, &mut registry)?;
        }
        Commands::Resume { id } => {
            cli::handle_resume(id, &mut registry)?;
        }
        Commands::Retry { id } => {
            cli::handle_retry(id, &mut registry)?;
        }
        Commands::Clean => {
            cli::handle_clean(&mut registry)?;
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
