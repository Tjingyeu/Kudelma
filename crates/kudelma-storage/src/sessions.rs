use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::id::{new_id, now_rfc3339};
use crate::{StorageError, atomic_write, paths::sessions_dir};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub model: String,
    #[serde(default)]
    pub agency_model: String,
    #[serde(default)]
    pub lead_model: String,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub mode: String,
    pub active_role: String,
    pub lead_mode: String,
    pub contract_id: Option<String>,
    pub workflow_id: Option<String>,
}

impl Default for SessionMeta {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: "New session".into(),
            model: String::new(),
            agency_model: String::new(),
            lead_model: String::new(),
            cwd: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            mode: "build".into(),
            active_role: "agency".into(),
            lead_mode: "build".into(),
            contract_id: None,
            workflow_id: None,
        }
    }
}

impl SessionMeta {
    pub fn new(model: &str, cwd: &Path) -> Self {
        let now = now_rfc3339();
        Self {
            id: new_id(),
            title: "New session".into(),
            model: model.into(),
            agency_model: model.into(),
            lead_model: model.into(),
            cwd: cwd.display().to_string(),
            created_at: now.clone(),
            updated_at: now,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            mode: "build".into(),
            active_role: "agency".into(),
            lead_mode: "build".into(),
            contract_id: None,
            workflow_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSession {
    pub meta: SessionMeta,
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub agency_messages: Vec<Value>,
    #[serde(default)]
    pub plan_path: Option<String>,
}

impl StoredSession {
    pub fn new(meta: SessionMeta) -> Self {
        Self {
            meta,
            messages: Vec::new(),
            agency_messages: Vec::new(),
            plan_path: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid session id: {0}")]
    InvalidId(String),
}

/// JSON-file session store. Each session is one atomically replaced document;
/// the claim lock decides who may write it.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new() -> Result<Self, SessionError> {
        Ok(Self {
            dir: sessions_dir()?,
        })
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> Result<PathBuf, SessionError> {
        validate_id(id)?;
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn save(&self, session: &StoredSession) -> Result<(), SessionError> {
        let path = self.path(&session.meta.id)?;
        let bytes = serde_json::to_vec_pretty(session)?;
        atomic_write(&path, &bytes)?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Result<StoredSession, SessionError> {
        let path = self.path(id)?;
        let bytes = fs::read(&path)
            .map_err(|_| SessionError::Storage(StorageError::NotFound(id.into())))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn delete(&self, id: &str) -> Result<(), SessionError> {
        let path = self.path(id)?;
        fs::remove_file(path)
            .map_err(|_| SessionError::Storage(StorageError::NotFound(id.into())))?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<SessionMeta>, SessionError> {
        fs::create_dir_all(&self.dir)?;
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(stored) = serde_json::from_slice::<StoredSession>(&bytes) else {
                continue;
            };
            sessions.push(stored.meta);
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    pub fn latest(&self, cwd: &Path) -> Result<Option<StoredSession>, SessionError> {
        let cwd = cwd.display().to_string();
        let mut sessions = self.list()?;
        sessions.retain(|meta| meta.cwd == cwd);
        let Some(meta) = sessions.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(self.load(&meta.id)?))
    }
}

fn validate_id(id: &str) -> Result<(), SessionError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(SessionError::InvalidId(id.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_lists_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        let meta = SessionMeta::new("anthropic/claude-sonnet-4-5", dir.path());
        let session = StoredSession::new(meta.clone());
        store.save(&session).unwrap();
        let loaded = store.load(&meta.id).unwrap();
        assert_eq!(loaded.meta.id, meta.id);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::with_dir(dir.path().to_path_buf());
        assert!(store.load("../escape").is_err());
    }

    #[test]
    fn new_sessions_track_role_models() {
        let dir = tempfile::tempdir().unwrap();
        let meta = SessionMeta::new("opencode-go/kimi-k2.7-code", dir.path());
        assert_eq!(meta.model, "opencode-go/kimi-k2.7-code");
        assert_eq!(meta.agency_model, meta.model);
        assert_eq!(meta.lead_model, meta.model);
    }

    #[test]
    fn legacy_session_metadata_loads_without_role_models() {
        let meta: SessionMeta =
            serde_json::from_str(r#"{"id":"legacy","model":"anthropic/claude-sonnet-4-5"}"#)
                .unwrap();
        assert_eq!(meta.model, "anthropic/claude-sonnet-4-5");
        assert!(meta.agency_model.is_empty());
        assert!(meta.lead_model.is_empty());
    }
}
