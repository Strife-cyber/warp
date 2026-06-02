use anyhow::Result;
use std::path::PathBuf;
use crate::download_registry::Registry;
use crate::core::DownloadStatus;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Adds a new download
    Add {
        /// The URL to download
        url: String,
        /// The optional target file path
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Speed limit (e.g. 1M for 1 MB/s, 500K for 500 KB/s)
        #[arg(long)]
        speed_limit: Option<String>,
        /// Proxy URL (e.g. http://proxy:8080)
        #[arg(long)]
        proxy: Option<String>,
        /// SHA-256 checksum for verification after download
        #[arg(long)]
        checksum: Option<String>,
        /// Priority (0-255, higher = sooner)
        #[arg(long, default_value_t = 0)]
        priority: u8,
    },
    /// Lists all known downloads
    List {
        /// Filter by category (video, audio, archive, document, image, other)
        #[arg(long)]
        category: Option<String>,
        /// Search URL, path, or ID
        #[arg(long)]
        search: Option<String>,
    },
    /// Removes a download by ID
    Remove {
        /// The ID of the download to remove
        id: String,
    },
    /// Runs all pending or paused downloads in the foreground
    Run,
    /// Inspects the .warp snapshot file for a download
    Inspect {
        /// The ID of the download to inspect
        id: String,
    },
    /// Pauses a download
    Pause {
        /// The ID of the download to pause
        id: String,
    },
    /// Resumes a paused download
    Resume {
        /// The ID of the download to resume
        id: String,
    },
    /// Retries a download that failed with an error
    Retry {
        /// The ID of the download to retry
        id: String,
    },
    /// Removes all completed downloads from the download_registry
    Clean,
    /// Launches the native egui download manager
    Gui,
    /// Launches the interactive terminal UI
    Tui,
    /// Runs the background HTTP daemon (shared registry API)
    Serve {
        #[arg(long, default_value_t = 9844)]
        port: u16,
    },
    /// Shows per-host download metrics
    Stats,
    /// View or update application settings
    Config {
        /// Global speed limit (e.g. 1M, 500K)
        #[arg(long)]
        global_speed_limit: Option<String>,
        /// Max concurrent segment workers (default 32)
        #[arg(long)]
        max_workers: Option<usize>,
    },
    /// Downloads an HLS (M3U8) video stream
    M3u8 {
        /// The M3U8 playlist URL
        url: String,
        /// Output file path (.ts)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Quality variant: best, high, med, low, or index number
        #[arg(long, default_value = "best")]
        quality: String,
        /// Number of concurrent segment downloads
        #[arg(long, default_value_t = 8)]
        concurrent: usize,
    },
}

fn parse_speed_limit(input: &str) -> Result<u64> {
    let input = input.trim().to_uppercase();
    if let Some(val) = input.strip_suffix("K") {
        let num: f64 = val.parse()?;
        Ok((num * 1024.0) as u64)
    } else if let Some(val) = input.strip_suffix("M") {
        let num: f64 = val.parse()?;
        Ok((num * 1024.0 * 1024.0) as u64)
    } else if let Some(val) = input.strip_suffix("G") {
        let num: f64 = val.parse()?;
        Ok((num * 1024.0 * 1024.0 * 1024.0) as u64)
    } else {
        let num: u64 = input.parse()?;
        Ok(num)
    }
}

pub async fn handle_add(
    url: String,
    output: Option<PathBuf>,
    speed_limit: Option<String>,
    proxy: Option<String>,
    checksum: Option<String>,
    priority: u8,
    registry: &Registry,
) -> Result<()> {
    let target_path = match output {
        Some(p) => p,
        None => {
            let filename = url.split('/').next_back().unwrap_or("download.bin");
            let filename = filename.split('?').next().unwrap_or("download.bin");
            PathBuf::from(filename)
        }
    };

    println!("Verifying URL: {}...", url);
    let client = reqwest::Client::new();
    let response = client.head(&url).send().await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!("URL verification failed: Status {}", response.status()));
    }

    let id = registry.add(url.clone(), target_path.clone()).await?;

    let max_speed = if let Some(ref limit) = speed_limit {
        Some(parse_speed_limit(limit)?)
    } else {
        None
    };

    if priority > 0 || proxy.is_some() || checksum.is_some() || max_speed.is_some() {
        registry
            .update_advanced(&id, Some(priority), proxy, checksum, max_speed)
            .await?;
    }

    println!("Added download {} -> {}", id, target_path.display());
    Ok(())
}

