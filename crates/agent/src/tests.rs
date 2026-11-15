use super::*;
use kiln_core::config::{LogicalModel, Protocol, ProviderConfig, Strategy, Target, ToolsConfig};
use kiln_core::Mode;
use kiln_provider::mock::MockProvider;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kiln-agent-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn target(provider: &str) -> Target {
    Target {
        provider: provider.into(),
        model: "m".into(),
        weight: 1,
    }
}

fn config_with(mode: Mode, targets: Vec<Target>, max_steps: usize) -> Arc<Config> {
    let mut cfg = Config::default();
    cfg.permissions.mode = mode;
    cfg.agent.max_steps = max_steps;
    cfg.providers.insert(
        "mock".into(),
        ProviderConfig {
            protocol: Protocol::OpenAiCompatible,
            base_url: "http://localhost:1".into(),
            api_key_env: None,
            api_key: None,
            headers: BTreeMap::new(),
            timeout_secs: 5,
        },
    );
    cfg.models.insert(
        "default".into(),
        LogicalModel {
            strategy: Strategy::Priority,
            targets,
        },
    );
    cfg.default_model = Some("default".into());
    Arc::new(cfg)
}

fn build_agent(
    cfg: Arc<Config>,
    providers: Vec<Arc<dyn kiln_provider::Provider>>,
    dir: &Path,
) -> Agent {
    let registry = Arc::new(Registry::from_providers(providers));
    let router = Arc::new(Router::new(cfg.models.clone()));
    let ctx = ToolCtx::new(dir, &ToolsConfig::default()).unwrap();
    let mut agent = Agent::new(
        cfg,
        registry,
        router,
        Arc::new(ToolRegistry::coding()),
        ctx,
        "default",
    );
    agent.sessions_dir = dir.join("sessions");
    agent
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ChatResponse {
    ChatResponse {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }],
        usage: Usage::default(),
    }
}

