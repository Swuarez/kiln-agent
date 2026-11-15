//! Live provider checks. These are `#[ignore]`d: they only run when you pass
//! `--ignored` and set the environment variables below. They exist so the adapters
//! get exercised against real endpoints without making the normal test suite
//! depend on a network or a paid key.
//!
//! Run one of them with:
//!
//! ```sh
//! KILN_LIVE_BASE_URL=https://api.openai.com/v1 \
//! KILN_LIVE_API_KEY_ENV=OPENAI_API_KEY \
//! KILN_LIVE_MODEL=gpt-4o-mini \
//! cargo test -p kiln-provider --test live -- --ignored openai_compatible
//! ```
//!
//! Replace the base URL, key env and model to target Anthropic or any other
//! OpenAI-compatible service.

use kiln_core::config::{Protocol, ProviderConfig};
use kiln_core::Message;
use kiln_provider::{build, ChatRequest};
use std::collections::BTreeMap;

fn live_config(protocol: Protocol) -> (String, ProviderConfig, String) {
    let base_url = std::env::var("KILN_LIVE_BASE_URL")
        .expect("KILN_LIVE_BASE_URL must be set, e.g. https://api.openai.com/v1");
    let key_env = std::env::var("KILN_LIVE_API_KEY_ENV")
        .expect("KILN_LIVE_API_KEY_ENV must be set, e.g. OPENAI_API_KEY");
    let model = std::env::var("KILN_LIVE_MODEL").expect("KILN_LIVE_MODEL must be set");
    let cfg = ProviderConfig {
        protocol,
        base_url,
        api_key_env: Some(key_env),
        api_key: None,
        headers: BTreeMap::new(),
        timeout_secs: 60,
    };
    (model, cfg, String::new())
}

#[tokio::test]
#[ignore = "needs KILN_LIVE_* env vars; see docs/TESTING.md"]
async fn openai_compatible_roundtrip() {
    let (model, cfg, _) = live_config(Protocol::OpenAiCompatible);
    let provider = build("live", &cfg).expect("provider builds");
    let models = provider.list_models().await.expect("model listing");
    assert!(!models.is_empty(), "provider listed no models");
    let resp = provider
        .chat(ChatRequest {
            model,
            system: Some("Reply with exactly one word and nothing else.".into()),
            messages: vec![Message::user("ready")],
            tools: vec![],
            max_tokens: 16,
            temperature: 0.0,
        })
        .await
        .expect("chat");
    assert!(!resp.text.trim().is_empty(), "empty reply: {resp:?}");
}

#[tokio::test]
#[ignore = "needs KILN_LIVE_* env vars; see docs/TESTING.md"]
async fn anthropic_roundtrip() {
    let (model, cfg, _) = live_config(Protocol::Anthropic);
    let provider = build("live", &cfg).expect("provider builds");
    let resp = provider
        .chat(ChatRequest {
            model,
            system: Some("Reply with exactly one word and nothing else.".into()),
            messages: vec![Message::user("ready")],
            tools: vec![],
            max_tokens: 16,
            temperature: 0.0,
        })
        .await
        .expect("chat");
    assert!(!resp.text.trim().is_empty(), "empty reply: {resp:?}");
}
