use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(sqlx::Type, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[sqlx(type_name = "TEXT", rename_all = "PascalCase")]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Paused,
    Completed,
    Error,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DownloadEntry {
    pub id: String,
    pub url: String,
    pub target_path: PathBuf,
    pub status: DownloadStatus,
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub max_speed_bytes: Option<u64>,
    #[serde(default)]
    pub error_message: Option<String>,
}
