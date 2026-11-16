use std::fs;
use std::io::Write;
use std::path::PathBuf;

use thiserror::Error;

use crate::paths::sessions_dir;

#[derive(Debug, Error)]
pub enum ClaimError {
    #[error("session {0} is already open in another process")]
    AlreadyOpen(String),
    #[error("invalid session id: {0}")]
    InvalidId(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A process-scoped claim that stops two agents from writing one session.
#[derive(Debug)]
pub struct SessionClaim {
    path: PathBuf,
    id: String,
}

impl SessionClaim {
    pub fn acquire(id: &str) -> Result<Self, ClaimError> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(ClaimError::InvalidId(id.into()));
        }
        let dir = sessions_dir().map_err(|e| ClaimError::Io(std::io::Error::other(e)))?;
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.lock"));
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(pid) = text.trim().parse::<u32>()
            && process_alive(pid)
            && pid != std::process::id()
        {
            return Err(ClaimError::AlreadyOpen(id.into()));
        }
        let _ = fs::remove_file(&path);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self {
            path,
            id: id.into(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn release(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for SessionClaim {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 performs an existence check and delivers nothing.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_ids_before_touching_disk() {
        let error = SessionClaim::acquire("../../escape").unwrap_err();
        assert!(matches!(error, ClaimError::InvalidId(_)));
    }
}
