use crate::{paths, Error, Mode, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Anything speaking `/chat/completions`: OpenAI, DeepSeek, GLM, NIM,
    /// OpenRouter, Groq, llama.cpp, vLLM, Ollama, LM Studio.
    #[default]
    #[serde(rename = "openai-compatible", alias = "openai")]
    OpenAiCompatible,
    /// Anthropic's native `/v1/messages` shape.
    Anthropic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(default)]
    pub protocol: Protocol,
    pub base_url: String,
    /// Preferred: name of an environment variable holding the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Discouraged: only for local servers that demand a dummy key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Extra headers, e.g. OpenRouter's attribution headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_provider_timeout")]
    pub timeout_secs: u64,
}

fn default_provider_timeout() -> u64 {
    180
}

impl ProviderConfig {
    /// Reads the key from the environment. The value is never logged.
    pub fn api_key(&self, provider_id: &str) -> Result<Option<String>> {
        if let Some(var) = &self.api_key_env {
            return match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => Ok(Some(v)),
                _ => Err(Error::config(format!(
                    "environment variable {var} is not set (needed by provider '{provider_id}')"
                ))),
            };
        }
        if self.api_key.is_some() {
            tracing::warn!(
                provider = provider_id,
                "api_key is stored in plain config; prefer api_key_env"
            );
        }
        Ok(self.api_key.clone())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Try targets in the order written; health only pushes cooling providers back.
    #[default]
    Priority,
    /// Rotate across targets, repeating a target by its weight.
    RoundRobin,
    /// Order by observed latency and error rate.
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub provider: String,
    pub model: String,
    /// Round-robin share. Ignored by other strategies.
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

/// A name the agent asks for ("coding"), resolved to one or more provider+model
/// pairs. This is the seam that makes routing a config concern, not a code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalModel {
    #[serde(default)]
    pub strategy: Strategy,
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub mode: Mode,
}

