//! Scripted provider used by tests and by future `--dry-run` demos. It answers
//! from a queue, so agent-loop behaviour is deterministic and free.

use crate::{ChatRequest, ChatResponse, Provider, ProviderError};
use async_trait::async_trait;
use std::sync::Mutex;

pub struct MockProvider {
    id: String,
    turns: Mutex<Vec<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl MockProvider {
    pub fn new(id: impl Into<String>, turns: Vec<ChatResponse>) -> Self {
        Self {
            id: id.into(),
            turns: Mutex::new(turns),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("lock").len()
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn protocol(&self) -> &'static str {
        "mock"
    }

    fn endpoint(&self) -> &str {
        "mock://"
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.requests.lock().expect("lock").push(req);
        let mut turns = self.turns.lock().expect("lock");
        if turns.is_empty() {
            return Err(ProviderError::Request(
                "mock: no scripted turns left".into(),
            ));
        }
        Ok(turns.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scripted_turns_are_returned_in_order() {
        let mock = MockProvider::new("m", vec![ChatResponse::default(), ChatResponse::default()]);
        let req = || ChatRequest {
            model: "x".into(),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 0,
            temperature: 0.0,
        };
        mock.chat(req()).await.expect("first");
        mock.chat(req()).await.expect("second");
        assert!(mock.chat(req()).await.is_err());
        assert_eq!(mock.request_count(), 3);
    }
}
