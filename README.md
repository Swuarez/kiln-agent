# Kiln

A Rust-native AI coding agent. One binary, any provider, real file edits and shell
commands inside your repository.

Status: **v0.1 — working headless coding loop.** The agent reads, searches, edits,
runs commands, and stops. See [Roadmap](#roadmap) for what is deliberately missing.

Measured on Windows 11, release build: 7.1 MB binary, ~30 ms startup for
`--version`. Numbers, not aspirations.

## What works today

- Provider-agnostic runtime: OpenAI-compatible (`/chat/completions`) and native
  Anthropic (`/v1/messages`).
- One config entry per provider — no code changes, no rebuild. Point it at
  OpenAI, DeepSeek, GLM, NVIDIA NIM, OpenRouter, Groq, Ollama, llama.cpp, vLLM or
  LM Studio.
- Logical models with fallback: a 429, timeout or 500 moves to the next target.
  Health decides order for `auto`; nothing else needs babysitting.
- Tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `grep`, `shell`. All
  confined to the workspace root; `..` and symlink escapes are rejected.
- Permissions: `ask` / `allow` / `readonly`, plus an unconditional confirmation for
  destructive commands (`rm -rf /`, `git push --force`, `git reset --hard`, …).
- Context budgeting: stale tool output shrinks, old turns drop, tool-call/result
  pairs never split.
- Sessions on disk (`~/.kiln/sessions/`), saved after every step.

## Install

Requires Rust 1.75+ (install from <https://rustup.rs>).

```bash
git clone <repo> kiln && cd kiln
cargo install --path crates/cli
```

Prebuilt binaries and package-manager support are not published yet; that is a
release-phase task, not something to pretend exists.

On Windows the MSVC build needs Visual Studio Build Tools (C++ workload). The
self-contained GNU toolchain also works: `cargo +stable-x86_64-pc-windows-gnu build`.

## Configure

```bash
kiln config init      # writes ~/.kiln/config.toml
kiln config path      # shows global, project and session locations
kiln config show      # merged config, keys redacted
```

Minimal config:

```toml
default_model = "default"

[providers.anthropic]
protocol = "anthropic"           # or "openai-compatible"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"

[providers.ollama]
base_url = "http://localhost:11434/v1"

[models.default]
strategy = "priority"            # priority | round-robin | auto

[[models.default.targets]]
provider = "anthropic"
model = "claude-sonnet-4-5"

[[models.default.targets]]
provider = "ollama"
model = "qwen2.5-coder:14b"
```

Project overrides go in `<repo>/.kiln/config.toml`; only the keys you mention are
overridden. Unknown keys are rejected so a typo cannot silently do nothing.

## Use

```bash
kiln                                  # interactive
kiln run "fix the failing test in src/parser.rs"
kiln run --yes "add a --version flag" # approve writes automatically
kiln models                           # logical models, targets, availability
kiln sessions list
kiln sessions show <id>
```

Headless runs deny writes and shell unless you pass `--yes`. That default is
deliberate: an unattended agent should not discover it can edit your repo.

## Routing and health

A logical model is an ordered list of `provider + model` targets.

| strategy | behaviour |
|----------|-----------|
| `priority` | config order; rate-limited targets move to the back |
| `round-robin` | rotates, honouring `weight` |
| `auto` | observed latency and error rate decide |

Every failure updates a local health record: 429s cool a target down (using
`Retry-After` when given), 5xx backs off briefly, auth failures back off for ten
minutes. Nothing is persisted between processes yet, so health is per-run.

## Security model

- API keys come from environment variables (`api_key_env`). Inline keys are
  accepted only for local servers and are warned about.
- Keys are never logged, never placed in prompts, and redacted by `config show`.
- File tools resolve paths through canonicalization and refuse anything outside
  the workspace.
- `readonly` mode performs no writes and runs no commands.
- Dangerous shell patterns require confirmation even in `allow` mode.

Known limits, stated rather than hidden: path checks are subject to TOCTOU races,
symlink swaps mid-run are not defended against, and there is no sandbox around
child processes yet.

## Roadmap

| Phase | Content |
|-------|---------|
| done | workspace, config, sessions, provider abstraction, OpenAI-compatible + Anthropic, tools, permissions, routing with health, headless CLI |
| next | TUI (ratatui), streaming responses, `--resume` polish, cost/token reporting |
| then | MCP client, connector registry, skill loading (Claude Code skill compatibility), declarative plugins |
| then | sub-agents with per-agent models and permissions, project memory |
| later | repository indexing (tree-sitter symbols), packaging/installers |

Code-executing plugins are intentionally not in v1. In-process plugins would
either get full trust or a fake sandbox; they need a separate process host and a
real capability boundary first.

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

All provider and agent tests use scripted in-memory providers: the suite is fast
and never calls a paid API. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for
the crate layout and design decisions.

## License

Apache-2.0. See [LICENSE](LICENSE) for the full text.
