//! Post-download automation — move, shell hook, cleanup, optional shutdown.

use std::process::Command;

use anyhow::{Context, Result};

use crate::download_registry::Registry;
use crate::core::{DownloadEntry, DownloadStatus};

pub async fn run_post_download(entry: &DownloadEntry) -> Result<()> {
    let action = &entry.post_action;

    if let Some(ref dest) = action.move_to {
        if entry.target_path.exists() {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::rename(&entry.target_path, dest)
                .await
                .with_context(|| format!("move {} → {}", entry.target_path.display(), dest.display()))?;
        }
    }

    if action.delete_warp {
        let warp = entry.target_path.with_extension("warp");
        if warp.exists() {
            tokio::fs::remove_file(warp).await.ok();
        }
        let hls_warp = entry.target_path.with_extension("hls.warp");
        if hls_warp.exists() {
            tokio::fs::remove_file(hls_warp).await.ok();
        }
    }

    if let Some(ref cmdline) = action.run_command {
        #[cfg(windows)]
        let status = Command::new("cmd")
            .args(["/C", cmdline])
            .status()
            .context("failed to spawn post-download command")?;
        #[cfg(not(windows))]
        let status = Command::new("sh")
            .args(["-c", cmdline])
            .status()
            .context("failed to spawn post-download command")?;

        if !status.success() {
            anyhow::bail!("post-download command exited with {status}");
        }
    }

    Ok(())
}

pub async fn maybe_shutdown(registry: &Registry) -> Result<()> {
    let pending = registry.list_not_completed().await?;
    let wants_shutdown = pending.iter().any(|e| e.post_action.shutdown_when_queue_empty);

    if wants_shutdown && pending.is_empty() {
        println!("Queue empty — shutdown requested by post-action.");
        #[cfg(windows)]
        Command::new("shutdown").args(["/s", "/t", "60"]).spawn()?;
        #[cfg(not(windows))]
        Command::new("shutdown").args(["-h", "+1"]).spawn()?;
    }

    // Also trigger shutdown when all remaining are completed (not error/paused)
    let all = registry.list().await?;
    let active = all
        .iter()
        .filter(|e| e.status != DownloadStatus::Completed)
        .count();
    if active == 0
        && all.iter().any(|e| e.post_action.shutdown_when_queue_empty)
    {
        println!("All downloads finished — initiating shutdown.");
        #[cfg(windows)]
        Command::new("shutdown").args(["/s", "/t", "60"]).spawn()?;
        #[cfg(not(windows))]
        Command::new("shutdown").args(["-h", "+1"]).spawn()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PostDownloadAction;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_move_after_download() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("file.bin");
        let dest = dir.path().join("done").join("file.bin");
        tokio::fs::write(&src, b"data").await.unwrap();

        let entry = DownloadEntry {
            target_path: src,
            post_action: PostDownloadAction {
                move_to: Some(dest.clone()),
                ..Default::default()
            },
            ..DownloadEntry::new_http("1".into(), "http://x".into(), PathBuf::from("x"))
        };

        run_post_download(&entry).await.unwrap();
        assert!(dest.exists());
        assert!(!entry.target_path.exists());
    }

    #[tokio::test]
    async fn test_delete_warp_after_download() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("file.bin");
        let warp = target.with_extension("warp");
        tokio::fs::write(&target, b"x").await.unwrap();
        tokio::fs::write(&warp, b"snap").await.unwrap();

        let entry = DownloadEntry {
            target_path: target,
            post_action: PostDownloadAction {
                delete_warp: true,
                ..Default::default()
            },
            ..DownloadEntry::new_http("1".into(), "http://x".into(), PathBuf::from("x"))
        };

        run_post_download(&entry).await.unwrap();
        assert!(!warp.exists());
    }

    #[tokio::test]
    async fn test_maybe_shutdown_noop_without_flag() {
        let registry = Registry::open_in_memory().await.unwrap();
        maybe_shutdown(&registry).await.unwrap();
    }
}
