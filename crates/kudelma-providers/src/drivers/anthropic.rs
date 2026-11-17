use std::collections::HashMap;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::cancel::CancelToken;
use crate::error::ProviderError;
use crate::model::Model;
use crate::retry::send_with_retry;
use crate::types::{ChatRequest, ContentBlock, ProviderEvent, Role, TokenUsage, ToolCall};
use crate::{Provider, drivers::http_client};

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    model: Model,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            client: http_client(),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
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
        let url = format!("{}/v1/messages", self.model.base_url);
        let body = anthropic_body(&self.model, &request);

        let response = send_with_retry("anthropic", &cancel, || {
            let mut builder = self
                .client
                .post(&url)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body);
            if !key.is_empty() {
                builder = builder.header("x-api-key", &key);
            }
            for (name, value) in &self.model.headers {
                builder = builder.header(name, value);
            }
            builder
        })
        .await?;

        let mut stream = response.bytes_stream().eventsource();
        let mut blocks: HashMap<u64, BlockState> = HashMap::new();
        let mut usage = TokenUsage::default();
        let mut saw_content = false;

        while let Some(event) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let event = event.map_err(|error| ProviderError::Stream {
                provider: "anthropic".into(),
                message: error.to_string(),
            })?;
            if event.data == "[DONE]" {
                break;
            }
            let data: Value =
                serde_json::from_str(&event.data).map_err(|source| ProviderError::Json {
                    provider: "anthropic".into(),
                    source,
                })?;
            match data["type"].as_str().unwrap_or_default() {
                "message_start" => {
                    usage.input_tokens = data["message"]["usage"]["input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    usage.cache_read_tokens = data["message"]["usage"]["cache_read_input_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    usage.cache_write_tokens =
                        data["message"]["usage"]["cache_creation_input_tokens"]
                            .as_u64()
                            .unwrap_or(0);
                }
                "content_block_start" => {
                    let index = data["index"].as_u64().unwrap_or(0);
                    let block = &data["content_block"];
                    let state = blocks.entry(index).or_default();
                    state.kind = block["type"].as_str().unwrap_or("text").to_string();
                    state.id = block["id"].as_str().unwrap_or_default().to_string();
                    state.name = block["name"].as_str().unwrap_or_default().to_string();
                    if let Some(text) = block["text"].as_str() {
                        if !text.is_empty() {
                            state.text.push_str(text);
                            saw_content = true;
                            let _ = events.send(ProviderEvent::TextDelta(text.to_string()));
                        }
                    }
                    if let Some(thinking) = block["thinking"].as_str() {
                        if !thinking.is_empty() {
                            state.thinking.push_str(thinking);
                            let _ = events.send(ProviderEvent::ThinkingDelta(thinking.to_string()));
                        }
                    }
                }
                "content_block_delta" => {
                    let index = data["index"].as_u64().unwrap_or(0);
                    let delta = &data["delta"];
                    let state = blocks.entry(index).or_default();
                    match delta["type"].as_str().unwrap_or_default() {
                        "text_delta" => {
                            if let Some(text) = delta["text"].as_str() {
                                state.text.push_str(text);
                                saw_content = true;
                                let _ = events.send(ProviderEvent::TextDelta(text.to_string()));
                            }
                        }
                        "thinking_delta" => {
                            if let Some(thinking) = delta["thinking"].as_str() {
                                state.thinking.push_str(thinking);
                                let _ =
                                    events.send(ProviderEvent::ThinkingDelta(thinking.to_string()));
                            }
                        }
                        "signature_delta" => {
                            if let Some(signature) = delta["signature"].as_str() {
                                state.signature.push_str(signature);
                            }
                        }
                        "input_json_delta" => {
                            if let Some(json) = delta["partial_json"].as_str() {
                                state.input.push_str(json);
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let index = data["index"].as_u64().unwrap_or(0);
                    if let Some(state) = blocks.get(&index) {
                        if state.kind == "tool_use" {
                            let input = parse_tool_input(&state.input);
                            saw_content = true;
                            let _ = events.send(ProviderEvent::ToolUse(ToolCall {
                                id: state.id.clone(),
                                name: state.name.clone(),
                                input,
                            }));
                        }
                    }
                }
                "message_delta" => {
                    usage.output_tokens = data["usage"]["output_tokens"]
                        .as_u64()
                        .unwrap_or(usage.output_tokens);
                }
                "message_stop" => break,
                "error" => {
                    return Err(ProviderError::Other(format!(
                        "anthropic stream error: {}",
                        data["error"]["message"].as_str().unwrap_or("unknown")
                    )));
                }
                _ => {}
            }
        }

        let _ = events.send(ProviderEvent::Usage(usage));
        if !saw_content {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(())
    }
}

#[derive(Default)]
struct BlockState {
    kind: String,
    id: String,
    name: String,
    text: String,
    thinking: String,
    signature: String,
    input: String,
}

fn parse_tool_input(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({ "_raw": raw }))
}

fn anthropic_body(model: &Model, request: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            continue;
        }
        let role = match message.role {
            Role::Assistant => "assistant",
            _ => "user",
        };
        let blocks: Vec<Value> = message.content.iter().filter_map(anthropic_block).collect();
        if blocks.is_empty() {
            continue;
        }
        if let Some(last) = messages.last_mut() {
            if last["role"] == role {
                if let Some(existing) = last["content"].as_array_mut() {
                    existing.extend(blocks);
                    continue;
                }
            }
        }
        messages.push(json!({ "role": role, "content": blocks }));
    }

    let mut tools: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect();
    if let Some(last) = tools.last_mut() {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }

    let mut body = json!({
        "model": model.id,
        "max_tokens": request.max_tokens.min(model.max_output_tokens).max(256),
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Some(system) = &request.system {
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" }
        }]);
    }
    if let Some(thinking) = request.thinking {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": thinking.budget_tokens,
        });
        body["max_tokens"] = json!(
            u64::from(thinking.budget_tokens + 4096)
                .max(body["max_tokens"].as_u64().unwrap_or(4096))
        );
    } else if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(last) = body["messages"]
        .as_array_mut()
        .and_then(|messages| messages.last_mut())
        .and_then(|message| message["content"].as_array_mut())
        .and_then(|blocks| blocks.last_mut())
    {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }
    body
}

