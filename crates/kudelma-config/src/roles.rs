use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CLARIFICATION_ROUNDS: u32 = 8;
pub const DEFAULT_AGENCY_TURNS: u32 = 10;
pub const DEFAULT_LEAD_TURNS: u32 = 40;
pub const DEFAULT_CODER_TURNS: u32 = 25;
pub const DEFAULT_MAX_CHILDREN: usize = 4;
pub const DEFAULT_EXPLORATION_MAX_FILES: usize = 30;
pub const DEFAULT_EXPLORATION_MAX_RESULTS: usize = 100;
pub const DEFAULT_EXPLORATION_ROUNDS: u32 = 5;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgencyRoleConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_turns: u32,
    pub max_clarification_rounds: u32,
}

impl AgencyRoleConfig {
    pub fn turns(&self) -> u32 {
        if self.max_turns == 0 {
            DEFAULT_AGENCY_TURNS
        } else {
            self.max_turns
        }
    }

    pub fn clarification_rounds(&self) -> u32 {
        if self.max_clarification_rounds == 0 {
            DEFAULT_CLARIFICATION_ROUNDS
        } else {
            self.max_clarification_rounds
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LeadRoleConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_turns: u32,
    pub max_children: usize,
}

impl LeadRoleConfig {
    pub fn turns(&self) -> u32 {
        if self.max_turns == 0 {
            DEFAULT_LEAD_TURNS
        } else {
            self.max_turns
        }
    }

    pub fn max_children(&self) -> usize {
        if self.max_children == 0 {
            DEFAULT_MAX_CHILDREN
        } else {
            self.max_children
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpecialistAgentsConfig {
    pub coder: CoderRoleConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExplorationConfig {
    pub enabled: bool,
    pub read_only: bool,
    pub prefer_deterministic_retrieval: bool,
    pub max_files: usize,
    pub max_results: usize,
    pub max_investigation_rounds: u32,
    pub allow_external_fetch: bool,
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            read_only: true,
            prefer_deterministic_retrieval: true,
            max_files: DEFAULT_EXPLORATION_MAX_FILES,
            max_results: DEFAULT_EXPLORATION_MAX_RESULTS,
            max_investigation_rounds: DEFAULT_EXPLORATION_ROUNDS,
            allow_external_fetch: true,
        }
    }
}

impl ExplorationConfig {
    pub fn file_limit(&self) -> usize {
        if self.max_files == 0 {
            DEFAULT_EXPLORATION_MAX_FILES
        } else {
            self.max_files
        }
    }

    pub fn result_limit(&self) -> usize {
        if self.max_results == 0 {
            DEFAULT_EXPLORATION_MAX_RESULTS
        } else {
            self.max_results
        }
    }

    pub fn rounds(&self) -> u32 {
        if self.max_investigation_rounds == 0 {
            DEFAULT_EXPLORATION_ROUNDS
        } else {
            self.max_investigation_rounds
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoderRoleConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_turns: u32,
    pub can_spawn: bool,
    /// Explicit opt-in for shell execution. Disabled by default because a
    /// shell can write outside `allowed_paths`; enabling it removes the file
    /// isolation guarantee unless the OS sandbox enforces it.
    pub allow_shell: bool,
}

impl CoderRoleConfig {
    pub fn turns(&self) -> u32 {
        if self.max_turns == 0 {
            DEFAULT_CODER_TURNS
        } else {
            self.max_turns
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPreset {
    #[default]
    Adaptive,
    Feature,
    BugFix,
    Refactor,
    Simple,
}

impl WorkflowPreset {
    pub const ALL: &'static [Self] = &[
        Self::Adaptive,
        Self::Feature,
        Self::BugFix,
        Self::Refactor,
        Self::Simple,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Feature => "feature-development",
            Self::BugFix => "bug-fix",
            Self::Refactor => "refactoring",
            Self::Simple => "simple-task",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Never,
    #[default]
    RiskBased,
    Always,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    Inline,
    SubagentDriven,
    #[default]
    Adaptive,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowConfig {
    pub default: WorkflowPreset,
    pub require_review: bool,
    pub require_verification: bool,
    pub require_spec_approval: ApprovalPolicy,
    pub require_plan_approval: ApprovalPolicy,
    pub verification_commands: Vec<String>,
    pub execution: WorkflowExecutionConfig,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            default: WorkflowPreset::Adaptive,
            require_review: true,
            require_verification: true,
            require_spec_approval: ApprovalPolicy::RiskBased,
            require_plan_approval: ApprovalPolicy::RiskBased,
            verification_commands: Vec::new(),
            execution: WorkflowExecutionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowExecutionConfig {
    pub strategy: ExecutionStrategy,
    pub max_parallel_coders: usize,
    pub fresh_coder_per_task: bool,
    pub review_after_each_task: bool,
}

impl Default for WorkflowExecutionConfig {
    fn default() -> Self {
        Self {
            strategy: ExecutionStrategy::Adaptive,
            max_parallel_coders: 3,
            fresh_coder_per_task: true,
            review_after_each_task: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillsConfig {
    pub enabled: bool,
    pub lazy_loading: bool,
    pub project_skills_require_trust: bool,
    pub disabled: Vec<String>,
    pub enabled_overrides: Vec<String>,
    pub builtins: BTreeMap<String, bool>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lazy_loading: true,
            project_skills_require_trust: true,
            disabled: Vec::new(),
            enabled_overrides: Vec::new(),
            builtins: BTreeMap::new(),
        }
    }
}

impl SkillsConfig {
    pub fn enabled_for(&self, name: &str) -> bool {
        if self.disabled.iter().any(|entry| entry == name) {
            return false;
        }
        if self.enabled_overrides.iter().any(|entry| entry == name) {
            return true;
        }
        self.builtins.get(name).copied().unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetsConfig {
    pub task_cost_usd: f64,
    pub role_cost_usd: BTreeMap<String, f64>,
    pub task_wall_clock_secs: u64,
}

impl Default for BudgetsConfig {
    fn default() -> Self {
        Self {
            task_cost_usd: 0.0,
            role_cost_usd: BTreeMap::new(),
            task_wall_clock_secs: 0,
        }
    }
}
