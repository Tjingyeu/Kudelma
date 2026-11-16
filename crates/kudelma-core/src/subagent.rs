use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use kudelma_config::{Config, ModelTier, ToolKey};
use kudelma_permissions::PermissionManager;
use kudelma_providers::{
    ChatRequest, ContentBlock, Message, Model, ProviderEvent, all_models, provider_for,
};
use kudelma_storage::AuthStore;
use kudelma_tools::{
    TodoItem, Tool, ToolContext, ToolError, ToolOutput, ToolPermission, ToolRegistry,
    builtin_tools, object_schema,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::approver::Approver;
use crate::events::{AgentEvent, TaskStatus};
use crate::tool_exec::{ToolRun, execute_call};

pub struct TaskState {
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub state_dir: PathBuf,
    pub session_id: String,
    pub model: RwLock<Model>,
    pub auth: Option<AuthStore>,
    pub permissions: Arc<PermissionManager>,
    pub approver: Arc<dyn Approver>,
    pub events: UnboundedSender<AgentEvent>,
    pub cancel: kudelma_providers::CancelToken,
    pub read_times: Arc<RwLock<std::collections::BTreeMap<PathBuf, SystemTime>>>,
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    pub allowed_tools: Option<std::collections::HashSet<String>>,
    pub disallowed_tools: std::collections::HashSet<String>,
}

pub struct TaskTool {
    pub state: Arc<TaskState>,
}

impl TaskTool {
    pub fn new(state: Arc<TaskState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        "Delegate work to a subagent with its own context. Types: explore (read-only search), plan (read-only architecture plan), coder (implement a focused change). Only the final summary comes back."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "description": { "type": "string", "description": "Short label shown in the task list" },
                "prompt": { "type": "string", "description": "Complete, self-contained instructions for the subagent" },
                "subagent_type": { "type": "string", "enum": ["explore", "plan", "coder"], "description": "Default coder" },
                "model": { "type": "string", "enum": ["weak", "medium", "strong"], "description": "Optional model tier" }
            }),
            &["description", "prompt"],
        )
    }

    fn key(&self) -> ToolKey {
        ToolKey::native("task")
    }

    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let label = args
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("subagent");
        let _ = ctx;
        Ok(ToolPermission::new(vec![
            kudelma_permissions::Scope::exact(format!("task:{label}")),
        ]))
    }

    fn summary(&self, args: &Value) -> String {
        args.get("description")
            .and_then(Value::as_str)
            .unwrap_or("subagent")
            .chars()
            .take(80)
            .collect()
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let state = &self.state;
        let description = args
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("subagent")
            .to_string();
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidArgs("missing `prompt`".into()))?
            .to_string();
        let kind = args
            .get("subagent_type")
            .and_then(Value::as_str)
            .unwrap_or("coder")
            .to_string();
        let tier = args
            .get("model")
            .and_then(Value::as_str)
            .and_then(|tier| match tier {
                "weak" => Some(ModelTier::Weak),
                "medium" => Some(ModelTier::Medium),
                "strong" => Some(ModelTier::Strong),
                _ => None,
            });
        let task_id = uuid::Uuid::new_v4().to_string();
        let label = format!("{kind}: {description}");
        let _ = state.events.send(AgentEvent::TaskUpdate {
            id: task_id.clone(),
            label: label.clone(),
            status: TaskStatus::Running,
            detail: None,
        });

        let model = select_model(state, tier);
        let registry = registry_for(&kind, state.allowed_tools.as_ref(), &state.disallowed_tools);
        let system = crate::prompt::build_subagent_prompt(&state.cwd, &kind);
        let result = run_loop(state, model, &registry, system, prompt, ctx, &label).await;

        match result {
            Ok(summary) => {
                let _ = state.events.send(AgentEvent::TaskUpdate {
                    id: task_id,
                    label,
                    status: TaskStatus::Done,
                    detail: Some(summary.chars().take(200).collect()),
                });
                Ok(ToolOutput::success(summary))
            }
            Err(error) => {
                let _ = state.events.send(AgentEvent::TaskUpdate {
                    id: task_id,
                    label,
                    status: TaskStatus::Failed,
                    detail: Some(error.clone()),
                });
                Ok(ToolOutput::error(error))
            }
        }
    }
}

