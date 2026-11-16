pub mod context;
pub mod contract;
pub mod ledger;
pub mod worker;

pub use crate::exploration::{
    Caller, Confidence, EvidenceCite, ExplorationLimits, ExplorationReport, ExplorationRequest,
    ExplorationScope, ExplorationStatus, Finding, FindingKind,
};
pub use context::{ContextItem, ContextPack, FactKind};
pub use contract::{
    Complexity, ContractApproval, ContractIntent, ContractRecord, ContractReferences,
    ContractStatus, ContractStore, TaskContract,
};
pub use ledger::{ExecutionLedger, LedgerEvent, RoleUsage, SkillUsage};
pub use worker::{TestRun, WorkerReport, WorkerStatus, WorkerTask};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    GeneralQuestion,
    CodeQuestion,
    ArchitectureQuestion,
    FeatureRequest,
    BugReport,
    ChangeRequest,
    PlanningRequest,
    StatusQuestion,
}

impl IntentKind {
    pub const ALL: &'static [Self] = &[
        Self::GeneralQuestion,
        Self::CodeQuestion,
        Self::ArchitectureQuestion,
        Self::FeatureRequest,
        Self::BugReport,
        Self::ChangeRequest,
        Self::PlanningRequest,
        Self::StatusQuestion,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::GeneralQuestion => "general_question",
            Self::CodeQuestion => "code_question",
            Self::ArchitectureQuestion => "architecture_question",
            Self::FeatureRequest => "feature_request",
            Self::BugReport => "bug_report",
            Self::ChangeRequest => "change_request",
            Self::PlanningRequest => "planning_request",
            Self::StatusQuestion => "status_question",
        }
    }

    pub fn is_development(self) -> bool {
        matches!(
            self,
            Self::FeatureRequest | Self::BugReport | Self::ChangeRequest | Self::PlanningRequest
        )
    }
}
