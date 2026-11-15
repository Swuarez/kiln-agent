# Kiln

Kiln is an AI coding agent written in Rust. It runs in your terminal, talks to
whichever model provider you configure, and edits files and runs commands inside
your repository.

> ## Work in progress
>
> This is v0.1 and it is incomplete. Only the headless coding loop is built.
> There is no TUI, no streaming, no MCP, no sub-agents and no memory yet.
> The config format and CLI flags can change without notice, and there are
> rough edges and bugs. Please don't point it at a repository you can't restore
> from git, and don't run it unattended just yet.
>
> If you want something stable today, use Claude Code or OpenCode and come back
> in a few months.

## What works right now

The agent can read files, search them, edit them, write new ones, and run shell
commands, then report back. That loop has been tested end to end against a local
mock provider, and the unit tests run on every push via GitHub Actions.

Everything else in the roadmap below is either a stub in the design docs or not
written at all.

## Building it

You need Rust 1.75 or newer (<https://rustup.rs>).

```bash
git clone https://github.com/Swuarez/kiln-agent
cd kiln-agent
cargo install --path crates/cli
```

There are no prebuilt binaries or packages yet, so building from source is the
only way in.

On Windows, the default MSVC toolchain needs the Visual Studio Build Tools with
the C++ workload installed. If you already have those, plain `cargo build` works.

## Setting it up

```bash
kiln config init      # writes ~/.kiln/config.toml
kiln config path      # where the global, project and session files live
kiln config show      # merged config, with keys redacted
```

A config with one cloud provider and one local fallback looks like this:

```toml
default_model = "default"

[providers.anthropic]
protocol = "anthropic"
base_url = "https://api.anthropic.com"
api_key_env = "ANTHROPIC_API_KEY"

[providers.ollama]
base_url = "http://localhost:11434/v1"

[models.default]
strategy = "priority"

[[models.default.targets]]
provider = "anthropic"
model = "claude-sonnet-4-5"

[[models.default.targets]]
provider = "ollama"
model = "qwen2.5-coder:14b"
```

Anything that speaks the OpenAI `/chat/completions` API goes under
`protocol = "openai-compatible"`, which covers DeepSeek, GLM, NIM, OpenRouter,
Groq, vLLM, llama.cpp and LM Studio. Anthropic is the only native protocol.

You can also drop a `.kiln/config.toml` into a project to override individual
keys from the global file. Unknown keys are rejected instead of ignored, so a
typo fails at startup rather than quietly doing nothing.

Model names in the file are not checked against anything, so use the exact IDs
your account can actually access.

## Using it

```bash
kiln                                    # interactive session
kiln run "fix the failing test in src/parser.rs"
kiln run --yes "add a --version flag"   # approve writes automatically
kiln models                             # show configured models and targets
kiln sessions list
kiln sessions show <id>
```

`kiln run` denies file changes and shell commands unless you pass `--yes`. That
is on purpose: an agent running without a human nearby should not be able to
start editing things on its own.

## Routing

A model in the config is a list of one or more `provider + model` pairs. When a
request fails, Kiln moves down the list. The ordering strategy is per model:

- `priority` goes in the order written, but a provider that just returned a
  rate limit gets pushed to the back.
- `round-robin` rotates between targets and respects `weight`.
- `auto` sorts by observed latency and error count.

Rate limits, server errors and auth failures are tracked per provider and model
for the lifetime of the process. A 429 sets a cooldown using the `Retry-After`
header when the provider sends one. This is the part I care most about, since
free and trial tiers change their limits constantly, and the plan is to lean on
observed behaviour rather than published quotas.

Health is not saved between runs yet, so every new process starts optimistic.

## Permissions

There are three modes, set in the config:

- `ask` (default): reads are automatic, writes and shell commands ask first.
- `allow`: everything runs, except commands that match a destructive pattern.
- `readonly`: no writes and no shell at all.

Destructive patterns (`rm -rf /`, `git push --force`, `git reset --hard`,
`git clean -fd`, `mkfs`, `diskpart`, piping a download into a shell, and similar)
always require confirmation, even in `allow` mode. The list is a blocklist of
command text, not a real sandbox, so it will miss creative ways of doing damage.

API keys are read from environment variables named by `api_key_env`. Kiln never
writes them to config, never logs them and never puts them in a prompt. Inline
`api_key` is accepted for local servers that demand a dummy value and logs a
warning.

File access is confined to the workspace by resolving paths and checking them
against the canonical root, which also catches symlinked directories pointing
outside. This check is not atomic, so a symlink swapped between the check and the
open would get through. Child processes are not sandboxed in any way.

## Where it's going

The first job is proving the adapters against real providers, since so far they
have only talked to a mock server. After that, in order: test and git tools so
the agent can check its own work, streaming plus a TUI, session durability,
repository indexing, MCP, skills, sub-agents, memory, and packaging.

Each step has a concrete definition of done, in
[docs/ROADMAP.md](docs/ROADMAP.md).

Plugins that execute code stay out until there is a separate process to run them
in. In-process plugins would mean either full trust or a sandbox that only looks
like one.

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
```

The test suite uses scripted in-memory providers, so it runs fast and never calls
a paid API. There are unit tests for config merging, the permission table,
routing and cooldowns, tool behaviour against temp directories, and the agent
loop. The only slow one shells out and checks output truncation.

Crate layout and the reasoning behind the bigger decisions are in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Every config key is documented in
[docs/CONFIGURATION.md](docs/CONFIGURATION.md).

Contributions and bug reports are welcome, but be aware the architecture is
still moving. If you want to add a provider, the trait is in
`crates/provider/src/lib.rs` and the adapter is usually under a hundred lines.

## License

Apache-2.0. See [LICENSE](LICENSE).
