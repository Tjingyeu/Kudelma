//! Agent Client Protocol (ACP) server over stdio NDJSON.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kudelma_config::Config;
use kudelma_core::session::SessionControl;
use kudelma_core::{
    Approver, PermissionAnswer, PermissionAsk, SessionHandle, SessionParams, TurnEndReason,
    spawn_session,
};
use kudelma_storage::{AuthStore, SessionMeta, TrustStore, canonicalize_lenient};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

type Pending = Arc<std::sync::Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

pub struct AcpParams {
    pub model: Option<String>,
    pub yolo: bool,
    pub trust: bool,
    pub config: Arc<Config>,
}

#[derive(Clone)]
struct State {
    out: UnboundedSender<Value>,
    pending: Pending,
    sessions: Arc<std::sync::Mutex<HashMap<String, SessionEntry>>>,
    completions: Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<TurnEndReason>>>>,
    next_id: Arc<AtomicI64>,
    model: Option<String>,
    yolo: bool,
    trust: bool,
    config: Arc<Config>,
}

struct SessionEntry {
    control: SessionControl,
    _meta: SessionMeta,
    _runner: Option<JoinHandle<()>>,
}

pub async fn run(params: AcpParams) -> color_eyre::Result<()> {
    let (out_tx, mut out_rx) = unbounded_channel::<Value>();
    let (in_tx, mut in_rx) = unbounded_channel::<Value>();
    let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));

    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(value) = out_rx.recv().await {
            let line = format!("{}\n", value);
            if stdout.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });

    let pending_reader = pending.clone();
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut lines = BufReader::new(stdin).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("method").is_some() {
                let _ = in_tx.send(value);
            } else if let Some(id) = value["id"].as_i64() {
                let sender = pending_reader
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                if let Some(sender) = sender {
                    if value.get("error").is_some() {
                        let message = value["error"]["message"]
                            .as_str()
                            .unwrap_or("client error")
                            .to_string();
                        let _ = sender.send(Err(message));
                    } else {
                        let _ = sender.send(Ok(value["result"].clone()));
                    }
                }
            }
        }
    });

    let state = State {
        out: out_tx,
        pending,
        sessions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        completions: Arc::new(std::sync::Mutex::new(HashMap::new())),
        next_id: Arc::new(AtomicI64::new(1000)),
        model: params.model,
        yolo: params.yolo,
        trust: params.trust,
        config: params.config,
    };

    while let Some(message) = in_rx.recv().await {
        let Some(method) = message["method"].as_str().map(str::to_string) else {
            continue;
        };
        let params = message["params"].clone();
        match message["id"].as_i64() {
            Some(id) => {
                let state = state.clone();
                tokio::spawn(async move {
                    match handle_request(&state, &method, params).await {
                        Ok(result) => {
                            let _ = state.out.send(json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result,
                            }));
                        }
                        Err(error) => {
                            let _ = state.out.send(json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32603, "message": error },
                            }));
                        }
                    }
                });
            }
            None => {
                if method == "session/cancel" {
                    let session_id = params["sessionId"].as_str().unwrap_or_default();
                    let control = state
                        .sessions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(session_id)
                        .map(|entry| entry.control.clone());
                    if let Some(control) = control {
                        control.cancel();
                    }
                }
            }
        }
    }
    let controls: Vec<SessionControl> = state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .map(|entry| entry.control.clone())
        .collect();
    for control in controls {
        control.shutdown();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

async fn handle_request(state: &State, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": true
                }
            },
            "authMethods": []
        })),
        "authenticate" => Ok(json!({})),
        "session/new" => session_new(state, &params).await,
        "session/prompt" => session_prompt(state, &params).await,
        "session/cancel" => {
            let session_id = params["sessionId"].as_str().unwrap_or_default();
            if let Some(control) = state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session_id)
                .map(|entry| entry.control.clone())
            {
                control.cancel();
            }
            Ok(json!({}))
        }
        "session/set_mode" => {
            let session_id = params["sessionId"].as_str().unwrap_or_default();
            let mode = params["modeId"].as_str().unwrap_or("build");
            let control = state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session_id)
                .map(|entry| entry.control.clone())
                .ok_or_else(|| format!("unknown session {session_id}"))?;
            let mode = if mode == "plan" {
                kudelma_core::AgentMode::Plan(
                    kudelma_storage::plans_dir()
                        .map_err(|error| error.to_string())?
                        .join(format!("{session_id}.md")),
                )
            } else {
                kudelma_core::AgentMode::Build
            };
            control.set_mode(mode);
            Ok(json!({}))
        }
        "session/set_model" => {
            let session_id = params["sessionId"].as_str().unwrap_or_default();
            let model = params["modelId"]
                .as_str()
                .ok_or_else(|| "missing modelId".to_string())?;
            let control = state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session_id)
                .map(|entry| entry.control.clone())
                .ok_or_else(|| format!("unknown session {session_id}"))?;
            control.set_model(model);
            Ok(json!({}))
        }
        other => Err(format!("method not supported: {other}")),
    }
}

