use std::sync::Arc;
use anyhow::Result;
use tokio::task::JoinSet;
use tokio::sync::Semaphore;
use super::manager::Manager;
use super::registry::{Registry, DownloadStatus};
use super::resources::calculate_optimal_workers;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub async fn run_all(registry: &mut Registry) -> Result<()> {
    let stats = calculate_optimal_workers();
    let suggested_workers = stats.suggested_workers;
    
    println!("Starting Engine...");
    println!("System CPU: {:.1}%. Global worker limit: {}", stats.cpu_usage, suggested_workers);

    // Global semaphore shared across ALL managers
    let semaphore = Arc::new(Semaphore::new(suggested_workers));
    let mut managers = JoinSet::new();
    let multi_progress = MultiProgress::new();

    // Find all incomplete downloads
    for (id, entry) in registry.downloads.iter_mut() {
        if entry.status == DownloadStatus::Completed {
            continue;
        }

        let url = entry.url.clone();
        let target_path = entry.target_path.clone();
        let id_clone = id.clone();
        let sem_clone = Arc::clone(&semaphore);
        
        let pb = multi_progress.add(ProgressBar::new(0));
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")?
            .progress_chars("#>-"));
        
        // Transition state to downloading
        entry.status = DownloadStatus::Downloading;

        // Spawn a task for each manager
        managers.spawn(async move {
            match Manager::from_url(url, target_path).await {
                Ok(mut manager) => {
                    pb.set_length(manager.metadata.size);
                    manager.set_progress_bar(pb);
                    match manager.run(suggested_workers, sem_clone).await {
                        Ok(_) => Ok((id_clone, DownloadStatus::Completed)),
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

    // Save the "Downloading" state
    registry.save().ok(); // ignore save error in tests or missing dirs

    if managers.is_empty() {
        println!("No pending downloads to run.");
        return Ok(());
    }

    // Await all managers
    while let Some(res) = managers.join_next().await {
        match res {
            Ok(Ok((id, new_status))) => {
                registry.update_status(&id, new_status);
            }
            Ok(Err((id, err_msg))) => {
                registry.update_status(&id, DownloadStatus::Error(err_msg));
            }
            Err(e) => {
                eprintln!("Manager task panicked: {}", e);
            }
        }
    }

    // Final save
    registry.save().ok();
    println!("All downloads processed.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_run_all_no_pending() {
        let mut registry = Registry::default();
        // No downloads added
        run_all(&mut registry).await.unwrap();
        assert_eq!(registry.downloads.len(), 0);
    }

    #[tokio::test]
    async fn test_engine_skips_completed() {
        let mut registry = Registry::default();
        let id = registry.add("url".to_string(), PathBuf::from("path"));
        registry.update_status(&id, DownloadStatus::Completed);
        
        // Should not spawn anything
        run_all(&mut registry).await.unwrap();
        
        assert_eq!(registry.downloads.get(&id).unwrap().status, DownloadStatus::Completed);
    }
}
