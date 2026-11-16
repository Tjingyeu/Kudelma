use serde::{Deserialize, Serialize};

/// Who invoked the shared exploration capability. Exploration has no model of
/// its own; the caller reasons over the returned evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Agency,
    Lead,
}

impl Caller {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agency => "agency",
            Self::Lead => "lead",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationMode {
    Codebase,
    CodeExplanation,
    TechnicalResearch,
    RuntimeInvestigation,
    ProjectKnowledge,
}

impl InvestigationMode {
    pub const ALL: &'static [Self] = &[
        Self::Codebase,
        Self::CodeExplanation,
        Self::TechnicalResearch,
        Self::RuntimeInvestigation,
        Self::ProjectKnowledge,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Codebase => "codebase",
            Self::CodeExplanation => "code_explanation",
            Self::TechnicalResearch => "technical_research",
            Self::RuntimeInvestigation => "runtime_investigation",
            Self::ProjectKnowledge => "project_knowledge",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw.to_ascii_lowercase().replace('-', "_").as_str() {
            "codebase" | "code" => Self::Codebase,
            "code_explanation" | "explanation" => Self::CodeExplanation,
            "technical_research" | "research" => Self::TechnicalResearch,
            "runtime_investigation" | "runtime" | "logs" => Self::RuntimeInvestigation,
            "project_knowledge" | "memory" => Self::ProjectKnowledge,
            _ => return None,
        })
    }

    pub fn includes_codebase(self) -> bool {
        matches!(
            self,
            Self::Codebase
                | Self::CodeExplanation
                | Self::TechnicalResearch
                | Self::RuntimeInvestigation
        )
    }

    pub fn includes_runtime(self) -> bool {
        matches!(self, Self::RuntimeInvestigation)
    }

    pub fn includes_memory(self) -> bool {
        matches!(self, Self::ProjectKnowledge)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExplorationScope {
    pub paths: Vec<String>,
    pub symbols: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationLimits {
    pub read_only: bool,
    pub max_files: usize,
    pub max_results: usize,
}

impl Default for ExplorationLimits {
    fn default() -> Self {
        Self {
            read_only: true,
            max_files: 30,
            max_results: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationRequest {
    pub request_id: String,
    pub caller: Caller,
    pub investigation_mode: InvestigationMode,
    pub objective: String,
    pub questions: Vec<String>,
    pub scope: ExplorationScope,
    pub limits: ExplorationLimits,
    pub expected_output: String,
}

impl ExplorationRequest {
    pub fn new(caller: Caller, mode: InvestigationMode, objective: impl Into<String>) -> Self {
        Self {
            request_id: kudelma_storage::new_id(),
            caller,
            investigation_mode: mode,
            objective: objective.into(),
            questions: Vec::new(),
            scope: ExplorationScope::default(),
            limits: ExplorationLimits::default(),
            expected_output: "findings with file:symbol:line evidence".into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.objective.trim().is_empty() {
            return Err("exploration objective is empty".into());
        }
        if !self.limits.read_only {
            return Err("the exploration capability is read-only".into());
        }
        Ok(())
    }
}
