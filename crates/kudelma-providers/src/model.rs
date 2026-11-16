use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use kudelma_config::{
    Config, ModelConfig, ModelTier as ConfigTier, ProviderConfig, ProviderKind, expand_env,
};
use kudelma_storage::AuthStore;

use crate::Provider;
use crate::drivers;
use crate::error::ProviderError;
use crate::types::TokenUsage;

pub use kudelma_config::ModelTier;

#[derive(Clone)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub input_cost: f64,
    pub output_cost: f64,
    pub tier: ModelTier,
    pub supports_thinking: bool,
    pub supports_tools: bool,
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: Option<String>,
    pub auth_env: String,
    pub headers: BTreeMap<String, String>,
}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Model")
            .field("provider", &self.provider)
            .field("id", &self.id)
            .field("context_window", &self.context_window)
            .finish_non_exhaustive()
    }
}

impl Model {
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn cost(&self, usage: TokenUsage) -> f64 {
        let input = usage.input_tokens as f64 * self.input_cost / 1_000_000.0;
        let output = usage.output_tokens as f64 * self.output_cost / 1_000_000.0;
        input + output
    }

    pub fn require_key(&self) -> Result<&str, ProviderError> {
        if let Some(key) = self.api_key.as_deref().filter(|key| !key.is_empty()) {
            return Ok(key);
        }
        if self.is_local() {
            return Ok("");
        }
        Err(ProviderError::MissingApiKey {
            provider: self.provider.clone(),
            env: self.auth_env.clone(),
        })
    }

    pub fn is_local(&self) -> bool {
        let url = self.base_url.to_ascii_lowercase();
        url.contains("://localhost")
            || url.contains("://127.0.0.1")
            || url.contains("://0.0.0.0")
            || url.contains("://[::1]")
    }
}

pub struct ProviderPreset {
    pub name: &'static str,
    pub kind: ProviderKind,
    pub base_url: &'static str,
    pub api_key_env: &'static str,
    pub models: Vec<ModelConfig>,
}

fn model(
    id: &str,
    context_window: u32,
    max_output_tokens: u32,
    input_cost: f64,
    output_cost: f64,
    tier: ConfigTier,
    supports_thinking: bool,
) -> ModelConfig {
    ModelConfig {
        id: id.into(),
        context_window,
        max_output_tokens,
        input_cost,
        output_cost,
        tier,
        supports_thinking,
        supports_tools: true,
    }
}

