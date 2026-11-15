//! Kiln CLI: interactive chat, one-shot runs, and configuration/session
//! inspection. Everything the agent does is already in `kiln-agent`; this file
//! is only argument parsing and event printing.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use kiln_agent::{Agent, AgentEvent, ApprovalRequest, Approver, AutoApprove, DenyAll};
use kiln_core::{paths, Config, Message, Session};
use kiln_provider::Registry;
use kiln_router::Router;
use kiln_tools::{ToolCtx, ToolRegistry};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "kiln",
    version,
    about = "A Rust-native AI coding agent with pluggable providers",
    args_conflicts_with_subcommands = false
)]
struct Cli {
    /// Repository to work in
    #[arg(long, global = true, default_value = ".")]
    dir: PathBuf,
    /// Logical model to use (see `kiln models`)
    #[arg(long, short = 'm', global = true)]
    model: Option<String>,
    /// Task label for model selection (see [task_models] in config)
    #[arg(long, global = true)]
    task: Option<String>,
    /// Print routing and tool details to stderr
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run one task and exit (non-interactive)
    Run {
        /// The task for the agent
        #[arg(required = true, trailing_var_arg = true, num_args = 1..)]
        task: Vec<String>,
        /// Approve writes and shell commands without prompting
        #[arg(long)]
        yes: bool,
    },
    /// Start an interactive session
    Chat {
        /// Resume an existing session id
        #[arg(long)]
        session: Option<String>,
    },
    /// List logical models and their targets
    Models,
    /// Check configured providers: keys, reachability, model ids
    Doctor,
    /// Inspect or create configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Inspect or remove saved sessions
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Inspect configured MCP servers and their tools
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Write a starter config to ~/.kiln/config.toml
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Print the merged configuration with secrets redacted
    Show,
    /// Print the config file locations
    Path,
}

#[derive(Subcommand)]
enum SessionAction {
    List,
    Show { id: String },
    Rm { id: String },
}

