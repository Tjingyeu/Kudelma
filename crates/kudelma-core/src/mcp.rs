use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kudelma_config::{Config, McpServerConfig, ToolKey};
use kudelma_permissions::Scope;
use kudelma_tools::{Tool, ToolContext, ToolError, ToolOutput, ToolPermission, object_schema};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::oneshot;

use crate::events::AgentEvent;

const REQUEST_TIMEOUT_SECS: u64 = 30;
const PROTOCOL_VERSION: &str = "2024-11-05";

type McpResponse = Result<Value, String>;
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<McpResponse>>>>;

pub struct McpManager {
    clients: Vec<Arc<McpClient>>,
}

impl McpManager {
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }
}

pub struct McpLoad {
    pub manager: McpManager,
    pub tools: Vec<Arc<dyn Tool>>,
    pub warnings: Vec<String>,
}

enum McpClient {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

struct StdioTransport {
    server: String,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: PendingMap,
    next_id: AtomicU64,
    _child: Child,
}

struct HttpTransport {
    server: String,
    url: String,
    client: reqwest::Client,
    next_id: AtomicU64,
    session_id: Mutex<Option<String>>,
}

struct McpToolInfo {
    name: String,
    description: String,
    schema: Value,
}

impl McpClient {
    fn server(&self) -> &str {
        match self {
            Self::Stdio(transport) => &transport.server,
            Self::Http(transport) => &transport.server,
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        match self {
            Self::Stdio(transport) => transport.request(method, params).await,
            Self::Http(transport) => transport.request(method, params).await,
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        match self {
            Self::Stdio(transport) => transport.notify(method, params).await,
            Self::Http(transport) => transport.notify(method, params).await,
        }
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("tools/list", params).await?;
            if let Some(items) = result["tools"].as_array() {
                for tool in items {
                    let Some(name) = tool["name"].as_str() else {
                        continue;
                    };
                    tools.push(McpToolInfo {
                        name: name.to_string(),
                        description: tool["description"]
                            .as_str()
                            .unwrap_or("MCP tool")
                            .to_string(),
                        schema: tool
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or_else(|| object_schema(json!({}), &[])),
                    });
                }
            }
            let next = result["nextCursor"].as_str().map(str::to_string);
            match next {
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                _ => break,
            }
        }
        Ok(tools)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let mut text = String::new();
        if let Some(content) = result["content"].as_array() {
            for block in content {
                match block["type"].as_str().unwrap_or("text") {
                    "text" => {
                        if let Some(value) = block["text"].as_str() {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(value);
                        }
                    }
                    "image" => text.push_str("[image content]"),
                    "resource" => {
                        if let Some(uri) = block["resource"]["uri"].as_str() {
                            text.push_str(&format!("[resource {uri}]"));
                        }
                    }
                    other => text.push_str(&format!("[{other} content]")),
                }
            }
        }
        if text.is_empty() {
            text = serde_json::to_string_pretty(&result).unwrap_or_default();
        }
        if result["isError"].as_bool().unwrap_or(false) {
            return Err(text);
        }
        Ok(text)
    }
}

impl StdioTransport {
    async fn spawn(name: &str, config: &McpServerConfig) -> Result<Self, String> {
        let command = config
            .command
            .as_deref()
            .ok_or_else(|| format!("MCP server `{name}` has no command"))?;
        let mut process = tokio::process::Command::new(command);
        process
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = process
            .spawn()
            .map_err(|error| format!("cannot start MCP server `{name}`: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("MCP server `{name}` has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("MCP server `{name}` has no stdout"))?;
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        spawn_reader(stdout, pending.clone());
        Ok(Self {
            server: name.into(),
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            _child: child,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(format!("{message}\n").as_bytes())
                .await
                .map_err(|error| format!("MCP write failed: {error}"))?;
            stdin
                .flush()
                .await
                .map_err(|error| format!("MCP flush failed: {error}"))?;
        }
        match tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("MCP server closed the connection".into()),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(format!("MCP request `{method}` timed out"))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .map_err(|error| format!("MCP write failed: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("MCP flush failed: {error}"))
    }
}

fn spawn_reader(stdout: ChildStdout, pending: PendingMap) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(id) = value["id"].as_u64() else {
                continue;
            };
            let sender = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            if let Some(sender) = sender {
                if value.get("error").is_some() {
                    let error = value["error"]["message"]
                        .as_str()
                        .unwrap_or("unknown MCP error");
                    let _ = sender.send(Err(error.to_string()));
                } else {
                    let _ = sender.send(Ok(value["result"].clone()));
                }
            }
        }
        let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err("MCP server exited".into()));
        }
    });
}

impl HttpTransport {
    fn new(name: &str, url: &str) -> Self {
        Self {
            server: name.into(),
            url: url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("static MCP client"),
            next_id: AtomicU64::new(1),
            session_id: Mutex::new(None),
        }
    }

