//! # Warp - High Performance Download Accelerator
//!
//! Warp is a multithreaded download manager designed to utilize system resources
//! efficiently while ensuring download integrity through atomic progress tracking
//! and a heartbeat-based snapshot system.

mod segment;
pub mod manager;
mod beat;
mod resources;
mod registry;
mod cli;
mod engine;
pub mod utils;
pub mod ui;

use clap::Parser;
use crate::cli::{Cli, Commands};
use crate::registry::Registry;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    // Load the global registry
    let mut registry = Registry::load()?;

    match cli.command {
        Commands::Add { url, output } => {
            cli::handle_add(url, output, &mut registry).await?;
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
        Commands::Gui => {
            ui::gui::run(registry).map_err(|e| anyhow::anyhow!("GUI error: {}", e))?;
        }
    }

    Ok(())
}
