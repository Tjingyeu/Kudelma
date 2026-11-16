#![allow(clippy::collapsible_if)]

//! Agent runtime: sessions, model turns, tool execution, compaction and MCP.

pub mod agent;
pub mod agents;
pub mod approver;
pub mod compact;
pub mod events;
pub mod exploration;
pub mod hooks;
pub mod lead;
pub mod mcp;
pub mod orchestration;
pub mod prompt;
pub mod runtime;
pub mod runtime_tools;
pub mod session;
pub mod skills;
pub mod subagent;
pub mod tool_exec;
pub mod verification;
pub mod workflows;

pub use agent::Agent;
pub use approver::{Approver, AutoApprover, ChannelApprover};
pub use events::{AgentEvent, PermissionAsk, TaskStatus, TurnEndReason};
pub use kudelma_permissions::PermissionAnswer;
pub use lead::{AgentMode, LeadMode};
pub use session::{SessionHandle, SessionParams, SessionTimeline, spawn_session};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Provider(#[from] kudelma_providers::ProviderError),
    #[error(transparent)]
    Config(#[from] kudelma_config::ConfigError),
    #[error(transparent)]
    Storage(#[from] kudelma_storage::StorageError),
    #[error(transparent)]
    Session(#[from] kudelma_storage::SessionError),
    #[error(transparent)]
    Tool(#[from] kudelma_tools::ToolError),
    #[error("session {0} is open in another process")]
    SessionBusy(String),
    #[error("{0}")]
    Other(String),
}

impl From<kudelma_storage::ClaimError> for CoreError {
    fn from(error: kudelma_storage::ClaimError) -> Self {
        Self::SessionBusy(error.to_string())
    }
}
