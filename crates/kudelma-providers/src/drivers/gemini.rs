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

pub struct GeminiProvider {
    model: Model,
    client: Client,
}

impl GeminiProvider {
    pub fn new(model: Model) -> Self {
        Self {
            model,
            client: http_client(),
        }
    }
}

#[async_trait]
impl Provider for GeminiProvider {
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
        let url = format!(
            "{}/models/{}:streamGenerateContent",
            self.model.base_url, self.model.id
        );
        let body = gemini_body(&self.model, &request);

        let response = send_with_retry("gemini", &cancel, || {
            let mut builder = self
                .client
                .post(&url)
                .header("content-type", "application/json")
                .json(&body);
            if key.is_empty() {
                builder = builder.query(&[("alt", "sse")]);
            } else {
                builder = builder.query(&[("alt", "sse"), ("key", key.as_str())]);
            }
            for (name, value) in &self.model.headers {
                builder = builder.header(name, value);
            }
            builder
        })
        .await?;

        let mut stream = response.bytes_stream().eventsource();
        let mut usage = TokenUsage::default();
        let mut saw_content = false;
        let mut call_counter = 0u32;

        while let Some(event) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let event = event.map_err(|error| ProviderError::Stream {
                provider: "gemini".into(),
                message: error.to_string(),
            })?;
            if event.data.trim().is_empty() || event.data == "[DONE]" {
                continue;
            }
            let value: Value =
                serde_json::from_str(&event.data).map_err(|source| ProviderError::Json {
                    provider: "gemini".into(),
                    source,
                })?;
            if let Some(error) = value.get("error") {
                return Err(ProviderError::Other(format!(
                    "gemini stream error: {}",
                    error["message"].as_str().unwrap_or("unknown")
                )));
            }
            if let Some(block_reason) = value["promptFeedback"]["blockReason"].as_str() {
                return Err(ProviderError::Other(format!(
                    "gemini blocked the prompt: {block_reason}"
                )));
            }
            if let Some(metadata) = value.get("usageMetadata") {
                usage.input_tokens = metadata["promptTokenCount"]
                    .as_u64()
                    .unwrap_or(usage.input_tokens);
                usage.output_tokens = metadata["candidatesTokenCount"]
                    .as_u64()
                    .unwrap_or(usage.output_tokens);
                usage.cache_read_tokens = metadata["cachedContentTokenCount"]
                    .as_u64()
                    .unwrap_or(usage.cache_read_tokens);
            }
            let Some(parts) = value["candidates"]
                .as_array()
                .and_then(|candidates| candidates.first())
                .and_then(|candidate| candidate["content"]["parts"].as_array())
            else {
                continue;
            };
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    if !text.is_empty() {
                        if part["thought"].as_bool().unwrap_or(false) {
                            let _ = events.send(ProviderEvent::ThinkingDelta(text.to_string()));
                        } else {
                            saw_content = true;
                            let _ = events.send(ProviderEvent::TextDelta(text.to_string()));
                        }
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let name = call["name"].as_str().unwrap_or_default().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    call_counter += 1;
                    saw_content = true;
                    let input = if call["args"].is_object() {
                        call["args"].clone()
                    } else {
                        json!({})
                    };
                    let _ = events.send(ProviderEvent::ToolUse(ToolCall {
                        id: format!("gemini_call_{call_counter}"),
                        name,
                        input,
                    }));
                }
            }
        }

        let _ = events.send(ProviderEvent::Usage(usage));
        if !saw_content {
            return Err(ProviderError::EmptyResponse);
        }
        Ok(())
    }
}

fn gemini_body(model: &Model, request: &ChatRequest) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            continue;
        }
        let role = match message.role {
            Role::Assistant => "model",
            _ => "user",
        };
        let parts: Vec<Value> = message.content.iter().filter_map(gemini_part).collect();
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut() {
            if last["role"] == role {
                if let Some(existing) = last["parts"].as_array_mut() {
                    existing.extend(parts);
                    continue;
                }
            }
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let declarations: Vec<Value> = request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
            })
        })
        .collect();

    let mut body = json!({ "contents": contents });
    if let Some(system) = &request.system {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if !declarations.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    let mut generation = json!({
        "maxOutputTokens": request.max_tokens.min(model.max_output_tokens).max(256),
    });
    if let Some(temperature) = request.temperature {
        generation["temperature"] = json!(temperature);
    }
    body["generationConfig"] = generation;
    body
}

fn gemini_part(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } if !text.is_empty() => Some(json!({ "text": text })),
        ContentBlock::Text { .. } => None,
        ContentBlock::Image { media_type, data } => Some(json!({
            "inlineData": { "mimeType": media_type, "data": data }
        })),
        ContentBlock::ToolUse { name, input, .. } => Some(json!({
            "functionCall": { "name": name, "args": input }
        })),
        ContentBlock::ToolResult {
            content,
            is_error,
            name,
            tool_use_id,
        } => Some(json!({
            "functionResponse": {
                "name": name.clone().unwrap_or_else(|| tool_use_id.clone()),
                "response": { "content": content, "isError": is_error }
            }
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    #[test]
    fn tool_definitions_are_wrapped() {
        let model = Model {
            provider: "gemini".into(),
            id: "gemini-2.5-pro".into(),
            display_name: "gemini".into(),
            context_window: 1000,
            max_output_tokens: 100,
            input_cost: 0.0,
            output_cost: 0.0,
            tier: crate::ModelTier::Medium,
            supports_thinking: true,
            supports_tools: true,
            kind: kudelma_config::ProviderKind::Gemini,
            base_url: "http://localhost".into(),
            api_key: None,
            auth_env: "KEY".into(),
            headers: Default::default(),
        };
        let request = ChatRequest {
            messages: vec![Message::user("hi")],
            system: Some("sys".into()),
            tools: vec![crate::types::ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: json!({ "type": "object" }),
            }],
            max_tokens: 10,
            temperature: None,
            thinking: None,
            session_id: None,
        };
        let body = gemini_body(&model, &request);
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "read");
        assert_eq!(body["contents"][0]["role"], "user");
    }
}
