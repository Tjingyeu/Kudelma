use kudelma_providers::{
    CancelToken, ChatRequest, ContentBlock, Message, ProviderError, ProviderEvent, Role,
    provider_for,
};

const MAX_BLOCK_CHARS: usize = 2000;

pub async fn summarize(
    model: &kudelma_providers::Model,
    history: &[Message],
    instructions: Option<&str>,
    cancel: &CancelToken,
) -> Result<String, ProviderError> {
    let mut transcript = String::new();
    for message in history {
        let role = match message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => {
                    transcript.push_str(&format!("{role}: {}\n\n", clip(text, MAX_BLOCK_CHARS)));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    transcript.push_str(&format!(
                        "{role} called `{name}` with {}\n\n",
                        clip(&input.to_string(), 500)
                    ));
                }
                ContentBlock::ToolResult { content, .. } => {
                    transcript.push_str(&format!(
                        "Tool result: {}\n\n",
                        clip(content, MAX_BLOCK_CHARS)
                    ));
                }
                _ => {}
            }
        }
    }
    let mut system = "You compact a coding agent's conversation. Preserve:\n\
        - the user's goal and constraints\n\
        - decisions and rejected alternatives\n\
        - files changed and why\n\
        - commands run and their important output\n\
        - open tasks and next steps\n\
        Drop greetings and redundant tool noise. Write a dense handoff summary in Markdown.\n"
        .to_string();
    if let Some(instructions) = instructions {
        system.push_str("\nExtra instructions: ");
        system.push_str(instructions);
    }
    let request = ChatRequest {
        messages: vec![Message::user(format!(
            "Summarize this conversation for your future self:\n\n{transcript}"
        ))],
        system: Some(system),
        tools: Vec::new(),
        max_tokens: 4096.min(model.max_output_tokens),
        temperature: Some(0.2),
        thinking: None,
    };
    let provider = provider_for(model);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let stream = provider.stream(request, tx, cancel.clone());
    tokio::pin!(stream);
    let mut text = String::new();
    loop {
        tokio::select! {
            result = &mut stream => {
                result?;
                break;
            }
            event = rx.recv() => {
                match event {
                    Some(ProviderEvent::TextDelta(delta)) => text.push_str(&delta),
                    Some(_) => {}
                    None => break,
                }
            }
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        }
    }
    while let Ok(event) = rx.try_recv() {
        if let ProviderEvent::TextDelta(delta) = event {
            text.push_str(&delta);
        }
    }
    if text.trim().is_empty() {
        return Err(ProviderError::EmptyResponse);
    }
    Ok(text)
}

pub fn fallback_history(history: &[Message]) -> Vec<Message> {
    let mut kept: Vec<Message> = history
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    kept.insert(
        0,
        Message::user(
            "[Earlier conversation was compacted because the context window filled up. \
             Continue from the recent messages below.]",
        ),
    );
    kept
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}
