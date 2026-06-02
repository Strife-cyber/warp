use anyhow::Result;
use sqlx::SqlitePool;
use async_trait::async_trait;
use crate::registry::{DownloadEntry, DownloadStatus};

pub struct Repository {
    pool: SqlitePool
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
                "#
        ).execute(&self.pool).await?;

        Ok(())
    }
}


#[async_trait]
pub trait DownloadRegistry {
    async fn add(&self, entry: DownloadEntry) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<DownloadEntry>>;
    async fn remove(&self, id: &str) -> Result<()>;
    async fn update_status(
        &self,
        id: &str,
        status: DownloadStatus
    ) -> Result<()>;
    async fn update(&self, id: &str, entry: DownloadEntry) -> Result<()>;
    async fn list(&self) -> Result<Vec<DownloadEntry>>;
    async fn clean_completed(&self) -> Result<usize>;
}

#[async_trait]
impl DownloadRegistry for Repository {
    async fn add(&self, entry: DownloadEntry) -> Result<()> {
        sqlx::query(
            r#"
                    INSERT INTO downloads (id, url, target_path, status, error_message, priority, proxy, checksum, max_speed_bytes)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#
        )
            .bind(&entry.id)
            .bind(&entry.url)
            .bind(entry.target_path.to_string_lossy().to_string())
            .bind(entry.status)
            .bind(&entry.error_message)
            .bind(entry.priority)
            .bind(&entry.proxy)
            .bind(&entry.checksum)
            .bind(entry.max_speed_bytes.map(|v| v as i64))
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<DownloadEntry>> {
        todo!()
    }

    async fn remove(&self, id: &str) -> Result<()> {
        todo!()
    }

    async fn update_status(&self, id: &str, status: DownloadStatus) -> Result<()> {
        todo!()
    }

    async fn update(&self, id: &str, entry: DownloadEntry) -> Result<()> {
        todo!()
    }

    async fn list(&self) -> Result<Vec<DownloadEntry>> {
        todo!()
    }

    async fn clean_completed(&self) -> Result<usize> {
        todo!()
    }
}
