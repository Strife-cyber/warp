use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Serialize, Deserialize};
use directories::ProjectDirs;
use anyhow::{Context, Result};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Paused,
    Completed,
    Error(String),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DownloadEntry {
    pub id: String,
    pub url: String,
    pub target_path: PathBuf,
    pub status: DownloadStatus,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Registry {
    pub downloads: HashMap<String, DownloadEntry>,
    #[serde(skip)]
    custom_path: Option<PathBuf>,
}

impl Registry {
    /// Returns the path to the registry JSON file, creating the directory if needed.
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
        
        Ok(config_dir.join("registry.json"))
    }

    /// Loads the registry from disk, or creates a new empty one if it doesn't exist.
    pub fn load() -> Result<Self> {
        let dummy = Self::default();
        let path = dummy.get_registry_path()?;
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            let mut registry: Registry = serde_json::from_str(&data)?;
            registry.custom_path = None;
            Ok(registry)
        } else {
            Ok(Registry::default())
        }
    }

    /// Saves the current state of the registry to disk.
    pub fn save(&self) -> Result<()> {
        let path = self.get_registry_path()?;
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Adds a new download entry and returns its generated ID.
    pub fn add(&mut self, url: String, target_path: PathBuf) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string(); // Simple ID generation
            
        let entry = DownloadEntry {
            id: id.clone(),
            url,
            target_path,
            status: DownloadStatus::Pending,
        };
        
        self.downloads.insert(id.clone(), entry);
        id
    }

    /// Removes an entry by ID.
    pub fn remove(&mut self, id: &str) -> Option<DownloadEntry> {
        self.downloads.remove(id)
    }

    /// Updates the status of a specific download.
    pub fn update_status(&mut self, id: &str, status: DownloadStatus) {
        if let Some(entry) = self.downloads.get_mut(id) {
            entry.status = status;
        }
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
        
        // Manual reload to bypass default path logic
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
        assert_eq!(registry.downloads.get(&id).unwrap().status, DownloadStatus::Completed);
        
        registry.update_status(&id, DownloadStatus::Error("Fail".to_string()));
        match &registry.downloads.get(&id).unwrap().status {
            DownloadStatus::Error(msg) => assert_eq!(msg, "Fail"),
            _ => panic!("Expected Error status"),
        }
    }
}
