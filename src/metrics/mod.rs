//! Per-host download metrics — speed, bytes, failures.

use std::time::Instant;

use anyhow::Result;
use sqlx::SqlitePool;
use url::Url;

#[derive(Debug, Clone, serde::Serialize)]
pub struct HostMetrics {
    pub host: String,
    pub downloads: i64,
    pub bytes_total: i64,
    pub avg_speed_bps: f64,
    pub failures: i64,
}

pub struct MetricsRecorder {
    pool: SqlitePool,
    started: Instant,
    bytes: u64,
    host: String,
}

impl MetricsRecorder {
    pub fn new(pool: SqlitePool, url: &str) -> Self {
        let host = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());

        Self {
            pool,
            started: Instant::now(),
            bytes: 0,
            host,
        }
    }

    pub fn add_bytes(&mut self, n: u64) {
        self.bytes += n;
    }

    pub async fn finish_success(self) -> Result<()> {
        self.record(false).await
    }

    pub async fn finish_failure(self) -> Result<()> {
        self.record(true).await
    }

    async fn record(self, failed: bool) -> Result<()> {
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let speed = self.bytes as f64 / elapsed;

        sqlx::query(
            r#"
            INSERT INTO host_metrics (host, downloads, bytes_total, avg_speed_bps, failures, updated_at)
            VALUES (?, 1, ?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(host) DO UPDATE SET
                downloads = downloads + 1,
                bytes_total = bytes_total + excluded.bytes_total,
                avg_speed_bps = (host_metrics.avg_speed_bps * host_metrics.downloads + excluded.avg_speed_bps)
                                  / (host_metrics.downloads + 1),
                failures = failures + excluded.failures,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(&self.host)
        .bind(self.bytes as i64)
        .bind(speed)
        .bind(if failed { 1i64 } else { 0i64 })
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

pub async fn list_host_metrics(pool: &SqlitePool) -> Result<Vec<HostMetrics>> {
    let rows = sqlx::query_as::<_, HostMetricsRow>(
        "SELECT host, downloads, bytes_total, avg_speed_bps, failures FROM host_metrics ORDER BY bytes_total DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| HostMetrics {
            host: r.host,
            downloads: r.downloads,
            bytes_total: r.bytes_total,
            avg_speed_bps: r.avg_speed_bps,
            failures: r.failures,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct HostMetricsRow {
    host: String,
    downloads: i64,
    bytes_total: i64,
    avg_speed_bps: f64,
    failures: i64,
}

pub async fn init_metrics_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS host_metrics (
            host TEXT PRIMARY KEY,
            downloads INTEGER NOT NULL DEFAULT 0,
            bytes_total INTEGER NOT NULL DEFAULT 0,
            avg_speed_bps REAL NOT NULL DEFAULT 0,
            failures INTEGER NOT NULL DEFAULT 0,
            updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_metrics_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn test_metrics_record_success() {
        let pool = test_pool().await;
        let mut rec = MetricsRecorder::new(pool.clone(), "http://cdn.example.com/file.bin");
        rec.add_bytes(10_000);
        rec.finish_success().await.unwrap();

        let stats = list_host_metrics(&pool).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].host, "cdn.example.com");
        assert_eq!(stats[0].downloads, 1);
        assert_eq!(stats[0].bytes_total, 10_000);
        assert_eq!(stats[0].failures, 0);
    }

    #[tokio::test]
    async fn test_metrics_record_failure_increments_failures() {
        let pool = test_pool().await;
        let rec = MetricsRecorder::new(pool.clone(), "http://bad.example.com/x");
        rec.finish_failure().await.unwrap();

        let stats = list_host_metrics(&pool).await.unwrap();
        assert_eq!(stats[0].failures, 1);
    }

    #[tokio::test]
    async fn test_metrics_running_average() {
        let pool = test_pool().await;
        let mut first = MetricsRecorder::new(pool.clone(), "http://host.test/a");
        first.add_bytes(1000);
        first.finish_success().await.unwrap();

        let mut second = MetricsRecorder::new(pool.clone(), "http://host.test/b");
        second.add_bytes(3000);
        second.finish_success().await.unwrap();

        let stats = list_host_metrics(&pool).await.unwrap();
        assert_eq!(stats[0].downloads, 2);
        assert_eq!(stats[0].bytes_total, 4000);
    }
}
