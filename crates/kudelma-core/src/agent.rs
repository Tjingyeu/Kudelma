use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use crate::approver::Approver;
use crate::compact;
use crate::events::{AgentEvent, TurnEndReason};
use crate::lead::AgentMode;
use crate::mcp::McpManager;
use crate::session::{SessionTimeline, timeline_from_messages};
use crate::subagent::{TaskState, TaskTool};
use crate::tool_exec::{ToolRun, execute_call};
use kudelma_config::Config;
use kudelma_permissions::PermissionManager;
use kudelma_providers::{
    CancelToken, ChatRequest, ContentBlock, ContextGauge, Message, Model, ProviderError, Role,
    TokenUsage, ToolCall, ToolDefinition, estimate_request_tokens, provider_for, resolve_model,
};
use kudelma_storage::{
    AuthStore, SessionClaim, SessionMeta, SessionStore, StoredSession, now_rfc3339,
};
use kudelma_tools::{
    BackgroundNotice, LocalEnv, TodoItem, ToolContext, ToolRegistry, builtin_tools,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

pub struct AgentInit {
    pub meta: SessionMeta,
    pub history: Vec<Message>,
    pub model: Model,
    pub config: Arc<Config>,
    pub permissions: Arc<PermissionManager>,
    pub approver: Arc<dyn Approver>,
    pub events: UnboundedSender<AgentEvent>,
    pub cancel: CancelToken,
    pub state_dir: PathBuf,
    pub store: SessionStore,
    pub claim: SessionClaim,
    pub runtime: Arc<crate::runtime::RuntimeState>,
    pub agency_history: Arc<std::sync::Mutex<Vec<Message>>>,
    pub max_turns: u32,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub allowed_tools: Option<HashSet<String>>,
    pub disallowed_tools: HashSet<String>,
    pub auth: Option<AuthStore>,
    pub mcp_tools: Vec<Arc<dyn kudelma_tools::Tool>>,
    pub mcp: McpManager,
    pub background_tx: UnboundedSender<BackgroundNotice>,
}

pub struct Agent {
    pub meta: SessionMeta,
    pub history: Vec<Message>,
    pub model: Model,
    config: Arc<Config>,
    tools: ToolRegistry,
    permissions: Arc<PermissionManager>,
    approver: Arc<dyn Approver>,
    events: UnboundedSender<AgentEvent>,
    cancel: CancelToken,
    state_dir: PathBuf,
    store: SessionStore,
    _claim: SessionClaim,
    background_tx: UnboundedSender<BackgroundNotice>,
    read_times: Arc<RwLock<std::collections::BTreeMap<PathBuf, SystemTime>>>,
    todos: Arc<RwLock<Vec<TodoItem>>>,
    task_state: Arc<TaskState>,
    runtime: Arc<crate::runtime::RuntimeState>,
    agency_history: Arc<std::sync::Mutex<Vec<Message>>>,
    active_role: String,
    system_prompt: Option<String>,
    append_system_prompt: Option<String>,
    allowed_tools: Option<HashSet<String>>,
    disallowed_tools: HashSet<String>,
    max_turns: u32,
    usage_total: TokenUsage,
    cost_total: f64,
    context_tokens: u64,
    auth: Option<AuthStore>,
    _mcp: McpManager,
}

impl Agent {
    pub fn new(init: AgentInit) -> Self {
        let read_times = Arc::new(RwLock::new(std::collections::BTreeMap::new()));
        let todos = Arc::new(RwLock::new(Vec::new()));
        let background_tx = init.background_tx.clone();
        let input_tokens = init.meta.input_tokens;
        let output_tokens = init.meta.output_tokens;
        let cost_usd = init.meta.cost_usd;
        let active_role = init.meta.active_role.clone();
        let tools = ToolRegistry::new();
        for tool in builtin_tools() {
            tools.register(tool);
        }
        let mcp_tool_names: Vec<String> = init
            .mcp_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        for tool in init.mcp_tools {
            tools.register(tool);
        }
        if mcp_tool_names.len() > init.config.agent.mcp_defer_threshold {
            tools.hide(&mcp_tool_names);
        }
        tools.register(Arc::new(kudelma_tools::ToolSearchTool::new(tools.clone())));
        let task_allowed = init.allowed_tools.clone();
        let task_disallowed = init.disallowed_tools.clone();
        let task_state = Arc::new(TaskState {
            cwd: PathBuf::from(&init.meta.cwd),
            config: init.config.clone(),
            state_dir: init.state_dir.clone(),
            session_id: init.meta.id.clone(),
            model: RwLock::new(init.model.clone()),
            auth: init.auth.clone(),
            permissions: init.permissions.clone(),
            approver: init.approver.clone(),
            events: init.events.clone(),
            cancel: init.cancel.clone(),
            read_times: read_times.clone(),
            todos: todos.clone(),
            allowed_tools: task_allowed,
            disallowed_tools: task_disallowed,
            trusted: init.runtime.trusted,
            yolo: init.runtime.yolo,
            runtime: init.runtime.clone(),
        });
        tools.register(Arc::new(TaskTool::new(task_state.clone())));
        crate::runtime_tools::register_lead_tools(
            &tools,
            init.runtime.clone(),
            kudelma_permissions::AgentRole::Lead,
        );
        if let Some(plan) = init.runtime.mode().plan_path() {
            init.permissions.set_plan_path(Some(plan.to_path_buf()));
        }
        Self {
            meta: init.meta,
            history: init.history,
            model: init.model,
            config: init.config,
            tools,
            permissions: init.permissions,
            approver: init.approver,
            events: init.events,
            cancel: init.cancel,
            state_dir: init.state_dir,
            store: init.store,
            _claim: init.claim,
            background_tx,
            read_times,
            todos,
            task_state,
            runtime: init.runtime,
            agency_history: init.agency_history,
            active_role,
            system_prompt: init.system_prompt,
            append_system_prompt: init.append_system_prompt,
            allowed_tools: init.allowed_tools,
            disallowed_tools: init.disallowed_tools,
            max_turns: init.max_turns,
            usage_total: TokenUsage {
                input_tokens,
                output_tokens,
                ..Default::default()
            },
            cost_total: cost_usd,
            context_tokens: 0,
            auth: init.auth,
            _mcp: init.mcp,
        }
    }

    pub async fn run_prompt(&mut self, text: String) -> TurnEndReason {
        if self.meta.title == "New session" {
            self.set_title(&text);
        }
        self.history.push(Message::user(text));
        let reason = self.run_loop().await;
        self.finish_turn(reason).await
    }

    pub async fn handle_background(&mut self, notice: BackgroundNotice) {
        self.push_background_notice(notice);
        let reason = self.run_loop().await;
        self.finish_turn(reason).await;
    }

    pub fn cancel_reset(&self) {
        self.cancel.reset();
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    pub fn emit_error(&self, message: impl Into<String>) {
        let _ = self.events.send(AgentEvent::Error(message.into()));
    }

    pub fn emit_info(&self, message: impl Into<String>) {
        let _ = self.events.send(AgentEvent::Info(message.into()));
    }

    pub async fn compact(&mut self, instructions: Option<String>) {
        if self.history.len() < 2 {
            let _ = self
                .events
                .send(AgentEvent::Info("Nothing to compact yet.".into()));
            return;
        }
        let _ = self
            .events
            .send(AgentEvent::Info("Compacting conversation...".into()));
        match compact::summarize(
            &self.model,
            &self.history,
            instructions.as_deref(),
            &self.cancel,
        )
        .await
        {
            Ok(summary) => {
                self.history = vec![
                    Message::user(format!(
                        "[Conversation compacted. Summary of earlier work:]\n\n{summary}"
                    )),
                    Message::assistant("Understood. I will continue from that summary."),
                ];
                self.context_tokens = estimate_request_tokens(
                    Some(&self.system_prompt_text()),
                    &self.history,
                    &self.definitions(),
                );
                let _ = self
                    .events
                    .send(AgentEvent::Info("Context compacted. Continuing.".into()));
            }
            Err(ProviderError::Cancelled) => {}
            Err(error) => {
                self.history = compact::fallback_history(&self.history);
                let _ = self.events.send(AgentEvent::Info(format!(
                    "Compaction failed ({error}); kept the most recent messages."
                )));
            }
        }
        self.cancel.reset();
        self.save();
    }

    pub fn set_mode(&mut self, mode: AgentMode) {
        self.set_mode_reason(mode, "requested by the developer");
    }

    pub fn set_mode_reason(&mut self, mode: AgentMode, reason: &str) {
        if self.runtime.mode() == mode {
            return;
        }
        self.runtime.set_mode(mode.clone(), reason);
        self.permissions
            .set_plan_path(mode.plan_path().map(Path::to_path_buf));
        self.meta.mode = mode.name().into();
        self.meta.lead_mode = mode.name().into();
        if let AgentMode::Plan(path) = &mode {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            self.history.push(Message::user(format!(
                "[context update] PLAN mode ({reason}). Explore read-only, then write the plan to {}.",
                path.display()
            )));
        } else {
            self.history.push(Message::user(format!(
                "[context update] {} mode ({reason}).",
                mode.name().to_uppercase()
            )));
        }
        self.save();
    }

    fn sync_mode(&mut self) {
        let mode = self.runtime.mode();
        let name = mode.name().to_string();
        if self.meta.lead_mode != name {
            self.history.push(Message::user(format!(
                "[context update] Lead mode is now {name}."
            )));
        }
        self.permissions
            .set_plan_path(mode.plan_path().map(Path::to_path_buf));
        self.meta.lead_mode = name.clone();
        self.meta.mode = name;
    }

    pub fn title_from(&mut self, text: &str) {
        if self.meta.title == "New session" {
            self.set_title(text);
        }
    }

    pub fn set_model(&mut self, spec: &str) -> Result<(), String> {
        let model = resolve_model(&self.config, self.auth.as_ref(), spec)
            .map_err(|error| error.to_string())?;
        self.model = model.clone();
        self.meta.model = model.spec();
        *self
            .task_state
            .model
            .write()
            .unwrap_or_else(|e| e.into_inner()) = model;
        self.emit_usage();
        let _ = self
            .events
            .send(AgentEvent::Info(format!("Model: {}", self.meta.model)));
        self.save();
        Ok(())
    }

    pub fn rewind(&mut self, user_index: usize) -> bool {
        let mut seen = 0;
        let mut cut = None;
        for (index, message) in self.history.iter().enumerate() {
            if message.role != Role::User {
                continue;
            }
            let text = message.text();
            if text.starts_with("[context update]")
                || text.starts_with("[background task")
                || text.starts_with("[Conversation compacted")
            {
                continue;
            }
            if seen == user_index {
                cut = Some(index);
                break;
            }
            seen += 1;
        }
        let Some(cut) = cut else {
            return false;
        };
        self.history.truncate(cut);
        let _ = self
            .events
            .send(AgentEvent::TimelineReset(timeline_from_messages(
                &self.history,
            )));
        self.save();
        true
    }

    pub fn timeline(&self) -> Vec<SessionTimeline> {
        timeline_from_messages(&self.history)
    }

    pub fn save(&mut self) {
        self.save_as_role("lead");
    }

    pub fn save_as_role(&mut self, role: &str) {
        self.active_role = role.into();
        let mut meta = self.meta.clone();
        meta.updated_at = now_rfc3339();
        meta.input_tokens = self.usage_total.input_tokens;
        meta.output_tokens = self.usage_total.output_tokens;
        meta.cost_usd = self.cost_total;
        let mode = self.runtime.mode();
        meta.mode = mode.name().into();
        meta.lead_mode = mode.name().into();
        meta.active_role = role.into();
        meta.contract_id = self.runtime.contract().map(|contract| contract.contract_id);
        meta.workflow_id = Some(self.runtime.workflow().id);
        meta.model = self.model.spec();
        self.meta = meta.clone();
        let messages = self
            .history
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect();
        let agency_messages = self
            .agency_history
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect();
        let stored = StoredSession {
            meta,
            messages,
            agency_messages,
            plan_path: mode.plan_path().map(|path| path.display().to_string()),
        };
        let _ = self.store.save(&stored);
    }

    pub async fn finish_external(&mut self, reason: TurnEndReason) -> TurnEndReason {
        self.meta.updated_at = now_rfc3339();
        let _ = self.events.send(AgentEvent::TurnDone {
            reason,
            session_id: self.meta.id.clone(),
            meta: self.meta.clone(),
        });
        self.cancel.reset();
        reason
    }

    pub async fn shutdown(&mut self) {
        let role = self.active_role.clone();
        self.save_as_role(&role);
        crate::hooks::run_session_end(&self.config.hooks, Path::new(&self.meta.cwd)).await;
    }

    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.history.push(Message::user(text.into()));
    }

    fn push_background_notice(&mut self, notice: BackgroundNotice) {
        let _ = self.events.send(AgentEvent::Info(format!(
            "background `{}` finished{}",
            notice.label,
            if notice.is_error {
                " with an error"
            } else {
                ""
            }
        )));
        self.history.push(Message::user(format!(
            "[background task `{}` finished]\n{}",
            notice.label, notice.content
        )));
    }

    async fn finish_turn(&mut self, reason: TurnEndReason) -> TurnEndReason {
        self.save();
        self.meta.updated_at = now_rfc3339();
        let _ = self.events.send(AgentEvent::TurnDone {
            reason,
            session_id: self.meta.id.clone(),
            meta: self.meta.clone(),
        });
        self.cancel.reset();
        reason
    }

    pub async fn run_handoff(&mut self, context: String) -> TurnEndReason {
        self.sync_mode();
        self.history.push(Message::user(context));
        let reason = self.run_loop().await;
        self.finish_turn(reason).await
    }

    async fn run_loop(&mut self) -> TurnEndReason {
        let mut steps = 0u32;
        let mut auto_verified = false;
        loop {
            self.sync_mode();
            if let Some(budget) = self.runtime.budget_blocker() {
                let _ = self.events.send(AgentEvent::Error(budget));
                return TurnEndReason::Blocked;
            }
            if steps >= self.max_turns {
                let _ = self.events.send(AgentEvent::Info(format!(
                    "Stopped after {} turns.",
                    self.max_turns
                )));
                return TurnEndReason::MaxSteps;
            }
            if self.cancel.is_cancelled() {
                return TurnEndReason::Cancelled;
            }
            self.maybe_compact().await;

            let definitions = self.definitions();
            let system = self.system_prompt_text();
            let request = ChatRequest {
                messages: self.history.clone(),
                system: Some(system.clone()),
                tools: definitions.clone(),
                max_tokens: self.model.max_output_tokens.clamp(1024, 16_384),
                temperature: None,
                thinking: None,
            };
            let provider = provider_for(&self.model);
            let (tx, mut rx) = unbounded_channel();
            let cancel = self.cancel.clone();
            let task = tokio::spawn(async move { provider.stream(request, tx, cancel).await });
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut usage = TokenUsage::default();
            let mut cancelled = false;
            loop {
                tokio::select! {
                    event = rx.recv() => {
                        match event {
                            Some(kudelma_providers::ProviderEvent::TextDelta(delta)) => {
                                text.push_str(&delta);
                                let _ = self.events.send(AgentEvent::TextDelta(delta));
                            }
                            Some(kudelma_providers::ProviderEvent::ThinkingDelta(delta)) => {
                                let _ = self.events.send(AgentEvent::ThinkingDelta(delta));
                            }
                            Some(kudelma_providers::ProviderEvent::ToolUse(call)) => {
                                calls.push(call);
                            }
                            Some(kudelma_providers::ProviderEvent::Usage(value)) => {
                                usage = value;
                            }
                            None => break,
                        }
                    }
                    _ = self.cancel.cancelled() => {
                        cancelled = true;
                        task.abort();
                        break;
                    }
                }
            }
            let join = task.await;
            if cancelled {
                if !text.is_empty() {
                    text.push_str("\n\n[interrupted by user]");
                    self.history.push(Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::Text { text }],
                    });
                }
                let _ = self
                    .events
                    .send(AgentEvent::Info("Interrupted by user.".into()));
                self.save();
                return TurnEndReason::Cancelled;
            }
            match join {
                Ok(Ok(())) => {}
                Ok(Err(ProviderError::Cancelled)) => {
                    if !text.is_empty() {
                        text.push_str("\n\n[interrupted]");
                        self.history.push(Message {
                            role: Role::Assistant,
                            content: vec![ContentBlock::Text { text }],
                        });
                    }
                    self.save();
                    return TurnEndReason::Cancelled;
                }
                Ok(Err(error)) => {
                    if !text.is_empty() {
                        self.history.push(Message {
                            role: Role::Assistant,
                            content: vec![ContentBlock::Text { text }],
                        });
                    }
                    let _ = self
                        .events
                        .send(AgentEvent::Error(format!("model error: {error}")));
                    self.save();
                    return TurnEndReason::Error;
                }
                Err(error) => {
                    let _ = self
                        .events
                        .send(AgentEvent::Error(format!("model task failed: {error}")));
                    return TurnEndReason::Error;
                }
            }

            if usage.input_tokens == 0 {
                usage.input_tokens =
                    estimate_request_tokens(Some(&system), &self.history, &definitions);
            }
            if usage.output_tokens == 0 {
                usage.output_tokens = text.chars().count() as u64 / 4 + 1;
            }
            self.add_usage(usage);
            self.context_tokens =
                estimate_request_tokens(Some(&system), &self.history, &definitions);
            self.emit_usage();
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
                let _ = self
                    .events
                    .send(AgentEvent::Error("model returned an empty response".into()));
                return TurnEndReason::Error;
            }
            self.history.push(Message {
                role: Role::Assistant,
                content: blocks,
            });
            if calls.is_empty() {
                if let Some(blocker) = self.runtime.completion_blocker() {
                    let attempts = self
                        .runtime
                        .blocked_attempts
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    if blocker.contains("verification gate")
                        && !auto_verified
                        && !self.config.workflow.verification_commands.is_empty()
                    {
                        auto_verified = true;
                        let commands = self.config.workflow.verification_commands.clone();
                        let _ = self.events.send(AgentEvent::Info(format!(
                            "Running {} configured verification command(s)...",
                            commands.len()
                        )));
                        for command in commands {
                            match self
                                .runtime
                                .verifier
                                .run(
                                    &command,
                                    Path::new(&self.meta.cwd),
                                    crate::verification::EvidenceKind::Custom,
                                    &self.cancel,
                                )
                                .await
                            {
                                Ok(evidence) => {
                                    self.runtime.record_verification(&evidence);
                                }
                                Err(error) => {
                                    let _ = self.events.send(AgentEvent::Error(format!(
                                        "verification command failed to start: {error}"
                                    )));
                                }
                            }
                        }
                        steps += 1;
                        continue;
                    }
                    let _ = self.events.send(AgentEvent::Info(blocker.clone()));
                    if attempts > 2 {
                        let _ = self.events.send(AgentEvent::Error(format!(
                            "Blocked by workflow gate: {blocker}"
                        )));
                        self.save();
                        return TurnEndReason::Blocked;
                    }
                    self.history.push(Message::user(format!(
                        "[workflow gate] {blocker}\nFix the gate, then answer again."
                    )));
                    steps += 1;
                    continue;
                }
                self.runtime.mark_accepted();
                self.save();
                return TurnEndReason::Completed;
            }

            let mut results = Vec::new();
            for call in calls {
                if !self.tool_allowed(&call.name) {
                    results.push(kudelma_providers::ToolResult {
                        tool_use_id: call.id.clone(),
                        content: format!(
                            "Error: tool `{}` is disabled by the current configuration",
                            call.name
                        ),
                        is_error: true,
                        name: Some(call.name.clone()),
                    });
                    continue;
                }
                let ctx = self.tool_context();
                let result = execute_call(
                    ToolRun {
                        registry: &self.tools,
                        permissions: &self.permissions,
                        approver: self.approver.as_ref(),
                        ctx: &ctx,
                        events: &self.events,
                        hooks: &self.config.hooks,
                        emit: true,
                    },
                    &call,
                )
                .await;
                if !result.is_error && matches!(call.name.as_str(), "write" | "edit" | "bash") {
                    self.runtime.invalidate_verification();
                }
                results.push(result);
            }
            self.history.push(Message::tool_results(results));
            steps += 1;
        }
    }

    async fn maybe_compact(&mut self) {
        let system = self.system_prompt_text();
        let definitions = self.definitions();
        let used = estimate_request_tokens(Some(&system), &self.history, &definitions);
        self.context_tokens = used;
        let window = u64::from(self.model.context_window);
        let threshold = u64::from(self.config.agent.compaction_percent.clamp(1, 100));
        if used * 100 < window * threshold {
            return;
        }
        if self.history.len() <= 2 {
            return;
        }
        self.compact(None).await;
    }

    fn add_usage(&mut self, usage: TokenUsage) {
        self.usage_total.input_tokens += usage.input_tokens;
        self.usage_total.output_tokens += usage.output_tokens;
        self.usage_total.cache_read_tokens += usage.cache_read_tokens;
        self.usage_total.cache_write_tokens += usage.cache_write_tokens;
        let cost = self.model.cost(usage);
        self.cost_total += cost;
        self.runtime
            .record_turn("lead", usage.input_tokens, usage.output_tokens, cost);
        self.meta.input_tokens = self.usage_total.input_tokens;
        self.meta.output_tokens = self.usage_total.output_tokens;
        self.meta.cost_usd = self.cost_total;
    }

    fn emit_usage(&self) {
        let gauge = ContextGauge {
            used: self.context_tokens,
            window: self.model.context_window,
        };
        let _ = self.events.send(AgentEvent::Usage {
            input_tokens: self.usage_total.input_tokens,
            output_tokens: self.usage_total.output_tokens,
            context_tokens: gauge.used,
            context_window: gauge.window,
            cost_usd: self.model.cost(TokenUsage {
                input_tokens: self.usage_total.input_tokens,
                output_tokens: self.usage_total.output_tokens,
                ..Default::default()
            }),
            total_cost_usd: self.cost_total,
        });
    }

    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.tools.definitions();
        if let Some(allowed) = &self.allowed_tools {
            definitions.retain(|definition| allowed.contains(&definition.name));
        }
        definitions.retain(|definition| !self.disallowed_tools.contains(&definition.name));
        definitions
    }

    fn tool_allowed(&self, name: &str) -> bool {
        if self.disallowed_tools.contains(name) {
            return false;
        }
        self.allowed_tools
            .as_ref()
            .map(|allowed| allowed.contains(name))
            .unwrap_or(true)
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            env: Arc::new(LocalEnv::new(PathBuf::from(&self.meta.cwd))),
            state_dir: self.state_dir.clone(),
            session_id: self.meta.id.clone(),
            cancel: self.cancel.clone(),
            read_times: self.read_times.clone(),
            todos: self.todos.clone(),
            config: self.config.clone(),
            background: Some(self.background_tx.clone()),
        }
    }

    fn system_prompt_text(&self) -> String {
        crate::prompt::build_lead_prompt(
            &self.config,
            Path::new(&self.meta.cwd),
            &self.runtime.mode(),
            &self.runtime.skills,
            self.runtime.contract().as_ref(),
            self.system_prompt.as_deref(),
            self.append_system_prompt.as_deref(),
        )
    }

    fn set_title(&mut self, text: &str) {
        let title: String = text
            .lines()
            .next()
            .unwrap_or(text)
            .chars()
            .take(72)
            .collect();
        if !title.trim().is_empty() {
            self.meta.title = title.trim().to_string();
            let _ = self
                .events
                .send(AgentEvent::Info(format!("Session: {}", self.meta.title)));
        }
    }
}