async fn session_new(state: &State, params: &Value) -> Result<Value, String> {
    let cwd = params["cwd"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd = canonicalize_lenient(&cwd);
    let trusted = state.trust
        || TrustStore::load()
            .map(|store| store.is_trusted(&cwd))
            .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)
        .map(Arc::new)
        .unwrap_or_else(|_| state.config.clone());
    let auth = AuthStore::load().ok();
    let approver = Arc::new(AcpApprover {
        session_id: Arc::new(std::sync::Mutex::new(String::new())),
        out: state.out.clone(),
        pending: state.pending.clone(),
        next_id: state.next_id.clone(),
    });
    let handle = spawn_session(SessionParams {
        cwd: cwd.clone(),
        config,
        model: state.model.clone(),
        auth,
        yolo: state.yolo,
        trusted,
        resume: None,
        session_id: None,
        plan: false,
        mode: None,
        workflow: None,
        direct_lead: false,
        auto_approve_contracts: true,
        max_turns: None,
        system_prompt: None,
        append_system_prompt: None,
        allowed_tools: None,
        disallowed_tools: Vec::new(),
        approver: Some(approver.clone()),
    })
    .await
    .map_err(|error| error.to_string())?;

    let SessionHandle {
        id,
        meta,
        events,
        control,
        timeline: _,
        runner,
        role: _,
        lead_mode: _,
        contract: _,
        workflow: _,
        skills: _,
    } = handle;
    *approver
        .session_id
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = id.clone();

    let out = state.out.clone();
    let completions = state.completions.clone();
    let notification_session = id.clone();
    tokio::spawn(async move {
        let mut events = events;
        while let Some(event) = events.recv().await {
            forward_event(&out, &notification_session, event, &completions);
        }
    });

    state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id.clone(),
            SessionEntry {
                control,
                _meta: meta,
                _runner: runner,
            },
        );
    Ok(json!({ "sessionId": id }))
}

async fn session_prompt(state: &State, params: &Value) -> Result<Value, String> {
    let session_id = params["sessionId"]
        .as_str()
        .ok_or_else(|| "missing sessionId".to_string())?
        .to_string();
    let prompt = params["prompt"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err("empty prompt".into());
    }
    let control = state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&session_id)
        .map(|entry| entry.control.clone())
        .ok_or_else(|| format!("unknown session {session_id}"))?;
    let (tx, rx) = oneshot::channel();
    {
        let mut completions = state.completions.lock().unwrap_or_else(|e| e.into_inner());
        if completions.contains_key(&session_id) {
            return Err("session is busy".into());
        }
        completions.insert(session_id.clone(), tx);
    }
    control.prompt(prompt);
    match tokio::time::timeout(Duration::from_secs(60 * 60), rx).await {
        Ok(Ok(reason)) => Ok(json!({ "stopReason": stop_reason(reason) })),
        Ok(Err(_)) => Err("session ended before the turn completed".into()),
        Err(_) => {
            control.cancel();
            Ok(json!({ "stopReason": "cancelled" }))
        }
    }
}

fn stop_reason(reason: TurnEndReason) -> &'static str {
    match reason {
        TurnEndReason::Completed => "end_turn",
        TurnEndReason::Cancelled => "cancelled",
        TurnEndReason::MaxSteps => "max_turn_requests",
        TurnEndReason::Error | TurnEndReason::ContextFull | TurnEndReason::Blocked => "refusal",
    }
}

