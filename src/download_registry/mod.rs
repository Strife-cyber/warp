//! Persistent download registry backed by SQLite.
//!
//! Each mutation is a single SQL statement — no in-memory HashMap, no whole-file
//! rewrite — so concurrent CLI/TUI/engine processes can add and run downloads
//! without clobbering each other.

pub mod json;
pub mod repository;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::ConnectOptions;

use crate::core::{DownloadCategory, DownloadEntry, DownloadStatus, AppSettings};
use repository::{DownloadRegistry, Repository};


/// Application-facing registry — thin wrapper over the SQLite `Repository`.
#[derive(Clone)]
pub struct Registry {
    inner: Arc<Repository>,
}

impl Registry {
    /// Opens (or creates) the default SQLite registry and migrates legacy JSON once.
    pub async fn open() -> Result<Self> {
        let pool = create_pool(default_db_path()?).await?;
        let repo = Repository::new(pool).await?;
        migrate_json_if_needed(&repo).await?;
        Ok(Self {
            inner: Arc::new(repo),
        })
    }

    /// In-memory SQLite — for unit tests only.
    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let repo = Repository::new(pool).await?;
        Ok(Self {
            inner: Arc::new(repo),
        })
    }

    pub async fn add_hls(
        &self,
        url: String,
        target_path: PathBuf,
        quality: Option<String>,
        concurrent: Option<u32>,
    ) -> Result<String> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before UNIX epoch")?
            .as_secs()
            .to_string();

        let entry = DownloadEntry::new_hls(id.clone(), url, target_path, quality, concurrent);
        self.inner.add(entry).await?;
        Ok(id)
    }

    pub async fn add(&self, url: String, target_path: PathBuf) -> Result<String> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before UNIX epoch")?
            .as_secs()
            .to_string();

        let entry = DownloadEntry::new_http(id.clone(), url, target_path);

        self.inner.add(entry).await?;
        Ok(id)
    }

    pub async fn get(&self, id: &str) -> Result<Option<DownloadEntry>> {
        self.inner.get(id).await
    }

    pub async fn remove(&self, id: &str) -> Result<Option<DownloadEntry>> {
        self.inner.remove(id).await
    }

    pub async fn update_status(&self, id: &str, status: DownloadStatus) -> Result<()> {
        self.inner.update_status(id, status).await
    }

    pub async fn update_entry(&self, id: &str, entry: DownloadEntry) -> Result<()> {
        self.inner.update(id, entry).await
    }

    pub async fn update_advanced(
        &self,
        id: &str,
        priority: Option<u8>,
        proxy: Option<String>,
        checksum: Option<String>,
        max_speed_bytes: Option<u64>,
    ) -> Result<()> {
        let mut entry = self
            .inner
            .get(id)
            .await?
            .with_context(|| format!("download id {id} not found"))?;

        if let Some(p) = priority {
            entry.priority = p;
        }
        if proxy.is_some() {
            entry.proxy = proxy;
        }
        if checksum.is_some() {
            entry.checksum = checksum;
        }
        if max_speed_bytes.is_some() {
            entry.max_speed_bytes = max_speed_bytes;
        }

        self.inner.update(id, entry).await
    }

    pub async fn list_filtered(
        &self,
        category: Option<DownloadCategory>,
        search: Option<&str>,
    ) -> Result<Vec<DownloadEntry>> {
        self.inner.list_filtered(category, search).await
    }

    pub async fn get_settings(&self) -> Result<AppSettings> {
        self.inner.get_settings().await
    }

    pub async fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.inner.save_settings(settings).await
    }

    pub fn pool(&self) -> sqlx::SqlitePool {
        self.inner.pool()
    }

    pub async fn list(&self) -> Result<Vec<DownloadEntry>> {
        self.inner.list().await
    }

    pub async fn list_not_completed(&self) -> Result<Vec<DownloadEntry>> {
        self.inner.list_not_completed().await
    }

    pub async fn clean_completed(&self) -> Result<usize> {
        self.inner.clean_completed().await
    }
}

fn default_db_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("com", "warp", "warp")
        .context("Could not determine project directories")?;
    let config_dir = proj_dirs.config_dir();
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir).context("Failed to create config directory")?;
    }
    Ok(config_dir.join("download_registry.db"))
}

async fn create_pool(db_path: PathBuf) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        // WAL lets readers (list/run) proceed while writers (add/status) commit.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // Busy timeout: retry instead of failing when CLI and TUI touch the DB at once.
        .busy_timeout(std::time::Duration::from_secs(5))
        .disable_statement_logging();

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("failed to open registry database at {}", db_path.display()))?;

    Ok(pool)
}

/// One-time import from `download_registry.json` when the SQLite file is still empty.
async fn migrate_json_if_needed(repo: &Repository) -> Result<()> {
    if !repo.is_empty().await? {
        return Ok(());
    }

    let json_registry = json::Registry::load()?;
    if json_registry.downloads.is_empty() {
        return Ok(());
    }

    for entry in json_registry.downloads.values() {
        repo.add(entry.clone()).await?;
    }

    // Rename JSON so we never double-import; user keeps a backup on disk.
    if let Ok(json_path) = json_registry.registry_path() {
        if json_path.exists() {
            let backup = json_path.with_extension("json.bak");
            if let Err(e) = std::fs::rename(&json_path, &backup) {
                eprintln!(
                    "Warning: migrated {} entries to SQLite but could not rename JSON backup: {e}",
                    json_registry.downloads.len()
                );
            } else {
                eprintln!(
                    "Migrated {} download(s) from {} to SQLite.",
                    json_registry.downloads.len(),
                    json_path.display()
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_add_and_list() {
        let registry = Registry::open_in_memory().await.unwrap();
        let id = registry
            .add("http://example.com".to_string(), PathBuf::from("file.bin"))
            .await
            .unwrap();

        let entries = registry.list().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].category, crate::core::DownloadCategory::Other);
    }

    #[tokio::test]
    async fn test_registry_update_advanced() {
        let registry = Registry::open_in_memory().await.unwrap();
        let id = registry
            .add("http://example.com".to_string(), PathBuf::from("f.bin"))
            .await
            .unwrap();

        registry
            .update_advanced(&id, Some(5), Some("http://proxy:8080".into()), None, Some(1024))
            .await
            .unwrap();

        let entry = registry.get(&id).await.unwrap().unwrap();
        assert_eq!(entry.priority, 5);
        assert_eq!(entry.proxy.as_deref(), Some("http://proxy:8080"));
        assert_eq!(entry.max_speed_bytes, Some(1024));
    }
}