pub fn builtin_providers() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            name: "anthropic",
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com",
            api_key_env: "ANTHROPIC_API_KEY",
            models: vec![
                model(
                    "claude-sonnet-4-5",
                    200_000,
                    32_768,
                    3.0,
                    15.0,
                    ConfigTier::Medium,
                    true,
                ),
                model(
                    "claude-opus-4-5",
                    200_000,
                    32_768,
                    15.0,
                    75.0,
                    ConfigTier::Strong,
                    true,
                ),
                model(
                    "claude-haiku-4-5",
                    200_000,
                    16_384,
                    1.0,
                    5.0,
                    ConfigTier::Weak,
                    false,
                ),
            ],
        },
        ProviderPreset {
            name: "openai",
            kind: ProviderKind::Openai,
            base_url: "https://api.openai.com/v1",
            api_key_env: "OPENAI_API_KEY",
            models: vec![
                model(
                    "gpt-4.1",
                    1_047_576,
                    32_768,
                    2.0,
                    8.0,
                    ConfigTier::Medium,
                    false,
                ),
                model(
                    "gpt-4.1-mini",
                    1_047_576,
                    32_768,
                    0.4,
                    1.6,
                    ConfigTier::Weak,
                    false,
                ),
                model("o3", 200_000, 100_000, 2.0, 8.0, ConfigTier::Strong, true),
            ],
        },
        ProviderPreset {
            name: "moonshot",
            kind: ProviderKind::Openai,
            base_url: "https://api.moonshot.ai/v1",
            api_key_env: "MOONSHOT_API_KEY",
            models: vec![
                model(
                    "kimi-k2.5",
                    256_000,
                    32_768,
                    0.6,
                    2.5,
                    ConfigTier::Medium,
                    true,
                ),
                model(
                    "kimi-k2-turbo",
                    256_000,
                    16_384,
                    1.2,
                    8.0,
                    ConfigTier::Medium,
                    false,
                ),
                model(
                    "kimi-k2-thinking",
                    256_000,
                    32_768,
                    0.6,
                    2.5,
                    ConfigTier::Strong,
                    true,
                ),
            ],
        },
        ProviderPreset {
            name: "deepseek",
            kind: ProviderKind::Openai,
            base_url: "https://api.deepseek.com/v1",
            api_key_env: "DEEPSEEK_API_KEY",
            models: vec![
                model(
                    "deepseek-chat",
                    128_000,
                    8_192,
                    0.28,
                    0.42,
                    ConfigTier::Medium,
                    false,
                ),
                model(
                    "deepseek-reasoner",
                    128_000,
                    64_000,
                    0.28,
                    0.42,
                    ConfigTier::Strong,
                    true,
                ),
            ],
        },
        ProviderPreset {
            name: "gemini",
            kind: ProviderKind::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta",
            api_key_env: "GEMINI_API_KEY",
            models: vec![
                model(
                    "gemini-2.5-pro",
                    1_048_576,
                    65_536,
                    1.25,
                    10.0,
                    ConfigTier::Strong,
                    true,
                ),
                model(
                    "gemini-2.5-flash",
                    1_048_576,
                    65_536,
                    0.3,
                    2.5,
                    ConfigTier::Medium,
                    true,
                ),
                model(
                    "gemini-2.5-flash-lite",
                    1_048_576,
                    65_536,
                    0.1,
                    0.4,
                    ConfigTier::Weak,
                    false,
                ),
            ],
        },
        ProviderPreset {
            name: "openrouter",
            kind: ProviderKind::Openai,
            base_url: "https://openrouter.ai/api/v1",
            api_key_env: "OPENROUTER_API_KEY",
            models: vec![
                model(
                    "anthropic/claude-sonnet-4.5",
                    200_000,
                    32_768,
                    3.0,
                    15.0,
                    ConfigTier::Medium,
                    true,
                ),
                model(
                    "moonshotai/kimi-k2",
                    256_000,
                    32_768,
                    0.6,
                    2.5,
                    ConfigTier::Medium,
                    false,
                ),
                model(
                    "deepseek/deepseek-chat",
                    128_000,
                    8_192,
                    0.28,
                    0.42,
                    ConfigTier::Weak,
                    false,
                ),
            ],
        },
        ProviderPreset {
            name: "xai",
            kind: ProviderKind::Openai,
            base_url: "https://api.x.ai/v1",
            api_key_env: "XAI_API_KEY",
            models: vec![
                model(
                    "grok-4",
                    256_000,
                    32_768,
                    3.0,
                    15.0,
                    ConfigTier::Strong,
                    true,
                ),
                model(
                    "grok-3-mini",
                    131_072,
                    16_384,
                    0.3,
                    0.5,
                    ConfigTier::Weak,
                    false,
                ),
            ],
        },
        ProviderPreset {
            name: "ollama",
            kind: ProviderKind::Openai,
            base_url: "http://localhost:11434/v1",
            api_key_env: "OLLAMA_API_KEY",
            models: vec![
                model(
                    "qwen2.5-coder:32b",
                    131_072,
                    8_192,
                    0.0,
                    0.0,
                    ConfigTier::Medium,
                    false,
                ),
                model(
                    "llama3.3:70b",
                    131_072,
                    8_192,
                    0.0,
                    0.0,
                    ConfigTier::Medium,
                    false,
                ),
            ],
        },
    ]
}

fn provider_model(
    provider: &str,
    kind: ProviderKind,
    base_url: &str,
    auth_env: &str,
    headers: BTreeMap<String, String>,
    model: &ModelConfig,
    api_key: Option<String>,
) -> Model {
    let headers = headers
        .into_iter()
        .map(|(name, value)| (name, expand_env(&value).unwrap_or(value)))
        .collect();
    Model {
        provider: provider.into(),
        id: model.id.clone(),
        display_name: model.id.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        input_cost: model.input_cost,
        output_cost: model.output_cost,
        tier: model.tier,
        supports_thinking: model.supports_thinking,
        supports_tools: model.supports_tools,
        kind,
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        auth_env: auth_env.into(),
        headers,
    }
}

fn resolve_api_key(
    provider: &str,
    config: Option<&ProviderConfig>,
    preset_env: &str,
    auth: Option<&AuthStore>,
    local: bool,
) -> Result<Option<String>, ProviderError> {
    if let Some(config) = config {
        if let Some(key) = &config.api_key {
            return Ok(Some(
                expand_env(key).map_err(|e| ProviderError::Other(e.to_string()))?,
            ));
        }
        if let Some(var) = &config.api_key_env {
            if let Ok(key) = std::env::var(var) {
                if !key.is_empty() {
                    return Ok(Some(key));
                }
            }
        }
    }
    if let Ok(key) = std::env::var(preset_env) {
        if !key.is_empty() {
            return Ok(Some(key));
        }
    }
    if let Some(key) = auth.and_then(|store| store.get(provider)) {
        return Ok(Some(key.into()));
    }
    if local {
        return Ok(None);
    }
    Ok(None)
}

fn is_local(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.contains("://localhost")
        || lower.contains("://127.0.0.1")
        || lower.contains("://0.0.0.0")
        || lower.contains("://[::1]")
}