pub async fn handle_list(
    registry: &Registry,
    category: Option<String>,
    search: Option<String>,
) -> Result<()> {
    let cat = category.as_deref().and_then(parse_category);
    let entries = registry
        .list_filtered(cat, search.as_deref())
        .await?;
    if entries.is_empty() {
        println!("No downloads in download_registry.");
        return Ok(());
    }

    let id_w = 15;
    let status_w = 12;
    let target_w = 45;
    let url_w = 50;

    println!(
        "{:<id_w$} | {:<status_w$} | {:<target_w$} | {:<url_w$}",
        "ID", "Status", "Target", "URL",
        id_w=id_w, status_w=status_w, target_w=target_w, url_w=url_w
    );
    println!(
        "{:-<id_w$}-+-{:-<status_w$}-+-{:-<target_w$}-+-{:-<url_w$}",
        "", "", "", "",
        id_w=id_w, status_w=status_w, target_w=target_w, url_w=url_w
    );

    for entry in entries {
        let status_str = match &entry.status {
            DownloadStatus::Error => "Error".to_string(),
            s => format!("{:?}", s),
        };

        let target_str = entry.target_path.to_string_lossy();
        let display_target = if target_str.len() > target_w {
            format!("...{}", &target_str[target_str.len() - (target_w - 3)..])
        } else {
            target_str.to_string()
        };

        let display_url = if entry.url.len() > url_w {
            format!("{}...", &entry.url[..url_w - 3])
        } else {
            entry.url.clone()
        };

        println!(
            "{:<id_w$} | {:<status_w$} | {:<target_w$} | {:<url_w$}",
            entry.id,
            status_str,
            display_target,
            display_url,
            id_w=id_w, status_w=status_w, target_w=target_w, url_w=url_w
        );
    }
    Ok(())
}

fn parse_category(input: &str) -> Option<crate::core::DownloadCategory> {
    use crate::core::DownloadCategory;
    match input.to_ascii_lowercase().as_str() {
        "video" => Some(DownloadCategory::Video),
        "audio" => Some(DownloadCategory::Audio),
        "archive" => Some(DownloadCategory::Archive),
        "document" => Some(DownloadCategory::Document),
        "image" => Some(DownloadCategory::Image),
        "other" => Some(DownloadCategory::Other),
        _ => None,
    }
}

pub async fn handle_stats(registry: &Registry) -> Result<()> {
    let rows = crate::metrics::list_host_metrics(&registry.pool()).await?;
    if rows.is_empty() {
        println!("No download metrics recorded yet.");
        return Ok(());
    }
    println!(
        "{:<30} {:>8} {:>14} {:>14} {:>8}",
        "Host", "Downloads", "Bytes", "Avg B/s", "Fails"
    );
    for r in rows {
        println!(
            "{:<30} {:>8} {:>14} {:>14} {:>8}",
            r.host, r.downloads, r.bytes_total, r.avg_speed_bps as u64, r.failures
        );
    }
    Ok(())
}

pub async fn handle_remove(id: String, registry: &Registry) -> Result<()> {
    if let Some(entry) = registry.remove(&id).await? {
        println!("Removed download: {} ({})", id, entry.url);
    } else {
        println!("Download ID {} not found.", id);
    }
    Ok(())
}

pub async fn handle_inspect(id: String, registry: &Registry) -> Result<()> {
    if let Some(entry) = registry.get(&id).await? {
        let warp_path = entry.target_path.with_extension("warp");
        if !warp_path.exists() {
            println!("No .warp file found for ID {}. Has the download started?", id);
            return Ok(());
        }

        println!("Inspecting snapshot: {}", warp_path.display());
        let snapshot = crate::download::beat::load_warp_file(&warp_path).await?;

        let json_string = serde_json::to_string_pretty(&snapshot)?;
        println!("\n--- .warp Content ---\n");
        println!("{}", json_string);
        println!("\n---------------------");
    } else {
        println!("Download ID {} not found.", id);
    }
    Ok(())
}