impl Default for Permissions {
    fn default() -> Self {
        Self { mode: Mode::Ask }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Hard stop for tool-call loops. Prevents a confused model from spinning forever.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    /// Approximate token budget for the prompt, before provider-specific limits.
    #[serde(default = "default_context_budget")]
    pub context_budget_tokens: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_max_steps() -> usize {
    40
}

fn default_context_budget() -> usize {
    60_000
}

fn default_temperature() -> f32 {
    0.2
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_steps: default_max_steps(),
            context_budget_tokens: default_context_budget(),
            temperature: default_temperature(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default = "default_shell_timeout")]
    pub shell_timeout_secs: u64,
    #[serde(default = "default_max_output")]
    pub max_output_bytes: usize,
    #[serde(default = "default_max_grep")]
    pub max_grep_results: usize,
}

fn default_shell_timeout() -> u64 {
    120
}

fn default_max_output() -> usize {
    20_000
}

fn default_max_grep() -> usize {
    200
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            shell_timeout_secs: default_shell_timeout(),
            max_output_bytes: default_max_output(),
            max_grep_results: default_max_grep(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Name of the logical model used when nothing more specific applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, LogicalModel>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// Same shape with everything optional, so a project file only overrides the
/// keys it actually mentions.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    default_model: Option<String>,
    providers: Option<BTreeMap<String, ProviderConfig>>,
    models: Option<BTreeMap<String, LogicalModel>>,
    permissions: Option<Permissions>,
    agent: Option<AgentConfig>,
    tools: Option<ToolsConfig>,
}

impl Config {
    pub fn load(cwd: &Path) -> Result<Self> {
        if let Ok(explicit) = std::env::var("KILN_CONFIG") {
            let path = PathBuf::from(explicit);
            let mut cfg = Config::default();
            cfg.apply(read_raw(&path)?);
            cfg.validate()?;
            return Ok(cfg);
        }

        let mut cfg = Config::default();
        for path in [paths::global_config_path(), paths::project_config_path(cwd)] {
            if path.exists() {
                cfg.apply(read_raw(&path)?);
                tracing::debug!(path = %path.display(), "loaded config");
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply(&mut self, raw: RawConfig) {
        if raw.default_model.is_some() {
            self.default_model = raw.default_model;
        }
        if let Some(providers) = raw.providers {
            self.providers.extend(providers);
        }
        if let Some(models) = raw.models {
            self.models.extend(models);
        }
        if let Some(p) = raw.permissions {
            self.permissions = p;
        }
        if let Some(a) = raw.agent {
            self.agent = a;
        }
        if let Some(t) = raw.tools {
            self.tools = t;
        }
    }

    /// Fails loudly on typos and dangling references. A config error that only
    /// shows up as a 404 mid-task costs far more than a clear message at startup.
    pub fn validate(&self) -> Result<()> {
        for (id, p) in &self.providers {
            if !p.base_url.starts_with("http://") && !p.base_url.starts_with("https://") {
                return Err(Error::config(format!(
                    "provider '{id}': base_url must start with http:// or https://"
                )));
            }
            if p.api_key.is_some() && p.api_key_env.is_some() {
                return Err(Error::config(format!(
                    "provider '{id}': set either api_key_env or api_key, not both"
                )));
            }
        }
        for (name, model) in &self.models {
            if model.targets.is_empty() {
                return Err(Error::config(format!("model '{name}' has no targets")));
            }
            for t in &model.targets {
                if !self.providers.contains_key(&t.provider) {
                    return Err(Error::config(format!(
                        "model '{name}' targets unknown provider '{}'",
                        t.provider
                    )));
                }
                if t.model.trim().is_empty() {
                    return Err(Error::config(format!(
                        "model '{name}': target for provider '{}' has an empty model id",
                        t.provider
                    )));
                }
                if t.weight == 0 {
                    return Err(Error::config(format!(
                        "model '{name}': weight must be at least 1"
                    )));
                }
            }
        }
        if let Some(default) = &self.default_model {
            if !self.models.contains_key(default) {
                return Err(Error::config(format!(
                    "default_model '{default}' is not defined under [models]"
                )));
            }
        }
        if self.agent.max_steps == 0 || self.agent.max_steps > 500 {
            return Err(Error::config("agent.max_steps must be between 1 and 500"));
        }
        if self.agent.context_budget_tokens < 2_000 {
            return Err(Error::config(
                "agent.context_budget_tokens must be at least 2000",
            ));
        }
        Ok(())
    }

    /// The logical model to use for a turn.
    pub fn model_name(&self, requested: Option<&str>) -> Result<String> {
        if let Some(name) = requested {
            if !self.models.contains_key(name) {
                return Err(Error::config(format!(
                    "unknown model '{name}'; configured: {}",
                    self.model_names().join(", ")
                )));
            }
            return Ok(name.to_string());
        }
        if let Some(name) = &self.default_model {
            return Ok(name.clone());
        }
        if self.models.contains_key("default") {
            return Ok("default".to_string());
        }
        Err(Error::config(
            "no model selected: set default_model in config or pass --model",
        ))
    }

    pub fn model(&self, name: &str) -> Result<&LogicalModel> {
        self.models.get(name).ok_or_else(|| {
            Error::config(format!(
                "unknown model '{name}'; configured: {}",
                self.model_names().join(", ")
            ))
        })
    }

    pub fn model_names(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }
}

fn read_raw(path: &Path) -> Result<RawConfig> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    toml::from_str(&text).map_err(|e| Error::config(format!("{}: {e}", path.display())))
}

/// Starter config written by `kiln config init`.
pub const TEMPLATE: &str = r#"# Kiln configuration
# Global: ~/.kiln/config.toml   Project: <repo>/.kiln/config.toml
# Project values override global values, key by key.

default_model = "default"

# --- Providers -------------------------------------------------------------
# A provider is a URL plus a protocol. Anything OpenAI-compatible works here.

[providers.anthropic]
protocol = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openai]
protocol = "openai-compatible"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

# Local models: no key needed.
[providers.ollama]
protocol = "openai-compatible"
base_url = "http://localhost:11434/v1"

# One-line additions for other OpenAI-compatible services:
# [providers.deepseek]
# base_url = "https://api.deepseek.com/v1"
# api_key_env = "DEEPSEEK_API_KEY"
#
# [providers.openrouter]
# base_url = "https://openrouter.ai/api/v1"
# api_key_env = "OPENROUTER_API_KEY"
# headers = { "HTTP-Referer" = "https://example.com" }
#
# [providers.nvidia]
# base_url = "https://integrate.api.nvidia.com/v1"
# api_key_env = "NVIDIA_API_KEY"

# --- Logical models --------------------------------------------------------
# A name the agent asks for. Targets are tried top to bottom, so this is also
# the fallback list: first healthy provider wins, next one covers a 429.

[models.default]
strategy = "priority"

[[models.default.targets]]
provider = "anthropic"
model = "claude-sonnet-4-5"

[[models.default.targets]]
provider = "openai"
model = "gpt-5"

[[models.default.targets]]
provider = "ollama"
model = "qwen2.5-coder:14b"

# Route work by task by pointing the agent at a different logical model:
# [models.cheap]
# strategy = "auto"
# [[models.cheap.targets]]
# provider = "ollama"
# model = "qwen2.5-coder:7b"

# --- Permissions -----------------------------------------------------------
[permissions]
mode = "ask"   # ask | allow | readonly

[agent]
max_steps = 40
context_budget_tokens = 60000
temperature = 0.2

[tools]
shell_timeout_secs = 120
max_output_bytes = 20000
max_grep_results = 200
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        let mut cfg = Config::default();
        cfg.providers.insert(
            "p".into(),
            ProviderConfig {
                protocol: Protocol::OpenAiCompatible,
                base_url: "https://example.com/v1".into(),
                api_key_env: None,
                api_key: None,
                headers: BTreeMap::new(),
                timeout_secs: 30,
            },
        );
        cfg.models.insert(
            "default".into(),
            LogicalModel {
                strategy: Strategy::Priority,
                targets: vec![Target {
                    provider: "p".into(),
                    model: "m".into(),
                    weight: 1,
                }],
            },
        );
        cfg.default_model = Some("default".into());
        cfg
    }

    #[test]
    fn template_parses_and_validates() {
        let raw: RawConfig = toml::from_str(TEMPLATE).expect("template is valid toml");
        let mut cfg = Config::default();
        cfg.apply(raw);
        cfg.validate().expect("template passes validation");
        assert_eq!(cfg.model_name(None).unwrap(), "default");
    }

    #[test]
    fn dangling_provider_is_rejected() {
        let mut cfg = base();
        cfg.models.get_mut("default").unwrap().targets[0].provider = "nope".into();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("unknown provider"), "{err}");
    }

