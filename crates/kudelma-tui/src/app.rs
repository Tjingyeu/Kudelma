use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kudelma_core::orchestration::TaskContract;
use kudelma_core::workflows::WorkflowState;
use kudelma_core::{AgentEvent, PermissionAsk, SessionTimeline, TaskStatus, TurnEndReason};
use kudelma_permissions::PermissionAnswer;
use kudelma_providers::Model;
use kudelma_storage::{SessionMeta, SessionStore};

use crate::markdown;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
}

#[derive(Debug, Clone)]
pub enum Block {
    User(String),
    Assistant {
        text: String,
        streaming: bool,
    },
    Thinking(String),
    Tool {
        id: String,
        name: String,
        summary: String,
        output: String,
        status: ToolStatus,
        is_error: bool,
    },
    Info(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct TaskView {
    pub id: String,
    pub label: String,
    pub status: TaskStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub completion: String,
    pub file: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderChoice {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

pub enum Overlay {
    Sessions {
        items: Vec<SessionMeta>,
        selected: usize,
    },
    Models {
        items: Vec<Model>,
        selected: usize,
    },
    Providers {
        items: Vec<ProviderChoice>,
        selected: usize,
    },
    ConnectKey {
        provider: ProviderChoice,
        key: String,
    },
    Tasks {
        selected: usize,
    },
    Rewind {
        items: Vec<(usize, String)>,
        selected: usize,
    },
    Help,
}

pub enum Effect {
    Prompt(String),
    Cancel,
    Background,
    Quit,
    Answer(String, PermissionAnswer),
    SetMode(String),
    ApproveContract(Option<String>),
    Verify(String),
    ConnectProvider {
        provider: String,
        key: String,
    },
    SwitchToAgency,
    Compact,
    NewSession {
        resume: Option<String>,
        cwd: Option<PathBuf>,
    },
    SetModel(String),
    Rewind(usize),
    Shell(String),
}

pub(crate) const COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show key bindings and commands"),
    ("/new", "Start a new session"),
    ("/sessions", "Browse saved sessions"),
    ("/resume <id>", "Resume a session by id"),
    (
        "/models",
        "Choose a model; press c to connect another provider",
    ),
    ("/connect", "Connect or replace an API-key provider account"),
    ("/model [spec]", "Switch directly to a provider/model"),
    ("/mode", "Switch Lead mode: plan, build, debug, review"),
    (
        "/contract",
        "Show the task contract; /contract approve to approve",
    ),
    ("/workflow", "Show workflow stage, gates and evidence"),
    (
        "/skills",
        "List skills applicable to the current role and mode",
    ),
    ("/verify", "Run a deterministic verification command"),
    ("/agency", "Return to Agency for a new or revised request"),
    ("/compact", "Summarize and compact context"),
    ("/usage", "Show token and cost usage"),
    ("/tasks", "Show subagents and background tasks"),
    ("/rewind", "Rewind to an earlier prompt"),
    ("/clear", "Clear the transcript view"),
    ("/cd <path>", "Change project directory"),
    ("/init", "Ask the agent to write AGENTS.md"),
    ("/quit", "Exit Kudelma"),
];

pub struct App {
    pub(crate) blocks: Vec<Block>,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) busy: bool,
    pub(crate) meta: SessionMeta,
    pub(crate) model_spec: String,
    pub(crate) mode_name: String,
    pub(crate) role: String,
    pub(crate) contract: Option<TaskContract>,
    pub(crate) workflow: Option<WorkflowState>,
    pub(crate) skills: Option<std::sync::Arc<kudelma_core::skills::SkillRegistry>>,
    pub(crate) context_tokens: u64,
    pub(crate) context_window: u32,
    pub(crate) total_cost: f64,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) scroll: u16,
    pub(crate) viewport: u16,
    pub(crate) permission: Option<PermissionAsk>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) suggestions: Vec<Suggestion>,
    pub(crate) suggestion_index: usize,
    pub(crate) files: Vec<String>,
    pub(crate) models: Vec<Model>,
    pub(crate) providers: Vec<ProviderChoice>,
    pub(crate) theme: Theme,
    pub(crate) expand_tools: bool,
    pub(crate) expand_thinking: bool,
    pub(crate) tasks: Vec<TaskView>,
    pub(crate) history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) draft: String,
    pub(crate) spinner: u64,
    pub(crate) should_quit: bool,
    pub(crate) cwd: PathBuf,
    pub(crate) session_id: String,
    pub(crate) notice: Option<String>,
}

