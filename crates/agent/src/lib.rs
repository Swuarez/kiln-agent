//! The agent runtime: one turn in, tool calls out, repeat until the model stops
//! asking for tools. Routing, permissions and context all live behind small
//! traits so MCP tools, plugin tools and a TUI can be added without touching
//! this loop.

pub mod context;
pub mod prompt;
pub mod subagent;

use async_trait::async_trait;
use kiln_core::config::Config;
use kiln_core::permission::{danger_reason, decide, Decision};
use kiln_core::{Error, Message, Result, Risk, Session, ToolCall};
use kiln_provider::{ChatRequest, ChatResponse, ProviderError, Registry, ToolSchema, Usage};
use kiln_router::{Failure, Router};
use kiln_tools::{ToolCtx, ToolRegistry};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    ModelSelected {
        provider: String,
        model: String,
        attempt: usize,
    },
    Text(String),
    ToolStart {
        name: String,
        arguments: serde_json::Value,
    },
    ToolEnd {
        name: String,
        ok: bool,
        summary: String,
        ms: u128,
    },
    Denied {
        name: String,
        reason: String,
    },
    ProviderError {
        provider: String,
        model: String,
        error: String,
    },
    Usage(Usage),
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool: String,
    pub risk: Risk,
    pub summary: String,
    /// Present when the call matched a destructive-command pattern.
    pub danger: Option<&'static str>,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: ApprovalRequest) -> bool;
}

/// `kiln run --yes`.
pub struct AutoApprove;

#[async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, _request: ApprovalRequest) -> bool {
        true
    }
}

/// Headless default: never blocks, never guesses. Denied calls come back to the
/// model as tool errors it can work around.
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _request: ApprovalRequest) -> bool {
        false
    }
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub text: String,
    pub steps: usize,
    pub usage: Usage,
    pub cost_usd: f64,
}

pub struct Agent {
    pub config: Arc<Config>,
    pub providers: Arc<Registry>,
    pub router: Arc<Router>,
    pub tools: Arc<ToolRegistry>,
    pub ctx: ToolCtx,
    pub model: String,
    pub max_tokens: u32,
    /// Where finished and in-flight sessions are written.
    pub sessions_dir: std::path::PathBuf,
    /// Skills, commands and memory injected into the system prompt.
    pub prompt_context: prompt::PromptContext,
}

impl Agent {
    pub fn new(
        config: Arc<Config>,
        providers: Arc<Registry>,
        router: Arc<Router>,
        tools: Arc<ToolRegistry>,
        ctx: ToolCtx,
        model: impl Into<String>,
    ) -> Self {
        Self {
            config,
            providers,
            router,
            tools,
            ctx,
            model: model.into(),
            max_tokens: 8192,
            sessions_dir: kiln_core::paths::sessions_dir(),
            prompt_context: prompt::PromptContext::default(),
        }
    }

    pub fn system_prompt(&self) -> String {
        prompt::system_prompt(
            &self.ctx.root,
            self.config.permissions.mode,
            &self.tools.names(),
            &self.prompt_context,
        )
    }

