use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, bail};
use kudelma_config::Config;
use kudelma_core::{
    AgentEvent, AutoApprover, SessionHandle, SessionParams, TurnEndReason, spawn_session,
};
use kudelma_providers::{Model, resolve_model};
use kudelma_storage::{AuthStore, StoredSession};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cli::{InputFormat, OutputFormat};

pub struct HeadlessOptions {
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub auth: Option<AuthStore>,
    pub model: Option<String>,
    pub yolo: bool,
    pub trusted: bool,
    pub resume: Option<StoredSession>,
    pub plan: bool,
    pub mode: Option<String>,
    pub workflow: Option<kudelma_config::WorkflowPreset>,
    pub direct_lead: bool,
    pub max_turns: Option<u32>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub output: OutputFormat,
    pub input: InputFormat,
    pub verbose: bool,
    pub prompt: Option<String>,
    pub session_id: Option<String>,
}

struct TurnResult {
    reason: TurnEndReason,
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    context_tokens: u64,
    context_window: u32,
    cost_usd: f64,
}

pub async fn run(options: HeadlessOptions) -> Result<()> {
    let model_spec = options
        .model
        .clone()
        .or_else(|| {
            options
                .resume
                .as_ref()
                .map(|session| session.meta.model.clone())
        })
        .unwrap_or_else(|| options.config.default_model.clone());
    let model = resolve_model(&options.config, options.auth.as_ref(), &model_spec)?;
    check_credentials(&model)?;

    if !options.yolo {
        eprintln!(
            "note: headless mode denies permission prompts that are not pre-approved; use --yolo or --allowed-tools"
        );
    }

    let mut handle = spawn_session(SessionParams {
        cwd: options.cwd.clone(),
        config: options.config.clone(),
        model: Some(model_spec.clone()),
        auth: options.auth.clone(),
        yolo: options.yolo,
        trusted: options.trusted,
        resume: options.resume.clone(),
        session_id: options.session_id.clone(),
        plan: options.plan,
        mode: options.mode.clone(),
        workflow: options.workflow,
        direct_lead: options.direct_lead,
        auto_approve_contracts: true,
        max_turns: options.max_turns,
        system_prompt: options.system_prompt.clone(),
        append_system_prompt: options.append_system_prompt.clone(),
        allowed_tools: options.allowed_tools.clone(),
        disallowed_tools: options.disallowed_tools.clone(),
        approver: Some(Arc::new(AutoApprover {
            allow: options.yolo,
        })),
    })
    .await?;

    if options.output == OutputFormat::StreamJson {
        println!(
            "{}",
            json!({
                "type": "system",
                "subtype": "init",
                "session_id": handle.id,
                "model": model_spec,
                "cwd": options.cwd,
            })
        );
        let _ = std::io::stdout().flush();
    }

    let mut events =
        std::mem::replace(&mut handle.events, tokio::sync::mpsc::unbounded_channel().1);

    match options.input {
        InputFormat::Text => {
            let prompt = match options.prompt.clone() {
                Some(prompt) => prompt,
                None => {
                    if std::io::stdin().is_terminal() {
                        bail!("no prompt given; pass a prompt or pipe one on stdin")
                    }
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
                    text
                }
            };
            if prompt.trim().is_empty() {
                bail!("empty prompt");
            }
            let result = run_turn(&handle, &mut events, &prompt, &options).await?;
            finish(&options, &handle, &result)?;
        }
        InputFormat::StreamJson => {
            let stdin = tokio::io::stdin();
            let mut lines = BufReader::new(stdin).lines();
            let mut turns = 0;
            while let Some(line) = lines.next_line().await? {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let text = match serde_json::from_str::<serde_json::Value>(line) {
                    Ok(value) => extract_prompt(&value),
                    Err(_) => Some(line.to_string()),
                };
                let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
                    continue;
                };
                turns += 1;
                let result = run_turn(&handle, &mut events, &text, &options).await?;
                if options.output != OutputFormat::StreamJson {
                    finish(&options, &handle, &result)?;
                } else {
                    print_result(&options, &handle, &result)?;
                }
                if matches!(
                    result.reason,
                    TurnEndReason::Error | TurnEndReason::ContextFull
                ) {
                    break;
                }
            }
            if turns == 0 {
                bail!("no user messages on stdin");
            }
        }
    }

    handle.close().await;
    Ok(())
}

