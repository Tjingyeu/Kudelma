#![allow(clippy::collapsible_if)]

//! Workspace environment abstraction and the built-in tool set.

pub mod builtin;
pub mod env;

pub use builtin::{
    BashTool, EditTool, GlobTool, GrepTool, IndexTool, LsTool, MemoryTool, ReadTool, SourceOutline,
    SymbolOutline, TodoWriteTool, ToolSearchTool, WebfetchTool, WriteTool, builtin_tools,
    outline_source, supports_source,
};
pub use env::{DirEntryInfo, LocalEnv, Meta, WorkspaceEnv};

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use kudelma_config::Config;
use kudelma_permissions::Scope;
use kudelma_providers::{CancelToken, ToolDefinition};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    InvalidArgs(String),
    #[error("path escapes the workspace: {0}")]
    OutsideWorkspace(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("timed out after {0}s")]
    Timeout(u64),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    pub metadata: Option<Value>,
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            metadata: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ToolPermission {
    pub scopes: Vec<Scope>,
    pub force_prompt: bool,
    pub reason: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    allowed: Option<HashSet<String>>,
    disallowed: HashSet<String>,
}

impl ToolPolicy {
    pub fn new(
        allowed: Option<impl IntoIterator<Item = String>>,
        disallowed: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            allowed: allowed.map(|items| items.into_iter().collect()),
            disallowed: disallowed.into_iter().collect(),
        }
    }

    pub fn permits(&self, name: &str) -> bool {
        if self.disallowed.contains(name) {
            return false;
        }
        self.allowed
            .as_ref()
            .is_none_or(|allowed| allowed.contains(name))
    }
}

impl ToolPermission {
    pub fn new(scopes: Vec<Scope>) -> Self {
        Self {
            scopes,
            force_prompt: false,
            reason: None,
            cwd: None,
        }
    }

