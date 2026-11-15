//! Anthropic's native `/v1/messages` protocol. It differs enough from the
//! OpenAI shape (system prompt is a top-level field, tool calls are content
//! blocks, tool results are user turns) that a real adapter beats pretending.

use crate::{brief, ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use async_trait::async_trait;
use kiln_core::config::ProviderConfig;
use kiln_core::{Message, ToolCall};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct Anthropic {
    id: String,
    client: reqwest::Client,
    url: String,
    models_url: String,
    api_key: String,
    headers: BTreeMap<String, String>,
}

impl Anthropic {
    pub fn new(id: &str, cfg: &ProviderConfig) -> Result<Self, ProviderError> {
        let api_key = cfg
            .api_key(id)
            .map_err(|e| ProviderError::Request(e.to_string()))?
            .ok_or_else(|| ProviderError::Request("anthropic requires an API key".into()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        Ok(Self {
            id: id.to_string(),
            client,
            url: endpoint(&cfg.base_url, "/v1/messages"),
            models_url: endpoint(&cfg.base_url, "/v1/models"),
            api_key,
            headers: cfg.headers.clone(),
        })
    }
}

/// Accepts bases with or without `/v1` so a pasted URL does not 404.
pub(crate) fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}{}", path.strip_prefix("/v1").unwrap_or(path))
    } else {
        format!("{base}{path}")
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &str {
        &self.id
    }

    fn protocol(&self) -> &'static str {
        "anthropic"
    }

    fn endpoint(&self) -> &str {
        &self.url
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let mut builder = self
            .client
            .get(&self.models_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ProviderError::Auth(brief(&text)));
        }
        if !status.is_success() {
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body: brief(&text),
            });
        }
        let parsed: RawModelList = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Decode(format!("{e}: {}", brief(&text))))?;
        Ok(parsed.data.into_iter().map(|m| m.id).collect())
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let body = build_body(&req);
        let mut builder = self
            .client
            .post(&self.url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }

        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Request("request timed out".into())
            } else if e.is_connect() {
                ProviderError::Request(format!("connection failed: {e}"))
            } else {
                ProviderError::Request(e.to_string())
            }
        })?;

        let status = resp.status();
        let retry_after_ms = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|secs| secs * 1000);
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if status.as_u16() == 429 {
            return Err(ProviderError::RateLimited { retry_after_ms });
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ProviderError::Auth(brief(&text)));
        }
        if !status.is_success() {
            // Anthropic nests the useful message in {"error":{"message":...}}.
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| brief(&text));
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body: detail,
            });
        }

        let parsed: RawResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Decode(format!("{e}: {}", brief(&text))))?;
        Ok(parsed.into_chat_response())
    }
}

pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "max_tokens": if req.max_tokens == 0 { 8192 } else { req.max_tokens },
        "temperature": req.temperature,
        "messages": build_messages(&req.messages),
    });
    if let Some(system) = &req.system {
        body["system"] = json!(system);
    }
    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            }))
            .collect::<Vec<_>>());
    }
    body
}

/// Tool results must arrive as a single user turn of `tool_result` blocks, and
/// nothing may precede the first user turn, so grouping is not optional.
pub(crate) fn build_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::System { .. } => {}
            Message::User { content } => {
                flush_results(&mut out, &mut pending_results);
                out.push(json!({"role": "user", "content": content}));
            }
            Message::Assistant {
                content,
                tool_calls,
            } => {
                flush_results(&mut out, &mut pending_results);
                let mut blocks: Vec<Value> = Vec::new();
                if !content.is_empty() {
                    blocks.push(json!({"type": "text", "text": content}));
                }
                for call in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
                is_error,
            } => {
                pending_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": content,
                    "is_error": is_error,
                }));
            }
        }
    }
    flush_results(&mut out, &mut pending_results);
    out
}

fn flush_results(out: &mut Vec<Value>, pending: &mut Vec<Value>) {
    if !pending.is_empty() {
        out.push(json!({"role": "user", "content": std::mem::take(pending)}));
    }
}

#[derive(Deserialize)]
struct RawModelList {
    #[serde(default)]
    data: Vec<RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    id: String,
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    content: Vec<RawBlock>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum RawBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

impl RawResponse {
    fn into_chat_response(self) -> ChatResponse {
        let mut text: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for block in self.content {
            match block {
                RawBlock::Text { text: t } => text.push(t),
                RawBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments: input,
                }),
                RawBlock::Other => {}
            }
        }
        ChatResponse {
            text: text.join("\n"),
            tool_calls,
            usage: self
                .usage
                .map(|u| Usage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                })
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_with_or_without_v1_hits_the_same_place() {
        assert_eq!(
            endpoint("https://api.anthropic.com", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://api.anthropic.com/v1/", "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn tool_results_are_grouped_into_one_user_turn() {
        let messages = vec![
            Message::user("go"),
            Message::assistant_calls(
                "working",
                vec![
                    ToolCall {
                        id: "t1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "a"}),
                    },
                    ToolCall {
                        id: "t2".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "b"}),
                    },
                ],
            ),
            Message::tool("t1", "a", false),
            Message::tool("t2", "b", false),
        ];
        let out = build_messages(&messages);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["content"][1]["type"], "tool_use");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(out[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn system_prompt_is_top_level_not_a_message() {
        let req = ChatRequest {
            model: "m".into(),
            system: Some("rules".into()),
            messages: vec![Message::user("hi")],
            tools: vec![],
            max_tokens: 0,
            temperature: 0.2,
        };
        let body = build_body(&req);
        assert_eq!(body["system"], "rules");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn parses_mixed_content_blocks() {
        let raw = r#"{"content":[
            {"type":"text","text":"thinking"},
            {"type":"tool_use","id":"toolu_1","name":"grep","input":{"pattern":"x"}},
            {"type":"some_future_block"}
        ],"usage":{"input_tokens":7,"output_tokens":3}}"#;
        let out = serde_json::from_str::<RawResponse>(raw)
            .unwrap()
            .into_chat_response();
        assert_eq!(out.text, "thinking");
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].arguments["pattern"], "x");
        assert_eq!(out.usage.output_tokens, 3);
    }
}
