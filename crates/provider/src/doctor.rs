//! `kiln doctor`: check each configured provider for a resolvable key, a
//! reachable endpoint and model ids that actually exist. The point is to find out
//! before a task starts, rather than discovering a 404 mid-loop.

use crate::build;
use kiln_core::Config;
use kiln_core::config::ProviderConfig;
use std::collections::BTreeSet;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct ProviderCheck {
    pub id: String,
    pub protocol: String,
    pub endpoint: String,
    /// True when the provider could be built and the endpoint responded.
    pub ready: bool,
    /// Why it is not ready, or a listing limitation (e.g. no /models endpoint).
    pub problem: Option<String>,
    pub latency_ms: u128,
    /// How many models the provider reported (0 if listing failed).
    pub model_count: usize,
    /// A short sample of the reported model ids.
    pub models: Vec<String>,
    /// Configured model ids the provider did not list.
    pub missing_models: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub checks: Vec<ProviderCheck>,
    pub usable_providers: usize,
    pub logical_models: usize,
    pub broken_logical_models: Vec<String>,
}

pub async fn run(cfg: &Config) -> Report {
    let mut checks = Vec::new();
    for (id, pcfg) in &cfg.providers {
        checks.push(check(id, pcfg, cfg).await);
    }
    checks.sort_by(|a, b| a.id.cmp(&b.id));

    let usable: BTreeSet<String> = checks
        .iter()
        .filter(|c| c.ready)
        .map(|c| c.id.clone())
        .collect();

    let mut broken = Vec::new();
    for (name, model) in &cfg.models {
        if !model
            .targets
            .iter()
            .any(|t| usable.contains(&t.provider))
        {
            broken.push(name.clone());
        }
    }

    let usable_providers = usable.len();
    Report {
        checks,
        usable_providers,
        logical_models: cfg.models.len(),
        broken_logical_models: broken,
    }
}

async fn check(id: &str, pcfg: &ProviderConfig, cfg: &Config) -> ProviderCheck {
    let mut check = ProviderCheck {
        id: id.to_string(),
        protocol: protocol_name(pcfg),
        endpoint: pcfg.base_url.clone(),
        ready: false,
        problem: None,
        latency_ms: 0,
        model_count: 0,
        models: Vec::new(),
        missing_models: Vec::new(),
    };

    let wanted: Vec<String> = cfg
        .models
        .values()
        .flat_map(|m| m.targets.iter())
        .filter(|t| t.provider == id)
        .map(|t| t.model.clone())
        .collect();

    let provider = match build(id, pcfg) {
        Ok(p) => p,
        Err(e) => {
            check.problem = Some(e.to_string());
            return check;
        }
    };
    check.endpoint = provider.endpoint().to_string();
    check.protocol = provider.protocol().to_string();

    let started = Instant::now();
    match provider.list_models().await {
        Ok(models) => {
            check.latency_ms = started.elapsed().as_millis();
            check.ready = true;
            check.model_count = models.len();
            check.missing_models = wanted
                .iter()
                .filter(|w| !models.iter().any(|m| m == *w))
                .cloned()
                .collect();
            // Keep the sample small: a provider listing 500 models would flood
            // the terminal and nothing useful is gained past a handful.
            check.models = models.into_iter().take(8).collect();
        }
        Err(e) => {
            check.latency_ms = started.elapsed().as_millis();
            // A 404 on /models is common on minimal servers; the chat endpoint
            // may still work, so mark the provider ready with a caveat.
            if let crate::ProviderError::Status { status, .. } = &e {
                if *status == 404 {
                    check.ready = true;
                    check.problem = Some("no model listing endpoint (/models)".to_string());
                    return check;
                }
            }
            check.problem = Some(e.to_string());
        }
    }
    check
}

fn protocol_name(pcfg: &ProviderConfig) -> String {
    match pcfg.protocol {
        kiln_core::config::Protocol::OpenAiCompatible => "openai-compatible".to_string(),
        kiln_core::config::Protocol::Anthropic => "anthropic".to_string(),
    }
}
