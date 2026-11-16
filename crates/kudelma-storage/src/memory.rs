use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::id::{new_id, now_rfc3339};
use crate::paths::{canonicalize_lenient, state_dir};
use crate::{StorageError, atomic_write};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemoryFile {
    entries: Vec<MemoryEntry>,
}

/// Long-term notes kept per project and globally.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    project_path: PathBuf,
    global_path: PathBuf,
}

impl MemoryStore {
    pub fn new(cwd: &Path) -> Result<Self, StorageError> {
        let root = state_dir()?;
        let key = project_key(cwd);
        Ok(Self {
            project_path: root.join("memory").join(format!("{key}.json")),
            global_path: root.join("memory_global.json"),
        })
    }

    pub fn add(&self, scope: &str, content: &str) -> Result<MemoryEntry, StorageError> {
        let path = self.path(scope);
        let mut file = self.read(&path);
        let entry = MemoryEntry {
            id: new_id(),
            content: content.trim().to_owned(),
            created_at: now_rfc3339(),
        };
        file.entries.push(entry.clone());
        self.write(&path, &file)?;
        Ok(entry)
    }

    pub fn list(&self, scope: &str) -> Vec<MemoryEntry> {
        self.read(&self.path(scope)).entries
    }

    pub fn remove(&self, scope: &str, id: &str) -> Result<bool, StorageError> {
        let path = self.path(scope);
        let mut file = self.read(&path);
        let before = file.entries.len();
        file.entries.retain(|entry| entry.id != id);
        let removed = file.entries.len() != before;
        self.write(&path, &file)?;
        Ok(removed)
    }

    fn path(&self, scope: &str) -> PathBuf {
        if scope == "global" {
            self.global_path.clone()
        } else {
            self.project_path.clone()
        }
    }

    fn read(&self, path: &Path) -> MemoryFile {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &Path, file: &MemoryFile) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec_pretty(file)?;
        atomic_write(path, &bytes)
    }
}

fn project_key(cwd: &Path) -> String {
    let canonical = canonicalize_lenient(cwd).display().to_string();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in canonical.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