impl App {
    pub fn new(
        meta: SessionMeta,
        timeline: Vec<SessionTimeline>,
        models: Vec<Model>,
        files: Vec<String>,
        theme: Theme,
        cwd: PathBuf,
    ) -> Self {
        let mut app = Self {
            blocks: Vec::new(),
            input: String::new(),
            cursor: 0,
            busy: false,
            model_spec: meta.model.clone(),
            mode_name: meta.mode.clone(),
            role: meta.active_role.clone(),
            contract: None,
            workflow: None,
            skills: None,
            context_tokens: 0,
            context_window: 0,
            total_cost: meta.cost_usd,
            input_tokens: meta.input_tokens,
            output_tokens: meta.output_tokens,
            scroll: 0,
            viewport: 0,
            permission: None,
            overlay: None,
            suggestions: Vec::new(),
            suggestion_index: 0,
            files,
            models,
            providers: Vec::new(),
            theme,
            expand_tools: false,
            expand_thinking: false,
            tasks: Vec::new(),
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            spinner: 0,
            should_quit: false,
            cwd,
            session_id: meta.id.clone(),
            notice: None,
            meta,
        };
        app.refresh_provider_choices();
        app.apply_timeline(&timeline);
        app
    }

    pub fn reset(&mut self, meta: SessionMeta, timeline: Vec<SessionTimeline>, models: Vec<Model>) {
        self.blocks.clear();
        self.input.clear();
        self.cursor = 0;
        self.busy = false;
        self.permission = None;
        self.overlay = None;
        self.tasks.clear();
        self.scroll = 0;
        self.model_spec = meta.model.clone();
        self.mode_name = meta.mode.clone();
        self.role = meta.active_role.clone();
        self.contract = None;
        self.workflow = None;
        self.skills = None;
        self.total_cost = meta.cost_usd;
        self.input_tokens = meta.input_tokens;
        self.output_tokens = meta.output_tokens;
        self.context_tokens = 0;
        self.context_window = 0;
        self.notice = None;
        self.session_id = meta.id.clone();
        self.meta = meta;
        self.models = models;
        self.refresh_provider_choices();
        self.apply_timeline(&timeline);
    }

