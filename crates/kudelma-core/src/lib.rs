#![allow(clippy::collapsible_if)]

//! Agent runtime: sessions, model turns, tool execution, compaction and MCP.

pub mod agent;
pub mod approver;
pub mod compact;
pub mod events;
pub mod hooks;
pub mod mcp;
pub mod prompt;
pub mod session;
pub mod subagent;
pub mod tool_exec;

pub use agent::AgentMode;
pub use approver::{Approver, AutoApprover, ChannelApprover};
pub use events::{AgentEvent, PermissionAsk, TaskStatus, TurnEndReason};
pub use kudelma_permissions::PermissionAnswer;
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
