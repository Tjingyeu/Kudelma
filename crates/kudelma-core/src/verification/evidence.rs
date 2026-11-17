use serde::{Deserialize, Serialize};

use super::revision::WorkspaceRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Test,
    Build,
    Lint,
    Acceptance,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceResult {
    Passed,
    Failed,
    Skipped,
    Blocked,
}

impl EvidenceResult {
    pub fn passed(self) -> bool {
        matches!(self, Self::Passed | Self::Skipped)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Blocked => "blocked",
        }
    }
}

/// Machine-recorded proof of a command. A model statement is never a
/// substitute for this record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub command: String,
    pub cwd: String,
    pub started_at: String,
    pub ended_at: String,
    pub exit_code: Option<i32>,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub log_path: Option<String>,
    pub git_head: Option<String>,
    pub git_diff_hash: Option<String>,
    #[serde(default)]
    pub revision_before: Option<WorkspaceRevision>,
    #[serde(default)]
    pub revision_after: Option<WorkspaceRevision>,
    pub changed_files: Vec<String>,
    pub result: EvidenceResult,
    pub skip_reason: Option<String>,
}

impl ExecutionEvidence {
    pub fn summary(&self) -> String {
        format!(
            "{} [{}] `{}` -> {} (exit {}, rev {})",
            self.id,
            self.result.name(),
            self.command,
            self.result.name(),
            self.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into()),
            self.git_head.as_deref().unwrap_or("no-git")
        )
    }
}
