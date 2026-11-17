use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kudelma_permissions::PermissionAnswer;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::events::{AgentEvent, PermissionAsk};

#[async_trait]
pub trait Approver: Send + Sync {
    async fn ask(&self, ask: PermissionAsk) -> PermissionAnswer;
}

/// Headless answer: no one is watching, so only yolo mode gets through.
pub struct AutoApprover {
    pub allow: bool,
}

#[async_trait]
impl Approver for AutoApprover {
    async fn ask(&self, ask: PermissionAsk) -> PermissionAnswer {
        if ask.requires_exact {
            return PermissionAnswer::Deny;
        }
        if self.allow {
            PermissionAnswer::AllowOnce
        } else {
            PermissionAnswer::Deny
        }
    }
}

/// Sends a permission request to the UI and waits for its tagged answer.
pub struct ChannelApprover {
    events: UnboundedSender<AgentEvent>,
    pending: Mutex<HashMap<String, oneshot::Sender<PermissionAnswer>>>,
}

impl ChannelApprover {
    pub fn new(events: UnboundedSender<AgentEvent>) -> Arc<Self> {
        Arc::new(Self {
            events,
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn respond(&self, request_id: &str, answer: PermissionAnswer) -> bool {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(request_id);
        sender
            .map(|sender| sender.send(answer).is_ok())
            .unwrap_or(false)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    async fn ask(&self, ask: PermissionAsk) -> PermissionAnswer {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ask.request_id.clone(), tx);
        if self
            .events
            .send(AgentEvent::PermissionRequest(ask.clone()))
            .is_err()
        {
            return PermissionAnswer::Deny;
        }
        match tokio::time::timeout(Duration::from_secs(300), rx).await {
            Ok(Ok(answer)) => answer,
            _ => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ask.request_id);
                PermissionAnswer::Deny
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(requires_exact: bool) -> PermissionAsk {
        PermissionAsk {
            request_id: "test".into(),
            tool: "bash".into(),
            scopes: vec!["echo hi > /tmp/x".into()],
            reason: None,
            requires_exact,
        }
    }

    #[tokio::test]
    async fn auto_approver_never_auto_approves_exact_requests() {
        let approver = AutoApprover { allow: true };
        assert_eq!(approver.ask(ask(true)).await, PermissionAnswer::Deny);
        assert_eq!(approver.ask(ask(false)).await, PermissionAnswer::AllowOnce);
    }
}
