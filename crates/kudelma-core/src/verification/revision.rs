use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A deterministic snapshot of the workspace state used to bind verification
/// evidence to the exact revision it observed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceRevision {
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub git_head: Option<String>,
    #[serde(default)]
    pub diff_hash: Option<String>,
    #[serde(default)]
    pub status_hash: Option<String>,
    #[serde(default)]
    pub untracked_hash: Option<String>,
    #[serde(default)]
    pub mutation_generation: u64,
    #[serde(default)]
    pub fingerprint: String,
}

impl WorkspaceRevision {
    pub async fn capture(cwd: &Path, mutation_generation: u64) -> Self {
        let path = cwd.to_path_buf();
        let workspace = cwd.display().to_string();
        tokio::task::spawn_blocking(move || capture_blocking(&path, mutation_generation))
            .await
            .unwrap_or_else(|_| WorkspaceRevision {
                workspace,
                mutation_generation,
                fingerprint: format!("unavailable:{mutation_generation}"),
                ..Default::default()
            })
    }

    pub fn matches(&self, other: &Self) -> bool {
        !self.fingerprint.is_empty() && self.fingerprint == other.fingerprint
    }
}

fn capture_blocking(cwd: &Path, generation: u64) -> WorkspaceRevision {
    let workspace = kudelma_storage::canonicalize_lenient(cwd)
        .display()
        .to_string();
    if let Some(revision) = git_revision(cwd, &workspace, generation) {
        return revision;
    }
    let fingerprint = file_fingerprint(cwd);
    let mut revision = WorkspaceRevision {
        workspace,
        mutation_generation: generation,
        fingerprint: String::new(),
        ..Default::default()
    };
    revision.fingerprint = combine(&[
        &revision.workspace,
        &generation.to_string(),
        &fingerprint,
        "non-git",
    ]);
    revision
}

fn git_revision(cwd: &Path, workspace: &str, generation: u64) -> Option<WorkspaceRevision> {
    let inside = run_git(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return None;
    }
    let head = run_git(cwd, &["rev-parse", "HEAD"]).map(|value| value.trim().to_string());
    let diff = run_git(cwd, &["diff", "HEAD", "--binary"])
        .or_else(|| run_git(cwd, &["diff", "--binary"]))
        .map(|value| hash(value.as_bytes()))
        .unwrap_or_else(|| "no-diff".into());
    let status = run_git(cwd, &["status", "--porcelain"])
        .map(|value| hash(value.as_bytes()))
        .unwrap_or_else(|| "no-status".into());
    let untracked = untracked_hash(cwd);
    let mut revision = WorkspaceRevision {
        workspace: workspace.into(),
        git_head: head,
        diff_hash: Some(diff.clone()),
        status_hash: Some(status.clone()),
        untracked_hash: Some(untracked.clone()),
        mutation_generation: generation,
        fingerprint: String::new(),
    };
    revision.fingerprint = combine(&[
        workspace,
        &generation.to_string(),
        revision.git_head.as_deref().unwrap_or("no-head"),
        &diff,
        &status,
        &untracked,
        "git",
    ]);
    Some(revision)
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

fn untracked_hash(cwd: &Path) -> String {
    let Some(files) = run_git(cwd, &["ls-files", "--others", "--exclude-standard", "-z"]) else {
        return "no-untracked".into();
    };
    let mut hash = Fnv::new();
    for relative in files.split('\0').filter(|value| !value.is_empty()) {
        hash.bytes(relative.as_bytes());
        let path = cwd.join(relative);
        hash_file(&path, &mut hash, true);
    }
    hash.finish()
}

fn file_fingerprint(cwd: &Path) -> String {
    let mut hash = Fnv::new();
    let walker = ignore::WalkBuilder::new(cwd)
        .standard_filters(true)
        .hidden(false)
        .follow_links(false)
        .build();
    let mut files = 0usize;
    for entry in walker.flatten() {
        if files >= 20_000 {
            hash.bytes(b"[limit]");
            break;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        files += 1;
        let relative = path.strip_prefix(cwd).unwrap_or(path);
        hash.bytes(relative.display().to_string().as_bytes());
        hash_file(path, &mut hash, false);
    }
    hash.finish()
}

fn hash_file(path: &Path, hash: &mut Fnv, bounded: bool) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    hash.bytes(&metadata.len().to_le_bytes());
    if bounded && metadata.len() > 2 * 1024 * 1024 {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let mut head = vec![0u8; 1024 * 1024];
        if let Ok(read) = file.read(&mut head) {
            hash.bytes(&head[..read]);
        }
        if file.seek(SeekFrom::End(-(1024 * 1024) as i64)).is_ok() {
            let mut tail = vec![0u8; 1024 * 1024];
            if let Ok(read) = file.read(&mut tail) {
                hash.bytes(&tail[..read]);
            }
        }
        return;
    }
    if let Ok(bytes) = std::fs::read(path) {
        hash.bytes(&bytes);
    }
}

fn combine(parts: &[&str]) -> String {
    let mut hash = Fnv::new();
    for part in parts {
        hash.bytes(part.as_bytes());
        hash.bytes(b"\x1f");
    }
    hash.finish()
}

fn hash(bytes: &[u8]) -> String {
    let mut hash = Fnv::new();
    hash.bytes(bytes);
    hash.finish()
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn tracked_and_untracked_content_changes_change_the_revision() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        git(dir.path(), &["config", "user.name", "test"]);
        std::fs::write(dir.path().join("tracked.txt"), "one").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "init"]);

        let initial = WorkspaceRevision::capture(dir.path(), 0).await;
        std::fs::write(dir.path().join("tracked.txt"), "two").unwrap();
        let tracked = WorkspaceRevision::capture(dir.path(), 0).await;
        assert!(!initial.matches(&tracked));

        std::fs::write(dir.path().join("untracked.txt"), "a").unwrap();
        let untracked_a = WorkspaceRevision::capture(dir.path(), 0).await;
        std::fs::write(dir.path().join("untracked.txt"), "b").unwrap();
        let untracked_b = WorkspaceRevision::capture(dir.path(), 0).await;
        assert!(!untracked_a.matches(&untracked_b));
    }

    #[tokio::test]
    async fn mutation_generation_changes_the_revision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "one").unwrap();
        let first = WorkspaceRevision::capture(dir.path(), 0).await;
        let second = WorkspaceRevision::capture(dir.path(), 1).await;
        assert!(!first.matches(&second));
    }
}
