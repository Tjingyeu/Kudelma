use std::path::{Path, PathBuf};

use kudelma_providers::CancelToken;
use kudelma_storage::{ArtifactStore, atomic_write, now_rfc3339};
use serde_json::json;

use super::evidence::{EvidenceKind, EvidenceResult, ExecutionEvidence};
use super::revision::WorkspaceRevision;

const MAX_EXCERPT: usize = 8_000;
const VERIFY_TIMEOUT_SECS: u64 = 900;

#[derive(Clone)]
pub struct Verifier {
    evidence: ArtifactStore,
    logs: PathBuf,
}

impl Verifier {
    pub fn new() -> Result<Self, kudelma_storage::StorageError> {
        Ok(Self {
            evidence: ArtifactStore::new(kudelma_storage::evidence_dir()?),
            logs: kudelma_storage::evidence_dir()?,
        })
    }

    pub fn store(&self) -> &ArtifactStore {
        &self.evidence
    }

    pub fn load(&self, id: &str) -> Result<ExecutionEvidence, kudelma_storage::StorageError> {
        self.evidence.load(id)
    }

    pub fn list(&self) -> Result<Vec<ExecutionEvidence>, kudelma_storage::StorageError> {
        let mut items: Vec<ExecutionEvidence> = self.evidence.list()?;
        items.sort_by(|a, b| b.ended_at.cmp(&a.ended_at));
        Ok(items)
    }

    pub async fn run(
        &self,
        command: &str,
        cwd: &Path,
        kind: EvidenceKind,
        mutation_generation: u64,
        cancel: &CancelToken,
    ) -> Result<ExecutionEvidence, String> {
        let id = kudelma_storage::new_id();
        let started_at = now_rfc3339();
        let revision_before = WorkspaceRevision::capture(cwd, mutation_generation).await;
        let mut process = tokio::process::Command::new("bash");
        process
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::select! {
            result = tokio::time::timeout(
                std::time::Duration::from_secs(VERIFY_TIMEOUT_SECS),
                process.output(),
            ) => result,
            _ = cancel.cancelled() => {
                return Err("verification cancelled".into());
            }
        };
        let ended_at = now_rfc3339();
        let revision_after = WorkspaceRevision::capture(cwd, mutation_generation).await;
        let (exit_code, stdout, stderr, result) = match output {
            Ok(Ok(output)) => {
                let result = if output.status.success() {
                    EvidenceResult::Passed
                } else {
                    EvidenceResult::Failed
                };
                (
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout).to_string(),
                    String::from_utf8_lossy(&output.stderr).to_string(),
                    result,
                )
            }
            Ok(Err(error)) => (
                None,
                String::new(),
                error.to_string(),
                EvidenceResult::Blocked,
            ),
            Err(_) => (
                None,
                String::new(),
                "verification timed out".into(),
                EvidenceResult::Failed,
            ),
        };

        let log_path = self.logs.join(format!("{id}.log"));
        let full = format!("$ {command}\n\n[stdout]\n{stdout}\n\n[stderr]\n{stderr}\n");
        let _ = atomic_write(&log_path, full.as_bytes());
        let evidence = ExecutionEvidence {
            id,
            kind,
            command: command.into(),
            cwd: revision_after.workspace.clone(),
            started_at,
            ended_at,
            exit_code,
            stdout_excerpt: excerpt(&stdout),
            stderr_excerpt: excerpt(&stderr),
            log_path: Some(log_path.display().to_string()),
            git_head: revision_after.git_head.clone(),
            git_diff_hash: revision_after.diff_hash.clone(),
            revision_before: Some(revision_before),
            revision_after: Some(revision_after),
            changed_files: changed_files(cwd).await,
            result,
            skip_reason: None,
        };
        self.evidence
            .save(&evidence.id, &evidence)
            .map_err(|error| error.to_string())?;
        Ok(evidence)
    }

    pub fn record_skip(
        &self,
        command: &str,
        reason: &str,
        kind: EvidenceKind,
    ) -> Result<ExecutionEvidence, String> {
        let now = now_rfc3339();
        let evidence = ExecutionEvidence {
            id: kudelma_storage::new_id(),
            kind,
            command: command.into(),
            cwd: String::new(),
            started_at: now.clone(),
            ended_at: now,
            exit_code: None,
            stdout_excerpt: String::new(),
            stderr_excerpt: String::new(),
            log_path: None,
            git_head: None,
            git_diff_hash: None,
            revision_before: None,
            revision_after: None,
            changed_files: Vec::new(),
            result: EvidenceResult::Skipped,
            skip_reason: Some(reason.into()),
        };
        self.evidence
            .save(&evidence.id, &evidence)
            .map_err(|error| error.to_string())?;
        Ok(evidence)
    }

    pub fn gate_view(evidence: &ExecutionEvidence) -> serde_json::Value {
        json!({
            "id": evidence.id,
            "kind": evidence.kind,
            "result": evidence.result,
            "command": evidence.command,
            "exit_code": evidence.exit_code,
            "git_head": evidence.git_head,
            "git_diff_hash": evidence.git_diff_hash,
        })
    }
}

async fn changed_files(cwd: &Path) -> Vec<String> {
    tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|line| line[3.min(line.len())..].trim().to_string())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn excerpt(text: &str) -> String {
    if text.chars().count() <= MAX_EXCERPT {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_EXCERPT / 2).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(MAX_EXCERPT / 2)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}\n[... output truncated ...]\n{tail}")
}
