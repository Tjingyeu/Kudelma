use serde::{Deserialize, Serialize};

use crate::verification::WorkspaceRevision;

pub use super::EvidenceCite;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Completed,
    Partial,
    Blocked,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerTask {
    pub task_id: String,
    pub parent_contract_id: String,
    pub objective: String,
    pub requirements: Vec<String>,
    pub constraints: Vec<String>,
    pub context_refs: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub dependencies: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub required_skills: Vec<String>,
    pub max_retries: u32,
}

impl WorkerTask {
    pub fn new(contract_id: impl Into<String>, objective: impl Into<String>) -> Self {
        Self {
            task_id: kudelma_storage::new_id(),
            parent_contract_id: contract_id.into(),
            objective: objective.into(),
            requirements: Vec::new(),
            constraints: Vec::new(),
            context_refs: Vec::new(),
            allowed_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            allowed_tools: Vec::new(),
            dependencies: Vec::new(),
            acceptance_criteria: Vec::new(),
            required_skills: Vec::new(),
            max_retries: 1,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestRun {
    pub command: String,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub evidence_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerReport {
    pub task_id: String,
    pub status: WorkerStatus,
    pub summary: String,
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub workspace_before: Option<WorkspaceRevision>,
    #[serde(default)]
    pub workspace_after: Option<WorkspaceRevision>,
    pub tests: Vec<TestRun>,
    pub assumptions: Vec<String>,
    pub unresolved_issues: Vec<String>,
    pub evidence_refs: Vec<String>,
}

impl WorkerReport {
    pub fn render(&self) -> String {
        format!(
            "Worker report {} ({:?})\nSummary: {}\nModified: {}\nUnresolved: {}",
            self.task_id,
            self.status,
            self.summary,
            self.modified_files.join(", "),
            self.unresolved_issues.join("; ")
        )
    }
}