#[derive(Subcommand)]
enum McpAction {
    /// Spawn each configured server and list its tools
    List,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("kiln: could not start async runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match run_cli(&cli, &runtime) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("kiln: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_cli(cli: &Cli, runtime: &tokio::runtime::Runtime) -> Result<std::process::ExitCode> {
    let cwd = cli
        .dir
        .canonicalize()
        .with_context(|| format!("cannot open working directory {}", cli.dir.display()))?;

    match &cli.command {
        Some(Command::Config { action }) => config_command(action),
        Some(Command::Models) => models_command(&cwd),
        Some(Command::Doctor) => doctor_command(&cwd, runtime),
        Some(Command::Sessions { action }) => sessions_command(action),
        Some(Command::Mcp { action }) => mcp_command(&cwd, action, runtime),
        Some(Command::Run { task, yes }) => {
            let text = task.join(" ");
            let approver: Box<dyn Approver> = if *yes {
                Box::new(AutoApprove)
            } else {
                eprintln!(
                    "headless mode: file writes and shell commands are denied. Pass --yes to allow them."
                );
                Box::new(DenyAll)
            };
            one_shot(
                &cwd,
                cli.model.as_deref(),
                cli.task.as_deref(),
                &text,
                approver.as_ref(),
                runtime,
                cli.verbose,
            )
        }
        Some(Command::Chat { session }) => chat(
            &cwd,
            cli.model.as_deref(),
            cli.task.as_deref(),
            session.as_deref(),
            runtime,
            cli.verbose,
        ),
        None => chat(
            &cwd,
            cli.model.as_deref(),
            cli.task.as_deref(),
            None,
            runtime,
            cli.verbose,
        ),
    }
}

async fn build_agent(
    cwd: &Path,
    model: Option<&str>,
    task: Option<&str>,
) -> Result<(Agent, Vec<String>)> {
    let config = Config::load(cwd)?;
    if config.providers.is_empty() {
        bail!(
            "no providers configured.\nRun `kiln config init`, then set the API key for at least one provider.\nConfig lives at {}",
            paths::global_config_path().display()
        );
    }
    let model_name = config.model_name(model, task)?;
    let config = Arc::new(config);
    let registry = Registry::from_config(&config)?;
    let mut warnings = registry.warnings().to_vec();
    if registry.is_empty() {
        bail!("no provider is usable:\n- {}", warnings.join("\n- "));
    }
    let router = Router::persistent(config.models.clone());
    let ctx = ToolCtx::new(cwd, &config.tools)?;
    let (mcp_tools, mcp_warnings) = kiln_mcp::discover(&config.mcp.servers).await;
    warnings.extend(mcp_warnings);
    let mut tool_registry = ToolRegistry::coding();
    for tool in mcp_tools {
        tool_registry = tool_registry.with(tool);
    }
    let agent = Agent::new(
        config,
        Arc::new(registry),
        Arc::new(router),
        Arc::new(tool_registry),
        ctx,
        model_name,
    );
    Ok((agent, warnings))
}

fn one_shot(
    cwd: &Path,
    model: Option<&str>,
    task_label: Option<&str>,
    text: &str,
    approver: &dyn Approver,
    runtime: &tokio::runtime::Runtime,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let (agent, warnings) = runtime.block_on(build_agent(cwd, model, task_label))?;
    print_warnings(&warnings);
    let mut session = Session::new(cwd);
    let mut printer = EventPrinter { verbose };

    match runtime.block_on(agent.run(&mut session, text, approver, &mut |e| printer.print(e))) {
        Ok(outcome) => {
            if verbose {
                eprintln!(
                    "[kiln] {} steps, {} input tokens, {} output tokens, ${:.4}",
                    outcome.steps,
                    outcome.usage.input_tokens,
                    outcome.usage.output_tokens,
                    outcome.cost_usd
                );
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("kiln: {e}");
            Ok(std::process::ExitCode::FAILURE)
        }
    }
}

fn chat(
    cwd: &Path,
    model: Option<&str>,
    task_label: Option<&str>,
    resume: Option<&str>,
    runtime: &tokio::runtime::Runtime,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let (agent, warnings) = runtime.block_on(build_agent(cwd, model, task_label))?;
    print_warnings(&warnings);

    let sessions_dir = paths::sessions_dir();
    let mut session = match resume {
        Some(id) => Session::load(&sessions_dir, id)?,
        None => Session::new(cwd),
    };

    eprintln!(
        "kiln · model {} · {} · session {}\nType /help for commands, /exit to quit.",
        agent.model,
        agent.ctx.root.display(),
        session.id
    );

    let approver = PromptApprover;
    let mut printer = EventPrinter { verbose };
    let stdin = std::io::stdin();
    loop {
        eprint!("› ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            eprintln!();
            break;
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match input {
            "/exit" | "/quit" => break,
            "/help" => {
                eprintln!("/exit  quit\n/help  this message\nAnything else is sent to the agent.");
                continue;
            }
            _ => {}
        }
        if let Err(e) = runtime.block_on(agent.run(&mut session, input, &approver, &mut |event| {
            printer.print(event)
        })) {
            eprintln!("kiln: {e}");
        }
        println!();
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// Interactive confirmation. Blocking stdin read is moved off the runtime.
struct PromptApprover;

#[async_trait]
impl Approver for PromptApprover {
    async fn approve(&self, request: ApprovalRequest) -> bool {
        let reason = request
            .danger
            .map(|d| format!(" [{d}]"))
            .unwrap_or_default();
        let prompt = format!(
            "allow {}? {}{} [y/N] ",
            request.tool, request.summary, reason
        );
        tokio::task::spawn_blocking(move || {
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                return false;
            }
            matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
        })
        .await
        .unwrap_or(false)
    }
}

struct EventPrinter {
    verbose: bool,
}

impl EventPrinter {
    fn print(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Text(text) => println!("{text}"),
            AgentEvent::ToolStart { name, arguments } => {
                eprintln!("→ {name} {}", kiln_agent::summarise(&name, &arguments));
            }
            AgentEvent::ToolEnd {
                name,
                ok,
                summary,
                ms,
            } => {
                let mark = if ok { "ok" } else { "error" };
                eprintln!("  {name}: {mark} ({ms}ms) {}", first_line(&summary));
            }
            AgentEvent::Denied { name, reason } => {
                eprintln!("  {name}: denied ({reason})");
            }
            AgentEvent::ProviderError {
                provider,
                model,
                error,
            } => {
                eprintln!("  {provider}/{model}: {error}");
            }
            AgentEvent::ModelSelected {
                provider,
                model,
                attempt,
            } => {
                if self.verbose && attempt > 0 {
                    eprintln!("  falling back to {provider}/{model}");
                }
            }
            AgentEvent::Usage(_) => {
                // Totals are printed once at the end of a run; per-turn lines
                // interleave with model text on a shared terminal.
            }
        }
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 100 {
        format!("{}…", line.chars().take(100).collect::<String>())
    } else {
        line.to_string()
    }
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

fn models_command(cwd: &Path) -> Result<std::process::ExitCode> {
    let config = Config::load(cwd)?;
    if config.models.is_empty() {
        bail!("no models configured. Run `kiln config init`.");
    }
    let registry = Registry::from_config(&config)?;
    let warnings = registry.warnings();
    let default = config.model_name(None, None).ok();
    for (name, model) in &config.models {
        let marker = if Some(name) == default.as_ref() {
            " (default)"
        } else {
            ""
        };
        println!("{name}{marker}  [{:?}]", model.strategy);
        for target in &model.targets {
            let status = if registry.get(&target.provider).is_some() {
                "ready"
            } else {
                "unavailable"
            };
            println!(
                "  {:<12} {:<40} {:<12} weight {}",
                target.provider, target.model, status, target.weight
            );
        }
    }
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn doctor_command(cwd: &Path, runtime: &tokio::runtime::Runtime) -> Result<std::process::ExitCode> {
    let config = Config::load(cwd)?;
    if config.providers.is_empty() {
        bail!("no providers configured. Run `kiln config init`.");
    }
    let report = runtime.block_on(kiln_provider::doctor::run(&config));
    for check in &report.checks {
        println!("{} ({})", check.id, check.protocol);
        println!("  endpoint  {}", check.endpoint);
        if check.ready {
            if let Some(problem) = &check.problem {
                println!("  status    ok, but: {problem}");
            } else {
                println!(
                    "  status    ok ({} models, {} ms)",
                    check.model_count, check.latency_ms
                );
            }
            if !check.models.is_empty() {
                println!("  sample    {}", check.models.join(", "));
            }
        } else if let Some(problem) = &check.problem {
            println!("  status    FAILED: {problem}");
        }
        if !check.missing_models.is_empty() {
            println!(
                "  warning   model ids not listed by this provider: {}",
                check.missing_models.join(", ")
            );
        }
        println!();
    }
    println!(
        "{} of {} providers ready; {} of {} logical models have a usable target.",
        report.usable_providers,
        report.checks.len(),
        report
            .logical_models
            .saturating_sub(report.broken_logical_models.len()),
        report.logical_models
    );
    for name in &report.broken_logical_models {
        eprintln!("  broken model: {name}");
    }
    if report.usable_providers == 0 {
        Ok(std::process::ExitCode::FAILURE)
    } else {
        Ok(std::process::ExitCode::SUCCESS)
    }
}

fn config_command(action: &ConfigAction) -> Result<std::process::ExitCode> {
    match action {
        ConfigAction::Init { force } => {
            let path = paths::global_config_path();
            if path.exists() && !*force {
                bail!(
                    "{} already exists; pass --force to overwrite",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, kiln_core::config::TEMPLATE)?;
            println!("wrote {}", path.display());
            println!("Set an API key for at least one provider, e.g. ANTHROPIC_API_KEY.");
        }
        ConfigAction::Show => {
            let mut config = Config::load(&std::env::current_dir()?)?;
            for provider in config.providers.values_mut() {
                if provider.api_key.is_some() {
                    provider.api_key = Some("<redacted>".into());
                }
            }
            print!("{}", toml::to_string_pretty(&config)?);
        }
        ConfigAction::Path => {
            println!("global: {}", paths::global_config_path().display());
            println!(
                "project: {}",
                paths::project_config_path(&std::env::current_dir()?).display()
            );
            println!("sessions: {}", paths::sessions_dir().display());
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn mcp_command(
    cwd: &Path,
    action: &McpAction,
    runtime: &tokio::runtime::Runtime,
) -> Result<std::process::ExitCode> {
    match action {
        McpAction::List => {
            let config = Config::load(cwd)?;
            if config.mcp.servers.is_empty() {
                println!("no MCP servers configured");
                println!("add one under [mcp.servers.<name>] in your config");
                return Ok(std::process::ExitCode::SUCCESS);
            }
            let (tools, warnings) = runtime.block_on(kiln_mcp::discover(&config.mcp.servers));
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            // Group discovered tools by server prefix.
            let mut by_server: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for tool in &tools {
                let name = tool.spec().name;
                let (server, rest) = name.split_once('_').unwrap_or(("", &name));
                by_server
                    .entry(server.to_string())
                    .or_default()
                    .push(rest.to_string());
            }
            for (server, names) in &by_server {
                println!("{server}: {} tool(s)", names.len());
                for n in names {
                    println!("  {n}");
                }
            }
            // Servers that produced no tools still deserve a line.
            for name in config.mcp.servers.keys() {
                if !by_server.contains_key(name) {
                    println!("{name}: no tools (see warnings above)");
                }
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

fn sessions_command(action: &SessionAction) -> Result<std::process::ExitCode> {
    let dir = paths::sessions_dir();
    match action {
        SessionAction::List => {
            let sessions = Session::list(&dir)?;
            if sessions.is_empty() {
                println!("no saved sessions");
            }
            for s in sessions {
                println!(
                    "{:<20} {:>4} msgs  {:<20} {}",
                    s.id,
                    s.messages,
                    ago(s.updated_ms),
                    s.title
                );
            }
        }
        SessionAction::Show { id } => {
            let session = Session::load(&dir, id)?;
            println!(
                "{} · {} · {}",
                session.id,
                session.cwd.display(),
                session.title
            );
            for message in &session.messages {
                match message {
                    Message::User { content } => println!("\n» {content}"),
                    Message::Assistant { content, .. } if !content.is_empty() => {
                        println!("\n{content}")
                    }
                    Message::Tool {
                        content, is_error, ..
                    } => println!(
                        "\n[tool{}] {content}",
                        if *is_error { " error" } else { "" }
                    ),
                    _ => {}
                }
            }
        }
        SessionAction::Rm { id } => {
            Session::delete(&dir, id)?;
            println!("removed {id}");
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let secs = now.saturating_sub(ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn init_tracing(verbose: bool) {
    let default = if verbose { "kiln=debug,warn" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
