//! Sub-agents: spend context on an investigation without paying for it in the
//! main transcript.
//!
//! The parent gets a bounded summary; the child's messages are thrown away. That
//! is the whole point — a repo-wide search can cost 200k tokens of reading and
//! come back as 4k of findings.

use crate::{Agent, Approver, AutoApprove, DenyAll};
use async_trait::async_trait;
use kiln_core::config::{Config, SubagentConfig};
use kiln_core::Session;
use kiln_tools::{Tool, ToolCtx, ToolOutcome, ToolSpec};
use serde_json::{json, Value};
use std::sync::Arc;

/// Tools a sub-agent never gets. Delegation is not recursive: a child that can
/// spawn children is a fork bomb with a token bill.
const FORBIDDEN: &[&str] = &["delegate"];

pub struct Delegate {
    config: Arc<Config>,
    providers: Arc<kiln_provider::Registry>,
    router: Arc<kiln_router::Router>,
    /// The parent's tools *without* `delegate`, so a child cannot recurse.
    tools: Arc<kiln_tools::ToolRegistry>,
    ctx: ToolCtx,
    model: String,
    max_tokens: u32,
    sessions_dir: std::path::PathBuf,
    prompt_context: crate::prompt::PromptContext,
    settings: SubagentConfig,
}

impl Delegate {
    /// Snapshot everything a child needs from its parent. Holding the parts
    /// rather than the parent avoids an `Agent`-owns-tool / tool-owns-`Agent`
    /// reference cycle, which would otherwise leak or dangle.
    pub fn new(parent: &Agent) -> Self {
        Self {
            config: parent.config.clone(),
            providers: parent.providers.clone(),
            router: parent.router.clone(),
            tools: Arc::new(parent.tools.without(&["delegate".to_string()])),
            ctx: parent.ctx.clone(),
            model: parent.model.clone(),
            max_tokens: parent.max_tokens,
            sessions_dir: parent.sessions_dir.clone(),
            prompt_context: parent.prompt_context.clone(),
            settings: parent.config.agent.subagents.clone(),
        }
    }

    /// Build a child agent with its own tool subset, step budget and session.
    fn child(&self, tools: &[String]) -> Agent {
        let mut allowed: Vec<String> = if tools.is_empty() {
            self.tools.names()
        } else {
            tools.to_vec()
        };
        allowed.retain(|name| !FORBIDDEN.contains(&name.as_str()));

        // A child must never be able to edit when its mode says read-only.
        if self.settings.mode == kiln_core::Mode::Readonly {
            let readable: Vec<String> = self
                .tools
                .specs()
                .into_iter()
                .filter(|spec| spec.risk == kiln_core::Risk::Read)
                .map(|spec| spec.name)
                .collect();
            allowed.retain(|name| readable.contains(name));
        }

        let mut child = Agent::new(
            self.config.clone(),
            self.providers.clone(),
            self.router.clone(),
            Arc::new(self.tools.subset(&allowed)),
            self.ctx.clone(),
            if self.settings.model.is_empty() {
                self.model.clone()
            } else {
                self.settings.model.clone()
            },
        );
        child.max_tokens = self.max_tokens;
        child.sessions_dir = self.sessions_dir.clone();
        child.prompt_context = child_prompt_context(&self.prompt_context);
        child
    }
}
/// Children do not get skills or commands re-injected: the parent's summary is
/// the budget, and duplicating a catalogue in every child multiplies cost.
fn child_prompt_context(parent: &crate::prompt::PromptContext) -> crate::prompt::PromptContext {
    crate::prompt::PromptContext {
        skills: String::new(),
        commands: String::new(),
        memory: parent.memory.clone(),
    }
}