    async fn post(&self, body: Value) -> Result<reqwest::Response, String> {
        let mut request = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(session) = self
            .session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            request = request.header("mcp-session-id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("MCP HTTP request failed: {error}"))?;
        if let Some(session) = response.headers().get("mcp-session-id") {
            if let Ok(session) = session.to_str() {
                *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(session.to_string());
            }
        }
        Ok(response)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self.post(body).await?;
        if !response.status().is_success() {
            return Err(format!("MCP HTTP status {}", response.status()));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if content_type.contains("text/event-stream") {
            let text = response.text().await.map_err(|error| error.to_string())?;
            for block in text.split("\n\n") {
                let data: String = block
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    if value["id"].as_u64() == Some(id) {
                        return extract_result(value);
                    }
                }
            }
            return Err("MCP SSE response did not contain our request".into());
        }
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("MCP JSON decode failed: {error}"))?;
        extract_result(value)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let response = self.post(body).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("MCP HTTP status {}", response.status()))
        }
    }
}

fn extract_result(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error") {
        return Err(error["message"].as_str().unwrap_or("MCP error").to_string());
    }
    Ok(value["result"].clone())
}

struct McpToolProxy {
    client: Arc<McpClient>,
    wire_name: &'static str,
    remote_name: String,
    description: &'static str,
    schema: Value,
}

#[async_trait]
impl Tool for McpToolProxy {
    fn name(&self) -> &'static str {
        self.wire_name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn key(&self) -> ToolKey {
        ToolKey::McpTool(self.client.server().to_string(), self.remote_name.clone())
    }

    fn permission(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolPermission, ToolError> {
        let rendered: String = args.to_string().chars().take(4000).collect();
        Ok(ToolPermission::new(vec![Scope::command(rendered)]))
    }

    fn summary(&self, _args: &Value) -> String {
        format!("{}:{}", self.client.server(), self.remote_name)
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        match self.client.call_tool(&self.remote_name, args).await {
            Ok(text) => Ok(ToolOutput::success(text)),
            Err(error) => Ok(ToolOutput::error(error)),
        }
    }
}

pub async fn connect_all(
    config: &Config,
    trusted: bool,
    events: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
) -> McpLoad {
    let mut manager = McpManager {
        clients: Vec::new(),
    };
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut warnings = Vec::new();
    if config.mcp.is_empty() {
        return McpLoad {
            manager,
            tools,
            warnings,
        };
    }
    if !trusted {
        warnings.push(
            "MCP servers skipped: the project is not trusted (run `kudelma trust add`)".into(),
        );
        return McpLoad {
            manager,
            tools,
            warnings,
        };
    }
    for (name, server) in &config.mcp {
        let client = if let Some(url) = &server.url {
            let client = Arc::new(McpClient::Http(HttpTransport::new(name, url)));
            if let Err(error) = client.request("initialize", initialize_params()).await {
                warnings.push(format!("MCP `{name}`: {error}"));
                continue;
            }
            client
        } else {
            let transport = match StdioTransport::spawn(name, server).await {
                Ok(transport) => transport,
                Err(error) => {
                    warnings.push(error);
                    continue;
                }
            };
            let client = Arc::new(McpClient::Stdio(transport));
            if let Err(error) = client.request("initialize", initialize_params()).await {
                warnings.push(format!("MCP `{name}`: {error}"));
                continue;
            }
            client
        };
        let _ = client.notify("notifications/initialized", json!({})).await;
        match client.list_tools().await {
            Ok(list) => {
                let count = list.len();
                for tool in list {
                    let wire = wire_name(name, &tool.name);
                    let description = format!("[{}] {}", name, tool.description);
                    tools.push(Arc::new(McpToolProxy {
                        client: client.clone(),
                        wire_name: Box::leak(wire.into_boxed_str()),
                        remote_name: tool.name,
                        description: Box::leak(description.into_boxed_str()),
                        schema: tool.schema,
                    }));
                }
                let _ = events.send(AgentEvent::Info(format!(
                    "MCP `{name}` connected ({count} tools)"
                )));
                manager.clients.push(client);
            }
            Err(error) => warnings.push(format!("MCP `{name}`: {error}")),
        }
    }
    McpLoad {
        manager,
        tools,
        warnings,
    }
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "kudelma", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn wire_name(server: &str, tool: &str) -> String {
    let sanitize = |text: &str| {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let server = sanitize(server);
    let tool = sanitize(tool);
    let full = format!("{server}__{tool}");
    if full.len() <= 64 {
        return full;
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in full.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let suffix = format!("_{:08x}", hash as u32);
    let keep = 64 - suffix.len();
    format!("{}{}", &full[..keep], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_bounded_and_valid() {
        let name = wire_name("very-long-server-name", &"x".repeat(100));
        assert!(name.len() <= 64);
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
    }
}
