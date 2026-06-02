use std::sync::Arc;
use anyhow::Result;
use tokio::task::JoinSet;
use tokio::sync::Semaphore;
use crate::manager::Manager;
use crate::download_registry::Registry;
use crate::registry::DownloadStatus;
use crate::resources::calculate_optimal_workers;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub async fn run_all(registry: &Registry) -> Result<()> {
    let stats = calculate_optimal_workers();
    let suggested_workers = stats.suggested_workers;
    
    println!("Starting Engine...");
    println!("System CPU: {:.1}%. Global worker limit: {}", stats.cpu_usage, suggested_workers);

    let semaphore = Arc::new(Semaphore::new(suggested_workers));
    let mut managers = JoinSet::new();
    let multi_progress = MultiProgress::new();

    // Query only incomplete rows — no full-table load into memory.
    let mut pending_downloads = registry.list_not_completed().await?;

    pending_downloads.sort_by(|a, b| b.priority.cmp(&a.priority));

    for entry in pending_downloads {
        let id_clone = entry.id.clone();
        let sem_clone = Arc::clone(&semaphore);
        
        let pb = multi_progress.add(ProgressBar::new(0));
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")?
            .progress_chars("#>-"));
        
        registry
            .update_status(&id_clone, DownloadStatus::Downloading)
            .await?;

        managers.spawn(async move {
            match Manager::from_entry(&entry).await {
                Ok(mut manager) => {
                    pb.set_length(manager.metadata.size);
                    manager.set_progress_bar(pb.clone());
                    match manager.run(suggested_workers, sem_clone).await {
                        Ok(_) => {
                            if let Some(expected_hash) = &entry.checksum {
                                pb.set_message("Verifying checksum...");
                                use sha2::{Sha256, Digest};
                                use hex::encode;
                                
                                match tokio::fs::read(&entry.target_path).await {
                                    Ok(bytes) => {
                                        let mut hasher = Sha256::new();
                                        hasher.update(&bytes);
                                        let result = hasher.finalize();
                                        let hash_hex = encode(result);
                                        if hash_hex.to_lowercase() != expected_hash.to_lowercase() {
                                            return Err((id_clone, format!("Checksum mismatch! Expected: {}, Got: {}", expected_hash, hash_hex)));
                                        }
                                        pb.set_message("Checksum OK");
                                    }
                                    Err(e) => return Err((id_clone, format!("Failed to read file for checksum: {}", e))),
                                }
                            }
                            Ok((id_clone, DownloadStatus::Completed))
                        },
                        Err(e) => Err((id_clone, e.to_string())),
                    }
                }
                Err(e) => {
                    pb.finish_with_message(format!("Error: {}", e));
                    Err((id_clone, e.to_string()))
                },
            }
        });
    }

    if managers.is_empty() {
        println!("No pending downloads to run.");
        return Ok(());
    }

    while let Some(res) = managers.join_next().await {
        match res {
            Ok(Ok((id, new_status))) => {
                registry.update_status(&id, new_status).await?;
            }
            Ok(Err((id, _err_msg))) => {
                registry.update_status(&id, DownloadStatus::Error).await?;
            }
            Err(e) => {
                eprintln!("Manager task panicked: {}", e);
            }
        }
    }

    println!("All downloads processed.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_run_all_no_pending() {
        let registry = Registry::open_in_memory().await.unwrap();
        run_all(&registry).await.unwrap();
        assert!(registry.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_engine_skips_completed() {
        let registry = Registry::open_in_memory().await.unwrap();
        let id = registry
            .add("url".to_string(), PathBuf::from("path"))
            .await
            .unwrap();
        registry
            .update_status(&id, DownloadStatus::Completed)
            .await
            .unwrap();
        
        run_all(&registry).await.unwrap();
        
        assert_eq!(
            registry.get(&id).await.unwrap().unwrap().status,
            DownloadStatus::Completed
        );
    }
}