fn select_model(state: &TaskState, tier: Option<ModelTier>) -> Model {
    let current = state
        .model
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(tier) = tier else {
        return current;
    };
    all_models(&state.config, state.auth.as_ref())
        .into_iter()
        .find(|model| {
            model.provider == current.provider && model.tier == tier && model.supports_tools
        })
        .unwrap_or(current)
}

fn registry_for(
    kind: &str,
    allowed: Option<&std::collections::HashSet<String>>,
    disallowed: &std::collections::HashSet<String>,
) -> ToolRegistry {
    let registry = ToolRegistry::new();
    let permitted: &[&str] = if kind == "coder" {
        &[
            "read",
            "write",
            "edit",
            "glob",
            "grep",
            "ls",
            "index",
            "bash",
            "todo_write",
            "memory",
            "webfetch",
        ]
    } else {
        &["read", "glob", "grep", "ls", "index", "webfetch"]
    };
    for tool in builtin_tools() {
        if !permitted.contains(&tool.name()) || disallowed.contains(tool.name()) {
            continue;
        }
        if let Some(allowed) = allowed
            && !allowed.contains(tool.name())
        {
            continue;
        }
        registry.register(tool);
    }
    registry
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    state: &Arc<TaskState>,
    model: Model,
    registry: &ToolRegistry,
    system: String,
    prompt: String,
    parent_ctx: &ToolContext,
    label: &str,
) -> Result<String, String> {
    let mut history = vec![Message::user(prompt)];
    let mut last_text = String::new();
    for _step in 0..25 {
        if state.cancel.is_cancelled() {
            return if last_text.is_empty() {
                Err("subagent cancelled".into())
            } else {
                Ok(format!("{last_text}\n[cancelled]"))
            };
        }
        let request = ChatRequest {
            messages: history.clone(),
            system: Some(system.clone()),
            tools: registry.definitions(),
            max_tokens: 8192.min(model.max_output_tokens),
            temperature: None,
            thinking: None,
        };
        let provider = provider_for(&model);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = state.cancel.clone();
        let task = tokio::spawn(async move { provider.stream(request, tx, cancel).await });
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                ProviderEvent::TextDelta(delta) => text.push_str(&delta),
                ProviderEvent::ToolUse(call) => tool_calls.push(call),
                _ => {}
            }
        }
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return if text.is_empty() {
                    Err(format!("subagent model error: {error}"))
                } else {
                    Ok(format!("{text}\n[model error: {error}]"))
                };
            }
            Err(error) => return Err(format!("subagent task failed: {error}")),
        }
        let mut blocks = Vec::new();
        if !text.is_empty() {
            last_text = text.clone();
            blocks.push(ContentBlock::Text { text });
        }
        for call in &tool_calls {
            blocks.push(ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }
        if blocks.is_empty() {
            return Ok(last_text);
        }
        history.push(Message {
            role: kudelma_providers::Role::Assistant,
            content: blocks,
        });
        if tool_calls.is_empty() {
            return Ok(last_text);
        }

        let mut results = Vec::new();
        let ctx = ToolContext {
            env: parent_ctx.env.clone(),
            state_dir: state.state_dir.clone(),
            session_id: state.session_id.clone(),
            cancel: state.cancel.clone(),
            read_times: state.read_times.clone(),
            todos: state.todos.clone(),
            config: state.config.clone(),
            background: None,
        };
        for call in tool_calls {
            let result = execute_call(
                ToolRun {
                    registry,
                    permissions: &state.permissions,
                    approver: state.approver.as_ref(),
                    ctx: &ctx,
                    events: &state.events,
                    hooks: &state.config.hooks,
                    emit: false,
                },
                &call,
            )
            .await;
            results.push(result);
        }
        history.push(Message::tool_results(results));
    }
    let _ = label;
    Ok(format!(
        "{last_text}\n[subagent stopped after 25 steps; continue yourself if needed]"
    ))
}
