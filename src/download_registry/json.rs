//! Legacy JSON-backed registry — kept for tests and one-time migration from disk.
//!
//! The app runtime uses SQLite (`Repository`); this module remains so existing JSON
//! files can be imported and unit tests can exercise the old format without a DB.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::core::{DownloadEntry, DownloadStatus};

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Registry {
    pub downloads: HashMap<String, DownloadEntry>,
    #[serde(skip)]
    custom_path: Option<PathBuf>,
}

#[allow(dead_code)] // kept for parity with SQLite API and legacy import/tests
impl Registry {
    fn get_registry_path(&self) -> Result<PathBuf> {
        if let Some(ref path) = self.custom_path {
            return Ok(path.clone());
        }
        let proj_dirs = ProjectDirs::from("com", "warp", "warp")
            .context("Could not determine project directories")?;
        let config_dir = proj_dirs.config_dir();

        if !config_dir.exists() {
            std::fs::create_dir_all(config_dir).context("Failed to create config directory")?;
        }

        Ok(config_dir.join("download_registry.json"))
    }

    pub fn load() -> Result<Self> {
        let dummy = Self::default();
        let path = dummy.get_registry_path()?;
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            match serde_json::from_str::<Registry>(&data) {
                Ok(mut registry) => {
                    registry.custom_path = None;
                    Ok(registry)
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse download_registry file at {}: {}",
                        path.display(),
                        e
                    );
                    eprintln!("Creating a new empty download_registry...");
                    Ok(Registry::default())
                }
            }
        } else {
            Ok(Registry::default())
        }
    }

    pub fn registry_path(&self) -> Result<PathBuf> {
        self.get_registry_path()
    }

    pub fn save(&self) -> Result<()> {
        let path = self.get_registry_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    pub fn add(&mut self, url: String, target_path: PathBuf) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();

        let entry = DownloadEntry::new_http(id.clone(), url, target_path);

        self.downloads.insert(id.clone(), entry);
        id
    }

    pub fn remove(&mut self, id: &str) -> Option<DownloadEntry> {
        self.downloads.remove(id)
    }

    pub fn update_status(&mut self, id: &str, status: DownloadStatus) {
        if let Some(entry) = self.downloads.get_mut(id) {
            entry.status = status;
        }
    }

    pub fn update_advanced(
        &mut self,
        id: &str,
        priority: Option<u8>,
        proxy: Option<String>,
        checksum: Option<String>,
        max_speed_bytes: Option<u64>,
    ) {
        if let Some(entry) = self.downloads.get_mut(id) {
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
        }
    }

    pub fn clean_completed(&mut self) -> usize {
        let before = self.downloads.len();
        self.downloads
            .retain(|_, entry| entry.status != DownloadStatus::Completed);
        before - self.downloads.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_registry_add_remove() {
        let mut registry = Registry::default();
        let id = registry.add("http://example.com".to_string(), PathBuf::from("file.txt"));

        assert_eq!(registry.downloads.len(), 1);
        assert!(registry.downloads.contains_key(&id));

        let removed = registry.remove(&id).unwrap();
        assert_eq!(removed.url, "http://example.com");
        assert_eq!(registry.downloads.len(), 0);
    }

    #[test]
    fn test_registry_save_load() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();

        let mut registry = Registry::default();
        registry.custom_path = Some(path.clone());

        let id = registry.add("http://test.com".to_string(), PathBuf::from("test.zip"));
        registry.save().unwrap();

        let data = std::fs::read_to_string(&path).unwrap();
        let mut loaded: Registry = serde_json::from_str(&data).unwrap();
        loaded.custom_path = Some(path);

        assert!(loaded.downloads.contains_key(&id));
        assert_eq!(loaded.downloads.get(&id).unwrap().url, "http://test.com");
    }

    #[test]
    fn test_update_status() {
        let mut registry = Registry::default();
        let id = registry.add("url".to_string(), PathBuf::from("path"));

        registry.update_status(&id, DownloadStatus::Completed);
        assert_eq!(
            registry.downloads.get(&id).unwrap().status,
            DownloadStatus::Completed
        );

        registry.update_status(&id, DownloadStatus::Error);
        assert_eq!(
            registry.downloads.get(&id).unwrap().status,
            DownloadStatus::Error
        );
    }
}