fn final_text(text: &str) -> ChatResponse {
    ChatResponse {
        text: text.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn executes_a_tool_then_returns_the_final_answer() {
    let dir = temp_dir("loop");
    let mock = Arc::new(MockProvider::new(
        "mock",
        vec![
            tool_call(
                "c1",
                "write_file",
                json!({"path": "out.txt", "content": "hi there"}),
            ),
            final_text("done"),
        ],
    ));
    let agent = build_agent(
        config_with(Mode::Ask, vec![target("mock")], 10),
        vec![mock.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    let outcome = agent
        .run(&mut session, "create out.txt", &AutoApprove, &mut |_| {})
        .await
        .expect("run");

    assert_eq!(outcome.text, "done");
    assert_eq!(outcome.steps, 2);
    assert_eq!(
        std::fs::read_to_string(dir.join("out.txt")).unwrap(),
        "hi there"
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .messages
        .iter()
        .any(|m| matches!(m, Message::Tool { .. })));
    assert!(session.path(&dir.join("sessions")).exists());
}

#[tokio::test]
async fn denied_tools_are_reported_to_the_model_not_executed() {
    let dir = temp_dir("deny");
    let mock = Arc::new(MockProvider::new(
        "mock",
        vec![
            tool_call(
                "c1",
                "write_file",
                json!({"path": "nope.txt", "content": "x"}),
            ),
            final_text("could not write"),
        ],
    ));
    let agent = build_agent(
        config_with(Mode::Ask, vec![target("mock")], 10),
        vec![mock.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    agent
        .run(&mut session, "write it", &DenyAll, &mut |_| {})
        .await
        .expect("run");

    assert!(!dir.join("nope.txt").exists());
    let requests = mock.requests();
    let tool_message = requests[1]
        .messages
        .iter()
        .find_map(|m| match m {
            Message::Tool {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .expect("tool result");
    assert!(tool_message.1, "denial must be flagged as an error");
    assert!(tool_message.0.contains("denied"), "{}", tool_message.0);
}

#[tokio::test]
async fn read_only_mode_blocks_writes_even_with_auto_approval() {
    let dir = temp_dir("readonly");
    let mock = Arc::new(MockProvider::new(
        "mock",
        vec![
            tool_call(
                "c1",
                "write_file",
                json!({"path": "ro.txt", "content": "x"}),
            ),
            final_text("blocked"),
        ],
    ));
    let agent = build_agent(
        config_with(Mode::Readonly, vec![target("mock")], 10),
        vec![mock.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    agent
        .run(&mut session, "write it", &AutoApprove, &mut |_| {})
        .await
        .expect("run");
    assert!(!dir.join("ro.txt").exists());
}

#[tokio::test]
async fn destructive_commands_escalate_even_in_allow_mode() {
    let dir = temp_dir("danger");
    let mock = Arc::new(MockProvider::new(
        "mock",
        vec![
            tool_call("c1", "shell", json!({"command": "git reset --hard"})),
            final_text("ok"),
        ],
    ));
    let agent = build_agent(
        config_with(Mode::Allow, vec![target("mock")], 10),
        vec![mock.clone()],
        &dir,
    );
    let recorder = Recorder::default();
    let mut session = Session::new(&dir);

    agent
        .run(&mut session, "reset", &recorder, &mut |_| {})
        .await
        .expect("run");

    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "dangerous command must be confirmed");
    assert!(seen[0].danger.is_some());
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<ApprovalRequest>>,
}

#[async_trait]
impl Approver for Recorder {
    async fn approve(&self, request: ApprovalRequest) -> bool {
        self.seen.lock().unwrap().push(request);
        false
    }
}

#[tokio::test]
async fn a_failing_provider_falls_back_to_the_next_target() {
    let dir = temp_dir("fallback");
    let broken = Arc::new(MockProvider::new("broken", Vec::new()));
    let good = Arc::new(MockProvider::new("good", vec![final_text("rescued")]));
    let agent = build_agent(
        config_with(Mode::Ask, vec![target("broken"), target("good")], 10),
        vec![broken.clone(), good.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    let outcome = agent
        .run(&mut session, "hello", &AutoApprove, &mut |_| {})
        .await
        .expect("run");
    assert_eq!(outcome.text, "rescued");
    assert_eq!(broken.request_count(), 1);
}

#[tokio::test]
async fn unknown_tools_return_an_error_the_model_can_recover_from() {
    let dir = temp_dir("unknown");
    let mock = Arc::new(MockProvider::new(
        "mock",
        vec![
            tool_call("c1", "does_not_exist", json!({})),
            final_text("recovered"),
        ],
    ));
    let agent = build_agent(
        config_with(Mode::Ask, vec![target("mock")], 10),
        vec![mock.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    let outcome = agent
        .run(&mut session, "go", &AutoApprove, &mut |_| {})
        .await
        .expect("run");
    assert_eq!(outcome.text, "recovered");
    let requests = mock.requests();
    let tool_message = requests[1]
        .messages
        .iter()
        .find_map(|m| match m {
            Message::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(tool_message.contains("unknown tool"), "{tool_message}");
    assert!(tool_message.contains("read_file"), "lists what exists");
}

#[tokio::test]
async fn step_limit_stops_a_looping_model_and_keeps_the_session() {
    let dir = temp_dir("limit");
    let endless: Vec<ChatResponse> = (0..5)
        .map(|i| tool_call(&format!("c{i}"), "list_dir", json!({"path": "."})))
        .collect();
    let mock = Arc::new(MockProvider::new("mock", endless));
    let agent = build_agent(
        config_with(Mode::Ask, vec![target("mock")], 2),
        vec![mock.clone()],
        &dir,
    );
    let mut session = Session::new(&dir);

    let err = agent
        .run(&mut session, "loop", &AutoApprove, &mut |_| {})
        .await
        .unwrap_err();
    assert!(err.to_string().contains("stopped after 2 steps"), "{err}");
    assert!(session.path(&dir.join("sessions")).exists());
}
