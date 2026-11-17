use kudelma_storage::SessionMeta;

use crate::orchestration::TaskContract;
use crate::verification::ExecutionEvidence;
use crate::workflows::WorkflowState;
#[derive(Debug, Clone)]
pub struct PermissionAsk {
    pub request_id: String,
    pub tool: String,
    pub scopes: Vec<String>,
    pub reason: Option<String>,
    pub requires_exact: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Done,
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEndReason {
    Completed,
    MaxSteps,
    Cancelled,
    Error,
    ContextFull,
    Blocked,
}

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolStart {
        id: String,
        name: String,
        summary: String,
    },
    ToolEnd {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    PermissionRequest(PermissionAsk),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        context_tokens: u64,
        context_window: u32,
        cost_usd: f64,
        total_cost_usd: f64,
    },
    Info(String),
    Error(String),
    TaskUpdate {
        id: String,
        label: String,
        status: TaskStatus,
        detail: Option<String>,
    },
    RoleChanged {
        role: String,
        model: String,
    },
    ModeChanged {
        from: String,
        to: String,
        reason: String,
    },
    ContractUpdated(TaskContract),
    WorkflowUpdated(WorkflowState),
    Verification(ExecutionEvidence),
    ReviewUpdated(crate::verification::ReviewReport),
    TimelineReset(Vec<crate::session::SessionTimeline>),
    TurnDone {
        reason: TurnEndReason,
        session_id: String,
        meta: SessionMeta,
    },
}
