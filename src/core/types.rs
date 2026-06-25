use std::path::{Path, PathBuf};

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

#[derive(sqlx::Type, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum DownloadKind {
    #[default]
    Http,
    Hls,
}

#[derive(sqlx::Type, Serialize, Deserialize, Debug, Clone, PartialEq, Default, Eq, Hash)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum DownloadCategory {
    #[default]
    Other,
    Video,
    Audio,
    Archive,
    Document,
    Image,
}

impl DownloadCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Other => "Other",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Archive => "Archive",
            Self::Document => "Document",
            Self::Image => "Image",
        }
    }

    pub fn all() -> &'static [DownloadCategory] {
        &[
            Self::Other,
            Self::Video,
            Self::Audio,
            Self::Archive,
            Self::Document,
            Self::Image,
        ]
    }
}

/// Actions executed after a download reaches Completed.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PostDownloadAction {
    #[serde(default)]
    pub move_to: Option<PathBuf>,
    #[serde(default)]
    pub run_command: Option<String>,
    #[serde(default)]
    pub delete_warp: bool,
    /// Shut down the machine when the entire queue is idle and completed.
    #[serde(default)]
    pub shutdown_when_queue_empty: bool,
}

/// Allowed download window (local time, inclusive start, exclusive end).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ScheduleWindow {
    pub start_hour: u8,
    pub start_minute: u8,
    pub end_hour: u8,
    pub end_minute: u8,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppSettings {
    #[serde(default = "default_daemon_port")]
    pub daemon_port: u16,
    #[serde(default)]
    pub global_max_speed_bytes: Option<u64>,
    #[serde(default)]
    pub schedule_windows: Vec<ScheduleWindow>,
    /// Upper bound on concurrent segment workers (auto-tuned below this).
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,
    /// Allow notifications (e.g. when a download is paused).
    #[serde(default)]
    pub allow_notifications: bool
}

fn default_max_workers() -> usize {
    32
}

fn default_daemon_port() -> u16 {
    9844
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            daemon_port: default_daemon_port(),
            global_max_speed_bytes: None,
            schedule_windows: Vec::new(),
            max_workers: default_max_workers(),
            allow_notifications: true
        }
    }
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
    #[serde(default)]
    pub kind: DownloadKind,
    #[serde(default)]
    pub category: DownloadCategory,
    #[serde(default)]
    pub hls_quality: Option<String>,
    #[serde(default)]
    pub hls_concurrent: Option<u32>,
    /// ISO-8601 UTC timestamp — download won't start until this moment.
    #[serde(default)]
    pub scheduled_at: Option<String>,
    #[serde(default)]
    pub mirror_urls: Vec<String>,
    #[serde(default)]
    pub post_action: PostDownloadAction,
}

impl DownloadEntry {
    pub fn new_http(id: String, url: String, target_path: PathBuf) -> Self {
        let category = infer_category(&url, &target_path);
        Self {
            id,
            url,
            target_path,
            status: DownloadStatus::Pending,
            priority: 0,
            proxy: None,
            checksum: None,
            max_speed_bytes: None,
            error_message: None,
            kind: DownloadKind::Http,
            category,
            hls_quality: None,
            hls_concurrent: None,
            scheduled_at: None,
            mirror_urls: Vec::new(),
            post_action: PostDownloadAction::default(),
        }
    }

    pub fn new_hls(id: String, url: String, target_path: PathBuf, quality: Option<String>, concurrent: Option<u32>) -> Self {
        Self {
            kind: DownloadKind::Hls,
            category: DownloadCategory::Video,
            hls_quality: quality,
            hls_concurrent: concurrent,
            ..Self::new_http(id, url, target_path)
        }
    }
}

/// Guess category from URL extension or filename.
pub fn infer_category(url: &str, path: &Path) -> DownloadCategory {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .or_else(|| {
            url.split('?')
                .next()
                .and_then(|u| u.rsplit('.').next())
        })
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "ts" | "m3u8" | "flv" => DownloadCategory::Video,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" => DownloadCategory::Audio,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => DownloadCategory::Archive,
        "pdf" | "doc" | "docx" | "txt" | "md" | "epub" => DownloadCategory::Document,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" => DownloadCategory::Image,
        _ => DownloadCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_infer_category_from_path() {
        assert_eq!(
            infer_category("http://x.com", Path::new("movie.mp4")),
            DownloadCategory::Video
        );
        assert_eq!(
            infer_category("http://x.com", Path::new("doc.pdf")),
            DownloadCategory::Document
        );
        assert_eq!(
            infer_category("http://x.com/file.zip", Path::new("download")),
            DownloadCategory::Archive
        );
    }

    #[test]
    fn test_new_http_defaults() {
        let entry = DownloadEntry::new_http(
            "1".into(),
            "http://example.com/video.mp4".into(),
            PathBuf::from("clip.mp4"),
        );
        assert_eq!(entry.kind, DownloadKind::Http);
        assert_eq!(entry.category, DownloadCategory::Video);
        assert_eq!(entry.status, DownloadStatus::Pending);
    }

    #[test]
    fn test_new_hls_sets_kind_and_quality() {
        let entry = DownloadEntry::new_hls(
            "1".into(),
            "http://cdn/play.m3u8".into(),
            PathBuf::from("out.ts"),
            Some("high".into()),
            Some(8),
        );
        assert_eq!(entry.kind, DownloadKind::Hls);
        assert_eq!(entry.hls_quality.as_deref(), Some("high"));
        assert_eq!(entry.hls_concurrent, Some(8));
    }

    #[test]
    fn test_category_labels() {
        assert_eq!(DownloadCategory::Video.label(), "Video");
        assert_eq!(DownloadCategory::all().len(), 6);
    }
}
