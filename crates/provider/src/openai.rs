//! OpenAI-compatible `/chat/completions`. One implementation covers OpenAI,
//! DeepSeek, GLM, NIM, OpenRouter, Groq, llama.cpp, vLLM, Ollama and LM Studio.

use crate::{brief, ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use async_trait::async_trait;
use kiln_core::config::ProviderConfig;
use kiln_core::{Message, ToolCall};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

pub struct OpenAiCompat {
    id: String,
    client: reqwest::Client,
    url: String,
    models_url: String,
    api_key: Option<String>,
    headers: BTreeMap<String, String>,
}

impl OpenAiCompat {
    pub fn new(id: &str, cfg: &ProviderConfig) -> Result<Self, ProviderError> {
        let api_key = cfg
            .api_key(id)
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let base = cfg.base_url.trim_end_matches('/');
        Ok(Self {
            id: id.to_string(),
            client,
            url: format!("{base}/chat/completions"),
            models_url: format!("{base}/models"),
            api_key,
            headers: cfg.headers.clone(),
        })
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.get(url);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        builder
    }
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        &self.id
    }

    fn protocol(&self) -> &'static str {
        "openai-compatible"
    }

    fn endpoint(&self) -> &str {
        &self.url
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let resp = self
            .request(&self.models_url)
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

        let mut builder = self.client.post(&self.url).json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }
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
            // Some gateways return 429 without a body worth showing.
            return Err(ProviderError::RateLimited { retry_after_ms });
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(ProviderError::Auth(brief(&text)));
        }
        if !status.is_success() {
            return Err(ProviderError::Status {
                status: status.as_u16(),
                body: brief(&text),
            });
        }

        let parsed: RawResponse = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Decode(format!("{e}: {}", brief(&text))))?;
        Ok(parsed.into_chat_response())
    }
}

/// Separated so the wire format is testable without a network.
pub(crate) fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = &req.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(req.messages.iter().map(openai_message));

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": false,
        "temperature": req.temperature,
    });
    if req.max_tokens > 0 {
        body["max_tokens"] = json!(req.max_tokens);
    }
    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            }))
            .collect::<Vec<_>>());
        body["tool_choice"] = json!("auto");
    }
    body
}

fn openai_message(msg: &Message) -> Value {
    match msg {
        Message::System { content } => json!({"role": "system", "content": content}),
        Message::User { content } => json!({"role": "user", "content": content}),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": c.arguments.to_string(),
                        }
                    })
                })
                .collect();
            if calls.is_empty() {
                json!({"role": "assistant", "content": content})
            } else {
                json!({"role": "assistant", "content": content, "tool_calls": calls})
            }
        }
        Message::Tool {
            tool_call_id,
            content,
            ..
        } => json!({"role": "tool", "tool_call_id": tool_call_id, "content": content}),
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
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawChoice {
    message: RawMessage,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Deserialize)]
struct RawToolCall {
    #[serde(default)]
    id: String,
    function: RawFunction,
}

#[derive(Deserialize)]
struct RawFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

impl RawResponse {
    fn into_chat_response(self) -> ChatResponse {
        let usage = self
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default();

        let Some(choice) = self.choices.into_iter().next() else {
            return ChatResponse {
                text: String::new(),
                tool_calls: Vec::new(),
                usage,
            };
        };

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, c)| ToolCall {
                id: if c.id.is_empty() {
                    format!("call_{i}")
                } else {
                    c.id
                },
                // Some open models emit malformed JSON arguments. Keep the raw
                // string so the tool returns an error the model can correct,
                // instead of killing the whole turn.
                arguments: serde_json::from_str(&c.function.arguments)
                    .unwrap_or(Value::String(c.function.arguments)),
                name: c.function.name,
            })
            .collect();

        ChatResponse {
            text: choice.message.content.unwrap_or_default(),
            tool_calls,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSchema;

    fn request() -> ChatRequest {
        ChatRequest {
            model: "test-model".into(),
            system: Some("be terse".into()),
            messages: vec![
                Message::user("read main.rs"),
                Message::assistant_calls(
                    "",
                    vec![ToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path": "main.rs"}),
                    }],
                ),
                Message::tool("call_1", "fn main() {}", false),
            ],
            tools: vec![ToolSchema {
                name: "read_file".into(),
                description: "read".into(),
                parameters: json!({"type": "object"}),
            }],
            max_tokens: 2048,
            temperature: 0.2,
        }
    }

    #[test]
    fn body_contains_system_messages_and_tools() {
        let body = build_body(&request());
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"main.rs\"}"
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn parses_text_and_tool_calls() {
        let raw = r#"{
            "choices": [{"message": {
                "content": "let me look",
                "tool_calls": [{"id": "call_9", "type": "function",
                    "function": {"name": "grep", "arguments": "{\"pattern\":\"main\"}"}}]
            }}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }"#;
        let parsed: RawResponse = serde_json::from_str(raw).unwrap();
        let out = parsed.into_chat_response();
        assert_eq!(out.text, "let me look");
        assert_eq!(out.tool_calls.len(), 1);
        assert_eq!(out.tool_calls[0].arguments["pattern"], "main");
        assert_eq!(out.usage.input_tokens, 10);
    }

    #[test]
    fn malformed_tool_arguments_do_not_panic() {
        let raw = r#"{"choices":[{"message":{"content":"","tool_calls":[
            {"id":"c","function":{"name":"grep","arguments":"{oops"}}]}}]}"#;
        let out = serde_json::from_str::<RawResponse>(raw)
            .unwrap()
            .into_chat_response();
        assert!(out.tool_calls[0].arguments.is_string());
    }

    #[test]
    fn empty_choices_yield_empty_response() {
        let out = serde_json::from_str::<RawResponse>(r#"{"choices":[]}"#)
            .unwrap()
            .into_chat_response();
        assert!(out.text.is_empty() && out.tool_calls.is_empty());
    }

    /// End-to-end over a real socket: verifies the URL, auth header and JSON
    /// decoding that unit tests on `build_body` cannot cover.
    #[tokio::test]
    async fn sends_an_authenticated_post_and_decodes_the_reply() {
        use kiln_core::config::Protocol;
        use std::collections::BTreeMap;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut chunk = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..n]);
            }
            let body = r#"{"choices":[{"message":{"content":"hello from the mock"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&head).to_lowercase()
        });

        std::env::set_var("KILN_TEST_HTTP_KEY", "sk-test-123");
        let cfg = ProviderConfig {
            protocol: Protocol::OpenAiCompatible,
            base_url: format!("http://{addr}/v1"),
            api_key_env: Some("KILN_TEST_HTTP_KEY".into()),
            api_key: None,
            headers: BTreeMap::new(),
            timeout_secs: 5,
        };
        let provider = OpenAiCompat::new("local", &cfg).expect("provider");
        let out = provider.chat(request()).await.expect("chat");
        assert_eq!(out.text, "hello from the mock");

        let head = server.await.unwrap();
        assert!(head.contains("post /v1/chat/completions"), "{head}");
        assert!(head.contains("bearer sk-test-123"), "{head}");
    }
}