    #[test]
    fn missing_default_model_is_rejected() {
        let mut cfg = base();
        cfg.default_model = Some("ghost".into());
        assert!(cfg.validate().unwrap_err().to_string().contains("ghost"));
    }

    #[test]
    fn project_file_overrides_only_mentioned_keys() {
        let mut cfg = base();
        let raw: RawConfig = toml::from_str("[permissions]\nmode = \"readonly\"").unwrap();
        cfg.apply(raw);
        assert_eq!(cfg.permissions.mode, Mode::Readonly);
        assert_eq!(cfg.agent.max_steps, 40);
        assert_eq!(cfg.models.len(), 1);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<RawConfig>("mystery = 1").unwrap_err();
        assert!(err.to_string().contains("mystery"), "{err}");
    }

    #[test]
    fn api_key_env_missing_is_reported_without_leaking() {
        std::env::remove_var("KILN_TEST_ABSENT_KEY");
        let p = ProviderConfig {
            protocol: Protocol::Anthropic,
            base_url: "https://example.com".into(),
            api_key_env: Some("KILN_TEST_ABSENT_KEY".into()),
            api_key: None,
            headers: BTreeMap::new(),
            timeout_secs: 30,
        };
        let err = p.api_key("anthropic").unwrap_err().to_string();
        assert!(err.contains("KILN_TEST_ABSENT_KEY"), "{err}");
    }
}
