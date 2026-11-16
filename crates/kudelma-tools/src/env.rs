use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use kudelma_storage::{atomic_write, clean_path};

#[derive(Debug, Clone)]
pub struct Meta {
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// All model-visible filesystem access goes through this boundary, which keeps
/// a future remote (SSH/container) workspace a drop-in replacement.
#[async_trait]
pub trait WorkspaceEnv: Send + Sync {
    fn cwd(&self) -> &Path;
    fn resolve(&self, raw: &str) -> PathBuf;
    async fn read(&self, path: &Path) -> std::io::Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> std::io::Result<()>;
    async fn metadata(&self, path: &Path) -> std::io::Result<Meta>;
    async fn list(&self, path: &Path) -> std::io::Result<Vec<DirEntryInfo>>;
    fn shell(&self, script: &str, cwd: &Path) -> std::io::Result<tokio::process::Command>;
}

#[derive(Debug, Clone)]
pub struct LocalEnv {
    cwd: PathBuf,
}

impl LocalEnv {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }
}

#[async_trait]
impl WorkspaceEnv for LocalEnv {
    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn resolve(&self, raw: &str) -> PathBuf {
        let expanded = if raw == "~" {
            dirs::home_dir().unwrap_or_else(|| self.cwd.clone())
        } else if let Some(rest) = raw.strip_prefix("~/") {
            dirs::home_dir()
                .unwrap_or_else(|| self.cwd.clone())
                .join(rest)
        } else {
            PathBuf::from(raw)
        };
        let path = if expanded.is_absolute() {
            expanded
        } else {
            self.cwd.join(expanded)
        };
        clean_path(&path)
    }

    async fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(path).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> std::io::Result<()> {
        let path = path.to_path_buf();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || {
            atomic_write(&path, &data).map_err(std::io::Error::other)
        })
        .await
        .map_err(std::io::Error::other)?
    }

    async fn metadata(&self, path: &Path) -> std::io::Result<Meta> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(Meta {
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    async fn list(&self, path: &Path) -> std::io::Result<Vec<DirEntryInfo>> {
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let meta = entry.metadata().await.ok();
            entries.push(DirEntryInfo {
                name: entry.file_name().to_string_lossy().to_string(),
                is_dir: meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: meta.map(|m| m.len()).unwrap_or(0),
            });
        }
        Ok(entries)
    }

    fn shell(&self, script: &str, cwd: &Path) -> std::io::Result<tokio::process::Command> {
        #[cfg(unix)]
        let mut command = {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new("bash");
            command.arg("-lc").arg(script).current_dir(cwd);
            command.process_group(0);
            command
        };
        #[cfg(not(unix))]
        let mut command = {
            let mut command = std::process::Command::new("cmd");
            command.arg("/C").arg(script).current_dir(cwd);
            command
        };
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        Ok(tokio::process::Command::from(command))
    }
}
