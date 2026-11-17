use std::path::{Path, PathBuf};

/// Lead's operating phase. These are modes of one Lead session, never
/// separate agents.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeadMode {
    Plan(PathBuf),
    Build,
    Debug,
    Review,
}

pub type AgentMode = LeadMode;

impl LeadMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Plan(_) => "plan",
            Self::Build => "build",
            Self::Debug => "debug",
            Self::Review => "review",
        }
    }

    pub fn plan_path(&self) -> Option<&Path> {
        match self {
            Self::Plan(path) => Some(path),
            _ => None,
        }
    }

    pub fn from_name(name: &str, plan_path: PathBuf) -> Option<Self> {
        Some(match name {
            "plan" => Self::Plan(plan_path),
            "build" => Self::Build,
            "debug" => Self::Debug,
            "review" => Self::Review,
            _ => return None,
        })
    }

    pub fn plan(path: PathBuf) -> Self {
        Self::Plan(path)
    }

    pub fn permission_mode(&self) -> kudelma_permissions::PermissionMode {
        match self {
            Self::Plan(_) => kudelma_permissions::PermissionMode::Plan,
            Self::Build => kudelma_permissions::PermissionMode::Build,
            Self::Debug => kudelma_permissions::PermissionMode::Debug,
            Self::Review => kudelma_permissions::PermissionMode::Review,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModeTransitionRecord {
    pub from: String,
    pub to: String,
    pub reason: String,
    pub at: String,
}