    fn apply_timeline(&mut self, timeline: &[SessionTimeline]) {
        for item in timeline {
            match item.role.as_str() {
                "user" => self.blocks.push(Block::User(item.text.clone())),
                "assistant" => self.blocks.push(Block::Assistant {
                    text: item.text.clone(),
                    streaming: false,
                }),
                "tool" => self.blocks.push(Block::Tool {
                    id: format!("timeline-{}", self.blocks.len()),
                    name: item.tool.clone().unwrap_or_else(|| "tool".into()),
                    summary: item.text.clone(),
                    output: String::new(),
                    status: ToolStatus::Done,
                    is_error: false,
                }),
                "tool_result" => {
                    let name = item.tool.clone();
                    let mut chosen = None;
                    for (index, block) in self.blocks.iter().enumerate().rev() {
                        if let Block::Tool {
                            name: tool_name,
                            output,
                            ..
                        } = block
                        {
                            let matches_name = name
                                .as_deref()
                                .map(|name| name == tool_name.as_str())
                                .unwrap_or(true);
                            if output.is_empty() && matches_name {
                                chosen = Some(index);
                                break;
                            }
                        }
                    }
                    if let Some(index) = chosen {
                        if let Block::Tool {
                            output,
                            is_error,
                            status,
                            ..
                        } = &mut self.blocks[index]
                        {
                            *output = item.text.clone();
                            *is_error = item.is_error;
                            *status = ToolStatus::Done;
                        }
                    } else {
                        self.blocks.push(Block::Info(item.text.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    pub fn on_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(delta) => {
                self.busy = true;
                match self.blocks.last_mut() {
                    Some(Block::Assistant { text, streaming }) if *streaming => {
                        text.push_str(&delta)
                    }
                    _ => self.blocks.push(Block::Assistant {
                        text: delta,
                        streaming: true,
                    }),
                }
            }
            AgentEvent::ThinkingDelta(delta) => match self.blocks.last_mut() {
                Some(Block::Thinking(text)) => text.push_str(&delta),
                _ => self.blocks.push(Block::Thinking(delta)),
            },
            AgentEvent::ToolStart { id, name, summary } => {
                if let Some(Block::Assistant { streaming, .. }) = self.blocks.last_mut() {
                    *streaming = false;
                }
                self.blocks.push(Block::Tool {
                    id,
                    name,
                    summary,
                    output: String::new(),
                    status: ToolStatus::Running,
                    is_error: false,
                });
            }
            AgentEvent::ToolEnd {
                id,
                output,
                is_error,
                ..
            } => {
                if let Some(Block::Tool {
                    output: slot,
                    status,
                    is_error: error_slot,
                    ..
                }) = self.blocks.iter_mut().rev().find(
                    |block| matches!(block, Block::Tool { id: block_id, .. } if block_id == &id),
                ) {
                    *slot = output;
                    *status = ToolStatus::Done;
                    *error_slot = is_error;
                }
            }
            AgentEvent::PermissionRequest(ask) => {
                self.permission = Some(ask);
            }
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                context_tokens,
                context_window,
                total_cost_usd,
                ..
            } => {
                self.input_tokens = input_tokens;
                self.output_tokens = output_tokens;
                self.context_tokens = context_tokens;
                self.context_window = context_window;
                self.total_cost = total_cost_usd;
            }
            AgentEvent::Info(message) => {
                if message.contains("MCP") || message.contains("mode") || message.contains("Model")
                {
                    self.notice = Some(message.clone());
                }
                self.blocks.push(Block::Info(message));
                self.scroll = 0;
            }
            AgentEvent::Error(message) => {
                self.busy = false;
                self.blocks.push(Block::Error(message));
                self.scroll = 0;
            }
            AgentEvent::TaskUpdate {
                id,
                label,
                status,
                detail,
            } => {
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = status;
                    task.label = label;
                    task.detail = detail;
                } else {
                    self.tasks.push(TaskView {
                        id,
                        label,
                        status,
                        detail,
                    });
                }
                if matches!(status, TaskStatus::Done | TaskStatus::Failed) {
                    self.notice = Some(format!(
                        "task {}: {}",
                        if status == TaskStatus::Done {
                            "completed"
                        } else {
                            "failed"
                        },
                        self.tasks
                            .last()
                            .map(|task| task.label.as_str())
                            .unwrap_or("")
                    ));
                }
            }
            AgentEvent::RoleChanged { role, model } => {
                self.role = role.clone();
                self.notice = Some(format!("{role} · {model}"));
                self.blocks
                    .push(Block::Info(format!("role: {role} ({model})")));
            }
            AgentEvent::ModeChanged { from, to, reason } => {
                self.mode_name = to.clone();
                self.notice = Some(format!("{from} -> {to}: {reason}"));
                self.blocks
                    .push(Block::Info(format!("mode: {from} -> {to} ({reason})")));
            }
            AgentEvent::ContractUpdated(contract) => {
                self.notice = Some(format!(
                    "contract v{}: {}",
                    contract.contract_version,
                    contract.status.name()
                ));
                self.contract = Some(contract);
            }
            AgentEvent::WorkflowUpdated(workflow) => {
                self.workflow = Some(workflow);
            }
            AgentEvent::Verification(evidence) => {
                self.notice = Some(format!(
                    "verify {}: {}",
                    evidence.result.name(),
                    evidence.command
                ));
                self.blocks.push(Block::Info(format!(
                    "verify [{}] {} {}",
                    evidence.result.name(),
                    evidence.command,
                    evidence
                        .exit_code
                        .map(|code| format!("exit {code}"))
                        .unwrap_or_default()
                )));
            }
            AgentEvent::ReviewUpdated(report) => {
                self.notice = Some(format!("review: {}", report.status.name()));
                self.blocks.push(Block::Info(report.render()));
            }
            AgentEvent::TimelineReset(timeline) => {
                self.blocks.clear();
                self.apply_timeline(&timeline);
                self.scroll = 0;
            }
            AgentEvent::TurnDone { reason, meta, .. } => {
                self.busy = false;
                self.meta = meta.clone();
                self.model_spec = meta.model.clone();
                self.mode_name = meta.mode.clone();
                self.role = meta.active_role.clone();
                self.total_cost = meta.cost_usd;
                if let Some(Block::Assistant { streaming, .. }) = self.blocks.last_mut() {
                    *streaming = false;
                }
                self.spinner = 0;
                if reason == TurnEndReason::Cancelled {
                    self.notice = Some("Turn cancelled".into());
                }
            }
        }
    }

    pub fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    pub fn submit_text(&mut self, text: impl Into<String>) -> Vec<Effect> {
        self.input = text.into();
        self.cursor = self.input.len();
        self.submit()
    }

    pub fn push_info(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Info(text.into()));
        self.scroll = 0;
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Error(text.into()));
        self.busy = false;
        self.scroll = 0;
    }

    pub fn set_mode_name(&mut self, name: impl Into<String>) {
        self.mode_name = name.into();
    }

    pub fn model_spec(&self) -> &str {
        &self.model_spec
    }

    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: std::path::PathBuf) {
        self.cwd = cwd;
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(Overlay::ConnectKey { key, .. }) = self.overlay.as_mut() {
            key.push_str(
                &text
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .collect::<String>(),
            );
        } else {
            self.insert_str(text);
        }
    }

    pub fn refresh_models(&mut self, models: Vec<Model>) {
        self.models = models;
        self.refresh_provider_choices();
    }

    pub fn open_model_picker(&mut self) {
        self.open_models();
    }

    fn refresh_provider_choices(&mut self) {
        let mut connected = std::collections::BTreeMap::<String, bool>::new();
        for model in &self.models {
            let available = model.api_key.is_some() || model.is_local();
            connected
                .entry(model.provider.clone())
                .and_modify(|value| *value |= available)
                .or_insert(available);
        }
        self.providers = connected
            .into_iter()
            .map(|(id, connected)| ProviderChoice {
                name: provider_display_name(&id),
                id,
                connected,
            })
            .collect();
    }