    /// Run one user task to completion. The session is saved after every step so
    /// an interrupted task can be resumed instead of restarted.
    pub async fn run(
        &self,
        session: &mut Session,
        task: &str,
        approver: &dyn Approver,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<RunOutcome> {
        let budget = self.config.agent.max_steps;
        self.run_with_budget(session, task, approver, budget, on_event)
            .await
    }

    /// Same loop with an explicit step budget. Sub-agents use this to get a
    /// tighter bound than the parent's.
    pub async fn run_with_budget(
        &self,
        session: &mut Session,
        task: &str,
        approver: &dyn Approver,
        max_steps: usize,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<RunOutcome> {
        if session.title == "untitled" {
            session.title = task
                .lines()
                .next()
                .unwrap_or("task")
                .chars()
                .take(60)
                .collect();
        }
        session.messages.push(Message::user(task));

        let mut usage = Usage::default();
        let mut cost_usd = 0.0_f64;
        let mut last_text = String::new();
        let max_steps = max_steps.max(1);

        for step in 0..max_steps {
            let fitted = context::fit(&session.messages, self.config.agent.context_budget_tokens);
            let response = match self.complete(&fitted, &mut *on_event).await {
                Ok((r, c)) => {
                    cost_usd += c;
                    r
                }
                Err(e) => {
                    // Persist whatever the user asked for, even when no model answered.
                    self.router.persist();
                    session.save(&self.sessions_dir)?;
                    return Err(e);
                }
            };
            usage.add(response.usage);
            on_event(AgentEvent::Usage(response.usage));
            last_text = response.text.clone();

            if !response.text.trim().is_empty() {
                on_event(AgentEvent::Text(response.text.clone()));
            }
            session.messages.push(Message::assistant_calls(
                response.text.clone(),
                response.tool_calls.clone(),
            ));

            if response.tool_calls.is_empty() {
                self.router.persist();
                session.save(&self.sessions_dir)?;
                return Ok(RunOutcome {
                    text: response.text,
                    steps: step + 1,
                    usage,
                    cost_usd,
                });
            }

            for call in &response.tool_calls {
                let result = self.execute_tool(call, approver, &mut *on_event).await;
                session
                    .messages
                    .push(Message::tool(call.id.clone(), result.0, result.1));
            }
            self.router.persist();
            session.save(&self.sessions_dir)?;
        }

        self.router.persist();
        session.save(&self.sessions_dir)?;
        Err(Error::Other(format!(
            "stopped after {max_steps} steps without finishing. The session is saved at {}.\nLast model message: {}",
            session.path(&self.sessions_dir).display(),
            last_text.chars().take(400).collect::<String>()
        )))
    }

    /// Ask each routed target in turn until one answers.
    async fn complete(
        &self,
        messages: &[Message],
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<(ChatResponse, f64)> {
        let targets = self.router.order(&self.model)?;
        let schemas: Vec<ToolSchema> = self
            .tools
            .specs()
            .into_iter()
            .map(|s| ToolSchema {
                name: s.name.to_string(),
                description: s.description.to_string(),
                parameters: s.parameters,
            })
            .collect();

        let mut errors: Vec<String> = Vec::new();
        for (attempt, target) in targets.iter().enumerate() {
            on_event(AgentEvent::ModelSelected {
                provider: target.provider.clone(),
                model: target.model.clone(),
                attempt,
            });
            let Some(provider) = self.providers.get(&target.provider) else {
                let msg = format!(
                    "provider '{}' is not usable (check its api_key_env)",
                    target.provider
                );
                on_event(AgentEvent::ProviderError {
                    provider: target.provider.clone(),
                    model: target.model.clone(),
                    error: msg.clone(),
                });
                errors.push(msg);
                continue;
            };

            let request = ChatRequest {
                model: target.model.clone(),
                system: Some(self.system_prompt()),
                messages: messages.to_vec(),
                tools: schemas.clone(),
                max_tokens: self.max_tokens,
                temperature: self.config.agent.temperature,
            };

            let started = Instant::now();
            match provider.chat(request).await {
                Ok(response) => {
                    let cost = target.input_cost_per_1k.unwrap_or(0.0)
                        * response.usage.input_tokens as f64
                        / 1000.0
                        + target.output_cost_per_1k.unwrap_or(0.0)
                            * response.usage.output_tokens as f64
                            / 1000.0;
                    self.router.record_success(target, started.elapsed());
                    return Ok((response, cost));
                }
                Err(e) => {
                    self.router.record_failure(target, classify(&e));
                    let msg = format!("{}: {e}", target.provider);
                    on_event(AgentEvent::ProviderError {
                        provider: target.provider.clone(),
                        model: target.model.clone(),
                        error: e.to_string(),
                    });
                    errors.push(msg);
                    if !e.worth_failing_over() {
                        break;
                    }
                }
            }
        }

        Err(Error::Other(format!(
            "no provider answered for model '{}':\n- {}",
            self.model,
            errors.join("\n- ")
        )))
    }

    /// Permission check, then the tool, then a bounded, model-friendly result.
    /// Returns (content, is_error).
    async fn execute_tool(
        &self,
        call: &ToolCall,
        approver: &dyn Approver,
        on_event: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> (String, bool) {
        let Some(tool) = self.tools.get(&call.name) else {
            let available = self.tools.names().join(", ");
            return (
                format!("unknown tool '{}'. Available tools: {available}", call.name),
                true,
            );
        };
        let spec = tool.spec();

        let danger = if spec.risk == Risk::Exec {
            call.arguments
                .get("command")
                .and_then(|v| v.as_str())
                .and_then(danger_reason)
        } else {
            None
        };
        let decision = decide(self.config.permissions.mode, spec.risk, danger);
        let summary = summarise(&call.name, &call.arguments);
        match decision {
            Decision::Allow => {}
            Decision::Deny(reason) => {
                on_event(AgentEvent::Denied {
                    name: call.name.clone(),
                    reason: reason.to_string(),
                });
                return (format!("denied: {reason}"), true);
            }
            Decision::Ask(reason) => {
                let approved = approver
                    .approve(ApprovalRequest {
                        tool: call.name.clone(),
                        risk: spec.risk,
                        summary: summary.clone(),
                        danger: danger.or(Some(reason)),
                    })
                    .await;
                if !approved {
                    on_event(AgentEvent::Denied {
                        name: call.name.clone(),
                        reason: reason.to_string(),
                    });
                    return (
                        format!("denied by the user: {reason}. Do not retry; find another approach or ask."),
                        true,
                    );
                }
            }
        }

        on_event(AgentEvent::ToolStart {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        let started = Instant::now();
        let timeout = self.ctx.shell_timeout.max(Duration::from_secs(30));
        let outcome =
            match tokio::time::timeout(timeout, tool.run(&call.arguments, &self.ctx)).await {
                Ok(outcome) => outcome,
                Err(_) => kiln_tools::ToolOutcome::err(format!(
                    "{} timed out after {}s",
                    call.name,
                    timeout.as_secs()
                )),
            };
        let ms = started.elapsed().as_millis();
        on_event(AgentEvent::ToolEnd {
            name: call.name.clone(),
            ok: !outcome.is_error,
            summary: truncate(&outcome.content, 300),
            ms,
        });
        (outcome.content, outcome.is_error)
    }
}

fn classify(e: &ProviderError) -> Failure {
    match e {
        ProviderError::RateLimited { retry_after_ms } => {
            Failure::RateLimited(retry_after_ms.map(Duration::from_millis))
        }
        ProviderError::Auth(_) => Failure::Auth,
        ProviderError::Request(_) => Failure::Network,
        ProviderError::Status { status, .. } => {
            if *status >= 500 {
                Failure::Server
            } else {
                Failure::Client
            }
        }
        ProviderError::Decode(_) => Failure::Client,
    }
}

/// Small helper so `classify` reads as one expression per case.
/// One line describing a tool call, for approval prompts and trace logs.
pub fn summarise(name: &str, args: &serde_json::Value) -> String {
    let pick = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "read_file" | "write_file" | "edit_file" | "list_dir" => pick("path").to_string(),
        "grep" => format!("{} in {}", pick("pattern"), pick("path")),
        "shell" => pick("command").to_string(),
        _ => args.to_string(),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests;
