use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use crate::paths::state_dir;
use crate::{StorageError, atomic_write_private};

/// Provider credentials live in a 0600 JSON file. Environment variables take
/// precedence at resolution time, so this file is a convenience, not a vault.
#[derive(Debug, Clone)]
pub struct AuthStore {
    path: PathBuf,
    keys: BTreeMap<String, String>,
}

impl AuthStore {
    pub fn load() -> Result<Self, StorageError> {
        let path = state_dir()?.join("auth.json");
        let keys = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self { path, keys })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn get(&self, provider: &str) -> Option<&str> {
        self.keys.get(provider).map(String::as_str)
    }

    pub fn set(&mut self, provider: &str, key: &str) -> Result<(), StorageError> {
        self.keys.insert(provider.into(), key.into());
        self.flush()
    }

    pub fn remove(&mut self, provider: &str) -> Result<bool, StorageError> {
        let removed = self.keys.remove(provider).is_some();
        self.flush()?;
        Ok(removed)
    }

    pub fn providers(&self) -> Vec<&str> {
        self.keys.keys().map(String::as_str).collect()
    }

    fn flush(&self) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(&self.keys)?;
        atomic_write_private(&self.path, &bytes)
    }
}