    pub fn on_mouse_scroll(&mut self, up: bool) {
        if up {
            self.scroll = self.scroll.saturating_add(3);
        } else {
            self.scroll = self.scroll.saturating_sub(3);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.permission.is_some() {
            return self.handle_permission_key(key);
        }
        if self.overlay.is_some() {
            return self.handle_overlay_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy {
                    self.busy = false;
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Char('q') if ctrl => {
                self.should_quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Char('x') if ctrl => {
                self.overlay = Some(Overlay::Tasks { selected: 0 });
                return vec![];
            }
            KeyCode::Char('b') if ctrl => {
                if self.busy {
                    self.notice = Some("Moving the current tool to the background…".into());
                    return vec![Effect::Background];
                }
                return vec![];
            }
            KeyCode::Char('t') if ctrl => {
                self.expand_thinking = !self.expand_thinking;
                return vec![];
            }
            KeyCode::Char('o') if ctrl => {
                self.expand_tools = !self.expand_tools;
                return vec![];
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(self.viewport.max(5));
                return vec![];
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.viewport.max(5));
                return vec![];
            }
            KeyCode::Esc => {
                if self.busy {
                    self.busy = false;
                    return vec![Effect::Cancel];
                }
                if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                    self.suggestions.clear();
                    return vec![];
                }
                self.open_rewind();
                return vec![];
            }
            KeyCode::Enter if shift || alt => {
                self.insert_str("\n");
                return vec![];
            }
            KeyCode::Char('j') if ctrl => {
                self.insert_str("\n");
                return vec![];
            }
            KeyCode::Enter => {
                return self.submit();
            }
            KeyCode::Tab => {
                self.complete_suggestion();
                return vec![];
            }
            KeyCode::Up => {
                if !self.suggestions.is_empty() {
                    self.suggestion_index = self.suggestion_index.saturating_sub(1);
                } else if !self.input.contains('\n') {
                    self.history_up();
                } else {
                    self.move_line(-1);
                }
                return vec![];
            }
            KeyCode::Down => {
                if !self.suggestions.is_empty() {
                    self.suggestion_index =
                        (self.suggestion_index + 1).min(self.suggestions.len() - 1);
                } else if !self.input.contains('\n') {
                    self.history_down();
                } else {
                    self.move_line(1);
                }
                return vec![];
            }
            KeyCode::Left => {
                self.cursor = prev_boundary(&self.input, self.cursor);
                return vec![];
            }
            KeyCode::Right => {
                self.cursor = next_boundary(&self.input, self.cursor);
                return vec![];
            }
            KeyCode::Home => {
                self.cursor = self.input[..self.cursor]
                    .rfind('\n')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                return vec![];
            }
            KeyCode::End => {
                self.cursor = self.input[self.cursor..]
                    .find('\n')
                    .map(|i| self.cursor + i)
                    .unwrap_or(self.input.len());
                return vec![];
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let previous = prev_boundary(&self.input, self.cursor);
                    self.input.replace_range(previous..self.cursor, "");
                    self.cursor = previous;
                    self.refresh_suggestions();
                }
                return vec![];
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    let next = next_boundary(&self.input, self.cursor);
                    self.input.replace_range(self.cursor..next, "");
                    self.refresh_suggestions();
                }
                return vec![];
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.insert_str(&c.to_string());
                return vec![];
            }
            _ => {}
        }
        vec![]
    }

    fn submit(&mut self) -> Vec<Effect> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return vec![];
        }
        self.input.clear();
        self.cursor = 0;
        self.suggestions.clear();
        self.history_index = None;
        self.scroll = 0;
        self.history.push(text.clone());
        if let Ok(mut history) = kudelma_storage::InputHistory::load() {
            history.push(&text);
        }
        if let Some(command) = text.strip_prefix('/') {
            return self.command(command);
        }
        if let Some(command) = text.strip_prefix('!') {
            let command = command
                .strip_prefix('!')
                .unwrap_or(command)
                .trim()
                .to_string();
            if command.is_empty() {
                return vec![];
            }
            self.blocks.push(Block::Info(format!("$ {command}")));
            return vec![Effect::Shell(command)];
        }
        self.blocks.push(Block::User(text.clone()));
        self.busy = true;
        vec![Effect::Prompt(text)]
    }

    fn command(&mut self, command: &str) -> Vec<Effect> {
        let mut parts = command.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let argument = parts.next().unwrap_or_default().trim();
        match name {
            "help" => {
                self.overlay = Some(Overlay::Help);
                vec![]
            }
            "quit" | "exit" => {
                self.should_quit = true;
                vec![Effect::Quit]
            }
            "new" => vec![Effect::NewSession {
                resume: None,
                cwd: None,
            }],
            "sessions" => {
                self.open_sessions();
                vec![]
            }
            "resume" if !argument.is_empty() => vec![Effect::NewSession {
                resume: Some(argument.to_string()),
                cwd: None,
            }],
            "resume" => {
                self.open_sessions();
                vec![]
            }
            "model" if !argument.is_empty() => vec![Effect::SetModel(argument.to_string())],
            "model" | "models" => {
                self.open_models();
                vec![]
            }
            "connect" => {
                self.open_providers();
                vec![]
            }
            "mode" => vec![Effect::SetMode(if argument.is_empty() {
                next_mode(&self.mode_name).to_string()
            } else {
                argument.to_ascii_lowercase()
            })],
            "contract" => {
                if argument == "approve" {
                    let id = self
                        .contract
                        .as_ref()
                        .map(|contract| contract.contract_id.clone());
                    vec![Effect::ApproveContract(id)]
                } else if let Some(contract) = &self.contract {
                    self.blocks.push(Block::Info(contract.render()));
                    vec![]
                } else {
                    self.blocks
                        .push(Block::Info("No task contract yet.".into()));
                    vec![]
                }
            }
            "workflow" => {
                if let Some(workflow) = &self.workflow {
                    self.blocks.push(Block::Info(workflow.render()));
                } else {
                    self.blocks
                        .push(Block::Info("No workflow state yet.".into()));
                }
                vec![]
            }
            "skills" => {
                if let Some(skills) = &self.skills {
                    let applicable = skills.applicable(&self.role, &self.mode_name);
                    if applicable.is_empty() {
                        self.blocks
                            .push(Block::Info("No applicable skills.".into()));
                    } else {
                        self.blocks.push(Block::Info(format!(
                            "Skills for {} / {}:",
                            self.role, self.mode_name
                        )));
                        for skill in applicable {
                            self.blocks.push(Block::Info(format!(
                                "  {} [{}]: {}",
                                skill.metadata.name,
                                skill.source.label(),
                                skill.metadata.description
                            )));
                        }
                    }
                } else {
                    self.blocks.push(Block::Info(
                        "Skill registry is not available in this session.".into(),
                    ));
                }
                vec![]
            }
            "verify" => {
                if argument.is_empty() {
                    self.blocks
                        .push(Block::Info("usage: /verify <command>".into()));
                    vec![]
                } else {
                    vec![Effect::Verify(argument.to_string())]
                }
            }
            "agency" => vec![Effect::SwitchToAgency],
            "compact" => vec![Effect::Compact],
            "usage" => {
                self.blocks.push(Block::Info(format!(
                    "tokens: {} in / {} out · context: {} / {} ({}%) · cost: ${:.4}",
                    self.input_tokens,
                    self.output_tokens,
                    self.context_tokens,
                    self.context_window,
                    if self.context_window == 0 {
                        0
                    } else {
                        (self.context_tokens * 100 / u64::from(self.context_window)).min(100)
                    },
                    self.total_cost
                )));
                vec![]
            }
            "tasks" => {
                self.overlay = Some(Overlay::Tasks { selected: 0 });
                vec![]
            }
            "rewind" => {
                self.open_rewind();
                vec![]
            }
            "clear" => {
                self.blocks.clear();
                vec![]
            }
            "cd" => {
                if argument.is_empty() {
                    self.blocks.push(Block::Info("usage: /cd <path>".into()));
                    return vec![];
                }
                vec![Effect::NewSession {
                    resume: None,
                    cwd: Some(PathBuf::from(argument)),
                }]
            }
            "init" => {
                self.blocks.push(Block::User(
                    "Create an AGENTS.md that documents this project's build, test and style conventions."
                        .into(),
                ));
                self.busy = true;
                vec![Effect::Prompt(
                    "Create an AGENTS.md that documents this project's build, test and style conventions."
                        .into(),
                )]
            }
            other => {
                self.blocks
                    .push(Block::Info(format!("unknown command /{other}; try /help")));
                vec![]
            }
        }
    }

    fn open_sessions(&mut self) {
        match SessionStore::new().and_then(|store| store.list()) {
            Ok(items) if items.is_empty() => {
                self.blocks
                    .push(Block::Info("No saved sessions yet.".into()));
            }
            Ok(items) => {
                self.overlay = Some(Overlay::Sessions { items, selected: 0 });
            }
            Err(error) => {
                self.blocks
                    .push(Block::Error(format!("cannot list sessions: {error}")));
            }
        }
    }

    fn open_models(&mut self) {
        let available: Vec<Model> = self
            .models
            .iter()
            .filter(|model| model.api_key.is_some() || model.is_local())
            .cloned()
            .collect();
        if available.is_empty() {
            self.open_providers();
            return;
        }
        let selected = available
            .iter()
            .position(|model| model.spec() == self.model_spec)
            .unwrap_or(0);
        self.overlay = Some(Overlay::Models {
            items: available,
            selected,
        });
    }

    fn open_providers(&mut self) {
        if self.providers.is_empty() {
            self.blocks.push(Block::Info(
                "No providers are configured. Add a custom [providers.<id>] entry to config.toml first."
                    .into(),
            ));
            return;
        }
        self.overlay = Some(Overlay::Providers {
            items: self.providers.clone(),
            selected: 0,
        });
    }

    fn open_rewind(&mut self) {
        let mut items = Vec::new();
        let mut index = 0;
        for block in &self.blocks {
            if let Block::User(text) = block {
                items.push((
                    index,
                    text.lines()
                        .next()
                        .unwrap_or(text)
                        .chars()
                        .take(70)
                        .collect(),
                ));
                index += 1;
            }
        }
        if items.is_empty() {
            self.blocks
                .push(Block::Info("Nothing to rewind to.".into()));
            return;
        }
        let selected = items.len().saturating_sub(1);
        self.overlay = Some(Overlay::Rewind { items, selected });
    }

    fn handle_permission_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let answer = match key.code {
            KeyCode::Char('y') | KeyCode::Enter => PermissionAnswer::AllowOnce,
            KeyCode::Char('a') => PermissionAnswer::AllowSession,
            KeyCode::Char('p') => PermissionAnswer::AllowAlwaysProject,
            KeyCode::Char('g') => PermissionAnswer::AllowAlwaysGlobal,
            KeyCode::Char('n') | KeyCode::Esc => PermissionAnswer::Deny,
            _ => return vec![],
        };
        let Some(ask) = self.permission.take() else {
            return vec![];
        };
        vec![Effect::Answer(ask.request_id, answer)]
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(overlay) = self.overlay.take() else {
            return vec![];
        };
        match overlay {
            Overlay::Help => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.overlay = Some(Overlay::Help);
                }
                vec![]
            }
            Overlay::Sessions {
                items,
                mut selected,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![],
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.overlay = Some(Overlay::Sessions { items, selected });
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Sessions { items, selected });
                    vec![]
                }
                KeyCode::Enter => {
                    let id = items.get(selected).map(|meta| meta.id.clone());
                    vec![Effect::NewSession {
                        resume: id,
                        cwd: None,
                    }]
                }
                _ => {
                    self.overlay = Some(Overlay::Sessions { items, selected });
                    vec![]
                }
            },
            Overlay::Models {
                items,
                mut selected,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![],
                KeyCode::Char('c') => {
                    self.open_providers();
                    vec![]
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.overlay = Some(Overlay::Models { items, selected });
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Models { items, selected });
                    vec![]
                }
                KeyCode::Enter => {
                    let spec = items.get(selected).map(|model| model.spec());
                    match spec {
                        Some(spec) => {
                            self.model_spec = spec.clone();
                            vec![Effect::SetModel(spec)]
                        }
                        None => vec![],
                    }
                }
                _ => {
                    self.overlay = Some(Overlay::Models { items, selected });
                    vec![]
                }
            },
            Overlay::Providers {
                items,
                mut selected,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![],
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.overlay = Some(Overlay::Providers { items, selected });
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Providers { items, selected });
                    vec![]
                }
                KeyCode::Enter => {
                    let Some(provider) = items.get(selected).cloned() else {
                        return vec![];
                    };
                    self.overlay = Some(Overlay::ConnectKey {
                        provider,
                        key: String::new(),
                    });
                    vec![]
                }
                _ => {
                    self.overlay = Some(Overlay::Providers { items, selected });
                    vec![]
                }
            },
            Overlay::ConnectKey {
                provider,
                key: mut api_key,
            } => match key.code {
                KeyCode::Esc => vec![],
                KeyCode::Enter if api_key.trim().is_empty() => {
                    self.blocks
                        .push(Block::Error("API key cannot be empty.".into()));
                    self.overlay = Some(Overlay::ConnectKey {
                        provider,
                        key: api_key,
                    });
                    vec![]
                }
                KeyCode::Enter => vec![Effect::ConnectProvider {
                    provider: provider.id,
                    key: api_key,
                }],
                KeyCode::Backspace => {
                    api_key.pop();
                    self.overlay = Some(Overlay::ConnectKey {
                        provider,
                        key: api_key,
                    });
                    vec![]
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT)
                        && !character.is_control() =>
                {
                    api_key.push(character);
                    self.overlay = Some(Overlay::ConnectKey {
                        provider,
                        key: api_key,
                    });
                    vec![]
                }
                _ => {
                    self.overlay = Some(Overlay::ConnectKey {
                        provider,
                        key: api_key,
                    });
                    vec![]
                }
            },
            Overlay::Tasks { mut selected } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![],
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.overlay = Some(Overlay::Tasks { selected });
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(self.tasks.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Tasks { selected });
                    vec![]
                }
                _ => {
                    self.overlay = Some(Overlay::Tasks { selected });
                    vec![]
                }
            },
            Overlay::Rewind {
                items,
                mut selected,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => vec![],
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    self.overlay = Some(Overlay::Rewind { items, selected });
                    vec![]
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(items.len().saturating_sub(1));
                    self.overlay = Some(Overlay::Rewind { items, selected });
                    vec![]
                }
                KeyCode::Enter => {
                    let index = items.get(selected).map(|(index, _)| *index);
                    match index {
                        Some(index) => vec![Effect::Rewind(index)],
                        None => vec![],
                    }
                }
                _ => {
                    self.overlay = Some(Overlay::Rewind { items, selected });
                    vec![]
                }
            },
        }
    }

    fn insert_str(&mut self, text: &str) {
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.refresh_suggestions();
    }

    fn refresh_suggestions(&mut self) {
        self.suggestion_index = 0;
        self.suggestions.clear();
        if self.input.starts_with('/') && !self.input.contains('\n') && !self.input.contains(' ') {
            let query = self.input.to_ascii_lowercase();
            for (command, detail) in COMMANDS {
                if command.starts_with(&query) {
                    self.suggestions.push(Suggestion {
                        label: command.to_string(),
                        detail: detail.to_string(),
                        completion: format!("{command} "),
                        file: false,
                    });
                }
            }
            return;
        }
        let Some(at) = self.input.rfind('@') else {
            return;
        };
        if at > 0 {
            let previous = self.input[..at].chars().last();
            if previous.map(|c| !c.is_whitespace()).unwrap_or(false) {
                return;
            }
        }
        let query = self.input[at + 1..].to_ascii_lowercase();
        if query.contains(' ') {
            return;
        }
        for file in &self.files {
            if file.to_ascii_lowercase().contains(&query) {
                self.suggestions.push(Suggestion {
                    label: file.clone(),
                    detail: String::new(),
                    completion: format!("@{file} "),
                    file: true,
                });
                if self.suggestions.len() >= 8 {
                    break;
                }
            }
        }
    }

    fn complete_suggestion(&mut self) {
        let Some(item) = self.suggestions.get(self.suggestion_index).cloned() else {
            return;
        };
        if item.file {
            if let Some(at) = self.input.rfind('@') {
                self.input.truncate(at);
                self.input.push_str(&item.completion);
                self.cursor = self.input.len();
            }
        } else {
            self.input = item.completion;
            self.cursor = self.input.len();
        }
        self.suggestions.clear();
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = self.input.clone();
                self.history.len() - 1
            }
        };
        self.history_index = Some(index);
        self.input = self.history[index].clone();
        self.cursor = self.input.len();
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input = std::mem::take(&mut self.draft);
        } else {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        }
        self.cursor = self.input.len();
    }

    fn move_line(&mut self, delta: i32) {
        let current_line_start = self.input[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let column = self.input[current_line_start..self.cursor].chars().count();
        if delta < 0 {
            if current_line_start == 0 {
                return;
            }
            let previous_end = current_line_start - 1;
            let previous_start = self.input[..previous_end]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            let offset = self.input[previous_start..previous_end]
                .char_indices()
                .nth(column)
                .map(|(i, _)| i)
                .unwrap_or(previous_end - previous_start);
            self.cursor = previous_start + offset;
        } else {
            let Some(relative_end) = self.input[self.cursor..].find('\n') else {
                return;
            };
            let next_start = self.cursor + relative_end + 1;
            if next_start >= self.input.len() {
                return;
            }
            let next_end = self.input[next_start..]
                .find('\n')
                .map(|i| next_start + i)
                .unwrap_or(self.input.len());
            let offset = self.input[next_start..next_end]
                .char_indices()
                .nth(column)
                .map(|(i, _)| i)
                .unwrap_or(next_end - next_start);
            self.cursor = next_start + offset;
        }
    }

    pub(crate) fn render_lines(&self, width: usize) -> Vec<ratatui::text::Line<'static>> {
        let mut lines = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if index > 0 {
                lines.push(ratatui::text::Line::default());
            }
            match block {
                Block::User(text) => {
                    lines.push(ratatui::text::Line::from(vec![
                        ratatui::text::Span::styled(
                            "› ",
                            ratatui::style::Style::default().fg(self.theme.user),
                        ),
                        ratatui::text::Span::styled(
                            text.clone(),
                            ratatui::style::Style::default().fg(self.theme.user),
                        ),
                    ]));
                }
                Block::Assistant { text, .. } => {
                    lines.extend(markdown::render_markdown(text, &self.theme));
                }
                Block::Thinking(text) => {
                    let rendered = markdown::render_markdown(text, &self.theme);
                    if self.expand_thinking || rendered.len() <= 4 {
                        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                            "thinking ▾",
                            ratatui::style::Style::default()
                                .fg(self.theme.dim)
                                .add_modifier(ratatui::style::Modifier::ITALIC),
                        )));
                        for line in rendered {
                            lines.push(indent_line(line, "  ", self.theme.dim));
                        }
                    } else {
                        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                            format!("thinking ▸ ({} lines, Ctrl-T to expand)", rendered.len()),
                            ratatui::style::Style::default()
                                .fg(self.theme.dim)
                                .add_modifier(ratatui::style::Modifier::ITALIC),
                        )));
                    }
                }
                Block::Tool {
                    name,
                    summary,
                    output,
                    status,
                    is_error,
                    ..
                } => {
                    let (marker, color) = match (status, is_error) {
                        (ToolStatus::Running, _) => ("◐", self.theme.warning),
                        (ToolStatus::Done, true) => ("✖", self.theme.error),
                        (ToolStatus::Done, false) => ("●", self.theme.success),
                    };
                    let mut header = vec![
                        ratatui::text::Span::styled(
                            format!("{marker} "),
                            ratatui::style::Style::default().fg(color),
                        ),
                        ratatui::text::Span::styled(
                            name.clone(),
                            ratatui::style::Style::default()
                                .fg(color)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ];
                    if !summary.is_empty() && summary != name {
                        header.push(ratatui::text::Span::styled(
                            format!(" {}", clip(summary, 120)),
                            ratatui::style::Style::default().fg(self.theme.dim),
                        ));
                    }
                    lines.push(ratatui::text::Line::from(header));
                    if !output.is_empty() {
                        let visible = if self.expand_tools {
                            output.clone()
                        } else {
                            preview(output, 10, 6)
                        };
                        let style = if *is_error {
                            ratatui::style::Style::default().fg(self.theme.error)
                        } else {
                            ratatui::style::Style::default().fg(self.theme.dim)
                        };
                        for line in visible.lines() {
                            lines.push(ratatui::text::Line::from(vec![
                                ratatui::text::Span::styled(
                                    "  │ ",
                                    ratatui::style::Style::default().fg(self.theme.dim),
                                ),
                                ratatui::text::Span::styled(clip(line, 500), style),
                            ]));
                        }
                    }
                }
                Block::Info(text) => {
                    lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                        format!("· {text}"),
                        ratatui::style::Style::default().fg(self.theme.dim),
                    )));
                }
                Block::Error(text) => {
                    lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                        format!("✖ {text}"),
                        ratatui::style::Style::default().fg(self.theme.error),
                    )));
                }
            }
        }
        let _ = width;
        lines
    }
}