    pub fn forced(scopes: Vec<Scope>, reason: impl Into<String>) -> Self {
        Self {
            scopes,
            force_prompt: true,
            reason: Some(reason.into()),
            cwd: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackgroundNotice {
    pub id: String,
    pub label: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone)]
pub struct ToolContext {
    pub env: Arc<dyn WorkspaceEnv>,
    pub state_dir: PathBuf,
    pub session_id: String,
    pub cancel: CancelToken,
    pub read_times: Arc<RwLock<BTreeMap<PathBuf, SystemTime>>>,
    pub todos: Arc<RwLock<Vec<TodoItem>>>,
    pub config: Arc<Config>,
    pub background: Option<UnboundedSender<BackgroundNotice>>,
}

impl ToolContext {
    pub fn new(
        env: Arc<dyn WorkspaceEnv>,
        state_dir: PathBuf,
        session_id: impl Into<String>,
        cancel: CancelToken,
        config: Arc<Config>,
    ) -> Self {
        Self {
            env,
            state_dir,
            session_id: session_id.into(),
            cancel,
            read_times: Arc::new(RwLock::new(BTreeMap::new())),
            todos: Arc::new(RwLock::new(Vec::new())),
            config,
            background: None,
        }
    }

    pub fn with_background(mut self, background: UnboundedSender<BackgroundNotice>) -> Self {
        self.background = Some(background);
        self
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.env.cwd()
    }

    pub fn read_time(&self, path: &std::path::Path) -> Option<SystemTime> {
        self.read_times
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .copied()
    }

    pub fn record_read(&self, path: PathBuf, modified: Option<SystemTime>) {
        if let Some(modified) = modified {
            self.read_times
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(path, modified);
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    fn key(&self) -> kudelma_config::ToolKey {
        kudelma_config::ToolKey::Native(self.name().to_string())
    }
    fn permission(&self, args: &Value, ctx: &ToolContext) -> Result<ToolPermission, ToolError>;
    fn read_only(&self) -> bool {
        false
    }
    fn summary(&self, args: &Value) -> String {
        args.get("path")
            .or_else(|| args.get("command"))
            .or_else(|| args.get("url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(80)
            .collect()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.schema(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry(Arc<RegistryInner>);

#[derive(Default)]
struct RegistryInner {
    tools: RwLock<BTreeMap<String, Arc<dyn Tool>>>,
    hidden: RwLock<HashSet<String>>,
    policy: RwLock<ToolPolicy>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.0
            .tools
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(tool.name().to_string(), tool);
    }

    pub fn register_all(&self, tools: Vec<Arc<dyn Tool>>) {
        for tool in tools {
            self.register(tool);
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if !self
            .0
            .policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .permits(name)
        {
            return None;
        }
        self.0
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    pub fn set_policy(&self, policy: ToolPolicy) {
        *self.0.policy.write().unwrap_or_else(|e| e.into_inner()) = policy;
    }

    pub fn policy(&self) -> ToolPolicy {
        self.0
            .policy
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn names(&self) -> Vec<String> {
        let policy = self.0.policy.read().unwrap_or_else(|e| e.into_inner());
        self.0
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|name| policy.permits(name))
            .cloned()
            .collect()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let hidden = self.0.hidden.read().unwrap_or_else(|e| e.into_inner());
        let policy = self.0.policy.read().unwrap_or_else(|e| e.into_inner());
        self.0
            .tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|tool| !hidden.contains(tool.name()))
            .filter(|tool| policy.permits(tool.name()))
            .map(|tool| tool.definition())
            .collect()
    }

    pub fn hide(&self, names: &[String]) {
        let mut hidden = self.0.hidden.write().unwrap_or_else(|e| e.into_inner());
        for name in names {
            hidden.insert(name.clone());
        }
    }

    pub fn reveal(&self, names: &[String]) {
        let mut hidden = self.0.hidden.write().unwrap_or_else(|e| e.into_inner());
        for name in names {
            hidden.remove(name);
        }
    }

    pub fn hidden_count(&self) -> usize {
        self.0
            .hidden
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    pub fn search(&self, query: &str) -> Vec<Arc<dyn Tool>> {
        let query = query.to_ascii_lowercase();
        let hidden = self.0.hidden.read().unwrap_or_else(|e| e.into_inner());
        let policy = self.0.policy.read().unwrap_or_else(|e| e.into_inner());
        let tools = self.0.tools.read().unwrap_or_else(|e| e.into_inner());
        tools
            .values()
            .filter(|tool| hidden.contains(tool.name()))
            .filter(|tool| policy.permits(tool.name()))
            .filter(|tool| {
                tool.name().to_ascii_lowercase().contains(&query)
                    || tool.description().to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown tool `{name}`")))?;
        tool.execute(args, ctx).await
    }

    pub fn permission(
        &self,
        name: &str,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<ToolPermission, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown tool `{name}`")))?;
        tool.permission(args, ctx)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[~]",
            Self::Completed => "[x]",
        }
    }
}

pub fn required_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolError::InvalidArgs(format!("missing string argument `{key}`")))
}

pub fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

pub fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Keep the head and the tail of long output. Errors and summaries usually sit
/// at the end, while the first lines carry context.
pub fn truncate_output(content: &str, max_bytes: usize, max_lines: usize) -> (String, bool) {
    let mut lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let mut dropped_lines = 0;
    if total_lines > max_lines {
        let head = max_lines * 2 / 5;
        let tail = max_lines - head;
        dropped_lines = total_lines - head - tail;
        let mut kept: Vec<&str> = lines[..head].to_vec();
        kept.push("[... output truncated ...]");
        kept.extend_from_slice(&lines[total_lines - tail..]);
        lines = kept;
    }
    let mut out = lines.join("\n");
    let mut dropped_bytes = 0;
    if out.len() > max_bytes {
        let head_bytes = max_bytes * 2 / 5;
        let tail_bytes = max_bytes - head_bytes;
        let mut head_end = head_bytes;
        while !out.is_char_boundary(head_end) {
            head_end -= 1;
        }
        let mut tail_start = out.len() - tail_bytes;
        while !out.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        dropped_bytes = tail_start - head_end;
        out = format!(
            "{}\n[... {} bytes truncated ...]\n{}",
            &out[..head_end],
            dropped_bytes,
            &out[tail_start..]
        );
    }
    (out, dropped_lines > 0 || dropped_bytes > 0)
}

pub fn clip_line(line: &str, max: usize) -> String {
    let width = line.chars().count();
    if width <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

pub fn display_path(cwd: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(cwd)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_head_and_tail() {
        let text = (0..100)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, truncated) = truncate_output(&text, 10_000, 10);
        assert!(truncated);
        assert!(out.contains("line 0"));
        assert!(out.contains("line 99"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn object_schema_marks_required_fields() {
        let schema = object_schema(json!({ "path": { "type": "string" } }), &["path"]);
        assert_eq!(schema["required"][0], "path");
    }

    #[test]
    fn tool_policy_filters_schema_and_execution() {
        let registry = ToolRegistry::new();
        registry.register_all(builtin_tools());
        registry.set_policy(ToolPolicy::new(
            Some(vec!["read".to_string()]),
            Vec::<String>::new(),
        ));
        assert!(registry.get("read").is_some());
        assert!(registry.get("bash").is_none());
        assert!(
            registry
                .definitions()
                .iter()
                .all(|tool| tool.name == "read")
        );
        assert!(matches!(
            registry.permission(
                "bash",
                &json!({ "command": "true" }),
                &ToolContext::new(
                    Arc::new(LocalEnv::new(std::path::PathBuf::from("."))),
                    std::path::PathBuf::from("."),
                    "test",
                    kudelma_providers::CancelToken::new(),
                    Arc::new(Config::default()),
                )
            ),
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[test]
    fn tool_policy_blocks_deferred_search() {
        let registry = ToolRegistry::new();
        registry.register_all(builtin_tools());
        registry.hide(&["bash".to_string()]);
        registry.set_policy(ToolPolicy::new(
            None::<Vec<String>>,
            vec!["bash".to_string()],
        ));
        assert!(registry.search("bash").is_empty());
    }
}
