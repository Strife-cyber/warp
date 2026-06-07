use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tokio::fs;
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::core::{
    DownloadCategory, DownloadEntry, DownloadKind, DownloadStatus,
};

/// Remove orphaned `.warp` / `.hls.warp` snapshot files for a given target path.
async fn delete_warp_files(target: &Path) {
    let warp = target.with_extension("warp");
    if warp.exists() {
        fs::remove_file(warp).await.ok();
    }
    let hls_warp = target.with_extension("hls.warp");
    if hls_warp.exists() {
        fs::remove_file(hls_warp).await.ok();
    }
}

#[derive(sqlx::FromRow)]
struct DownloadRecord {
    id: String,
    url: String,
    target_path: String,
    status: DownloadStatus,
    error_message: Option<String>,
    priority: i64,
    proxy: Option<String>,
    checksum: Option<String>,
    max_speed_bytes: Option<i64>,
    download_kind: DownloadKind,
    category: DownloadCategory,
    hls_quality: Option<String>,
    hls_concurrent: Option<i64>,
    scheduled_at: Option<String>,
    mirror_urls: Option<String>,
    post_action_json: Option<String>,
}

impl From<DownloadRecord> for DownloadEntry {
    fn from(record: DownloadRecord) -> Self {
        let mirror_urls = record
            .mirror_urls
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let post_action = record
            .post_action_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        Self {
            id: record.id,
            url: record.url,
            target_path: PathBuf::from(record.target_path),
            status: record.status,
            error_message: record.error_message,
            priority: record.priority as u8,
            proxy: record.proxy,
            checksum: record.checksum,
            max_speed_bytes: record.max_speed_bytes.map(|v| v as u64),
            kind: record.download_kind,
            category: record.category,
            hls_quality: record.hls_quality,
            hls_concurrent: record.hls_concurrent.map(|v| v as u32),
            scheduled_at: record.scheduled_at,
            mirror_urls,
            post_action,
        }
    }
}

fn mirrors_json(entry: &DownloadEntry) -> Result<String> {
    Ok(serde_json::to_string(&entry.mirror_urls)?)
}

fn post_action_json(entry: &DownloadEntry) -> Result<String> {
    Ok(serde_json::to_string(&entry.post_action)?)
}

pub struct Repository {
    pool: SqlitePool,
}

impl Repository {
    pub async fn new(pool: SqlitePool) -> Result<Self> {
        let repo = Self { pool };
        repo.initialize().await?;
        Ok(repo)
    }

    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    async fn initialize(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS downloads (
                id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                target_path TEXT NOT NULL,
                status TEXT NOT NULL,
                error_message TEXT,
                priority INTEGER NOT NULL DEFAULT 0,
                proxy TEXT,
                checksum TEXT,
                max_speed_bytes INTEGER,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        self.migrate_columns().await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_downloads_priority ON downloads(priority DESC)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_downloads_category ON downloads(category)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        crate::metrics::init_metrics_schema(&self.pool).await?;

        Ok(())
    }

    async fn migrate_columns(&self) -> Result<()> {
        // Ignore errors — column may already exist on upgraded databases.
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN download_kind TEXT NOT NULL DEFAULT 'http'")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN category TEXT NOT NULL DEFAULT 'other'")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN hls_quality TEXT")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN hls_concurrent INTEGER")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN scheduled_at TEXT")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN mirror_urls TEXT")
            .execute(&self.pool).await;
        let _ = sqlx::query("ALTER TABLE downloads ADD COLUMN post_action_json TEXT")
            .execute(&self.pool).await;
        Ok(())
    }

    pub async fn is_empty(&self) -> Result<bool> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM downloads")
            .fetch_one(&self.pool)
            .await?;
        Ok(count.0 == 0)
    }

    pub async fn get_settings(&self) -> Result<crate::core::AppSettings> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM app_settings WHERE key = 'main'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(row
            .and_then(|(v,)| serde_json::from_str(&v).ok())
            .unwrap_or_default())
    }

    pub async fn save_settings(&self, settings: &crate::core::AppSettings) -> Result<()> {
        let json = serde_json::to_string(settings)?;
        sqlx::query(
            r#"
            INSERT INTO app_settings (key, value) VALUES ('main', ?)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
        )
        .bind(json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait]
pub trait DownloadRegistry {
    async fn add(&self, entry: DownloadEntry) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<DownloadEntry>>;
    async fn remove(&self, id: &str) -> Result<Option<DownloadEntry>>;
    async fn update_status(&self, id: &str, status: DownloadStatus) -> Result<()>;
    async fn update(&self, id: &str, entry: DownloadEntry) -> Result<()>;
    async fn list(&self) -> Result<Vec<DownloadEntry>>;
    async fn list_not_completed(&self) -> Result<Vec<DownloadEntry>>;
    async fn list_filtered(
        &self,
        category: Option<DownloadCategory>,
        search: Option<&str>,
    ) -> Result<Vec<DownloadEntry>>;
    async fn clean_completed(&self) -> Result<usize>;
}

const SELECT_BY_ID: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes, download_kind, category, hls_quality, hls_concurrent, scheduled_at, mirror_urls, post_action_json ",
    "FROM downloads WHERE id = ?"
);

