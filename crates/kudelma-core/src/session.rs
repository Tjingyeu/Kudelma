use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use kudelma_config::Config;
use kudelma_permissions::PermissionManager;
use kudelma_providers::{CancelToken, ContentBlock, Message, Role, resolve_model};
use kudelma_storage::{
    AuthStore, SessionClaim, SessionMeta, SessionStore, StoredSession, plans_dir, state_dir,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::CoreError;
use crate::agent::{Agent, AgentInit, AgentMode};
use crate::approver::{Approver, ChannelApprover};
use crate::events::AgentEvent;
use crate::mcp;

#[derive(Debug, Clone)]
pub struct SessionTimeline {
    pub role: String,
    pub text: String,
    pub tool: Option<String>,
    pub is_error: bool,
}

pub fn timeline_from_messages(messages: &[Message]) -> Vec<SessionTimeline> {
    let mut timeline = Vec::new();
    for message in messages {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => continue,
        };
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    if text.starts_with("[context update]")
                        || text.starts_with("[background task")
                        || text.starts_with("[Conversation compacted")
                    {
                        continue;
                    }
                    timeline.push(SessionTimeline {
                        role: role.into(),
                        text: text.clone(),
                        tool: None,
                        is_error: false,
                    });
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    timeline.push(SessionTimeline {
                        role: "tool".into(),
                        text: clip(&input.to_string(), 500),
                        tool: Some(name.clone()),
                        is_error: false,
                    });
                }
                ContentBlock::ToolResult {
                    content,
                    is_error,
                    name,
                    ..
                } => {
                    timeline.push(SessionTimeline {
                        role: "tool_result".into(),
                        text: clip(content, 8000),
                        tool: name.clone(),
                        is_error: *is_error,
                    });
                }
                _ => {}
            }
        }
    }
    timeline
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

pub struct SessionParams {
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub model: Option<String>,
    pub auth: Option<AuthStore>,
    pub yolo: bool,
    pub trusted: bool,
    pub resume: Option<StoredSession>,
    pub session_id: Option<String>,
    pub plan: bool,
    pub max_turns: Option<u32>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub approver: Option<Arc<dyn Approver>>,
}

pub struct SessionHandle {
    pub control: SessionControl,
    pub id: String,
    pub meta: SessionMeta,
    pub timeline: Vec<SessionTimeline>,
    pub events: UnboundedReceiver<AgentEvent>,
    pub runner: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct SessionControl {
    pub id: String,
    pub cancel: CancelToken,
    commands: UnboundedSender<SessionCommand>,
    approver: Option<Arc<ChannelApprover>>,
}

impl SessionControl {
    pub fn prompt(&self, text: impl Into<String>) {
        let _ = self.commands.send(SessionCommand::Prompt(text.into()));
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
        let _ = self.commands.send(SessionCommand::Cancel);
    }

    pub fn background(&self) {
        self.cancel.cancel_for_background();
        let _ = self.commands.send(SessionCommand::Cancel);
    }

    pub fn compact(&self, instructions: Option<String>) {
        let _ = self.commands.send(SessionCommand::Compact(instructions));
    }

    pub fn set_mode(&self, mode: AgentMode) {
        let _ = self.commands.send(SessionCommand::SetMode(mode));
    }

    pub fn set_model(&self, spec: impl Into<String>) {
        let _ = self.commands.send(SessionCommand::SetModel(spec.into()));
    }

    pub fn rewind(&self, user_index: usize) {
        let _ = self.commands.send(SessionCommand::Rewind(user_index));
    }

    pub fn shutdown(&self) {
        let _ = self.commands.send(SessionCommand::Shutdown);
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        answer: kudelma_permissions::PermissionAnswer,
    ) -> bool {
        match &self.approver {
            Some(approver) => approver.respond(request_id, answer),
            None => false,
        }
    }
}

impl SessionHandle {
    pub fn prompt(&self, text: impl Into<String>) {
        self.control.prompt(text);
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    pub fn background(&self) {
        self.control.background();
    }

    pub fn compact(&self, instructions: Option<String>) {
        self.control.compact(instructions);
    }

    pub fn set_mode(&self, mode: AgentMode) {
        self.control.set_mode(mode);
    }

    pub fn set_model(&self, spec: impl Into<String>) {
        self.control.set_model(spec);
    }

    pub fn rewind(&self, user_index: usize) {
        self.control.rewind(user_index);
    }

    pub fn shutdown(&self) {
        self.control.shutdown();
    }

    pub fn respond_permission(
        &self,
        request_id: &str,
        answer: kudelma_permissions::PermissionAnswer,
    ) -> bool {
        self.control.respond_permission(request_id, answer)
    }

    pub async fn close(mut self) {
        self.control.shutdown();
        if let Some(runner) = self.runner.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), runner).await;
        }
    }
}

enum SessionCommand {
    Prompt(String),
    Cancel,
    Compact(Option<String>),
    SetMode(AgentMode),
    SetModel(String),
    Rewind(usize),
    Shutdown,
}