fn forward_event(
    out: &UnboundedSender<Value>,
    session_id: &str,
    event: kudelma_core::AgentEvent,
    completions: &Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<TurnEndReason>>>>,
) {
    use kudelma_core::AgentEvent;
    let update = match event {
        AgentEvent::TextDelta(text) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": text }
        })),
        AgentEvent::ThinkingDelta(text) => Some(json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": text }
        })),
        AgentEvent::ToolStart { id, name, summary } => Some(json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": if summary.is_empty() { name.clone() } else { format!("{name}: {summary}") },
            "kind": tool_kind(&name),
            "status": "in_progress"
        })),
        AgentEvent::ToolEnd {
            id,
            output,
            is_error,
            ..
        } => Some(json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if is_error { "failed" } else { "completed" },
            "content": [{ "type": "content", "content": { "type": "text", "text": output } }]
        })),
        AgentEvent::Info(text) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[info] {text}\n") }
        })),
        AgentEvent::Error(text) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[error] {text}\n") }
        })),
        AgentEvent::TaskUpdate {
            id, label, status, ..
        } => Some(json!({
            "sessionUpdate": if status == kudelma_core::TaskStatus::Running { "tool_call" } else { "tool_call_update" },
            "toolCallId": id,
            "title": label,
            "kind": "other",
            "status": match status {
                kudelma_core::TaskStatus::Running => "in_progress",
                kudelma_core::TaskStatus::Done => "completed",
                kudelma_core::TaskStatus::Failed => "failed",
            }
        })),
        AgentEvent::Usage {
            context_tokens,
            context_window,
            ..
        } => Some(json!({
            "sessionUpdate": "usage_update",
            "used": context_tokens,
            "size": context_window
        })),
        AgentEvent::PermissionRequest(_) => None,
        AgentEvent::TimelineReset(_) => None,
        AgentEvent::RoleChanged { role, model } => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[role] {role} ({model})\n") }
        })),
        AgentEvent::ModeChanged { from, to, reason } => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[mode] {from} -> {to}: {reason}\n") }
        })),
        AgentEvent::ContractUpdated(contract) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[contract {} v{}] {}\n", contract.contract_id, contract.contract_version, contract.status.name()) }
        })),
        AgentEvent::WorkflowUpdated(workflow) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[workflow] {} · {}\n", workflow.kind.name(), workflow.stage.name()) }
        })),
        AgentEvent::Verification(evidence) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[verify {}] {}\n", evidence.result.name(), evidence.command) }
        })),
        AgentEvent::ReviewUpdated(report) => Some(json!({
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": format!("\n[review {}] {}\n", report.status.name(), report.render()) }
        })),
        AgentEvent::TurnDone { reason, .. } => {
            let sender = completions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(session_id);
            if let Some(sender) = sender {
                let _ = sender.send(reason);
            }
            None
        }
    };
    if let Some(update) = update {
        let _ = out.send(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": { "sessionId": session_id, "update": update }
        }));
    }
}

fn tool_kind(name: &str) -> &'static str {
    match name {
        "bash" => "execute",
        "read" | "glob" | "grep" | "index" | "ls" => "read",
        "write" | "edit" => "edit",
        "webfetch" => "fetch",
        _ => "other",
    }
}

struct AcpApprover {
    session_id: Arc<std::sync::Mutex<String>>,
    out: UnboundedSender<Value>,
    pending: Pending,
    next_id: Arc<AtomicI64>,
}

#[async_trait]
impl Approver for AcpApprover {
    async fn ask(&self, ask: PermissionAsk) -> PermissionAnswer {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let session_id = self
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session_id,
                "toolCall": {
                    "toolCallId": ask.request_id,
                    "title": format!("{} ({})", ask.tool, ask.scopes.join("; ")),
                    "kind": tool_kind(&ask.tool),
                    "status": "pending"
                },
                "options": [
                    { "optionId": "allow_once", "name": "Allow once", "kind": "allow_once" },
                    { "optionId": "allow_session", "name": "Allow for session", "kind": "allow_always" },
                    { "optionId": "reject_once", "name": "Deny", "kind": "reject_once" }
                ]
            }
        });
        if self.out.send(message).is_err() {
            return PermissionAnswer::Deny;
        }
        let response = tokio::time::timeout(Duration::from_secs(600), rx).await;
        let option = match response {
            Ok(Ok(Ok(result))) => result["outcome"]["optionId"]
                .as_str()
                .unwrap_or("reject_once")
                .to_string(),
            _ => "reject_once".to_string(),
        };
        match option.as_str() {
            "allow_once" => PermissionAnswer::AllowOnce,
            "allow_session" => PermissionAnswer::AllowSession,
            _ => PermissionAnswer::Deny,
        }
    }
}
