use std::path::PathBuf;

use anyhow::{Result, bail};
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::registry::{DownloadEntry, DownloadStatus};

/// SQLite row shape — `target_path` is stored as TEXT; we convert to `PathBuf` at the edge.
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
}

impl From<DownloadRecord> for DownloadEntry {
    fn from(record: DownloadRecord) -> Self {
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
        }
    }
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

        // Hot-path indexes: engine filters by status, sorts by priority.
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

        Ok(())
    }

    /// Returns true when the registry already holds at least one row.
    pub async fn is_empty(&self) -> Result<bool> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM downloads")
                .fetch_one(&self.pool)
                .await?;
        Ok(count.0 == 0)
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
    /// Active downloads only — avoids loading completed rows into memory for the engine.
    async fn list_not_completed(&self) -> Result<Vec<DownloadEntry>>;
    async fn clean_completed(&self) -> Result<usize>;
}

const SELECT_BY_ID: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes ",
    "FROM downloads WHERE id = ?"
);

const DELETE_RETURNING: &str = concat!(
    "DELETE FROM downloads WHERE id = ? RETURNING ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes"
);

const SELECT_ALL: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes ",
    "FROM downloads ORDER BY priority DESC, created_at ASC"
);

const SELECT_NOT_COMPLETED: &str = concat!(
    "SELECT ",
    "id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes ",
    "FROM downloads WHERE status != ? ORDER BY priority DESC, created_at ASC"
);

#[async_trait]
impl DownloadRegistry for Repository {
    async fn add(&self, entry: DownloadEntry) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO downloads (
                id, url, target_path, status, error_message,
                priority, proxy, checksum, max_speed_bytes
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        // RETURNING avoids a separate SELECT+DELETE round trip (SQLite 3.35+).
        let record = sqlx::query_as::<_, DownloadRecord>(DELETE_RETURNING)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.map(DownloadEntry::from))
    }

    async fn update_status(&self, id: &str, status: DownloadStatus) -> Result<()> {
        let rows = sqlx::query(
            r#"
            UPDATE downloads
            SET status = ?, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
            "#,
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
                url = ?,
                target_path = ?,
                status = ?,
                error_message = ?,
                priority = ?,
                proxy = ?,
                checksum = ?,
                max_speed_bytes = ?,
                updated_at = CURRENT_TIMESTAMP
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

    async fn clean_completed(&self) -> Result<usize> {
        let result = sqlx::query("DELETE FROM downloads WHERE status = ?")
            .bind(DownloadStatus::Completed)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() as usize)
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
        DownloadEntry {
            id: id.to_string(),
            url: format!("http://example.com/{id}"),
            target_path: PathBuf::from(format!("{id}.bin")),
            status: DownloadStatus::Pending,
            priority: 10,
            proxy: Some("http://proxy:8080".to_string()),
            checksum: Some("abc123".to_string()),
            max_speed_bytes: Some(1024),
            error_message: None,
        }
    }

    #[tokio::test]
    async fn test_add_get_remove() {
        let repo = test_repo().await;
        let entry = sample_entry("1");

        repo.add(entry.clone()).await.unwrap();
        let fetched = repo.get("1").await.unwrap().unwrap();
        assert_eq!(fetched.url, entry.url);
        assert_eq!(fetched.priority, 10);

        let removed = repo.remove("1").await.unwrap().unwrap();
        assert_eq!(removed.id, "1");
        assert!(repo.get("1").await.unwrap().is_none());
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

        repo.update_status("2", DownloadStatus::Completed).await.unwrap();
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
}
