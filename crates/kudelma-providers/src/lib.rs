#![allow(clippy::collapsible_if)]

//! Provider-independent message model and LLM wire adapters.

pub mod cancel;
pub mod drivers;
pub mod error;
pub mod model;
pub mod retry;
pub mod tokens;
pub mod types;

pub use cancel::{CancelReason, CancelToken};
pub use error::ProviderError;
pub use model::{ProviderPreset, all_models, builtin_providers, provider_for, resolve_model};
pub use tokens::{ContextGauge, estimate_message_tokens, estimate_request_tokens, estimate_tokens};
pub use types::{
    ChatRequest, ContentBlock, Message, ProviderEvent, Role, SystemPrompt, ThinkingConfig,
    TokenUsage, ToolCall, ToolDefinition, ToolResult, Usage,
};

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedSender;

pub use model::{Model, ModelTier};

#[async_trait]
pub trait Provider: Send + Sync {
    fn model(&self) -> &Model;

    async fn stream(
        &self,
        request: ChatRequest,
        events: UnboundedSender<ProviderEvent>,
        cancel: CancelToken,
    ) -> Result<(), ProviderError>;
}

pub type SharedProvider = Arc<dyn Provider>;
