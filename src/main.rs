//! # Warp - High Performance Download Accelerator

mod cli;
mod core;
mod daemon;
mod download;
mod download_registry;
mod gui;
mod hls;
mod metrics;
mod pipeline;
mod ui;
mod utils;

use clap::Parser;
use cli::{Cli, Commands};

fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    if matches!(cli.command, Commands::Gui | Commands::Tui) {
        let rt = tokio::runtime::Runtime::new()?;
        let registry = rt.block_on(download_registry::Registry::open())?;
        return match cli.command {
            Commands::Gui => gui::run_gui(registry),
            Commands::Tui => ui::tui::run(registry),
            _ => unreachable!(),
        };
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(cli))
}

async fn run_async(cli: Cli) -> Result<(), anyhow::Error> {
    let registry = download_registry::Registry::open().await?;

    match cli.command {
        Commands::Add { url, output, speed_limit, proxy, checksum, priority } => {
            cli::handle_add(url, output, speed_limit, proxy, checksum, priority, &registry).await?;
        }
        Commands::List { category, search } => {
            cli::handle_list(&registry, category, search).await?;
        }
        Commands::Remove { id } => {
            cli::handle_remove(id, &registry).await?;
        }
        Commands::Run => {
            pipeline::run_all(&registry).await?;
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
        Commands::Gui => unreachable!("handled in main"),
        Commands::Tui => unreachable!("handled in main"),
        Commands::Serve { port } => {
            daemon::serve(registry, port).await?;
        }
        Commands::Stats => {
            cli::handle_stats(&registry).await?;
        }
        Commands::Config { global_speed_limit, max_workers } => {
            cli::handle_config(global_speed_limit, max_workers, &registry).await?;
        }
        Commands::M3u8 { url, output, quality, concurrent } => {
            let id = hls::download_hls_via_registry(&registry, &url, &output, quality, concurrent).await?;
            println!("Added HLS download {id} — run `warp run` to start.");
        }
    }

    Ok(())
}
