use std::sync::Arc;

use kudelma_permissions::{AgentRole, CoderScope, PermissionManager};
use kudelma_providers::{
    ChatRequest, ContentBlock, Message, Model, ProviderEvent, Role, TokenUsage, provider_for,
};
use kudelma_tools::{ToolContext, ToolRegistry, builtin_tools};
use tokio::sync::mpsc::UnboundedSender;

use crate::approver::Approver;
use crate::events::AgentEvent;
use crate::orchestration::TaskContract;
use crate::prompt::build_agency_prompt;
use crate::runtime::RuntimeState;
use crate::runtime_tools::register_lead_tools;
use crate::tool_exec::{ToolRun, execute_call};

pub enum AgencyOutcome {
    Reply,
    Handoff(Box<TaskContract>),
    Cancelled,
}

pub struct AgencyAgent {
    pub runtime: Arc<RuntimeState>,
    pub model: Model,
    pub history: Vec<Message>,
    pub approver: Arc<dyn Approver>,
    pub events: UnboundedSender<AgentEvent>,
    pub ctx: ToolContext,
    pub max_turns: u32,
}

impl AgencyAgent {
    pub fn new(
        runtime: Arc<RuntimeState>,
        model: Model,
        history: Vec<Message>,
        approver: Arc<dyn Approver>,
        events: UnboundedSender<AgentEvent>,
        ctx: ToolContext,
    ) -> Self {
        let max_turns = runtime.config.agency.turns();
        Self {
            runtime,
            model,
            history,
            approver,
            events,
            ctx,
            max_turns,
        }
    }

    pub async fn respond(&mut self, user_text: String) -> Result<AgencyOutcome, String> {
        self.history.push(Message::user(user_text));
        let registry = ToolRegistry::new();
        for tool in builtin_tools() {
            if matches!(tool.name(), "read" | "glob" | "grep" | "index" | "ls") {
                registry.register(tool);
            }
        }
        let permissions = Arc::new(
            PermissionManager::new(
                &self.runtime.config,
                &self.runtime.cwd,
                self.runtime.trusted,
                self.runtime.yolo,
            )
            .for_role(AgentRole::Agency, CoderScope::default()),
        );
        register_lead_tools(&registry, self.runtime.clone(), AgentRole::Agency);

        for _ in 0..self.max_turns {
            if self.ctx.cancel.is_cancelled() {
                return Ok(AgencyOutcome::Cancelled);
            }
            let system = build_agency_prompt(
                &self.runtime.config,
                &self.runtime.cwd,
                &self.runtime.skills,
                self.runtime.contract().as_ref(),
                Some(&self.runtime.workflow()),
            );
            let request = ChatRequest {
                messages: self.history.clone(),
                system: Some(system),
                tools: registry.definitions(),
                max_tokens: 4096.min(self.model.max_output_tokens),
                temperature: None,
                thinking: None,
            };
            let provider = provider_for(&self.model);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let cancel = self.ctx.cancel.clone();
            let task = tokio::spawn(async move { provider.stream(request, tx, cancel).await });
            let mut text = String::new();
            let mut calls = Vec::new();
            let mut usage = TokenUsage::default();
            while let Some(event) = rx.recv().await {
                match event {
                    ProviderEvent::TextDelta(delta) => {
                        text.push_str(&delta);
                        let _ = self.events.send(AgentEvent::TextDelta(delta));
                    }
                    ProviderEvent::ThinkingDelta(delta) => {
                        let _ = self.events.send(AgentEvent::ThinkingDelta(delta));
                    }
                    ProviderEvent::ToolUse(call) => calls.push(call),
                    ProviderEvent::Usage(value) => usage = value,
                }
            }
            let joined = task.await.map_err(|error| error.to_string())?;
            if let Err(error) = joined {
                if error.cancelled() {
                    return Ok(AgencyOutcome::Cancelled);
                }
                return Err(format!("agency model error: {error}"));
            }
            self.runtime.record_turn(
                "agency",
                usage.input_tokens,
                usage.output_tokens,
                self.model.cost(usage),
            );
            let mut blocks = Vec::new();
            if !text.is_empty() {
                blocks.push(ContentBlock::Text { text });
            }
            for call in &calls {
                blocks.push(ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                });
            }
            if blocks.is_empty() {
                return Ok(AgencyOutcome::Reply);
            }
            self.history.push(Message {
                role: Role::Assistant,
                content: blocks,
            });
            if calls.is_empty() {
                return Ok(AgencyOutcome::Reply);
            }

            let mut submitted = false;
            let mut results = Vec::new();
            for call in calls {
                if call.name == "submit_contract" {
                    submitted = true;
                }
                let result = execute_call(
                    ToolRun {
                        registry: &registry,
                        permissions: &permissions,
                        approver: self.approver.as_ref(),
                        ctx: &self.ctx,
                        events: &self.events,
                        hooks: &self.runtime.config.hooks,
                        emit: true,
                    },
                    &call,
                )
                .await;
                results.push(result);
            }
            self.history.push(Message::tool_results(results));

            if submitted {
                if let Some(contract) = self.runtime.contract() {
                    if contract.status.can_handoff() {
                        return Ok(AgencyOutcome::Handoff(Box::new(contract)));
                    }
                }
            }
        }
        Ok(AgencyOutcome::Reply)
    }
}