#[async_trait]
impl Tool for Delegate {
    fn spec(&self) -> ToolSpec {
        let default_tools = self
            .tools
            .specs()
            .into_iter()
            .filter(|spec| spec.risk == kiln_core::Risk::Read)
            .map(|spec| spec.name)
            .filter(|name| !FORBIDDEN.contains(&name.as_str()))
            .collect::<Vec<_>>()
            .join(", ");

        ToolSpec {
            name: "delegate".to_string(),
            description: format!(
                "Hand a self-contained research task to a sub-agent and get back a short \
                 summary. Use it to explore a large area without filling your own context, \
                 and for parallel investigations. The sub-agent cannot edit files and cannot \
                 see this conversation. At most {} characters come back. Default tools \
                 available to it: {default_tools}.",
                self.settings.max_summary_chars
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "What to find out. Be specific and self-contained."
                    },
                    "tools": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tool names to allow. Omit for the read-only defaults."
                    }
                },
                "required": ["task"]
            }),
            risk: kiln_core::Risk::Read,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        if !self.settings.enabled {
            return ToolOutcome::err("sub-agents are disabled (agent.subagents.enabled = false)");
        }
        let Some(task) = args.get("task").and_then(Value::as_str) else {
            return ToolOutcome::err("missing required argument 'task'");
        };
        if task.trim().is_empty() {
            return ToolOutcome::err("'task' is empty");
        }
        let requested_tools: Vec<String> = args
            .get("tools")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let child = self.child(&requested_tools);
        if child.tools.names().is_empty() {
            return ToolOutcome::err(
                "no usable tools for the sub-agent; check the tool names you passed",
            );
        }

        let mut session = Session::new(&ctx.root);
        // A child prompt inside a tool call cannot be answered, so the mode
        // decides outright: read-only denies, everything else approves.
        let approver: Box<dyn Approver> = if self.settings.mode == kiln_core::Mode::Readonly {
            Box::new(DenyAll)
        } else {
            Box::new(AutoApprove)
        };

        let started = std::time::Instant::now();
        let budget = self.settings.max_steps;
        match child
            .run_with_budget(&mut session, task, approver.as_ref(), budget, &mut |_| {})
            .await
        {
            Ok(outcome) => {
                let summary = clip(&outcome.text, self.settings.max_summary_chars);
                let body = if summary.trim().is_empty() {
                    "(the sub-agent finished without producing a summary)".to_string()
                } else {
                    summary
                };
                ToolOutcome::ok(format!(
                    "sub-agent finished in {} step(s), {} ms\n\n{}",
                    outcome.steps,
                    started.elapsed().as_millis(),
                    body
                ))
            }
            Err(e) => ToolOutcome::err(format!("sub-agent failed: {e}")),
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.trim().to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!(
        "{}\n\n[summary truncated at {max} characters]",
        head.trim_end()
    )
}