pub async fn spawn_session(params: SessionParams) -> Result<SessionHandle, CoreError> {
    let model_spec = params
        .model
        .clone()
        .or_else(|| {
            params
                .resume
                .as_ref()
                .map(|session| session.meta.model.clone())
        })
        .unwrap_or_else(|| params.config.default_model.clone());
    let model = resolve_model(&params.config, params.auth.as_ref(), &model_spec)?;

    let mut meta = params
        .resume
        .as_ref()
        .map(|session| session.meta.clone())
        .unwrap_or_else(|| SessionMeta::new(&model.spec(), &params.cwd));
    if let Some(id) = &params.session_id {
        if !params
            .resume
            .as_ref()
            .is_some_and(|session| session.meta.id == *id)
        {
            meta.id = id.clone();
        }
    }
    meta.mode = if params.plan {
        "plan".into()
    } else {
        meta.mode.clone()
    };
    meta.model = model.spec();
    let id = meta.id.clone();

    let claim = SessionClaim::acquire(&id)?;
    let state_dir = state_dir()?;
    let store = SessionStore::new()?;
    let history: Vec<Message> = params
        .resume
        .as_ref()
        .map(|session| {
            session
                .messages
                .iter()
                .filter_map(|value| serde_json::from_value(value.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let plan_path = params
        .resume
        .as_ref()
        .and_then(|session| session.plan_path.clone())
        .map(PathBuf::from)
        .or_else(|| {
            (params.plan || meta.mode == "plan")
                .then(|| plans_dir().ok().map(|dir| dir.join(format!("{id}.md"))))
                .flatten()
        });
    let mode = match plan_path {
        Some(path) => AgentMode::Plan(path),
        None => AgentMode::Build,
    };

    let (event_tx, event_rx) = unbounded_channel();
    let (background_tx, mut background_rx) = unbounded_channel();
    let (approver, channel_approver): (Arc<dyn Approver>, Option<Arc<ChannelApprover>>) =
        match params.approver {
            Some(approver) => (approver, None),
            None => {
                let channel = ChannelApprover::new(event_tx.clone());
                (channel.clone(), Some(channel))
            }
        };
    let cancel = CancelToken::new();
    let permissions = Arc::new(PermissionManager::new(
        &params.config,
        &params.cwd,
        params.trusted,
        params.yolo,
    ));

    let mcp = mcp::connect_all(&params.config, params.trusted, &event_tx).await;
    for warning in &mcp.warnings {
        let _ = event_tx.send(AgentEvent::Info(warning.clone()));
    }
    let mcp_tools = mcp.tools.clone();

    let allowed_tools = params
        .allowed_tools
        .map(|tools| tools.into_iter().collect::<HashSet<_>>());
    if let Some(allowed) = &allowed_tools {
        for name in allowed {
            permissions.allow_tool_for_session(name);
        }
    }
    let disallowed_tools = params.disallowed_tools.into_iter().collect::<HashSet<_>>();
    let max_turns = params.max_turns.unwrap_or(params.config.agent.max_turns);

    let mut agent = Agent::new(AgentInit {
        meta: meta.clone(),
        history: history.clone(),
        model,
        config: params.config.clone(),
        permissions,
        approver,
        events: event_tx,
        cancel: cancel.clone(),
        state_dir: state_dir.clone(),
        store,
        claim,
        mode: mode.clone(),
        max_turns,
        system_prompt: params.system_prompt,
        append_system_prompt: params.append_system_prompt,
        allowed_tools,
        disallowed_tools,
        auth: params.auth,
        mcp_tools,
        mcp: mcp.manager,
        background_tx,
    });
    agent.save();
    let timeline = agent.timeline();

    let (command_tx, mut command_rx) = unbounded_channel();
    let runner = tokio::spawn(async move {
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    match command {
                        Some(SessionCommand::Prompt(text)) => {
                            let _ = agent.run_prompt(text).await;
                        }
                        Some(SessionCommand::Cancel) => {
                            agent.cancel_reset();
                        }
                        Some(SessionCommand::Compact(instructions)) => {
                            agent.compact(instructions).await;
                        }
                        Some(SessionCommand::SetMode(mode)) => {
                            agent.set_mode(mode);
                        }
                        Some(SessionCommand::SetModel(spec)) => {
                            if let Err(error) = agent.set_model(&spec) {
                                agent.emit_error(error);
                            }
                        }
                        Some(SessionCommand::Rewind(index)) => {
                            if !agent.rewind(index) {
                                agent.emit_info("Nothing to rewind to.");
                            }
                        }
                        Some(SessionCommand::Shutdown) | None => break,
                    }
                }
                notice = background_rx.recv() => {
                    if let Some(notice) = notice {
                        agent.handle_background(notice).await;
                    }
                }
            }
        }
        agent.shutdown().await;
    });

    Ok(SessionHandle {
        control: SessionControl {
            id: id.clone(),
            cancel,
            commands: command_tx,
            approver: channel_approver,
        },
        id,
        meta,
        timeline,
        events: event_rx,
        runner: Some(runner),
    })
}
