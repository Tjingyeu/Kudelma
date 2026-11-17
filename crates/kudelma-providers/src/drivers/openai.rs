use std::collections::BTreeMap;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::cancel::CancelToken;
use crate::error::ProviderError;
use crate::model::Model;
use crate::retry::send_with_retry;
use crate::types::{ChatRequest, ContentBlock, Message, ProviderEvent, Role, TokenUsage, ToolCall};
use crate::{Provider, drivers::http_client};

pub struct OpenAiProvider {
    model: Model,
    client: Client,
}

impl OpenAiProvider {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            client: http_client(),
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn model(&self) -> &Model {
        &self.model
    }

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
        cancel: CancelToken,
    ) -> Result<(), ProviderError> {
        let key = self.model.require_key()?.to_string();
        let url = format!("{}/chat/completions", self.model.base_url);
        let body = openai_body(&self.model, &request);

        let response = send_with_retry("openai", &cancel, || {
            let mut builder = self
                .client
                .post(&url)
                .header("content-type", "application/json")
                .json(&body);
            if !key.is_empty() {
                builder = builder.header("authorization", format!("Bearer {key}"));
            }
            for (name, value) in &self.model.headers {
                builder = builder.header(name, value);
            }
            add_provider_headers(builder, &self.model.provider, request.session_id.as_deref())
        })
        .await?;

        let mut stream = response.bytes_stream().eventsource();
        let mut tool_calls: BTreeMap<u64, ToolAccumulator> = BTreeMap::new();
        let mut usage = TokenUsage::default();
        let mut saw_content = false;

        while let Some(event) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let event = event.map_err(|error| ProviderError::Stream {
                provider: self.model.provider.clone(),
                message: error.to_string(),
            })?;
            let data = event.data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                break;
            }
            let value: Value =
                serde_json::from_str(data).map_err(|source| ProviderError::Json {
                    provider: self.model.provider.clone(),
                    source,
                })?;
            if let Some(error) = value.get("error") {
                return Err(ProviderError::Other(format!(
                    "{} stream error: {}",
                    self.model.provider,
                    error["message"].as_str().unwrap_or("unknown")
                )));
            }
            if let Some(usage_value) = value.get("usage").filter(|v| !v.is_null()) {
                usage.input_tokens = usage_value["prompt_tokens"]
                    .as_u64()
                    .unwrap_or(usage.input_tokens);
                usage.output_tokens = usage_value["completion_tokens"]
                    .as_u64()
                    .unwrap_or(usage.output_tokens);
                usage.cache_read_tokens = usage_value["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(usage.cache_read_tokens);
            }
            let Some(choice) = value["choices"].as_array().and_then(|c| c.first()) else {
                continue;
            };
            let delta = &choice["delta"];
            if let Some(text) = delta_text(delta) {
                if !text.is_empty() {
                    saw_content = true;
                    let _ = events.send(ProviderEvent::TextDelta(text));
                }
            }
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
            {
                if !reasoning.is_empty() {
                    let _ = events.send(ProviderEvent::ThinkingDelta(reasoning.to_string()));
                }
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for (position, call) in calls.iter().enumerate() {
                    let index = call["index"].as_u64().unwrap_or(position as u64);
                    let accumulator = tool_calls.entry(index).or_default();
                    if let Some(id) = call["id"].as_str() {
                        if !id.is_empty() {
                            accumulator.id = id.to_string();
                        }
                    }
                    if let Some(name) = call["function"]["name"].as_str() {
                        if !name.is_empty() {
                            accumulator.name.push_str(name);
                        }
                    }
                    if let Some(arguments) = call["function"]["arguments"].as_str() {
                        accumulator.arguments.push_str(arguments);
                    }
                }
            }
        }

        let mut emitted_tool = false;
        for (_, accumulator) in tool_calls {
            if accumulator.name.is_empty() {
                continue;
            }
            let id = if accumulator.id.is_empty() {
                format!("call_{}", uuid_like())
            } else {
                accumulator.id
            };
            let input = if accumulator.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&accumulator.arguments)
                    .unwrap_or_else(|_| json!({ "_raw": accumulator.arguments }))
            };
            emitted_tool = true;
            let _ = events.send(ProviderEvent::ToolUse(ToolCall {
                id,
                name: accumulator.name,
                input,
            }));
        }

        let _ = events.send(ProviderEvent::Usage(usage));
        if !saw_content && !emitted_tool {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(())
    }
}

fn add_provider_headers(
    mut builder: RequestBuilder,
    provider: &str,
    session_id: Option<&str>,
) -> RequestBuilder {
    if provider == "opencode-go" {
        builder = builder.header("user-agent", concat!("kudelma/", env!("CARGO_PKG_VERSION")));
        if let Some(session_id) = session_id {
            builder = builder.header("x-opencode-session", session_id);
        }
    }
    builder
}

#[derive(Default)]
struct ToolAccumulator {
    id: String,
    name: String,
    arguments: String,
}