/// Run several sub-agent tasks concurrently, respecting the configured cap.
pub async fn run_parallel(
    config: Arc<Config>,
    parent: Arc<Agent>,
    tasks: Vec<String>,
    tools: Vec<String>,
) -> Vec<String> {
    let _ = config;
    let delegate = Arc::new(Delegate::new(&parent));
    let limit = delegate.settings.max_concurrency.max(1);
    let ctx = parent.ctx.clone();

    let mut results = Vec::with_capacity(tasks.len());
    for chunk in tasks.chunks(limit) {
        let mut joins = Vec::new();
        for task in chunk {
            let d = delegate.clone();
            let ctx = ctx.clone();
            let task = task.clone();
            let tools = tools.clone();
            joins.push(tokio::spawn(async move {
                d.run(&json!({ "task": task, "tools": tools }), &ctx)
                    .await
                    .content
            }));
        }
        for join in joins {
            results.push(
                join.await
                    .unwrap_or_else(|e| format!("sub-agent task panicked: {e}")),
            );
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_delegate() -> (Delegate, Arc<Agent>) {
        use kiln_core::config::{LogicalModel, Protocol, ProviderConfig, Strategy, Target};
        use kiln_router::Router;
        use kiln_tools::ToolRegistry;
        use std::collections::BTreeMap;

        let mut config = Config::default();
        config.providers.insert(
            "p".into(),
            ProviderConfig {
                protocol: Protocol::OpenAiCompatible,
                base_url: "http://localhost:1/v1".into(),
                api_key_env: None,
                api_key: Some("x".into()),
                headers: BTreeMap::new(),
                timeout_secs: 1,
            },
        );
        config.models.insert(
            "default".into(),
            LogicalModel {
                strategy: Strategy::Priority,
                targets: vec![Target {
                    provider: "p".into(),
                    model: "m".into(),
                    weight: 1,
                    capabilities: Vec::new(),
                    input_cost_per_1k: None,
                    output_cost_per_1k: None,
                }],
                required_capabilities: Vec::new(),
            },
        );
        config.default_model = Some("default".into());

        let config = Arc::new(config);
        let router = Arc::new(Router::new(config.models.clone()));
        let ctx = ToolCtx::new(&std::env::temp_dir(), &config.tools).unwrap();
        let agent = Arc::new(Agent::new(
            config.clone(),
            Arc::new(kiln_provider::Registry::from_providers(vec![])),
            router,
            Arc::new(ToolRegistry::coding()),
            ctx,
            "default",
        ));
        (Delegate::new(&agent), agent)
    }

    #[test]
    fn clips_long_summaries() {
        let out = clip(&"x".repeat(100), 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("truncated at 10 characters"));
    }

    #[test]
    fn short_summaries_pass_through_trimmed() {
        assert_eq!(clip("  hello  ", 100), "hello");
    }

    #[test]
    fn tool_spec_states_the_budget_and_the_limit() {
        let (delegate, _parent) = test_delegate();
        let spec = delegate.spec();
        assert!(spec.description.contains("characters come back"));
        assert!(spec.description.contains("cannot edit files"));
    }

    #[test]
    fn readonly_children_never_get_write_tools() {
        let (delegate, _parent) = test_delegate();

        let child = delegate.child(&[]);
        assert!(!child.tools.names().is_empty(), "fixture found no tools");
        for name in child.tools.names() {
            let spec = child.tools.get(&name).unwrap().spec();
            assert_eq!(
                spec.risk,
                kiln_core::Risk::Read,
                "{name} is not read-only but reached a read-only child"
            );
        }
    }

    #[test]
    fn children_cannot_delegate_again() {
        let (delegate, _parent) = test_delegate();

        let child = delegate.child(&["delegate".to_string(), "read_file".to_string()]);
        assert!(!child.tools.names().contains(&"delegate".to_string()));
        assert!(child.tools.names().contains(&"read_file".to_string()));
    }

    #[test]
    fn requested_tool_subset_is_honoured() {
        let (delegate, _parent) = test_delegate();

        let child = delegate.child(&["read_file".to_string()]);
        assert_eq!(child.tools.names(), vec!["read_file".to_string()]);
    }

    #[test]
    fn unknown_tool_names_are_ignored_not_fatal() {
        let (delegate, _parent) = test_delegate();

        let child = delegate.child(&["read_file".to_string(), "does_not_exist".to_string()]);
        assert_eq!(child.tools.names(), vec!["read_file".to_string()]);
    }

    #[test]
    fn child_does_not_inherit_the_skill_catalogue() {
        let parent_ctx = crate::prompt::PromptContext {
            skills: "- pdf: fill forms".into(),
            commands: "/review: review".into(),
            memory: "repo is a monorepo".into(),
        };
        let child_ctx = child_prompt_context(&parent_ctx);
        assert!(child_ctx.skills.is_empty());
        assert!(child_ctx.commands.is_empty());
        assert_eq!(child_ctx.memory, "repo is a monorepo");
    }

    #[tokio::test]
    async fn disabled_subagents_report_cleanly() {
        let (mut delegate, _parent) = test_delegate();
        delegate.settings.enabled = false;
        let ctx = ToolCtx::new(&std::env::temp_dir(), &Config::default().tools).unwrap();
        let out = delegate
            .run(&json!({"task": "find the auth middleware"}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("disabled"));
    }
}
