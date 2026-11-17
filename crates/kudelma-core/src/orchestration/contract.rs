use serde::{Deserialize, Serialize};

use kudelma_storage::{ArtifactStore, StorageError, now_rfc3339};

use super::IntentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    L0,
    L1,
    L2,
    L3,
    Bug,
}

impl Complexity {
    pub fn name(self) -> &'static str {
        match self {
            Self::L0 => "L0",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
            Self::Bug => "bug",
        }
    }

    pub fn requires_plan(self) -> bool {
        matches!(self, Self::L2 | Self::L3)
    }

    pub fn requires_approval(self) -> bool {
        matches!(self, Self::L2 | Self::L3 | Self::Bug)
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.to_ascii_lowercase().as_str() {
            "l0" => Self::L0,
            "l1" => Self::L1,
            "l2" => Self::L2,
            "l3" => Self::L3,
            "bug" | "bug_fix" => Self::Bug,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    NeedsClarification,
    ReadyForLead,
    Approved,
    Superseded,
}

impl ContractStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::NeedsClarification => "needs_clarification",
            Self::ReadyForLead => "ready_for_lead",
            Self::Approved => "approved",
            Self::Superseded => "superseded",
        }
    }

    pub fn can_handoff(self) -> bool {
        matches!(self, Self::ReadyForLead | Self::Approved)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractIntent {
    pub summary: String,
    pub goals: Vec<String>,
    pub requirements: Vec<String>,
    pub non_goals: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractApproval {
    pub developer_confirmed: bool,
    pub approved_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractReferences {
    pub conversation_messages: Vec<String>,
    pub project_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContract {
    pub contract_version: u32,
    pub contract_id: String,
    pub status: ContractStatus,
    pub source_session_id: String,
    pub intent_kind: IntentKind,
    pub complexity: Complexity,
    pub workflow: String,
    pub intent: ContractIntent,
    pub constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub assumptions: Vec<String>,
    pub unresolved_questions: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub approval: ContractApproval,
    pub references: ContractReferences,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskContract {
    pub fn new(
        session_id: &str,
        intent_kind: IntentKind,
        complexity: Complexity,
        summary: impl Into<String>,
    ) -> Self {
        let now = now_rfc3339();
        Self {
            contract_version: 1,
            contract_id: kudelma_storage::new_id(),
            status: ContractStatus::ReadyForLead,
            source_session_id: session_id.into(),
            intent_kind,
            complexity,
            workflow: "adaptive".into(),
            intent: ContractIntent {
                summary: summary.into(),
                ..Default::default()
            },
            constraints: Vec::new(),
            decisions: Vec::new(),
            assumptions: Vec::new(),
            unresolved_questions: Vec::new(),
            acceptance_criteria: Vec::new(),
            approval: ContractApproval::default(),
            references: ContractReferences::default(),
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.intent.summary.trim().is_empty() {
            return Err("contract intent summary is empty".into());
        }
        if self.status == ContractStatus::Approved && !self.unresolved_questions.is_empty() {
            return Err("an approved contract cannot have unresolved questions".into());
        }
        if self.status == ContractStatus::Approved && !self.approval.developer_confirmed {
            return Err("an approved contract needs developer confirmation".into());
        }
        Ok(())
    }

    pub fn approve(&mut self) -> Result<(), String> {
        if self.status != ContractStatus::ReadyForLead {
            return Err(format!(
                "contract {} is {}; only ready_for_lead contracts can be approved",
                self.contract_id,
                self.status.name()
            ));
        }
        if !self.unresolved_questions.is_empty() {
            return Err("contract still has unresolved questions".into());
        }
        self.status = ContractStatus::Approved;
        self.approval.developer_confirmed = true;
        self.approval.approved_at = Some(now_rfc3339());
        self.updated_at = now_rfc3339();
        self.validate()
    }

    pub fn needs_approval(&self) -> bool {
        self.status == ContractStatus::ReadyForLead
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "Contract {} v{} ({})\nIntent: {}\nComplexity: {}\nWorkflow: {}\n",
            self.contract_id,
            self.contract_version,
            self.status.name(),
            self.intent.summary,
            self.complexity.name(),
            self.workflow
        );
        for (label, values) in [
            ("Goals", &self.intent.goals),
            ("Requirements", &self.intent.requirements),
            ("Non-goals", &self.intent.non_goals),
            ("Constraints", &self.constraints),
            ("Decisions", &self.decisions),
            ("Assumptions", &self.assumptions),
            ("Unresolved", &self.unresolved_questions),
            ("Acceptance", &self.acceptance_criteria),
        ] {
            if !values.is_empty() {
                out.push_str(&format!("{label}:\n"));
                for value in values {
                    out.push_str(&format!("- {value}\n"));
                }
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractRecord {
    pub current: Option<TaskContract>,
    pub history: Vec<TaskContract>,
}

impl ContractRecord {
    pub fn ids(&self) -> Vec<String> {
        self.current
            .iter()
            .map(|contract| contract.contract_id.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ContractStore {
    store: ArtifactStore,
}

impl ContractStore {
    pub fn new() -> Result<Self, StorageError> {
        Ok(Self {
            store: ArtifactStore::new(kudelma_storage::contracts_dir()?),
        })
    }

    pub fn from_store(store: ArtifactStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &ArtifactStore {
        &self.store
    }

    pub fn save(&self, contract: &TaskContract) -> Result<(), StorageError> {
        let id = format!("{}-v{}", contract.contract_id, contract.contract_version);
        self.store.save(&id, contract)?;
        self.store.save(&contract.contract_id, contract)
    }

    pub fn update(&self, contract: TaskContract) -> Result<TaskContract, StorageError> {
        let mut next = contract;
        next.contract_version += 1;
        next.updated_at = now_rfc3339();
        next.approval = ContractApproval::default();
        next.status = if next.unresolved_questions.is_empty() {
            ContractStatus::ReadyForLead
        } else {
            ContractStatus::NeedsClarification
        };
        next.validate()
            .map_err(|error| StorageError::Io(std::io::Error::other(error)))?;
        self.save(&next)?;
        Ok(next)
    }

    pub fn load(&self, id: &str) -> Result<TaskContract, StorageError> {
        self.store.load(id)
    }

    pub fn list(&self) -> Result<Vec<TaskContract>, StorageError> {
        let mut contracts: Vec<TaskContract> = self.store.list()?;
        contracts.retain(|contract| contract.status != ContractStatus::Superseded);
        contracts.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        contracts.dedup_by(|a, b| a.contract_id == b.contract_id);
        Ok(contracts)
    }

    pub fn approve(&self, id: &str) -> Result<TaskContract, String> {
        let mut contract = self.load(id).map_err(|error| error.to_string())?;
        contract.approve()?;
        self.save(&contract).map_err(|error| error.to_string())?;
        contract.validate()?;
        Ok(contract)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{ContextItem, ContextPack, IntentKind};

    #[test]
    fn approved_contract_rejects_unresolved_questions() {
        let mut contract = TaskContract::new("s", IntentKind::FeatureRequest, Complexity::L2, "x");
        contract.status = ContractStatus::Approved;
        contract.approval.developer_confirmed = true;
        assert!(contract.validate().is_ok());
        contract.unresolved_questions.push("choose storage".into());
        assert!(contract.validate().is_err());
    }

    #[test]
    fn only_ready_contracts_can_be_approved() {
        let mut contract = TaskContract::new("s", IntentKind::FeatureRequest, Complexity::L2, "x");
        contract.status = ContractStatus::NeedsClarification;
        assert!(contract.approve().is_err());
        contract.status = ContractStatus::ReadyForLead;
        contract.unresolved_questions.push("choose storage".into());
        assert!(contract.approve().is_err());
        contract.unresolved_questions.clear();
        contract.approve().unwrap();
        assert_eq!(contract.status, ContractStatus::Approved);
        assert!(contract.approval.developer_confirmed);
    }

    #[test]
    fn approval_records_timestamp_and_confirmation() {
        let mut contract = TaskContract::new("s", IntentKind::BugReport, Complexity::Bug, "fix");
        assert_eq!(contract.status, ContractStatus::ReadyForLead);
        contract.approve().unwrap();
        assert_eq!(contract.status, ContractStatus::Approved);
        assert!(contract.approval.developer_confirmed);
        assert!(contract.approval.approved_at.is_some());
    }

    #[test]
    fn context_pack_labels_are_rendered() {
        let mut pack = ContextPack::new("agency", "handoff");
        pack.push(ContextItem::fact("workspace is /tmp"));
        pack.push(ContextItem::evidence("test failed", "cargo test"));
        pack.push(ContextItem::assumption("API stays compatible"));
        pack.push(ContextItem::unknown("deployment target"));
        let text = pack.render();
        assert!(text.contains("confirmed fact"));
        assert!(text.contains("observed evidence"));
        assert!(text.contains("assumption"));
        assert!(text.contains("unknown"));
    }

    #[test]
    fn store_rejects_contracts_that_are_not_ready_for_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContractStore::from_store(ArtifactStore::new(dir.path().to_path_buf()));
        let mut contract = TaskContract::new("s", IntentKind::FeatureRequest, Complexity::L2, "x");
        contract.status = ContractStatus::NeedsClarification;
        store.save(&contract).unwrap();
        assert!(store.approve(&contract.contract_id).is_err());
    }

    #[test]
    fn contract_revision_clears_the_previous_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContractStore::from_store(ArtifactStore::new(dir.path().to_path_buf()));
        let mut contract = TaskContract::new("s", IntentKind::FeatureRequest, Complexity::L2, "x");
        contract.approve().unwrap();
        store.save(&contract).unwrap();
        let next = store.update(contract).unwrap();
        assert_eq!(next.status, ContractStatus::ReadyForLead);
        assert!(!next.approval.developer_confirmed);
        assert!(next.approval.approved_at.is_none());
    }
}
