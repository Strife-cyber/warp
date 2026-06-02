//! Unified download dispatch — HTTP and HLS share one execution path.

use std::sync::Arc;

use anyhow::Result;
use indicatif::ProgressBar;
use tokio::sync::Semaphore;

use crate::core::{DownloadEntry, DownloadKind, DownloadStatus};
use crate::download::{Manager, rate_limit::RateLimiter, RunLimits};
use crate::hls;
use crate::metrics::MetricsRecorder;

pub struct EngineContext {
    pub global_limiter: Option<Arc<RateLimiter>>,
    pub metrics_pool: sqlx::SqlitePool,
}

pub struct ExecuteResult {
    pub status: DownloadStatus,
    pub error_message: Option<String>,
}

/// Run a single registry entry through the appropriate backend.
pub async fn execute_entry(
    entry: &DownloadEntry,
    ctx: &EngineContext,
    suggested_workers: usize,
    semaphore: Arc<Semaphore>,
    pb: Option<ProgressBar>,
) -> ExecuteResult {
    let mut metrics = MetricsRecorder::new(ctx.metrics_pool.clone(), &entry.url);

    let limits = RunLimits {
        global: ctx.global_limiter.clone(),
        local: entry
            .max_speed_bytes
            .map(|b| Arc::new(RateLimiter::new(b))),
    };

    let outcome = match entry.kind {
        DownloadKind::Hls => {
            hls::run_entry(entry, semaphore, limits.clone(), pb.as_ref()).await
        }
        DownloadKind::Http => {
            run_http(entry, suggested_workers, semaphore, limits, pb).await
        }
    };

    match &outcome {
        Ok(bytes) => {
            metrics.add_bytes(*bytes);
            let _ = metrics.finish_success().await;
        }
        Err(_) => {
            let _ = metrics.finish_failure().await;
        }
    }

    match outcome {
        Ok(_bytes) => ExecuteResult {
            status: DownloadStatus::Completed,
            error_message: None,
        },
        Err(e) => ExecuteResult {
            status: DownloadStatus::Error,
            error_message: Some(e.to_string()),
        },
    }
}

async fn run_http(
    entry: &DownloadEntry,
    suggested_workers: usize,
    semaphore: Arc<Semaphore>,
    limits: RunLimits,
    pb: Option<ProgressBar>,
) -> Result<u64> {
    let mut manager = Manager::from_entry(entry).await?;
    if let Some(bar) = pb {
        manager.set_progress_bar(bar);
    }
    manager.run(suggested_workers, semaphore, limits).await?;
    Ok(manager.metadata.total_progress().await)
}
