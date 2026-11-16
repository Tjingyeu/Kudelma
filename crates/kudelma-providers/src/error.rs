use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("unknown provider '{0}'. Configure it under [providers.{0}]")]
    UnknownProvider(String),
    #[error("invalid model spec '{0}'. Use provider/model, e.g. anthropic/claude-sonnet-4-5")]
    InvalidSpec(String),
    #[error("no API key for {provider}. Set {env} or run `kudelma auth login {provider}`")]
    MissingApiKey { provider: String, env: String },
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{provider} returned HTTP {status}: {body}")]
    Status {
        provider: String,
        status: u16,
        body: String,
    },
    #[error("could not decode {provider} response: {source}")]
    Json {
        provider: String,
        source: serde_json::Error,
    },
    #[error("stream error from {provider}: {message}")]
    Stream { provider: String, message: String },
    #[error("request cancelled")]
    Cancelled,
    #[error("model returned an empty response")]
    EmptyResponse,
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    pub fn cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}
