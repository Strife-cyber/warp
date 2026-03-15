use clap::{Parser, Subcommand};
use std::path::PathBuf;
use crate::registry::Registry;
use anyhow::Result;

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
    },
    /// Lists all known downloads
    List,
    /// Removes a download by ID
    Remove {
        /// The ID of the download to remove
        id: String,
    },
    /// Runs all pending or paused downloads in the foreground
    Run,
    /// Inspects the .warp snapshot file for a download (converts binary to TOML)
    Inspect {
        /// The ID of the download to inspect
        id: String,
    },
}

pub async fn handle_add(url: String, output: Option<PathBuf>, registry: &mut Registry) -> Result<()> {
    let target_path = match output {
        Some(p) => p,
        None => {
            let filename = url.split('/').last().unwrap_or("download.bin");
            let filename = filename.split('?').next().unwrap_or("download.bin");
            PathBuf::from(filename)
        }
    };

    // Verify URL and get content length immediately
    println!("Verifying URL: {}...", url);
    let client = reqwest::Client::new();
    let response = client.head(&url).send().await?;
    
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("URL verification failed: Status {}", response.status()));
    }

    let id = registry.add(url.clone(), target_path.clone());
    registry.save()?;
    
    println!("Added download {} -> {}", id, target_path.display());
    Ok(())
}

pub fn handle_list(registry: &Registry) {
    if registry.downloads.is_empty() {
        println!("No downloads in registry.");
        return;
    }

    // Standardized widths
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
    
    for (id, entry) in &registry.downloads {
        let status_str = match &entry.status {
            crate::registry::DownloadStatus::Error(_) => "Error".to_string(),
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
            id,
            status_str,
            display_target,
            display_url,
            id_w=id_w, status_w=status_w, target_w=target_w, url_w=url_w
        );
    }
}

pub fn handle_remove(id: String, registry: &mut Registry) -> Result<()> {
    if let Some(entry) = registry.remove(&id) {
        registry.save()?;
        println!("Removed download: {} ({})", id, entry.url);
    } else {
        println!("Download ID {} not found.", id);
    }
    Ok(())
}

pub async fn handle_inspect(id: String, registry: &Registry) -> Result<()> {
    if let Some(entry) = registry.downloads.get(&id) {
        let warp_path = entry.target_path.with_extension("warp");
        if !warp_path.exists() {
            println!("No .warp file found for ID {}. Has the download started?", id);
            return Ok(());
        }

        println!("Inspecting snapshot: {}", warp_path.display());
        let snapshot = crate::beat::load_warp_file(&warp_path).await?;
        
        let toml_string = toml::to_string_pretty(&snapshot)?;
        println!("\n--- .warp Content (TOML) ---\n");
        println!("{}", toml_string);
        println!("\n---------------------------");
    } else {
        println!("Download ID {} not found.", id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires internet access
    async fn test_handle_add_derives_filename() {
        let mut registry = Registry::default();
        let url = "http://example.com/somefile.txt?query=1".to_string();
        handle_add(url.clone(), None, &mut registry).await.ok();
        
        let entry = registry.downloads.values().next().unwrap();
        assert_eq!(entry.url, url);
        assert_eq!(entry.target_path.to_str().unwrap(), "somefile.txt");
    }

    #[tokio::test]
    #[ignore] // Requires internet access
    async fn test_handle_add_explicit_path() {
        let mut registry = Registry::default();
        let path = PathBuf::from("explicit.bin");
        handle_add("http://example.com".to_string(), Some(path.clone()), &mut registry).await.ok();
        
        let entry = registry.downloads.values().next().unwrap();
        assert_eq!(entry.target_path, path);
    }
}
