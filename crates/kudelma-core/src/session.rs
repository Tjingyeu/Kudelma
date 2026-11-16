use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kudelma_config::Config;
use kudelma_permissions::PermissionManager;
use kudelma_providers::{CancelToken, ContentBlock, Message, Role, resolve_model};
use kudelma_storage::{
    AuthStore, SessionClaim, SessionMeta, SessionStore, StoredSession, plans_dir, state_dir,
};
use kudelma_tools::{LocalEnv, ToolContext};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::CoreError;
use crate::agent::{Agent, AgentInit};
use crate::agents::agency::{AgencyAgent, AgencyOutcome};
use crate::approver::{Approver, ChannelApprover};
use crate::events::AgentEvent;
use crate::lead::{AgentMode, LeadMode};
use crate::mcp;
use crate::orchestration::{Complexity, ContextItem, ContextPack, ContractStatus, TaskContract};
use crate::runtime::RuntimeState;
use crate::skills::SkillRegistry;
use crate::verification::{GateKind, GateStatus};
use crate::workflows::{WorkflowKind, WorkflowState, WorkflowStore};

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
    pub mode: Option<String>,
    pub workflow: Option<kudelma_config::WorkflowPreset>,
    pub direct_lead: bool,
    pub auto_approve_contracts: bool,
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
    pub role: String,
    pub lead_mode: String,
    pub contract: Option<crate::orchestration::TaskContract>,
    pub workflow: crate::workflows::WorkflowState,
    pub skills: Arc<SkillRegistry>,
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

    pub fn approve_contract(&self, contract_id: Option<String>) {
        let _ = self
            .commands
            .send(SessionCommand::ApproveContract(contract_id));
    }

    pub fn verify(&self, command: String) {
        let _ = self.commands.send(SessionCommand::Verify(command));
    }

    pub fn switch_to_agency(&self) {
        let _ = self.commands.send(SessionCommand::SwitchToAgency);
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

    pub fn approve_contract(&self, contract_id: Option<String>) {
        self.control.approve_contract(contract_id);
    }

    pub fn verify(&self, command: String) {
        self.control.verify(command);
    }

    pub fn switch_to_agency(&self) {
        self.control.switch_to_agency();
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
    ApproveContract(Option<String>),
    Verify(String),
    SwitchToAgency,
    SetMode(AgentMode),
    SetModel(String),
    Rewind(usize),
    Shutdown,
}

pub async fn spawn_session(params: SessionParams) -> Result<SessionHandle, CoreError> {
    let lead_spec = params
        .model
        .clone()
        .or_else(|| {
            params
                .resume
                .as_ref()
                .map(|session| session.meta.model.clone())
        })
        .unwrap_or_else(|| params.config.default_model.clone());
    let lead_model = resolve_model(&params.config, params.auth.as_ref(), &lead_spec)?;

    let mut meta = params
        .resume
        .as_ref()
        .map(|session| session.meta.clone())
        .unwrap_or_else(|| SessionMeta::new(&lead_model.spec(), &params.cwd));
    if let Some(id) = &params.session_id {
        if !params
            .resume
            .as_ref()
            .is_some_and(|session| session.meta.id == *id)
        {
            meta.id = id.clone();
        }
    }
    meta.model = lead_model.spec();
    if params.direct_lead {
        meta.active_role = "lead".into();
    }
    if params.plan {
        meta.mode = "plan".into();
    } else if let Some(mode) = &params.mode {
        meta.mode = mode.clone();
    }
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
    let agency_seed: Vec<Message> = params
        .resume
        .as_ref()
        .map(|session| {
            session
                .agency_messages
                .iter()
                .filter_map(|value| serde_json::from_value(value.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let agency_history = Arc::new(Mutex::new(agency_seed.clone()));

    let plan_path = params
        .resume
        .as_ref()
        .and_then(|session| session.plan_path.clone())
        .map(PathBuf::from)
        .or_else(|| {
            (params.plan || meta.mode == "plan" || params.mode.as_deref() == Some("plan"))
                .then(|| plans_dir().ok().map(|dir| dir.join(format!("{id}.md"))))
                .flatten()
        });

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
    let skills = Arc::new(SkillRegistry::discover(
        &params.config,
        &params.cwd,
        params.trusted,
    ));
    for diagnostic in skills.diagnostics() {
        let _ = event_tx.send(AgentEvent::Info(format!("skill: {diagnostic}")));
    }

    let preset = params.workflow.unwrap_or(params.config.workflow.default);
    let kind = WorkflowKind::from_preset(preset).unwrap_or(WorkflowKind::SimpleTask);
    let complexity = match kind {
        WorkflowKind::BugFix => Complexity::Bug,
        WorkflowKind::Feature | WorkflowKind::Refactor => Complexity::L2,
        _ => Complexity::L1,
    };
    let mut workflow = meta
        .workflow_id
        .as_ref()
        .and_then(|workflow_id| WorkflowStore::new().ok()?.load(workflow_id).ok())
        .unwrap_or_else(|| WorkflowState::new(&id, kind, complexity));
    if params.direct_lead {
        workflow.set_gate(
            GateKind::Contract,
            GateStatus::Skipped,
            Some("direct lead run".into()),
            None,
        );
        if workflow.kind == WorkflowKind::SimpleTask {
            workflow.set_gate(
                GateKind::Plan,
                GateStatus::Skipped,
                Some("direct lead simple task".into()),
                None,
            );
        }
    }
    if workflow.kind == WorkflowKind::SimpleTask {
        workflow.set_gate(
            GateKind::Review,
            GateStatus::Skipped,
            Some("simple task uses proportionate verification".into()),
            None,
        );
        if params.config.workflow.verification_commands.is_empty() {
            workflow.set_gate(
                GateKind::Verification,
                GateStatus::Skipped,
                Some("simple task: no verification command configured".into()),
                None,
            );
        }
    }

    let runtime = Arc::new(
        RuntimeState::new(
            &id,
            &params.cwd,
            params.config.clone(),
            params.trusted,
            params.yolo,
            params.auth.clone(),
            event_tx.clone(),
            skills,
            workflow,
        )
        .map_err(CoreError::Other)?,
    );
    if let Some(contract_id) = meta.contract_id.clone()
        && let Ok(contract) = runtime.contracts.load(&contract_id)
    {
        let _ = runtime.set_contract(contract);
    }
    let initial_mode_name = params.mode.clone().unwrap_or_else(|| {
        if meta.mode != "build" && !meta.mode.is_empty() {
            meta.mode.clone()
        } else {
            match kind {
                WorkflowKind::Feature | WorkflowKind::Refactor => "plan".to_string(),
                WorkflowKind::BugFix => "debug".to_string(),
                _ => "build".to_string(),
            }
        }
    });
    if let Some(mode) = LeadMode::from_name(
        &initial_mode_name,
        plan_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("plan.md")),
    ) {
        runtime.set_mode(mode, "session start");
    }

    let permissions = Arc::new(
        PermissionManager::new(&params.config, &params.cwd, params.trusted, params.yolo)
            .for_role(kudelma_permissions::AgentRole::Lead, Default::default()),
    );

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
            if name == "verify" {
                permissions.allow_tool_for_session("bash");
            }
        }
    }
    let disallowed_tools = params.disallowed_tools.into_iter().collect::<HashSet<_>>();
    let max_turns = params
        .max_turns
        .unwrap_or_else(|| params.config.lead.turns());

    let mut agent = Agent::new(AgentInit {
        meta: meta.clone(),
        history: history.clone(),
        model: lead_model.clone(),
        config: params.config.clone(),
        permissions,
        approver: approver.clone(),
        events: event_tx.clone(),
        cancel: cancel.clone(),
        state_dir: state_dir.clone(),
        store,
        claim,
        runtime: runtime.clone(),
        agency_history: agency_history.clone(),
        max_turns,
        system_prompt: params.system_prompt,
        append_system_prompt: params.append_system_prompt,
        allowed_tools,
        disallowed_tools,
        auth: params.auth.clone(),
        mcp_tools,
        mcp: mcp.manager,
        background_tx,
    });
    agent.save_as_role(if params.direct_lead { "lead" } else { "agency" });
    let handle_meta = agent.meta.clone();
    let timeline = agent.timeline();

    let agency_model = runtime
        .model_for("agency", &lead_model)
        .unwrap_or_else(|_| lead_model.clone());
    let agency_ctx = ToolContext::new(
        Arc::new(LocalEnv::new(params.cwd.clone())),
        state_dir,
        id.clone(),
        cancel.clone(),
        params.config.clone(),
    );
    let mut agency = AgencyAgent::new(
        runtime.clone(),
        agency_model,
        agency_seed,
        approver,
        event_tx.clone(),
        agency_ctx,
    );
    let mut pending_contract: Option<TaskContract> =
        runtime.contract().filter(TaskContract::needs_approval);
    if params.auto_approve_contracts {
        if let Some(contract) = pending_contract.clone() {
            let _ = runtime.approve_contract(&contract.contract_id);
            pending_contract = None;
        }
    }
    let mut active_lead = params.direct_lead || meta.active_role == "lead";
    let mut clarification_rounds = 0u32;
    let auto_approve_contracts = params.auto_approve_contracts;
    let cwd = params.cwd.clone();
    let initial_lead_mode = runtime.mode_name();
    let initial_contract = runtime.contract();
    let initial_workflow = runtime.workflow();
    let initial_skills = runtime.skills.clone();
    let initial_role = handle_meta.active_role.clone();

    let (command_tx, mut command_rx) = unbounded_channel();
    let runner = tokio::spawn(async move {
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    match command {
                        Some(SessionCommand::Prompt(text)) => {
                            if active_lead {
                                let _ = agent.run_prompt(text).await;
                            } else {
                                match agency.respond(text.clone()).await {
                                    Ok(AgencyOutcome::Reply) => {
                                        *agency_history.lock().unwrap_or_else(|e| e.into_inner()) = agency.history.clone();
                                        if runtime.contract().map(|contract| contract.status == ContractStatus::NeedsClarification).unwrap_or(false) {
                                            clarification_rounds += 1;
                                            if clarification_rounds > runtime.config.agency.clarification_rounds() {
                                                let _ = event_tx.send(AgentEvent::Info(
                                                    "Clarification budget reached; summarize the unresolved decisions and ask the developer to choose a default.".into()
                                                ));
                                            }
                                        }
                                        agent.title_from(&text);
                                        agent.save_as_role("agency");
                                        let _ = event_tx.send(AgentEvent::RoleChanged {
                                            role: "agency".into(),
                                            model: runtime.model_for("agency", &agent.model).map(|model| model.spec()).unwrap_or_else(|_| agent.model.spec()),
                                        });
                                        let _ = agent.finish_external(crate::events::TurnEndReason::Completed).await;
                                    }
                                    Ok(AgencyOutcome::Cancelled) => {
                                        *agency_history.lock().unwrap_or_else(|e| e.into_inner()) = agency.history.clone();
                                        agent.save_as_role("agency");
                                        let _ = agent.finish_external(crate::events::TurnEndReason::Cancelled).await;
                                    }
                                    Ok(AgencyOutcome::Handoff(contract)) => {
                                        let contract = *contract;
                                        *agency_history.lock().unwrap_or_else(|e| e.into_inner()) = agency.history.clone();
                                        if contract.needs_approval() {
                                            if auto_approve_contracts {
                                                match runtime.approve_contract(&contract.contract_id) {
                                                    Ok(approved) => {
                                                        start_lead(&mut agent, &runtime, &approved, &cwd).await;
                                                        active_lead = true;
                                                        pending_contract = None;
                                                    }
                                                    Err(error) => {
                                                        pending_contract = Some(contract);
                                                        let _ = event_tx.send(AgentEvent::Error(format!("cannot approve contract: {error}")));
                                                        agent.save_as_role("agency");
                                                        let _ = agent.finish_external(crate::events::TurnEndReason::Error).await;
                                                    }
                                                }
                                            } else {
                                                pending_contract = Some(contract.clone());
                                                agent.save_as_role("agency");
                                                let _ = event_tx.send(AgentEvent::Info(format!(
                                                    "Contract pending approval: /contract approve {}",
                                                    contract.contract_id
                                                )));
                                                let _ = agent.finish_external(crate::events::TurnEndReason::Completed).await;
                                            }
                                        } else {
                                            start_lead(&mut agent, &runtime, &contract, &cwd).await;
                                            active_lead = true;
                                            pending_contract = None;
                                        }
                                    }
                                    Err(error) => {
                                        let _ = event_tx.send(AgentEvent::Error(format!("agency error: {error}")));
                                        let _ = agent.finish_external(crate::events::TurnEndReason::Error).await;
                                    }
                                }
                            }
                        }
                        Some(SessionCommand::Cancel) => {
                            agent.cancel_reset();
                        }
                        Some(SessionCommand::Compact(instructions)) => {
                            agent.compact(instructions).await;
                        }
                        Some(SessionCommand::ApproveContract(contract_id)) => {
                            let target = contract_id
                                .or_else(|| pending_contract.as_ref().map(|contract| contract.contract_id.clone()))
                                .or_else(|| runtime.contract().map(|contract| contract.contract_id));
                            if let Some(contract_id) = target {
                                match runtime.approve_contract(&contract_id) {
                                    Ok(contract) => {
                                        pending_contract = None;
                                        start_lead(&mut agent, &runtime, &contract, &cwd).await;
                                        active_lead = true;
                                    }
                                    Err(error) => {
                                        let _ = event_tx.send(AgentEvent::Error(format!("cannot approve contract: {error}")));
                                    }
                                }
                            } else {
                                let _ = event_tx.send(AgentEvent::Info("No task contract to approve.".into()));
                            }
                        }
                        Some(SessionCommand::SwitchToAgency) => {
                            active_lead = false;
                            let _ = event_tx.send(AgentEvent::RoleChanged {
                                role: "agency".into(),
                                model: runtime
                                    .model_for("agency", &agent.model)
                                    .map(|model| model.spec())
                                    .unwrap_or_else(|_| agent.model.spec()),
                            });
                            let _ = event_tx.send(AgentEvent::Info(
                                "Agency mode: describe a new or revised request.".into(),
                            ));
                        }
                        Some(SessionCommand::SetMode(mode)) => {
                            agent.set_mode(mode);
                        }
                        Some(SessionCommand::Verify(command)) => {                            match runtime.verifier.run(
                                &command,
                                &cwd,
                                crate::verification::EvidenceKind::Custom,
                                &agent.cancel_token(),
                            ).await {
                                Ok(evidence) => {
                                    runtime.record_verification(&evidence);
                                    let _ = event_tx.send(AgentEvent::Info(format!(
                                        "verification {}: {}",
                                        evidence.result.name(),
                                        command
                                    )));
                                }
                                Err(error) => {
                                    let _ = event_tx.send(AgentEvent::Error(format!(
                                        "verification failed to run: {error}"
                                    )));
                                }
                            }
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
        meta: handle_meta,
        timeline,
        role: initial_role,
        lead_mode: initial_lead_mode,
        contract: initial_contract,
        workflow: initial_workflow,
        skills: initial_skills,
        events: event_rx,
        runner: Some(runner),
    })
}

async fn start_lead(
    agent: &mut Agent,
    runtime: &Arc<RuntimeState>,
    contract: &TaskContract,
    cwd: &std::path::Path,
) {
    let _ = runtime.events.send(AgentEvent::RoleChanged {
        role: "lead".into(),
        model: agent.model.spec(),
    });
    let plan_path = plans_dir()
        .unwrap_or_else(|_| cwd.join(".kudelma/plans"))
        .join(format!("{}.md", runtime.session_id));
    let mode = if contract.complexity == Complexity::Bug {
        LeadMode::Debug
    } else if contract.complexity.requires_plan() {
        LeadMode::Plan(plan_path)
    } else {
        LeadMode::Build
    };
    runtime.set_mode(mode.clone(), "handoff from agency");
    let mut pack =
        ContextPack::new("agency", "approved task handoff").with_contract(contract.clone());
    pack.push(ContextItem::fact(format!(
        "Developer approval: {}",
        contract.approval.developer_confirmed
    )));
    pack.push(ContextItem::assumption(
        "Agency conversation history is intentionally not copied into Lead context.",
    ));
    let context = format!("[Agency -> Lead handoff]\n{}", pack.render());
    let _ = agent.run_handoff(context).await;
}
