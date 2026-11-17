//! Crash-safe persistence for sessions, credentials, trust and memory.

pub mod auth;
pub mod history;
pub mod id;
pub mod lock;
pub mod memory;
pub mod paths;
pub mod sessions;
pub mod store;
pub mod trust;

pub use auth::AuthStore;
pub use history::InputHistory;
pub use id::{KudelmaId, new_id, now_rfc3339};
pub use lock::{ClaimError, SessionClaim};
pub use memory::{MemoryEntry, MemoryStore};
pub use paths::{
    canonicalize_lenient, clean_path, config_dir, contracts_dir, evidence_dir, exploration_dir,
    incremental_canonicalize, is_within, plans_dir, reviews_dir, sessions_dir, state_dir,
    tasks_dir, user_skills_dir, workflows_dir,
};
pub use sessions::{SessionError, SessionMeta, SessionStore, StoredSession};
pub use store::ArtifactStore;
pub use trust::TrustStore;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("home directory not found")]
    HomeNotSet,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("invalid session id: {0}")]
    InvalidId(String),
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    atomic_write_mode(path, data, None)
}

pub fn atomic_write_private(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    atomic_write_mode(path, data, Some(0o600))
}

fn atomic_write_mode(path: &Path, data: &[u8], mode: Option<u32>) -> Result<(), StorageError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let existing = fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777);
        let target = mode.or(existing).unwrap_or(0o644);
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(target))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    tmp.as_file().sync_all()?;
    let (_, tmp_path) = tmp.into_parts();
    fs::rename(&tmp_path, path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        StorageError::Io(e)
    })?;
    sync_parent(path);
    Ok(())
}

pub fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Ok(file) = fs::File::open(dir)
    {
        let _ = file.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, StorageError> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_write(path, &bytes)
}

pub fn ensure_data_dir() -> Result<PathBuf, StorageError> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
