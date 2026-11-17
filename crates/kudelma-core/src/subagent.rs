use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use kudelma_config::{Config, ExecutionStrategy, ModelTier, ToolKey};
use kudelma_permissions::{AgentRole, CoderScope, PermissionManager};
use kudelma_providers::{
    ChatRequest, ContentBlock, Message, Model, ProviderEvent, all_models, provider_for,
};
use kudelma_storage::{ArtifactStore, AuthStore, now_rfc3339, tasks_dir};
use kudelma_tools::{
    TodoItem, Tool, ToolContext, ToolError, ToolOutput, ToolPermission, ToolRegistry,
    builtin_tools, object_schema,
};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::approver::Approver;
use crate::events::{AgentEvent, TaskStatus};
use crate::orchestration::{WorkerReport, WorkerStatus, WorkerTask};
use crate::runtime::RuntimeState;
use crate::tool_exec::{ToolRun, execute_call};

pub struct TaskState {
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub state_dir: PathBuf,
    pub session_id: String,
    pub model: RwLock<Model>,
    pub auth: RwLock<Option<AuthStore>>,
    pub permissions: Arc<PermissionManager>,
    pub approver: Arc<dyn Approver>,
    pub events: UnboundedSender<AgentEvent>,
    pub cancel: kudelma_providers::CancelToken,
    pub read_times: Arc<RwLock<std::collections::BTreeMap<PathBuf, SystemTime>>>,
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    pub allowed_tools: Option<std::collections::HashSet<String>>,
    pub disallowed_tools: std::collections::HashSet<String>,
    pub trusted: bool,
    pub yolo: bool,
    pub runtime: Arc<RuntimeState>,
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
        "Delegate bounded implementation to a Coder with its own context. Only the final Worker Report comes back. Use `explore_codebase`, `index`, `grep` or `read` for read-only investigation."
    }

    fn schema(&self) -> Value {
        object_schema(
            json!({
                "description": { "type": "string", "description": "Short label shown in the task list" },
                "prompt": { "type": "string", "description": "Complete, self-contained implementation instructions for the Coder" },
                "subagent_type": { "type": "string", "enum": ["coder"], "description": "Only coder is supported" },
                "model": { "type": "string", "enum": ["weak", "medium", "strong"], "description": "Optional model tier" },
                "allowed_paths": { "type": "array", "items": { "type": "string" }, "description": "Coder write scope" },
                "forbidden_paths": { "type": "array", "items": { "type": "string" }, "description": "Paths the coder must not modify" }
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
        if matches!(
            state.config.workflow.execution.strategy,
            ExecutionStrategy::Inline
        ) {
            return Err(ToolError::Other(
                "workflow.execution.strategy=inline disables Coder dispatch".into(),
            ));
        }
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
        if kind == "plan" {
            return Err(ToolError::InvalidArgs(
                "the plan subagent was retired; use the lead_mode tool with mode=plan instead"
                    .into(),
            ));
        }
        if kind == "explore" {
            return Err(ToolError::InvalidArgs(
                "the standalone explore subagent was retired in the three-model architecture; use the `explore_codebase` tool for shared deterministic read-only exploration, or `index`/`grep`/`read` directly"
                    .into(),
            ));
        }
        if kind != "coder" {
            return Err(ToolError::InvalidArgs(format!(
                "unknown subagent_type `{kind}`; only coder is supported"
            )));
        }
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
        state.runtime.begin_worker().map_err(ToolError::Other)?;
        let task_store =
            ArtifactStore::new(tasks_dir().map_err(|error| ToolError::Other(error.to_string()))?);
        let mut worker_task = WorkerTask::new(
            state
                .runtime
                .contract()
                .map(|contract| contract.contract_id)
                .unwrap_or_else(|| "none".into()),
            prompt.clone(),
        );
        worker_task.task_id = task_id.clone();
        worker_task.requirements.push(description.clone());
        worker_task.allowed_paths = string_paths(&args, "allowed_paths", &state.cwd)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect();
        worker_task.forbidden_paths = string_paths(&args, "forbidden_paths", &state.cwd)
            .into_iter()
            .map(|path| path.display().to_string())
            .collect();
        let _ = task_store.save(&format!("task-{task_id}"), &worker_task);
        let _ = state.events.send(AgentEvent::TaskUpdate {
            id: task_id.clone(),
            label: label.clone(),
            status: TaskStatus::Running,
            detail: None,
        });

        let started_at = now_rfc3339();
        let workspace_before = state.runtime.current_revision().await;
        let modified = Arc::new(Mutex::new(BTreeSet::new()));

        let current = state
            .model
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let base = state
            .runtime
            .model_for("coder", &current)
            .unwrap_or(current);
        let model = select_model(state, &base, tier);
        let registry = registry_for(
            state.allowed_tools.as_ref(),
            &state.disallowed_tools,
            state.config.agents.coder.allow_shell,
        );
        let system = format!(
            "{}\n{}",
            crate::prompt::build_subagent_prompt(&state.cwd),
            state.runtime.skills.prompt_section("coder", "build")
        );
        let allowed_paths = string_paths(&args, "allowed_paths", &state.cwd);
        let allowed_paths = if allowed_paths.is_empty() {
            vec![state.cwd.clone()]
        } else {
            allowed_paths
        };
        let scope = CoderScope {
            allowed_paths,
            forbidden_paths: string_paths(&args, "forbidden_paths", &state.cwd),
            allow_shell: state.config.agents.coder.allow_shell,
        };
        let role = AgentRole::Coder;
        let permissions =
            PermissionManager::new(&state.config, &state.cwd, state.trusted, state.yolo)
                .for_role(role, scope);
        registry.register(Arc::new(crate::runtime_tools::SkillTool {
            runtime: state.runtime.clone(),
            role,
        }));
        let result = run_loop(
            state,
            model,
            &registry,
            &permissions,
            "coder",
            system,
            prompt,
            ctx,
            &label,
            state.config.agents.coder.turns(),
            &format!("{}:coder:{}", state.session_id, task_id),
            &modified,
        )
        .await;

        let ended_at = now_rfc3339();
        let workspace_after = state.runtime.current_revision().await;
        let modified_files: Vec<String> = modified
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .cloned()
            .collect();
        match result {
            Ok(summary) => {
                state.runtime.invalidate_verification();
                if state.config.workflow.execution.review_after_each_task {
                    state.runtime.require_review_after_worker();
                }
                state.runtime.finish_worker(true);
                let report = WorkerReport {
                    task_id: task_id.clone(),
                    status: WorkerStatus::Completed,
                    summary: summary.clone(),
                    modified_files: modified_files.clone(),
                    started_at: Some(started_at),
                    ended_at: Some(ended_at),
                    workspace_before: Some(workspace_before),
                    workspace_after: Some(workspace_after),
                    tests: Vec::new(),
                    assumptions: Vec::new(),
                    unresolved_issues: Vec::new(),
                    evidence_refs: Vec::new(),
                };
                let _ = task_store.save(&format!("report-{task_id}"), &report);
                let _ = state.events.send(AgentEvent::TaskUpdate {
                    id: task_id.clone(),
                    label,
                    status: TaskStatus::Done,
                    detail: Some(summary.chars().take(200).collect()),
                });
                Ok(ToolOutput::success(format!(
                    "{summary}\n\n[task id: {task_id}]"
                )))
            }
            Err(error) => {
                state.runtime.finish_worker(false);
                let status = if error.contains("blocked by budget") {
                    WorkerStatus::Blocked
                } else if error.contains("cancelled") {
                    WorkerStatus::Cancelled
                } else {
                    WorkerStatus::Failed
                };
                let report = WorkerReport {
                    task_id: task_id.clone(),
                    status,
                    summary: error.clone(),
                    modified_files,
                    started_at: Some(started_at),
                    ended_at: Some(ended_at),
                    workspace_before: Some(workspace_before),
                    workspace_after: Some(workspace_after),
                    tests: Vec::new(),
                    assumptions: Vec::new(),
                    unresolved_issues: vec![error.clone()],
                    evidence_refs: Vec::new(),
                };
                let _ = task_store.save(&format!("report-{task_id}"), &report);
                let _ = state.events.send(AgentEvent::TaskUpdate {
                    id: task_id.clone(),
                    label,
                    status: TaskStatus::Failed,
                    detail: Some(error.clone()),
                });
                Ok(ToolOutput::error(format!(
                    "{error}\n\n[task id: {task_id}]"
                )))
            }
        }
    }
}

fn string_paths(args: &Value, key: &str, base: &std::path::Path) -> Vec<PathBuf> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|path| {
                    let path = PathBuf::from(path);
                    if path.is_absolute() {
                        path
                    } else {
                        base.join(path)
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn select_model(state: &TaskState, current: &Model, tier: Option<ModelTier>) -> Model {
    let Some(tier) = tier else {
        return current.clone();
    };
    all_models(
        &state.config,
        state
            .auth
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref(),
    )
    .into_iter()
    .find(|model| model.provider == current.provider && model.tier == tier && model.supports_tools)
    .unwrap_or_else(|| current.clone())
}

fn registry_for(
    allowed: Option<&std::collections::HashSet<String>>,
    disallowed: &std::collections::HashSet<String>,
    allow_shell: bool,
) -> ToolRegistry {
    let registry = ToolRegistry::new();
    let mut permitted = vec![
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "ls",
        "index",
        "todo_write",
        "memory",
        "webfetch",
    ];
    if allow_shell {
        permitted.push("bash");
    }
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
    registry.set_policy(kudelma_tools::ToolPolicy::new(
        allowed.cloned().map(|set| set.into_iter()),
        disallowed.iter().cloned(),
    ));
    registry
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    state: &Arc<TaskState>,
    model: Model,
    registry: &ToolRegistry,
    permissions: &PermissionManager,
    role: &str,
    system: String,
    prompt: String,
    parent_ctx: &ToolContext,
    label: &str,
    max_turns: u32,
    session_id: &str,
    modified: &Arc<Mutex<BTreeSet<String>>>,
) -> Result<String, String> {
    let mut history = vec![Message::user(prompt)];
    let mut last_text = String::new();
    for _step in 0..max_turns {
        if state.cancel.is_cancelled() {
            return if last_text.is_empty() {
                Err("subagent cancelled".into())
            } else {
                Ok(format!("{last_text}\n[cancelled]"))
            };
        }
        if let Some(budget) = state.runtime.budget_blocker() {
            return Err(format!("blocked by budget: {budget}"));
        }
        let request = ChatRequest {
            messages: history.clone(),
            system: Some(system.clone()),
            tools: registry.definitions(),
            max_tokens: 8192.min(model.max_output_tokens),
            temperature: None,
            thinking: None,
            session_id: Some(session_id.to_string()),
        };
        let provider = provider_for(&model);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cancel = state.cancel.clone();
        let task = tokio::spawn(async move { provider.stream(request, tx, cancel).await });
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = kudelma_providers::TokenUsage::default();
        while let Some(event) = rx.recv().await {
            match event {
                ProviderEvent::TextDelta(delta) => text.push_str(&delta),
                ProviderEvent::ToolUse(call) => tool_calls.push(call),
                ProviderEvent::Usage(value) => usage = value,
                ProviderEvent::ThinkingDelta(_) => {}
            }
        }
        state.runtime.record_turn(
            role,
            usage.input_tokens,
            usage.output_tokens,
            model.cost(usage),
        );
        if let Some(budget) = state.runtime.budget_blocker() {
            return Err(format!("blocked by budget: {budget}"));
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
                    permissions,
                    approver: state.approver.as_ref(),
                    ctx: &ctx,
                    events: &state.events,
                    hooks: &state.config.hooks,
                    emit: false,
                },
                &call,
            )
            .await;
            if !result.is_error
                && kudelma_permissions::WRITE_TOOLS.contains(&call.name.as_str())
                && let Some(path) = call.input.get("path").and_then(Value::as_str)
            {
                modified
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(path.to_string());
            }
            results.push(result);
        }
        history.push(Message::tool_results(results));
    }
    let _ = label;
    Ok(format!(
        "{last_text}\n[subagent stopped after {max_turns} steps; continue yourself if needed]"
    ))
}
