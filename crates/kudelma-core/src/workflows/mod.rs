use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use kudelma_config::{Config, WorkflowPreset};
use kudelma_storage::{ArtifactStore, StorageError, now_rfc3339};

use crate::orchestration::{Complexity, ContractStatus, TaskContract};
use crate::verification::{GateKind, GateStatus, GateView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowKind {
    Feature,
    BugFix,
    Refactor,
    SimpleTask,
    CodeQuestion,
}

impl WorkflowKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Feature => "feature-development",
            Self::BugFix => "bug-fix",
            Self::Refactor => "refactoring",
            Self::SimpleTask => "simple-task",
            Self::CodeQuestion => "codebase-question",
        }
    }

    pub fn from_preset(preset: WorkflowPreset) -> Option<Self> {
        Some(match preset {
            WorkflowPreset::Adaptive => return None,
            WorkflowPreset::Feature => Self::Feature,
            WorkflowPreset::BugFix => Self::BugFix,
            WorkflowPreset::Refactor => Self::Refactor,
            WorkflowPreset::Simple => Self::SimpleTask,
        })
    }

    pub fn from_contract(contract: &TaskContract) -> Self {
        match contract.intent_kind {
            crate::orchestration::IntentKind::BugReport => Self::BugFix,
            crate::orchestration::IntentKind::CodeQuestion
            | crate::orchestration::IntentKind::ArchitectureQuestion => Self::CodeQuestion,
            crate::orchestration::IntentKind::FeatureRequest
            | crate::orchestration::IntentKind::PlanningRequest => match contract.complexity {
                Complexity::L0 | Complexity::L1 => Self::SimpleTask,
                _ => Self::Feature,
            },
            crate::orchestration::IntentKind::ChangeRequest => {
                if contract.complexity == Complexity::L3 {
                    Self::Refactor
                } else if contract.complexity == Complexity::Bug {
                    Self::BugFix
                } else {
                    Self::SimpleTask
                }
            }
            _ => Self::SimpleTask,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStage {
    Intent,
    Plan,
    Build,
    Debug,
    Review,
    Done,
    Blocked,
}

impl WorkflowStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Plan => "plan",
            Self::Build => "build",
            Self::Debug => "debug",
            Self::Review => "review",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeTransition {
    pub from: String,
    pub to: String,
    pub reason: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub session_id: String,
    pub kind: WorkflowKind,
    pub stage: WorkflowStage,
    pub mode: String,
    pub complexity: Complexity,
    pub contract_id: Option<String>,
    #[serde(default)]
    pub contract_version: u32,
    #[serde(default)]
    pub plan_path: Option<String>,
    #[serde(default)]
    pub plan_hash: Option<String>,
    #[serde(default)]
    pub design_hash: Option<String>,
    #[serde(default)]
    pub review_report_id: Option<String>,
    #[serde(default)]
    pub review_fingerprint: Option<String>,
    #[serde(default)]
    pub review_contract_version: Option<u32>,
    pub gates: BTreeMap<String, GateView>,
    pub transitions: Vec<ModeTransition>,
    pub skills_loaded: Vec<String>,
    pub evidence_ids: Vec<String>,
    pub review_completed: bool,
    pub accepted: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkflowState {
    pub fn new(session_id: &str, kind: WorkflowKind, complexity: Complexity) -> Self {
        let now = now_rfc3339();
        let mut gates = BTreeMap::new();
        for gate in GateKind::ALL {
            gates.insert(
                gate.name().to_string(),
                GateView {
                    kind: *gate,
                    status: GateStatus::Pending,
                    reason: None,
                    evidence_id: None,
                },
            );
        }
        let stage = match kind {
            WorkflowKind::Feature | WorkflowKind::Refactor => WorkflowStage::Plan,
            WorkflowKind::BugFix => WorkflowStage::Debug,
            WorkflowKind::SimpleTask => WorkflowStage::Build,
            WorkflowKind::CodeQuestion => WorkflowStage::Intent,
        };
        Self {
            id: kudelma_storage::new_id(),
            session_id: session_id.into(),
            kind,
            stage,
            mode: stage.name().into(),
            complexity,
            contract_id: None,
            contract_version: 0,
            plan_path: None,
            plan_hash: None,
            design_hash: None,
            review_report_id: None,
            review_fingerprint: None,
            review_contract_version: None,
            gates,
            transitions: Vec::new(),
            skills_loaded: Vec::new(),
            evidence_ids: Vec::new(),
            review_completed: false,
            accepted: false,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn gate(&self, kind: GateKind) -> GateStatus {
        self.gates
            .get(kind.name())
            .map(|gate| gate.status)
            .unwrap_or(GateStatus::Pending)
    }

    pub fn set_gate(
        &mut self,
        kind: GateKind,
        status: GateStatus,
        reason: Option<String>,
        evidence_id: Option<String>,
    ) {
        self.gates.insert(
            kind.name().to_string(),
            GateView {
                kind,
                status,
                reason,
                evidence_id,
            },
        );
        self.updated_at = now_rfc3339();
    }

    pub fn attach_contract(&mut self, contract: &TaskContract) {
        let same_version = self.contract_id.as_deref() == Some(contract.contract_id.as_str())
            && self.contract_version == contract.contract_version;
        if !same_version {
            self.accepted = false;
            self.evidence_ids.clear();
            self.review_completed = false;
            self.plan_path = None;
            self.plan_hash = None;
            self.design_hash = None;
            self.review_report_id = None;
            self.review_fingerprint = None;
            self.review_contract_version = None;
            self.stage = match WorkflowKind::from_contract(contract) {
                WorkflowKind::Feature | WorkflowKind::Refactor => WorkflowStage::Plan,
                WorkflowKind::BugFix => WorkflowStage::Debug,
                WorkflowKind::SimpleTask => WorkflowStage::Build,
                WorkflowKind::CodeQuestion => WorkflowStage::Intent,
            };
            self.set_gate(GateKind::Finish, GateStatus::Pending, None, None);
        }
        self.contract_id = Some(contract.contract_id.clone());
        self.contract_version = contract.contract_version;
        self.complexity = contract.complexity;
        self.kind = WorkflowKind::from_contract(contract);
        let status = if contract.status == ContractStatus::Approved {
            GateStatus::Passed
        } else if contract.status == ContractStatus::ReadyForLead {
            GateStatus::Unverified
        } else {
            GateStatus::Pending
        };
        if !same_version && self.kind != WorkflowKind::SimpleTask {
            self.set_gate(
                GateKind::Review,
                GateStatus::Pending,
                Some("review required for this workflow".into()),
                None,
            );
            self.set_gate(
                GateKind::Verification,
                GateStatus::Pending,
                Some("verification required before completion".into()),
                None,
            );
        }
        self.set_gate(
            GateKind::Contract,
            status,
            Some(format!("contract {}", contract.status.name())),
            None,
        );
        if !same_version {
            if contract.complexity.requires_plan() {
                self.set_gate(
                    GateKind::Plan,
                    GateStatus::Pending,
                    Some("implementation plan required".into()),
                    None,
                );
            } else {
                self.set_gate(
                    GateKind::Plan,
                    GateStatus::Skipped,
                    Some("not required".into()),
                    None,
                );
            }
            self.set_gate(
                GateKind::Design,
                if contract.complexity == Complexity::L3 {
                    GateStatus::Pending
                } else {
                    GateStatus::Skipped
                },
                None,
                None,
            );
        }
    }

    pub fn record_mode(&mut self, from: &str, to: &str, reason: &str) {
        self.mode = to.into();
        self.stage = match to {
            "plan" => WorkflowStage::Plan,
            "debug" => WorkflowStage::Debug,
            "review" => WorkflowStage::Review,
            _ => WorkflowStage::Build,
        };
        self.transitions.push(ModeTransition {
            from: from.into(),
            to: to.into(),
            reason: reason.into(),
            at: now_rfc3339(),
        });
        if to == "review" {
            self.set_gate(
                GateKind::Review,
                GateStatus::Pending,
                Some("review in progress".into()),
                None,
            );
        }
        self.updated_at = now_rfc3339();
    }

    pub fn record_evidence(&mut self, evidence_id: &str, status: GateStatus) {
        if !self.evidence_ids.contains(&evidence_id.to_string()) {
            self.evidence_ids.push(evidence_id.to_string());
        }
        self.set_gate(
            GateKind::Verification,
            status,
            None,
            Some(evidence_id.to_string()),
        );
    }

    pub fn can_finish(&self, config: &Config) -> Result<(), String> {
        if config.workflow.require_verification
            && self.gate(GateKind::Verification) != GateStatus::Passed
            && self.gate(GateKind::Verification) != GateStatus::Skipped
        {
            return Err("verification evidence is missing or failing".into());
        }
        if config.workflow.require_review
            && self.gate(GateKind::Review) != GateStatus::Passed
            && self.gate(GateKind::Review) != GateStatus::Skipped
        {
            return Err("review gate has not passed".into());
        }
        if self.gate(GateKind::Contract) == GateStatus::Pending {
            return Err("task contract is not ready".into());
        }
        Ok(())
    }

    pub fn mark_review_complete(&mut self) {
        self.review_completed = true;
        self.set_gate(GateKind::Review, GateStatus::Passed, None, None);
    }

    pub fn record_review(
        &mut self,
        report_id: &str,
        fingerprint: &str,
        contract_version: u32,
        approved: bool,
        reason: Option<String>,
    ) {
        self.review_report_id = Some(report_id.into());
        self.review_fingerprint = Some(fingerprint.into());
        self.review_contract_version = Some(contract_version);
        self.review_completed = approved;
        self.set_gate(
            GateKind::Review,
            if approved {
                GateStatus::Passed
            } else {
                GateStatus::Failed
            },
            reason,
            Some(report_id.into()),
        );
        if !approved {
            self.stage = WorkflowStage::Build;
        }
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "Workflow {} ({}) session {}\nstage: {} · mode: {} · contract: {}\n",
            self.id,
            self.kind.name(),
            self.session_id,
            self.stage.name(),
            self.mode,
            self.contract_id.as_deref().unwrap_or("none")
        );
        out.push_str("gates: ");
        for (index, gate) in GateKind::ALL.iter().enumerate() {
            if index > 0 {
                out.push_str(" · ");
            }
            out.push_str(&format!("{}={}", gate.name(), self.gate(*gate).label()));
        }
        out.push('\n');
        if let Some(last) = self.transitions.last() {
            out.push_str(&format!(
                "last transition: {} -> {}: {}\n",
                last.from, last.to, last.reason
            ));
        }
        out
    }
}

#[derive(Clone)]
pub struct WorkflowStore {
    store: ArtifactStore,
}

impl WorkflowStore {
    pub fn new() -> Result<Self, StorageError> {
        Ok(Self {
            store: ArtifactStore::new(kudelma_storage::workflows_dir()?),
        })
    }

    pub fn save(&self, state: &WorkflowState) -> Result<(), StorageError> {
        self.store.save(&state.id, state)
    }

    pub fn load(&self, id: &str) -> Result<WorkflowState, StorageError> {
        self.store.load(id)
    }

    pub fn list(&self) -> Result<Vec<WorkflowState>, StorageError> {
        let mut items: Vec<WorkflowState> = self.store.list()?;
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(items)
    }

    pub fn delete(&self, id: &str) -> Result<(), StorageError> {
        self.store.delete(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{Complexity, ContractStatus, IntentKind, TaskContract};

    fn contract(complexity: Complexity, status: ContractStatus) -> TaskContract {
        let mut contract =
            TaskContract::new("session", IntentKind::FeatureRequest, complexity, "x");
        contract.status = status;
        contract
    }

    #[test]
    fn feature_contract_requires_review_and_verification() {
        let mut state = WorkflowState::new("s", WorkflowKind::SimpleTask, Complexity::L1);
        state.set_gate(GateKind::Review, GateStatus::Skipped, None, None);
        state.attach_contract(&contract(Complexity::L2, ContractStatus::Approved));
        assert_eq!(state.gate(GateKind::Review), GateStatus::Pending);
        assert_eq!(state.gate(GateKind::Verification), GateStatus::Pending);
        assert_eq!(state.kind, WorkflowKind::Feature);
    }

    #[test]
    fn plan_is_not_completed_by_leaving_plan_mode() {
        let mut state = WorkflowState::new("s", WorkflowKind::Feature, Complexity::L2);
        state.record_mode("plan", "build", "plan approved");
        assert_eq!(state.gate(GateKind::Plan), GateStatus::Pending);
    }

    #[test]
    fn completion_blocks_without_evidence() {
        let mut state = WorkflowState::new("s", WorkflowKind::Feature, Complexity::L2);
        state.attach_contract(&contract(Complexity::L2, ContractStatus::Approved));
        state.mark_review_complete();
        let config = Config::default();
        assert!(state.can_finish(&config).is_err());
        state.record_evidence("e1", GateStatus::Passed);
        assert!(state.can_finish(&config).is_ok());
    }
}
