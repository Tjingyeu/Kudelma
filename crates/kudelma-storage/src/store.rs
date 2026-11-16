use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{StorageError, atomic_write};

/// JSON artifact store for contracts, workflow records, worker tasks and
/// execution evidence. IDs are validated so a model or CLI input can never
/// escape the configured directory.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    dir: PathBuf,
}

impl ArtifactStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, id: &str) -> Result<PathBuf, StorageError> {
        validate_id(id)?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn save<T: Serialize>(&self, id: &str, value: &T) -> Result<(), StorageError> {
        let path = self.path(id)?;
        let bytes = serde_json::to_vec_pretty(value)?;
        atomic_write(&path, &bytes)
    }

    pub fn load<T: DeserializeOwned>(&self, id: &str) -> Result<T, StorageError> {
        let path = self.path(id)?;
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn delete(&self, id: &str) -> Result<(), StorageError> {
        let path = self.path(id)?;
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn list<T: DeserializeOwned>(&self) -> Result<Vec<T>, StorageError> {
        fs::create_dir_all(&self.dir)?;
        let mut items = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if let Ok(value) = serde_json::from_slice(&bytes) {
                items.push(value);
            }
        }
        Ok(items)
    }

    pub fn list_ids(&self) -> Result<Vec<String>, StorageError> {
        fs::create_dir_all(&self.dir)?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                ids.push(stem.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }
}

fn validate_id(id: &str) -> Result<(), StorageError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(StorageError::InvalidId(id.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct Item {
        name: String,
    }

    #[test]
    fn round_trips_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path().to_path_buf());
        store.save("one", &Item { name: "a".into() }).unwrap();
        let loaded: Item = store.load("one").unwrap();
        assert_eq!(loaded.name, "a");
        let all: Vec<Item> = store.list().unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path().to_path_buf());
        assert!(store.path("../escape").is_err());
    }
}