fn extract_prompt(value: &serde_json::Value) -> Option<String> {
    if let Some(content) = value["message"]["content"].as_array() {
        let text = content
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return Some(text);
        }
    }
    if let Some(content) = value["content"].as_array() {
        let text = content
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return Some(text);
        }
    }
    value["text"]
        .as_str()
        .or_else(|| value["prompt"].as_str())
        .map(str::to_string)
}

async fn run_turn(
    handle: &SessionHandle,
    events: &mut UnboundedReceiver<AgentEvent>,
    prompt: &str,
    options: &HeadlessOptions,
) -> Result<TurnResult> {
    handle.control.prompt(prompt);
    let mut text = String::new();
    let mut input_tokens = 0;
    let mut output_tokens = 0;
    let mut context_tokens = 0;
    let mut context_window = 0;
    let mut cost_usd = 0.0;
    loop {
        let Some(event) = events.recv().await else {
            bail!("agent session closed unexpectedly");
        };
        match event {
            AgentEvent::TextDelta(delta) => {
                text.push_str(&delta);
                if options.output == OutputFormat::Text {
                    print!("{delta}");
                    let _ = std::io::stdout().flush();
                } else if options.output == OutputFormat::StreamJson {
                    println!("{}", json!({ "type": "assistant_delta", "text": delta }));
                }
            }
            AgentEvent::ThinkingDelta(delta) => {
                if options.output == OutputFormat::StreamJson {
                    println!("{}", json!({ "type": "thinking_delta", "text": delta }));
                }
            }
            AgentEvent::ToolStart { id, name, summary } => {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                    if options.output == OutputFormat::Text {
                        println!();
                    }
                }
                if options.verbose {
                    eprintln!("\n[tool] {name} {summary}");
                }
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({ "type": "tool_start", "id": id, "name": name, "summary": summary })
                    );
                }
            }
            AgentEvent::ToolEnd {
                id, name, is_error, ..
            } => {
                if options.verbose {
                    eprintln!("[{}] {name}", if is_error { "error" } else { "done" });
                }
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({ "type": "tool_end", "id": id, "name": name, "is_error": is_error })
                    );
                }
            }
            AgentEvent::Info(message) => {
                if options.output == OutputFormat::StreamJson {
                    println!("{}", json!({ "type": "info", "message": message }));
                } else if options.verbose {
                    eprintln!("[info] {message}");
                }
            }
            AgentEvent::Error(message) => {
                if options.output == OutputFormat::StreamJson {
                    println!("{}", json!({ "type": "error", "message": message }));
                } else {
                    eprintln!("\n\x1b[31m✖ {message}\x1b[0m");
                }
            }
            AgentEvent::Usage {
                input_tokens: input,
                output_tokens: output,
                context_tokens: context,
                context_window: window,
                total_cost_usd,
                ..
            } => {
                input_tokens = input;
                output_tokens = output;
                context_tokens = context;
                context_window = window;
                cost_usd = total_cost_usd;
            }
            AgentEvent::PermissionRequest(ask) => {
                eprintln!(
                    "\n\x1b[33mpermission denied: {} ({}) — rerun with --yolo to allow\x1b[0m",
                    ask.tool,
                    ask.scopes.join("; ")
                );
                handle.control.respond_permission(
                    &ask.request_id,
                    kudelma_permissions::PermissionAnswer::Deny,
                );
            }
            AgentEvent::TaskUpdate { .. } => {}
            AgentEvent::RoleChanged { role, model } => {
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({ "type": "role_changed", "role": role, "model": model })
                    );
                } else if options.verbose {
                    eprintln!("[role] {role} ({model})");
                }
            }
            AgentEvent::ModeChanged { from, to, reason } => {
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({ "type": "mode_changed", "from": from, "to": to, "reason": reason })
                    );
                } else if options.verbose {
                    eprintln!("[mode] {from} -> {to}: {reason}");
                }
            }
            AgentEvent::ContractUpdated(contract) => {
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({
                            "type": "contract",
                            "id": contract.contract_id,
                            "version": contract.contract_version,
                            "status": contract.status.name(),
                        })
                    );
                } else if options.verbose {
                    eprintln!(
                        "[contract] {} v{} {}",
                        contract.contract_id,
                        contract.contract_version,
                        contract.status.name()
                    );
                }
            }
            AgentEvent::WorkflowUpdated(workflow) => {
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({
                            "type": "workflow",
                            "id": workflow.id,
                            "kind": workflow.kind.name(),
                            "stage": workflow.stage.name(),
                            "mode": workflow.mode,
                        })
                    );
                }
            }
            AgentEvent::Verification(evidence) => {
                if options.output == OutputFormat::StreamJson {
                    println!(
                        "{}",
                        json!({
                            "type": "verification",
                            "id": evidence.id,
                            "result": evidence.result.name(),
                            "command": evidence.command,
                            "exit_code": evidence.exit_code,
                        })
                    );
                } else if options.verbose {
                    eprintln!("[verify {}] {}", evidence.result.name(), evidence.command);
                }
            }
            AgentEvent::TimelineReset(_) => {}
            AgentEvent::TurnDone { reason, .. } => {
                if options.output == OutputFormat::Text && !text.is_empty() {
                    println!();
                }
                return Ok(TurnResult {
                    reason,
                    text,
                    input_tokens,
                    output_tokens,
                    context_tokens,
                    context_window,
                    cost_usd,
                });
            }
        }
    }
}

