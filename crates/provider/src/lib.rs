//! Provider abstraction. The agent runtime only ever sees `Provider`, so a new
//! vendor is a new implementation, never a change to the loop.

pub mod anthropic;
pub mod doctor;
pub mod mock;
pub mod openai;

use async_trait::async_trait;
use kiln_core::{Config, Message, ToolCall};
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a tool looks like to a model.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn protocol(&self) -> &'static str;
    /// URL `chat` posts to. Diagnostics only.
    fn endpoint(&self) -> &str;
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Model ids the provider reports, used by `kiln doctor` to catch typos
    /// before a task starts. Not every server implements the listing, so the
    /// default is an error rather than an empty list.
    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Err(ProviderError::Request(
            "this provider does not expose a model list".into(),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Request(String),
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("rate limited (retry after {retry_after_ms:?} ms)")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("could not decode response: {0}")]
    Decode(String),
}

impl ProviderError {
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_ms } => *retry_after_ms,
            _ => None,
        }
    }

    /// Whether switching to another provider could plausibly help.
    pub fn worth_failing_over(&self) -> bool {
        !matches!(self, Self::Auth(_))
    }
}

/// Builds one provider from config. Shared by the registry and `kiln doctor`,
/// so the two can never disagree about why a provider is unavailable.
pub fn build(
    id: &str,
    cfg: &kiln_core::config::ProviderConfig,
) -> Result<Arc<dyn Provider>, ProviderError> {
    use kiln_core::config::Protocol;
    match cfg.protocol {
        Protocol::OpenAiCompatible => openai::OpenAiCompat::new(id, cfg)
            .map(|p| Arc::new(p) as Arc<dyn Provider>),
        Protocol::Anthropic => {
            anthropic::Anthropic::new(id, cfg).map(|p| Arc::new(p) as Arc<dyn Provider>)
        }
    }
}

/// Every configured provider, keyed by config id. Missing credentials demote a
/// provider to a warning instead of a hard failure, so fallback chains still work.
pub struct Registry {
    providers: BTreeMap<String, Arc<dyn Provider>>,
    warnings: Vec<String>,
}

impl Registry {
    pub fn from_config(cfg: &Config) -> kiln_core::Result<Self> {
        let mut providers: BTreeMap<String, Arc<dyn Provider>> = BTreeMap::new();
        let mut warnings = Vec::new();

        for (id, p) in &cfg.providers {
            match build(id, p) {
                Ok(provider) => {
                    providers.insert(id.clone(), provider);
                }
                Err(e) => warnings.push(format!("provider '{id}' disabled: {e}")),
            }
        }

        Ok(Self {
            providers,
            warnings,
        })
    }

    /// Injected providers, for tests and for plugins that supply their own.
    pub fn from_providers(providers: impl IntoIterator<Item = Arc<dyn Provider>>) -> Self {
        Self {
            providers: providers
                .into_iter()
                .map(|p| {
                    let id = p.id().to_string();
                    (id, p)
                })
                .collect(),
            warnings: Vec::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<&str> {
        self.providers.keys().map(String::as_str).collect()
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// Trims error bodies so a provider's HTML error page never floods the terminal
/// or the model's context.
pub(crate) fn brief(body: &str) -> String {
    let cleaned = body.trim();
    if cleaned.chars().count() <= 800 {
        return cleaned.to_string();
    }
    let head: String = cleaned.chars().take(800).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_keeps_short_bodies_intact() {
        assert_eq!(brief(" nope "), "nope");
    }

    #[test]
    fn brief_caps_long_bodies() {
        let long = "x".repeat(5000);
        assert!(brief(&long).chars().count() < 900);
    }
}