fn delta_text(delta: &Value) -> Option<String> {
    if let Some(text) = delta["content"].as_str() {
        return Some(text.to_string());
    }
    if let Some(parts) = delta["content"].as_array() {
        let mut out = String::new();
        for part in parts {
            if let Some(text) = part["text"].as_str() {
                out.push_str(text);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn openai_body(model: &Model, request: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        match message.role {
            Role::System => {
                let text = message.text();
                if !text.is_empty() {
                    messages.push(json!({ "role": "system", "content": text }));
                }
            }
            Role::User => openai_user_message(message, &mut messages),
            Role::Assistant => openai_assistant_message(message, &mut messages),
        }
    }

    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect();

    let mut body = json!({
        "model": model.id,
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] = json!("auto");
    }
    let max_tokens = request.max_tokens.min(model.max_output_tokens).max(256);
    let reasoning = model.id.starts_with('o') || model.id.contains("reasoner");
    if reasoning {
        body["max_completion_tokens"] = json!(max_tokens);
    } else {
        body["max_tokens"] = json!(max_tokens);
    }
    if matches!(
        model.provider.as_str(),
        "openai" | "deepseek" | "moonshot" | "openrouter" | "xai" | "opencode-go"
    ) && !reasoning
    {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(temperature) = request.temperature {
        if !reasoning {
            body["temperature"] = json!(temperature);
        }
    }
    body
}

fn openai_user_message(message: &Message, out: &mut Vec<Value>) {
    let mut parts: Vec<Value> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => pending_results.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })),
            ContentBlock::Text { text } if !text.is_empty() => {
                parts.push(json!({ "type": "text", "text": text }));
            }
            ContentBlock::Image { media_type, data } => {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{media_type};base64,{data}") }
                }));
            }
            _ => {}
        }
    }
    out.extend(pending_results);
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 && parts[0]["type"] == "text" {
        out.push(json!({ "role": "user", "content": parts[0]["text"] }));
    } else {
        out.push(json!({ "role": "user", "content": parts }));
    }
}

fn openai_assistant_message(message: &Message, out: &mut Vec<Value>) {
    let mut text = String::new();
    let mut calls: Vec<Value> = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: part } => text.push_str(part),
            ContentBlock::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": input.to_string() }
            })),
            _ => {}
        }
    }
    if text.is_empty() && calls.is_empty() {
        return;
    }
    let mut value = json!({
        "role": "assistant",
        "content": if text.is_empty() { Value::Null } else { Value::String(text) },
    });
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    out.push(value);
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolDefinition;

    #[test]
    fn converts_tool_results_to_tool_messages() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        let model = Model {
            provider: "openai".into(),
            id: "gpt-4.1".into(),
            display_name: "gpt".into(),
            context_window: 1000,
            max_output_tokens: 100,
            input_cost: 0.0,
            output_cost: 0.0,
            tier: crate::ModelTier::Medium,
            supports_thinking: false,
            supports_tools: true,
            kind: kudelma_config::ProviderKind::Openai,
            base_url: "http://localhost".into(),
            api_key: None,
            auth_env: "KEY".into(),
            headers: Default::default(),
        };
        let request = ChatRequest {
            messages: vec![
                Message::assistant("x"),
                Message::tool_results(vec![crate::types::ToolResult {
                    tool_use_id: "abc".into(),
                    content: "done".into(),
                    is_error: false,
                    name: Some("read".into()),
                    metadata: None,
                }]),
            ],
            system: Some("sys".into()),
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: json!({ "type": "object" }),
            }],
            max_tokens: 10,
            temperature: None,
            thinking: None,
            session_id: None,
        };
        let body = openai_body(&model, &request);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
    }

    #[test]
    fn opencode_go_requests_carry_identity_and_session_headers() {
        let request = add_provider_headers(
            Client::new().post("https://opencode.ai/inference/go/openai/v1/chat/completions"),
            "opencode-go",
            Some("session-agency-123"),
        )
        .build()
        .unwrap();
        assert_eq!(
            request.headers()["x-opencode-session"],
            "session-agency-123"
        );
        assert_eq!(
            request.headers()["user-agent"],
            concat!("kudelma/", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn other_providers_do_not_receive_opencode_headers() {
        let request = add_provider_headers(
            Client::new().post("https://api.example.invalid/v1/chat/completions"),
            "openai",
            Some("session-agency-123"),
        )
        .build()
        .unwrap();
        assert!(request.headers().get("x-opencode-session").is_none());
    }

    #[test]
    fn opencode_go_requests_include_stream_usage() {
        let model = Model {
            provider: "opencode-go".into(),
            id: "glm-5.3-flash".into(),
            display_name: "glm".into(),
            context_window: 1000,
            max_output_tokens: 100,
            input_cost: 0.0,
            output_cost: 0.0,
            tier: crate::ModelTier::Medium,
            supports_thinking: true,
            supports_tools: true,
            kind: kudelma_config::ProviderKind::Openai,
            base_url: "https://opencode.ai/inference/go/openai/v1".into(),
            api_key: Some("test".into()),
            auth_env: "OPENCODE_API_KEY".into(),
            headers: Default::default(),
        };
        let request = ChatRequest {
            messages: vec![Message::user("hi")],
            system: None,
            tools: Vec::new(),
            max_tokens: 10,
            temperature: None,
            thinking: None,
            session_id: Some("session-1".into()),
        };
        let body = openai_body(&model, &request);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
}