fn indent_line(
    mut line: ratatui::text::Line<'static>,
    prefix: &str,
    color: ratatui::style::Color,
) -> ratatui::text::Line<'static> {
    let mut spans = vec![ratatui::text::Span::styled(
        prefix.to_string(),
        ratatui::style::Style::default().fg(color),
    )];
    spans.append(&mut line.spans);
    ratatui::text::Line::from(spans)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

fn preview(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head + tail {
        return text.to_string();
    }
    let mut out: Vec<&str> = lines[..head].to_vec();
    out.push("[…]");
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out.join("\n")
}

fn prev_boundary(text: &str, index: usize) -> usize {
    text[..index]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_boundary(text: &str, index: usize) -> usize {
    text[index..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| index + i)
        .unwrap_or(text.len())
}

fn next_mode(current: &str) -> &'static str {
    match current {
        "build" => "plan",
        "plan" => "debug",
        "debug" => "review",
        _ => "build",
    }
}

fn provider_display_name(provider: &str) -> String {
    match provider {
        "anthropic" => "Anthropic".into(),
        "openai" => "OpenAI".into(),
        "opencode-go" => "OpenCode Go".into(),
        "moonshot" => "Moonshot".into(),
        "deepseek" => "DeepSeek".into(),
        "gemini" => "Google Gemini".into(),
        "openrouter" => "OpenRouter".into(),
        "xai" => "xAI".into(),
        "ollama" => "Ollama".into(),
        custom => custom
            .split(['-', '_'])
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app() -> App {
        App::new(
            SessionMeta::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Theme::for_name("system"),
            PathBuf::from("."),
        )
    }

    fn provider() -> ProviderChoice {
        ProviderChoice {
            id: "opencode-go".into(),
            name: "OpenCode Go".into(),
            connected: false,
        }
    }

    #[test]
    fn pasted_api_key_never_enters_the_prompt() {
        let mut app = app();
        app.overlay = Some(Overlay::ConnectKey {
            provider: provider(),
            key: String::new(),
        });
        app.on_paste("sk-secret");
        assert!(app.input.is_empty());
        match app.overlay.as_ref().unwrap() {
            Overlay::ConnectKey { key, .. } => assert_eq!(key, "sk-secret"),
            _ => panic!("connect overlay closed"),
        }
    }

    #[test]
    fn enter_emits_a_connect_effect_with_the_hidden_key() {
        let mut app = app();
        app.overlay = Some(Overlay::ConnectKey {
            provider: provider(),
            key: "sk-secret".into(),
        });
        let effects = app.handle_overlay_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::ConnectProvider { provider, key } => {
                assert_eq!(provider, "opencode-go");
                assert_eq!(key, "sk-secret");
            }
            _ => panic!("expected ConnectProvider"),
        }
    }

    #[test]
    fn escape_closes_the_connect_overlay_without_leaking_the_key() {
        let mut app = app();
        app.overlay = Some(Overlay::ConnectKey {
            provider: provider(),
            key: "sk-secret".into(),
        });
        assert!(
            app.handle_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_empty()
        );
        assert!(app.overlay.is_none());
        assert!(app.blocks.iter().all(|block| match block {
            Block::Info(text) | Block::Error(text) => !text.contains("sk-secret"),
            _ => true,
        }));
    }
}
