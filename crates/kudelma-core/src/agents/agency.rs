use std::sync::Arc;

use kudelma_permissions::{AgentRole, CoderScope, PermissionManager};
use kudelma_providers::resolve_model;
use kudelma_providers::{
    ChatRequest, ContentBlock, Message, Model, ProviderEvent, Role, TokenUsage, provider_for,
};
use kudelma_storage::AuthStore;
use kudelma_tools::{ToolContext, ToolRegistry, builtin_tools};
use tokio::sync::mpsc::UnboundedSender;

use crate::approver::Approver;
use crate::events::AgentEvent;
use crate::orchestration::TaskContract;
use crate::runtime::RuntimeState;
use crate::runtime_tools::register_lead_tools;
use crate::tool_exec::{ToolRun, execute_call};

fn text_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub enum AgencyOutcome {
    Reply,
    Handoff(Box<TaskContract>),
    Blocked(String),
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
    allowed_tools: Option<std::collections::HashSet<String>>,
    disallowed_tools: std::collections::HashSet<String>,
    auth: Option<AuthStore>,
    dynamic_context_hash: Option<u64>,
}

impl AgencyAgent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: Arc<RuntimeState>,
        model: Model,
        history: Vec<Message>,
        approver: Arc<dyn Approver>,
        events: UnboundedSender<AgentEvent>,
        ctx: ToolContext,
        allowed_tools: Option<std::collections::HashSet<String>>,
        disallowed_tools: std::collections::HashSet<String>,
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
            allowed_tools,
            disallowed_tools,
            auth: None,
            dynamic_context_hash: None,
        }
    }

    pub fn set_model(&mut self, spec: &str) -> Result<String, String> {
        let auth = AuthStore::load().ok().or_else(|| self.auth.clone());
        let model = resolve_model(&self.runtime.config, auth.as_ref(), spec)
            .map_err(|error| error.to_string())?;
        self.model = model;
        self.auth = auth;
        let spec = self.model.spec();
        let _ = self
            .events
            .send(AgentEvent::Info(format!("Agency model: {spec}")));
        Ok(spec)
    }

    pub fn update_auth(&mut self, auth: AuthStore) {
        self.auth = Some(auth);
    }

    pub fn refresh_model(&mut self) -> Result<String, String> {
        let spec = self.model.spec();
        let auth = AuthStore::load().ok().or_else(|| self.auth.clone());
        let model = resolve_model(&self.runtime.config, auth.as_ref(), &spec)
            .map_err(|error| error.to_string())?;
        self.model = model;
        self.auth = auth;
        Ok(self.model.spec())
    }

    fn sync_dynamic_context(&mut self) {
        let context = crate::prompt::build_agency_context(
            &self.runtime.config,
            &self.runtime.cwd,
            &self.runtime.skills,
            self.runtime.contract().as_ref(),
            Some(&self.runtime.workflow()),
        );
        let hash = text_hash(&context);
        if self.dynamic_context_hash != Some(hash) {
            self.dynamic_context_hash = Some(hash);
            self.history
                .push(Message::user(format!("[context update]\n{context}")));
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
        registry.set_policy(kudelma_tools::ToolPolicy::new(
            self.allowed_tools.clone().map(|set| set.into_iter()),
            self.disallowed_tools.iter().cloned(),
        ));

        for _ in 0..self.max_turns {
            if self.ctx.cancel.is_cancelled() {
                return Ok(AgencyOutcome::Cancelled);
            }
            if let Some(budget) = self.runtime.budget_blocker() {
                return Ok(AgencyOutcome::Blocked(budget));
            }
            self.sync_dynamic_context();
            let system =
                crate::prompt::build_agency_stable_prompt(&self.runtime.config, &self.runtime.cwd);
            let request = ChatRequest {
                messages: self.history.clone(),
                system: Some(system),
                tools: registry.definitions(),
                max_tokens: 4096.min(self.model.max_output_tokens),
                temperature: None,
                thinking: None,
                session_id: Some(format!("{}:agency", self.runtime.session_id)),
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
            if let Some(budget) = self.runtime.budget_blocker() {
                return Ok(AgencyOutcome::Blocked(budget));
            }
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
