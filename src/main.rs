//! # Warp - High Performance Download Accelerator
//!
//! Warp is a multithreaded download manager designed to utilize system resources
//! efficiently while ensuring download integrity through atomic progress tracking
//! and a heartbeat-based snapshot system.

pub mod ui;
mod downloader;

use clap::Parser;
use crate::downloader::cli::{Cli, Commands};
use crate::downloader::registry::Registry;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    // Load the global registry
    let mut registry = Registry::load()?;

    match cli.command {
        Commands::Add { url, output } => {
            downloader::cli::handle_add(url, output, &mut registry).await?;
        }
        Commands::List => {
            downloader::cli::handle_list(&registry);
        }
        Commands::Remove { id } => {
            downloader::cli::handle_remove(id, &mut registry)?;
        }
        Commands::Run => {
            downloader::engine::run_all(&mut registry).await?;
        }
        Commands::Inspect { id } => {
            downloader::cli::handle_inspect(id, &registry).await?;
        }
        Commands::Pause { id } => {
            downloader::cli::handle_pause(id, &mut registry)?;
        }
        Commands::Resume { id } => {
            downloader::cli::handle_resume(id, &mut registry)?;
        }
        Commands::Retry { id } => {
            downloader::cli::handle_retry(id, &mut registry)?;
        }
        Commands::Clean => {
            downloader::cli::handle_clean(&mut registry)?;
        }
        Commands::Tui => {
            ui::tui::run(registry)?;
        }
        Commands::Gui => {
            ui::gui::run(registry).map_err(|e| anyhow::anyhow!("GUI error: {}", e))?;
        }
    }

    Ok(())
}
