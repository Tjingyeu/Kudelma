use serde_json::Value;

use crate::types::{ContentBlock, Message, ToolDefinition};

/// Fast, allocation-free-enough heuristic used for the context gauge.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64 / 4) + 1
}

pub fn estimate_message_tokens(message: &Message) -> u64 {
    let mut total = 4;
    for block in &message.content {
        total += match block {
            ContentBlock::Text { text } => estimate_tokens(text),
            ContentBlock::Thinking { thinking, .. } => estimate_tokens(thinking),
            ContentBlock::ToolUse { name, input, .. } => {
                estimate_tokens(name) + estimate_tokens(&input.to_string()) + 8
            }
            ContentBlock::ToolResult { content, .. } => estimate_tokens(content) + 8,
            ContentBlock::Image { data, .. } => data.len() as u64 / 4,
            ContentBlock::Unknown => 0,
        };
    }
    total
}

pub fn estimate_request_tokens(
    system: Option<&str>,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> u64 {
    let system = system.map(estimate_tokens).unwrap_or(0);
    let messages: u64 = messages.iter().map(estimate_message_tokens).sum();
    let tools: u64 = tools
        .iter()
        .map(|tool| {
            estimate_tokens(&tool.name)
                + estimate_tokens(&tool.description)
                + estimate_tokens(&tool.input_schema.to_string())
        })
        .sum();
    system + messages + tools + 16
}

#[derive(Debug, Clone, Copy)]
pub struct ContextGauge {
    pub used: u64,
    pub window: u32,
}

impl ContextGauge {
    pub fn percent(&self) -> u8 {
        if self.window == 0 {
            return 0;
        }
        ((self.used.saturating_mul(100) / u64::from(self.window)).min(100)) as u8
    }

    pub fn should_compact(&self, threshold_percent: u8) -> bool {
        self.percent() >= threshold_percent
    }
}

pub fn value_preview(value: &Value, max: usize) -> String {
    let text = value.to_string();
    if text.chars().count() <= max {
        return text;
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_are_monotonic() {
        assert!(estimate_tokens("hello world") > 0);
        assert!(estimate_tokens(&"a".repeat(400)) >= 100);
    }

    #[test]
    fn gauge_percent_is_capped() {
        let gauge = ContextGauge {
            used: 500,
            window: 100,
        };
        assert_eq!(gauge.percent(), 100);
    }
}