pub fn all_models(config: &Config, auth: Option<&AuthStore>) -> Vec<Model> {
    let mut models = Vec::new();
    let presets = builtin_providers();
    for preset in &presets {
        let overrides = config.provider(preset.name);
        let base_url = overrides
            .and_then(|c| c.base_url.clone())
            .unwrap_or_else(|| preset.base_url.to_string());
        let base_url = expand_env(&base_url).unwrap_or(base_url);
        let headers = overrides.map(|c| c.headers.clone()).unwrap_or_default();
        let local = is_local(&base_url);
        let api_key = resolve_api_key(preset.name, overrides, preset.api_key_env, auth, local)
            .ok()
            .flatten();
        let mut merged = preset.models.clone();
        if let Some(overrides) = overrides {
            for custom in &overrides.models {
                if let Some(existing) = merged.iter_mut().find(|m| m.id == custom.id) {
                    *existing = custom.clone();
                } else {
                    merged.push(custom.clone());
                }
            }
        }
        for model in &merged {
            models.push(provider_model(
                preset.name,
                overrides.map(|c| c.kind).unwrap_or(preset.kind),
                &base_url,
                preset.api_key_env,
                headers.clone(),
                model,
                api_key.clone(),
            ));
        }
    }
    for (name, provider) in &config.providers {
        if presets.iter().any(|preset| preset.name == name) {
            continue;
        }
        let Some(base_url) = &provider.base_url else {
            continue;
        };
        let headers = provider.headers.clone();
        let local = is_local(base_url);
        let api_key = resolve_api_key(name, Some(provider), "KUDELMA_API_KEY", auth, local)
            .ok()
            .flatten();
        let env = provider
            .api_key_env
            .clone()
            .unwrap_or_else(|| "KUDELMA_API_KEY".into());
        for model in &provider.models {
            models.push(provider_model(
                name,
                provider.kind,
                base_url,
                &env,
                headers.clone(),
                model,
                api_key.clone(),
            ));
        }
    }
    models
}

pub fn resolve_model(
    config: &Config,
    auth: Option<&AuthStore>,
    spec: &str,
) -> Result<Model, ProviderError> {
    let (provider_name, model_id) = match spec.split_once('/') {
        Some((provider, model)) => (provider.to_string(), model.to_string()),
        None => {
            let default = if config.default_model.is_empty() {
                kudelma_config::DEFAULT_MODEL
            } else {
                &config.default_model
            };
            let provider = default
                .split_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| "anthropic".into());
            (provider, spec.to_string())
        }
    };

    if let Some(found) = all_models(config, auth)
        .into_iter()
        .find(|model| model.provider == provider_name && model.id == model_id)
    {
        return Ok(found);
    }

    let presets = builtin_providers();
    let preset = presets.iter().find(|p| p.name == provider_name);
    let overrides = config.provider(&provider_name);
    if preset.is_none() && overrides.is_none() {
        return Err(ProviderError::UnknownProvider(provider_name));
    }
    let base_url = overrides
        .and_then(|c| c.base_url.clone())
        .or_else(|| preset.map(|p| p.base_url.to_string()))
        .ok_or_else(|| ProviderError::UnknownProvider(provider_name.clone()))?;
    let base_url = expand_env(&base_url).unwrap_or(base_url);
    let kind = overrides
        .map(|c| c.kind)
        .or_else(|| preset.map(|p| p.kind))
        .unwrap_or(ProviderKind::Openai);
    let auth_env = preset
        .map(|p| p.api_key_env.to_string())
        .or_else(|| overrides.and_then(|c| c.api_key_env.clone()))
        .unwrap_or_else(|| "KUDELMA_API_KEY".into());
    let local = is_local(&base_url);
    let api_key = resolve_api_key(&provider_name, overrides, &auth_env, auth, local)?;
    let config_model = ModelConfig {
        id: model_id.clone(),
        ..ModelConfig::default()
    };
    Ok(provider_model(
        &provider_name,
        kind,
        &base_url,
        &auth_env,
        overrides.map(|c| c.headers.clone()).unwrap_or_default(),
        &config_model,
        api_key,
    ))
}

pub fn provider_for(model: &Model) -> Arc<dyn Provider> {
    match model.kind {
        ProviderKind::Anthropic => {
            Arc::new(drivers::anthropic::AnthropicProvider::new(model.clone()))
        }
        ProviderKind::Openai => Arc::new(drivers::openai::OpenAiProvider::new(model.clone())),
        ProviderKind::Gemini => Arc::new(drivers::gemini::GeminiProvider::new(model.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_and_nested_model_ids() {
        let config = Config::default();
        let model = resolve_model(&config, None, "openrouter/anthropic/claude-sonnet-4.5").unwrap();
        assert_eq!(model.provider, "openrouter");
        assert_eq!(model.id, "anthropic/claude-sonnet-4.5");
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let config = Config::default();
        assert!(resolve_model(&config, None, "nope/model").is_err());
    }

    #[test]
    fn local_models_do_not_require_a_key() {
        let config = Config::default();
        let model = resolve_model(&config, None, "ollama/qwen2.5-coder:32b").unwrap();
        assert_eq!(model.require_key().unwrap(), "");
        assert!(model.is_local());
    }
}