fn anthropic_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } if !text.is_empty() => {
            Some(json!({ "type": "text", "text": text }))
        }
        ContentBlock::Text { .. } => None,
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => Some(json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        })),
        ContentBlock::Image { media_type, data } => Some(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data }
        })),
        ContentBlock::Thinking { .. } | ContentBlock::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ToolDefinition, ToolResult};

    #[test]
    fn merges_adjacent_user_tool_results() {
        let model = Model {
            provider: "anthropic".into(),
            id: "claude".into(),
            display_name: "claude".into(),
            context_window: 200_000,
            max_output_tokens: 4096,
            input_cost: 0.0,
            output_cost: 0.0,
            tier: crate::ModelTier::Medium,
            supports_thinking: false,
            supports_tools: true,
            kind: kudelma_config::ProviderKind::Anthropic,
            base_url: "http://localhost".into(),
            api_key: None,
            auth_env: "KEY".into(),
            headers: Default::default(),
        };
        let request = ChatRequest {
            messages: vec![
                Message::assistant("working"),
                Message::tool_results(vec![ToolResult {
                    tool_use_id: "1".into(),
                    content: "ok".into(),
                    is_error: false,
                    name: Some("read".into()),
                    metadata: None,
                }]),
            ],
            system: None,
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: json!({ "type": "object" }),
            }],
            max_tokens: 4096,
            temperature: None,
            thinking: None,
            session_id: None,
        };
        let body = anthropic_body(&model, &request);
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["messages"].as_array().unwrap().len() == 2);
    }
}