fn finish(options: &HeadlessOptions, handle: &SessionHandle, result: &TurnResult) -> Result<()> {
    if options.output != OutputFormat::Text {
        print_result(options, handle, result)?;
    }
    if matches!(
        result.reason,
        TurnEndReason::Error | TurnEndReason::ContextFull | TurnEndReason::Blocked
    ) {
        bail!("agent turn failed");
    }
    Ok(())
}

fn print_result(
    options: &HeadlessOptions,
    handle: &SessionHandle,
    result: &TurnResult,
) -> Result<()> {
    if options.output == OutputFormat::Text {
        return Ok(());
    }
    let value = json!({
        "type": "result",
        "subtype": match result.reason {
            TurnEndReason::Completed => "success",
            TurnEndReason::Cancelled => "cancelled",
            TurnEndReason::MaxSteps => "max_turns",
            TurnEndReason::Error | TurnEndReason::ContextFull | TurnEndReason::Blocked => "error",
        },
        "result": result.text,
        "session_id": handle.id,
        "usage": {
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "context_tokens": result.context_tokens,
            "context_window": result.context_window,
        },
        "total_cost_usd": (result.cost_usd * 1_000_000.0).round() / 1_000_000.0,
        "is_error": matches!(result.reason, TurnEndReason::Error | TurnEndReason::ContextFull | TurnEndReason::Blocked),
    });
    println!("{value}");
    let _ = std::io::stdout().flush();
    Ok(())
}

pub fn check_credentials(model: &Model) -> Result<()> {
    if model.api_key.is_some() {
        return Ok(());
    }
    let local = model.base_url.contains("localhost")
        || model.base_url.contains("127.0.0.1")
        || model.base_url.contains("0.0.0.0");
    if local {
        return Ok(());
    }
    bail!(
        "no API key for {}. Set {} or run `kudelma auth login {}`",
        model.provider,
        model.auth_env,
        model.provider
    )
}
