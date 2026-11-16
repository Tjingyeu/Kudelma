use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::{canonicalize_lenient, state_dir};
use crate::{StorageError, atomic_write};

/// Folders the user explicitly trusts to load project-level config, hooks and
/// MCP servers.
#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
    folders: Vec<String>,
}

impl TrustStore {
    pub fn load() -> Result<Self, StorageError> {
        let path = state_dir()?.join("trusted_folders.json");
        let folders = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self { path, folders })
    }

    pub fn is_trusted(&self, cwd: &Path) -> bool {
        let cwd = canonicalize_lenient(cwd);
        self.folders
            .iter()
            .any(|folder| canonicalize_lenient(Path::new(folder)) == cwd)
    }

    pub fn list(&self) -> &[String] {
        &self.folders
    }

    pub fn add(&mut self, cwd: &Path) -> Result<(), StorageError> {
        let folder = canonicalize_lenient(cwd).display().to_string();
        if !self.folders.contains(&folder) {
            self.folders.push(folder);
            self.flush()?;
        }
        Ok(())
    }

    pub fn remove(&mut self, cwd: &Path) -> Result<bool, StorageError> {
        let folder = canonicalize_lenient(cwd).display().to_string();
        let before = self.folders.len();
        self.folders.retain(|entry| entry != &folder);
        let removed = self.folders.len() != before;
        self.flush()?;
        Ok(removed)
    }

    fn flush(&self) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(&self.folders)?;
        atomic_write(&self.path, &bytes)
    }
}