const DELETE_RETURNING: &str = concat!(
    "DELETE FROM downloads WHERE id = ? RETURNING ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes, download_kind, category, hls_quality, hls_concurrent, scheduled_at, mirror_urls, post_action_json"
);

const SELECT_ALL: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes, download_kind, category, hls_quality, hls_concurrent, scheduled_at, mirror_urls, post_action_json ",
    "FROM downloads ORDER BY priority DESC, created_at ASC"
);

const SELECT_NOT_COMPLETED: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes, download_kind, category, hls_quality, hls_concurrent, scheduled_at, mirror_urls, post_action_json ",
    "FROM downloads WHERE status != ? ORDER BY priority DESC, created_at ASC"
);

const SELECT_COMPLETED: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes, download_kind, category, hls_quality, hls_concurrent, scheduled_at, mirror_urls, post_action_json ",
    "FROM downloads WHERE status = ? ORDER BY priority DESC, created_at ASC"
);

#[async_trait]
impl DownloadRegistry for Repository {
    async fn add(&self, entry: DownloadEntry) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO downloads (
                id, url, target_path, status, error_message, priority, proxy, checksum,
                max_speed_bytes, download_kind, category, hls_quality, hls_concurrent,
                scheduled_at, mirror_urls, post_action_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&entry.id)
        .bind(&entry.url)
        .bind(entry.target_path.to_string_lossy().as_ref())
        .bind(&entry.status)
        .bind(&entry.error_message)
        .bind(i64::from(entry.priority))
        .bind(&entry.proxy)
        .bind(&entry.checksum)
        .bind(entry.max_speed_bytes.map(i64::try_from).transpose()?)
        .bind(&entry.kind)
        .bind(&entry.category)
        .bind(&entry.hls_quality)
        .bind(entry.hls_concurrent.map(i64::from))
        .bind(&entry.scheduled_at)
        .bind(mirrors_json(&entry)?)
        .bind(post_action_json(&entry)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<DownloadEntry>> {
        let record = sqlx::query_as::<_, DownloadRecord>(SELECT_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(record.map(DownloadEntry::from))
    }

    async fn remove(&self, id: &str) -> Result<Option<DownloadEntry>> {
        let record = sqlx::query_as::<_, DownloadRecord>(DELETE_RETURNING)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(ref entry) = record {
            delete_warp_files(Path::new(&entry.target_path)).await;
        }
        Ok(record.map(DownloadEntry::from))
    }

    async fn update_status(&self, id: &str, status: DownloadStatus) -> Result<()> {
        let rows = sqlx::query(
            "UPDATE downloads SET status = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(&status)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if rows == 0 {
            bail!("download id {id} not found");
        }
        Ok(())
    }

    async fn update(&self, id: &str, entry: DownloadEntry) -> Result<()> {
        if entry.id != id {
            bail!("entry id mismatch: expected {id}, got {}", entry.id);
        }
        let rows = sqlx::query(
            r#"
            UPDATE downloads SET
                url = ?, target_path = ?, status = ?, error_message = ?, priority = ?,
                proxy = ?, checksum = ?, max_speed_bytes = ?, download_kind = ?, category = ?,
                hls_quality = ?, hls_concurrent = ?, scheduled_at = ?, mirror_urls = ?,
                post_action_json = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
        )
        .bind(&entry.url)
        .bind(entry.target_path.to_string_lossy().as_ref())
        .bind(&entry.status)
        .bind(&entry.error_message)
        .bind(i64::from(entry.priority))
        .bind(&entry.proxy)
        .bind(&entry.checksum)
        .bind(entry.max_speed_bytes.map(i64::try_from).transpose()?)
        .bind(&entry.kind)
        .bind(&entry.category)
        .bind(&entry.hls_quality)
        .bind(entry.hls_concurrent.map(i64::from))
        .bind(&entry.scheduled_at)
        .bind(mirrors_json(&entry)?)
        .bind(post_action_json(&entry)?)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if rows == 0 {
            bail!("download id {id} not found");
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<DownloadEntry>> {
        let records = sqlx::query_as::<_, DownloadRecord>(SELECT_ALL)
            .fetch_all(&self.pool)
            .await?;
        Ok(records.into_iter().map(DownloadEntry::from).collect())
    }

    async fn list_not_completed(&self) -> Result<Vec<DownloadEntry>> {
        let records = sqlx::query_as::<_, DownloadRecord>(SELECT_NOT_COMPLETED)
            .bind(DownloadStatus::Completed)
            .fetch_all(&self.pool)
            .await?;
        Ok(records.into_iter().map(DownloadEntry::from).collect())
    }

    async fn list_filtered(
        &self,
        category: Option<DownloadCategory>,
        search: Option<&str>,
    ) -> Result<Vec<DownloadEntry>> {
        let mut entries = self.list().await?;
        if let Some(cat) = category {
            entries.retain(|e| e.category == cat);
        }
        if let Some(q) = search {
            let q = q.to_ascii_lowercase();
            entries.retain(|e| {
                e.url.to_ascii_lowercase().contains(&q)
                    || e.target_path.to_string_lossy().to_ascii_lowercase().contains(&q)
                    || e.id.contains(&q)
            });
        }
        Ok(entries)
    }

    async fn clean_completed(&self) -> Result<usize> {
        // Fetch completed entries first so we know which .warp files to delete.
        let records = sqlx::query_as::<_, DownloadRecord>(SELECT_COMPLETED)
            .bind(DownloadStatus::Completed)
            .fetch_all(&self.pool)
            .await?;

        // Delete orphaned .warp / .hls.warp snapshot files.
        for record in &records {
            delete_warp_files(Path::new(&record.target_path)).await;
        }

        // Now delete the database entries.
        sqlx::query("DELETE FROM downloads WHERE status = ?")
            .bind(DownloadStatus::Completed)
            .execute(&self.pool)
            .await?;

        Ok(records.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_repo() -> Repository {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        Repository::new(pool).await.unwrap()
    }

    fn sample_entry(id: &str) -> DownloadEntry {
        DownloadEntry::new_http(
            id.to_string(),
            format!("http://example.com/{id}"),
            PathBuf::from(format!("{id}.bin")),
        )
    }

    #[tokio::test]
    async fn test_add_get_remove() {
        let repo = test_repo().await;
        let mut entry = sample_entry("1");
        entry.priority = 10;
        entry.proxy = Some("http://proxy:8080".into());
        entry.checksum = Some("abc".into());
        entry.max_speed_bytes = Some(1024);

        repo.add(entry.clone()).await.unwrap();
        assert_eq!(repo.get("1").await.unwrap().unwrap().url, entry.url);

        let removed = repo.remove("1").await.unwrap().unwrap();
        assert_eq!(removed.id, "1");
    }

    #[tokio::test]
    async fn test_list_filtered_by_category() {
        let repo = test_repo().await;
        let mut v = sample_entry("v");
        v.category = DownloadCategory::Video;
        repo.add(v).await.unwrap();
        repo.add(sample_entry("o")).await.unwrap();

        assert_eq!(
            repo.list_filtered(Some(DownloadCategory::Video), None)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn test_update_status() {
        let repo = test_repo().await;
        repo.add(sample_entry("2")).await.unwrap();

        repo.update_status("2", DownloadStatus::Downloading)
            .await
            .unwrap();
        assert_eq!(
            repo.get("2").await.unwrap().unwrap().status,
            DownloadStatus::Downloading
        );

        repo.update_status("2", DownloadStatus::Completed)
            .await
            .unwrap();
        assert_eq!(
            repo.get("2").await.unwrap().unwrap().status,
            DownloadStatus::Completed
        );
    }

    #[tokio::test]
    async fn test_update_entry() {
        let repo = test_repo().await;
        repo.add(sample_entry("3")).await.unwrap();

        let mut updated = sample_entry("3");
        updated.url = "http://changed.com".to_string();
        updated.priority = 99;
        repo.update("3", updated.clone()).await.unwrap();

        let fetched = repo.get("3").await.unwrap().unwrap();
        assert_eq!(fetched.url, "http://changed.com");
        assert_eq!(fetched.priority, 99);
    }

    #[tokio::test]
    async fn test_list_and_clean_completed() {
        let repo = test_repo().await;
        repo.add(sample_entry("a")).await.unwrap();
        let mut completed = sample_entry("b");
        completed.status = DownloadStatus::Completed;
        repo.add(completed).await.unwrap();

        assert_eq!(repo.list().await.unwrap().len(), 2);
        assert_eq!(repo.list_not_completed().await.unwrap().len(), 1);

        let removed = repo.clean_completed().await.unwrap();
        assert_eq!(removed, 1);
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_persistence_across_connections() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        let url = format!("sqlite:{}?mode=rwc", db_path.display());

        {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            let repo = Repository::new(pool).await.unwrap();
            repo.add(sample_entry("persist")).await.unwrap();
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let repo = Repository::new(pool).await.unwrap();
        assert!(repo.get("persist").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_extended_fields_roundtrip() {
        use crate::core::{DownloadKind, PostDownloadAction};

        let repo = test_repo().await;
        let mut entry = sample_entry("ext");
        entry.kind = DownloadKind::Hls;
        entry.hls_quality = Some("high".into());
        entry.hls_concurrent = Some(4);
        entry.mirror_urls = vec!["http://mirror.example.com".into()];
        entry.post_action = PostDownloadAction {
            delete_warp: true,
            ..Default::default()
        };

        repo.add(entry.clone()).await.unwrap();
        let loaded = repo.get("ext").await.unwrap().unwrap();
        assert_eq!(loaded.kind, DownloadKind::Hls);
        assert_eq!(loaded.hls_quality.as_deref(), Some("high"));
        assert_eq!(loaded.mirror_urls.len(), 1);
        assert!(loaded.post_action.delete_warp);
    }

    #[tokio::test]
    async fn test_list_filtered_by_search() {
        let repo = test_repo().await;
        let mut a = sample_entry("a");
        a.url = "http://unique-host.example/file".into();
        repo.add(a).await.unwrap();
        repo.add(sample_entry("b")).await.unwrap();

        let hits = repo
            .list_filtered(None, Some("unique-host"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    #[tokio::test]
    async fn test_settings_roundtrip() {
        use crate::core::AppSettings;

        let repo = test_repo().await;
        let settings = AppSettings {
            global_max_speed_bytes: Some(512_000),
            daemon_port: 9999,
            ..Default::default()
        };
        repo.save_settings(&settings).await.unwrap();
        let loaded = repo.get_settings().await.unwrap();
        assert_eq!(loaded.global_max_speed_bytes, Some(512_000));
        assert_eq!(loaded.daemon_port, 9999);
    }
}
