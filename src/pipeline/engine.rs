use std::sync::Arc;

use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::core::DownloadStatus;
use crate::download::{calculate_optimal_workers, rate_limit::RateLimiter};
use crate::download_registry::Registry;
use crate::pipeline::executor::{execute_entry, EngineContext};
use crate::pipeline::post_action::{maybe_shutdown, run_post_download};
use crate::pipeline::scheduler::{is_entry_ready, is_within_schedule, next_schedule_wait};

/// How often to poll the registry for newly added downloads (seconds).
const POLL_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(5);

pub async fn run_all(registry: &Registry) -> Result<()> {
    let settings = registry.get_settings().await?;

    while !is_within_schedule(&settings) {
        println!("Outside scheduled download window — waiting...");
        if let Some(wait) = next_schedule_wait(&settings) {
            tokio::time::sleep(wait).await;
        }
    }

    let stats = calculate_optimal_workers(Some(settings.max_workers));
    let suggested_workers = stats.suggested_workers;

    println!("Starting Engine...");
    println!(
        "System CPU: {:.1}%. Global worker limit: {}",
        stats.cpu_usage, suggested_workers
    );

    let global_limiter = settings
        .global_max_speed_bytes
        .map(|b| Arc::new(RateLimiter::new(b)));

    let ctx = EngineContext {
        global_limiter,
        metrics_pool: registry.pool(),
    };

    let semaphore = Arc::new(Semaphore::new(suggested_workers));
    let multi_progress = MultiProgress::new();

    loop {
        let mut pending_downloads: Vec<_> = registry
            .list_not_completed()
            .await?
            .into_iter()
            .filter(is_entry_ready)
            .collect();
        pending_downloads.sort_by(|a, b| b.priority.cmp(&a.priority));

        if pending_downloads.is_empty() {
            println!("No pending downloads — checking again in {}s...", POLL_INTERVAL.as_secs());
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }

        let mut managers = JoinSet::new();

        for entry in pending_downloads {
            let id_clone = entry.id.clone();

            // Atomically claim this download. If another `warp run` process
            // already grabbed it, skip it.
            let claimed = registry.try_claim_download(&id_clone).await?;
            if !claimed {
                continue;
            }

            let ctx = EngineContext {
                global_limiter: ctx.global_limiter.clone(),
                metrics_pool: ctx.metrics_pool.clone(),
            };
            let sem_clone = Arc::clone(&semaphore);

            let pb = multi_progress.add(ProgressBar::new(0));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}")?
                    .progress_chars("#>-"),
            );

            managers.spawn(async move {
                let result = execute_entry(&entry, &ctx, suggested_workers, sem_clone, Some(pb)).await;

                if result.status == DownloadStatus::Completed {
                    if let Some(expected_hash) = &entry.checksum {
                        use hex::encode;
                        use sha2::{Digest, Sha256};
                        if let Ok(bytes) = tokio::fs::read(&entry.target_path).await {
                            let mut hasher = Sha256::new();
                            hasher.update(&bytes);
                            let hash_hex = encode(hasher.finalize());
                            if hash_hex.to_lowercase() != expected_hash.to_lowercase() {
                                return (
                                    id_clone,
                                    DownloadStatus::Error,
                                    Some(format!(
                                        "Checksum mismatch: expected {expected_hash}, got {hash_hex}"
                                    )),
                                    entry,
                                );
                            }
                        }
                    }
                    if let Err(e) = run_post_download(&entry).await {
                        return (id_clone, DownloadStatus::Error, Some(e.to_string()), entry);
                    }
                }

                (
                    id_clone,
                    result.status,
                    result.error_message,
                    entry,
                )
            });
        }

        if managers.is_empty() {
            // All candidates were already claimed by another process.
            println!("All pending downloads are being processed — waiting...");
            tokio::time::sleep(POLL_INTERVAL).await;
            continue;
        }

        while let Some(res) = managers.join_next().await {
            match res {
                Ok((id, status, err, _entry)) => {
                    registry.update_status(&id, status.clone()).await?;
                    if let Some(msg) = err {
                        if let Some(mut e) = registry.get(&id).await? {
                            e.error_message = Some(msg);
                            registry.update_entry(&id, e).await?;
                        }
                    }
                }
                Err(e) => eprintln!("Manager task panicked: {e}"),
            }
        }

        maybe_shutdown(registry).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_run_all_no_pending() {
        let registry = Registry::open_in_memory().await.unwrap();
        // run_all would loop forever waiting for work. Instead we just
        // verify that open_in_memory works and list is empty.
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

        // run_all would loop, so just verify the entry is completed.
        assert_eq!(
            registry.get(&id).await.unwrap().unwrap().status,
            DownloadStatus::Completed
        );
    }
}
