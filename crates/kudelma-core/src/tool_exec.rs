use kudelma_config::{HooksConfig, ToolKey};
use kudelma_permissions::{PermissionAnswer, PermissionCheck, PermissionManager};
use kudelma_providers::{ToolCall, ToolResult};
use kudelma_tools::{ToolContext, ToolOutput, ToolRegistry};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::approver::Approver;
use crate::events::{AgentEvent, PermissionAsk};
use crate::hooks;

pub struct ToolRun<'a> {
    pub registry: &'a ToolRegistry,
    pub permissions: &'a PermissionManager,
    pub approver: &'a dyn Approver,
    pub ctx: &'a ToolContext,
    pub events: &'a UnboundedSender<AgentEvent>,
    pub hooks: &'a HooksConfig,
    pub emit: bool,
}

pub async fn execute_call(run: ToolRun<'_>, call: &ToolCall) -> ToolResult {
    let Some(tool) = run.registry.get(&call.name) else {
        let result = tool_result(call, format!("Unknown tool `{}`.", call.name), true, None);
        if run.emit {
            let _ = run.events.send(AgentEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
                summary: String::new(),
            });
            let _ = run.events.send(AgentEvent::ToolEnd {
                id: call.id.clone(),
                name: call.name.clone(),
                output: clip(&result.content, 8000),
                is_error: true,
            });
        }
        return result;
    };
    let summary = tool.summary(&call.input);
    if run.emit {
        let _ = run.events.send(AgentEvent::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            summary: summary.clone(),
        });
    }
    let permission = match tool.permission(&call.input, run.ctx) {
        Ok(permission) => permission,
        Err(error) => {
            return fail(&run, call, format!("Invalid arguments: {error}"));
        }
    };

    if let Err(error) = check_permission(&run, call, tool.key(), tool.name(), &permission).await {
        return fail(&run, call, error);
    }

    let hook = hooks::run_pre_tool(
        run.hooks,
        &call.name,
        &call.input,
        run.ctx.cwd(),
        &run.ctx.cancel,
    )
    .await;
    if let Some(message) = &hook.message {
        let _ = run.events.send(AgentEvent::Info(message.clone()));
    }
    if !hook.allowed {
        return fail(
            &run,
            call,
            format!(
                "Blocked by the pre_tool_use hook: {}",
                hook.message.unwrap_or_default()
            ),
        );
    }

    let output = match tool.execute(call.input.clone(), run.ctx).await {
        Ok(output) => output,
        Err(error) => ToolOutput::error(error.to_string()),
    };

    hooks::run_post_tool(
        run.hooks,
        &call.name,
        &call.input,
        &output.content,
        run.ctx.cwd(),
        &run.ctx.cancel,
    )
    .await;

    if run.emit {
        let _ = run.events.send(AgentEvent::ToolEnd {
            id: call.id.clone(),
            name: call.name.clone(),
            output: clip(&output.content, 8000),
            is_error: output.is_error,
        });
    }

    tool_result(call, output.content, output.is_error, output.metadata)
}

fn fail(run: &ToolRun<'_>, call: &ToolCall, content: String) -> ToolResult {
    if run.emit {
        let _ = run.events.send(AgentEvent::ToolEnd {
            id: call.id.clone(),
            name: call.name.clone(),
            output: clip(&content, 8000),
            is_error: true,
        });
    }
    tool_result(call, content, true, None)
}

async fn check_permission(
    run: &ToolRun<'_>,
    _call: &ToolCall,
    key: ToolKey,
    tool_name: &str,
    permission: &kudelma_tools::ToolPermission,
) -> Result<(), String> {
    let check = run
        .permissions
        .check_with_force(&key, &permission.scopes, permission.force_prompt);
    match check {
        PermissionCheck::Allowed => Ok(()),
        PermissionCheck::Denied(reason) => Err(format!(
            "Permission denied for `{tool_name}` ({}): {reason}. Do not retry; choose another approach or ask the user.",
            scope_text(&permission.scopes)
        )),
        PermissionCheck::NeedsPrompt { scopes, reason, .. } => {
            let request_id = uuid::Uuid::new_v4().to_string();
            let ask = PermissionAsk {
                request_id: request_id.clone(),
                tool: tool_name.to_string(),
                scopes: scopes.iter().map(|scope| scope.value.clone()).collect(),
                reason: permission.reason.clone().or(reason),
            };
            let answer = run.approver.ask(ask).await;
            if let Err(error) = run.permissions.apply_decision(&key, &scopes, &answer) {
                let _ = run
                    .events
                    .send(AgentEvent::Info(format!("could not persist rule: {error}")));
            }
            if answer.is_allow() {
                Ok(())
            } else {
                let guidance = answer
                    .guidance()
                    .map(|text| format!(" User guidance: {text}"))
                    .unwrap_or_default();
                Err(format!(
                    "Permission denied for `{tool_name}` ({}).{guidance} Do not retry; ask the user or choose another approach.",
                    scope_text(&permission.scopes)
                ))
            }
        }
    }
}

fn scope_text(scopes: &[kudelma_permissions::Scope]) -> String {
    let values: Vec<&str> = scopes.iter().map(|scope| scope.value.as_str()).collect();
    if values.is_empty() {
        "no scope".into()
    } else {
        values.join("; ")
    }
}

fn tool_result(
    call: &ToolCall,
    content: String,
    is_error: bool,
    metadata: Option<Value>,
) -> ToolResult {
    let content = if is_error {
        format!("Error: {content}")
    } else {
        content
    };
    let content = match metadata {
        Some(_) => content,
        None => content,
    };
    ToolResult {
        tool_use_id: call.id.clone(),
        content,
        is_error,
        name: Some(call.name.clone()),
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("\n[… output truncated in the UI; the model saw the full result]");
    out
}

#[allow(dead_code)]
fn _answer_is_allow(answer: &PermissionAnswer) -> bool {
    answer.is_allow()
}
