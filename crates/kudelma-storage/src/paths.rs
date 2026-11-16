use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::StorageError;

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KUDELMA_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kudelma")
}

pub fn state_dir() -> Result<PathBuf, StorageError> {
    if let Ok(dir) = std::env::var("KUDELMA_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .ok_or(StorageError::HomeNotSet)?;
    Ok(base.join("kudelma"))
}

pub fn sessions_dir() -> Result<PathBuf, StorageError> {
    Ok(state_dir()?.join("sessions"))
}

pub fn session_data_dir(id: &str) -> Result<PathBuf, StorageError> {
    Ok(sessions_dir()?.join("data").join(id))
}

pub fn plans_dir() -> Result<PathBuf, StorageError> {
    Ok(state_dir()?.join("plans"))
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Collapse `.` and `..` without touching the filesystem.
pub fn clean_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolve every symlink from left to right, then apply `..`, so a symlink
/// followed by `..` cannot escape the directory it appeared to stay inside.
pub fn incremental_canonicalize(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    canonicalize_inner(&absolute, 0)
}

fn canonicalize_inner(path: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 40 {
        return None;
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => result.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            Component::Normal(name) => {
                result.push(name);
                if let Ok(meta) = std::fs::symlink_metadata(&result)
                    && meta.file_type().is_symlink()
                {
                    let target = std::fs::read_link(&result).ok()?;
                    let target = if target.is_absolute() {
                        target
                    } else {
                        result.parent()?.join(target)
                    };
                    result = canonicalize_inner(&target, depth + 1)?;
                }
            }
        }
    }
    Some(result)
}

/// Best-effort canonical form: follows symlinks when possible, otherwise
/// applies lexical cleanup. Useful for paths that do not exist yet.
pub fn canonicalize_lenient(path: &Path) -> PathBuf {
    incremental_canonicalize(path).unwrap_or_else(|| clean_path(path))
}

pub fn is_within(parent: &Path, child: &Path) -> bool {
    let parent = canonicalize_lenient(parent);
    let child = canonicalize_lenient(child);
    child.starts_with(&parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_parent_components() {
        assert_eq!(clean_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(clean_path(Path::new("a/./b")), PathBuf::from("a/b"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_cannot_escape() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("root")).unwrap();
        symlink(outside.path(), dir.path().join("root/link")).unwrap();
        let target = dir.path().join("root/link/../secret.txt");
        let resolved = incremental_canonicalize(&target).unwrap();
        assert!(!resolved.starts_with(dir.path().join("root")));
    }
}
