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
}

pub fn handle_add(url: String, output: Option<PathBuf>, registry: &mut Registry) -> Result<()> {
    // If output is not provided, try to extract it from the URL
    let target_path = match output {
        Some(p) => p,
        None => {
            let filename = url.split('/').last().unwrap_or("download.bin");
            // Remove URL parameters if any
            let filename = filename.split('?').next().unwrap_or("download.bin");
            PathBuf::from(filename)
        }
    };

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

    println!("{:<15} | {:<12} | {:<30} | {}", "ID", "Status", "Target", "URL");
    println!("{:-<15}-+-{:-<12}-+-{:-<30}-+-{:-<40}", "", "", "", "");
    
    for (id, entry) in &registry.downloads {
        println!(
            "{:<15} | {:<12?} | {:<30} | {}",
            id,
            entry.status,
            entry.target_path.display(),
            entry.url
        );
    }
}

pub fn handle_remove(id: String, registry: &mut Registry) -> Result<()> {
    if let Some(entry) = registry.remove(&id) {
        registry.save()?;
        println!("Removed download: {} ({})", id, entry.url);
        // We could also optionally delete the .warp snapshot or the file here
    } else {
        println!("Download ID {} not found.", id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_add_derives_filename() {
        let mut registry = Registry::default();
        // Bypass actual saving to disk by using default path which won't exist or we can ignore
        // In a real test we'd mock registry.save()
        
        let url = "http://example.com/somefile.txt?query=1".to_string();
        handle_add(url.clone(), None, &mut registry).ok(); // ignore save error
        
        let entry = registry.downloads.values().next().unwrap();
        assert_eq!(entry.url, url);
        assert_eq!(entry.target_path.to_str().unwrap(), "somefile.txt");
    }

    #[test]
    fn test_handle_add_explicit_path() {
        let mut registry = Registry::default();
        let path = PathBuf::from("explicit.bin");
        handle_add("url".to_string(), Some(path.clone()), &mut registry).ok();
        
        let entry = registry.downloads.values().next().unwrap();
        assert_eq!(entry.target_path, path);
    }
}