pub async fn handle_pause(id: String, registry: &Registry) -> Result<()> {
    match registry.get(&id).await? {
        Some(_) => {
            registry.update_status(&id, DownloadStatus::Paused).await?;
            println!("Paused download: {}", id);
        }
        None => println!("Download ID {} not found.", id),
    }
    Ok(())
}

pub async fn handle_resume(id: String, registry: &Registry) -> Result<()> {
    match registry.get(&id).await? {
        Some(_) => {
            registry.update_status(&id, DownloadStatus::Pending).await?;
            println!("Resumed download: {}", id);
        }
        None => println!("Download ID {} not found.", id),
    }
    Ok(())
}

pub async fn handle_retry(id: String, registry: &Registry) -> Result<()> {
    match registry.get(&id).await? {
        Some(_) => {
            registry.update_status(&id, DownloadStatus::Pending).await?;
            println!("Retrying download: {}", id);
        }
        None => println!("Download ID {} not found.", id),
    }
    Ok(())
}

pub async fn handle_clean(registry: &Registry) -> Result<()> {
    let removed = registry.clean_completed().await?;
    println!("Cleaned {} completed downloads.", removed);
    Ok(())
}

pub async fn handle_config(
    global_speed_limit: Option<String>,
    max_workers: Option<usize>,
    registry: &Registry,
) -> Result<()> {
    let mut settings = registry.get_settings().await?;
    let mut changed = false;

    if let Some(limit) = global_speed_limit {
        settings.global_max_speed_bytes = Some(parse_speed_limit(&limit)?);
        changed = true;
    }
    if let Some(workers) = max_workers {
        if workers == 0 {
            return Err(anyhow::anyhow!("max_workers must be at least 1"));
        }
        settings.max_workers = workers;
        changed = true;
    }

    if changed {
        registry.save_settings(&settings).await?;
        if let Some(b) = settings.global_max_speed_bytes {
            println!("Global speed limit set to {b} bytes/s.");
        }
        if max_workers.is_some() {
            println!("Max workers set to {}.", settings.max_workers);
        }
    } else {
        println!("daemon_port: {}", settings.daemon_port);
        match settings.global_max_speed_bytes {
            Some(b) => println!("global_max_speed_bytes: {b}"),
            None => println!("global_max_speed_bytes: (unlimited)"),
        }
        println!("max_workers: {}", settings.max_workers);
        println!("schedule_windows: {}", settings.schedule_windows.len());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_handle_add_derives_filename() {
        let registry = Registry::open_in_memory().await.unwrap();
        let url = "http://example.com/somefile.txt?query=1".to_string();
        handle_add(url.clone(), None, None, None, None, 0, &registry)
            .await
            .ok();
        let entry = registry.list().await.unwrap().pop().unwrap();
        assert_eq!(entry.url, url);
        assert_eq!(entry.target_path.to_str().unwrap(), "somefile.txt");
    }

    #[tokio::test]
    #[ignore]
    async fn test_handle_add_explicit_path() {
        let registry = Registry::open_in_memory().await.unwrap();
        let path = PathBuf::from("explicit.bin");
        handle_add(
            "http://example.com".to_string(),
            Some(path.clone()),
            None,
            None,
            None,
            0,
            &registry,
        )
        .await
        .ok();
        let entry = registry.list().await.unwrap().pop().unwrap();
        assert_eq!(entry.target_path, path);
    }

    #[test]
    fn test_parse_speed_limit() {
        assert_eq!(parse_speed_limit("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_speed_limit("500K").unwrap(), 500 * 1024);
        assert_eq!(parse_speed_limit("2G").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_speed_limit("1000").unwrap(), 1000);
        assert_eq!(parse_speed_limit("2.5M").unwrap(), (2.5 * 1024.0 * 1024.0) as u64);
    }
}
