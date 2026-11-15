# Roadmap

Kiln is at v0.1: a headless coding loop that works, tested against a mock
provider but not yet against a live API. Everything below is ordered by
dependency and by what makes the tool usable sooner, not by how impressive it
sounds.

Sizes are honest guesses for one person: **S** is a weekend, **M** is a few
weeks of evenings, **L** is a month or more.

| # | Milestone | Theme | Size | Status |
|---|-----------|-------|------|--------|
| 1 | Trust the wire | make v0.1 survive real providers | S | partial |
| 2 | Verify loop | tests, builds and git as first-class tools | M | planned |
| 3 | Streaming + TUI | usable for long interactive sessions | M | planned |
| 4 | Durability | resume, cost accounting, persisted health | M | planned |
| 5 | Repo intelligence | symbol search on large repos | L | planned |
| 6 | MCP | external tool servers | M | partial |
| 7 | Skills | Claude Code `SKILL.md` compatibility | S | planned |
| 8 | Sub-agents | delegation with real isolation | L | planned |
| 9 | Memory | facts that survive sessions | M | planned |
| 10 | Plugins | process-isolated extensions | L | planned |
| 11 | Packaging | binaries people can install | M | planned |

---

## 1. Trust the wire — S

**Goal:** prove the adapters against real endpoints before building on them.

- Opt-in integration tests that hit a real provider, skipped unless the API key
  env var is present, so CI stays free and offline.
- A `kiln doctor` command: checks each configured provider for a resolvable key,
  reaches the base URL, and (where the protocol allows it) lists available
  models so a wrong model ID is caught before a task starts.
- Document a manual test matrix: one OpenAI-compatible provider, Anthropic, and
  Ollama. Note anything that behaves differently from the mock.

**Done when:** a full read → edit → shell → verify task has been run against at
least two real providers, and the differences are written down.

**Progress:** `kiln doctor` and `crates/provider/tests/live.rs` are in place, plus
`docs/TESTING.md` with the manual matrix. The live checks themselves are blocked
on having an API key, so the matrix cells are still empty.

## 2. Verify loop — M

**Goal:** the agent should verify its own work without falling back to raw shell,
because a failing test is the single most useful piece of context in the loop.

- `run_tests`: detects the project type (cargo, npm, pytest, go test), runs the
  right command, and returns failures parsed out of the output instead of a wall
  of text.
- `build` / `check` alongside it, with the same parsing.
- Read-only git tools: `git_status`, `git_diff`, `git_log`. No writes.
- `git_commit` behind an explicit approval, never `push`.

**Done when:** the agent fixes a deliberately broken test using `run_tests`
rather than `shell`, and the tool result is small enough to read.

## 3. Streaming + TUI — M

**Goal:** a 40-step task should not look like a frozen terminal.

- Streaming in the `Provider` trait as an event stream; tool-call arguments
  assembled incrementally in both adapters. This is the hard half.
- Ratatui TUI: transcript pane, live tool activity, inline approval prompt,
  Ctrl-C cancellation that aborts both the HTTP request and any child process.
- Fall back to the current line-based mode when stdout is not a TTY.

**Done when:** an interactive session runs for 20 minutes without the terminal
appearing hung, and Ctrl-C during a long command leaves no orphan process.

## 4. Durability — M

**Goal:** nothing is lost when a task is interrupted, and usage is visible.

- Resume anywhere: `kiln run --continue [id]`, including mid-tool-loop replay.
- Smarter compaction: summarize old turns with a cheap model instead of blind
  truncation, keeping the summary in the session.
- Token and cost accounting per turn and per session; `kiln sessions show` gains
  a usage summary. Cost needs per-model pricing in config.
- Persist provider health to `~/.kiln/health.json` so a 429 cooldown survives
  restarts.

**Done when:** kill the process mid-task, restart, continue, and the transcript
plus health state are intact.

## 5. Repository intelligence — L

**Goal:** finding code should be one tool call, not ten greps.

- Incremental tree-sitter index, cached per repo under `~/.kiln/index/`, rebuilt
  on mtime change.
- `find_symbol` and `find_references`. Regex-grade accuracy first; real
  resolution only where tree-sitter makes it cheap.
- A repo map in the system prompt: directory structure and top-level symbols,
  bounded by token budget.

**Done when:** on a 100k-LOC repo, locating a function takes one call and under
100 ms, and the index survives edits.

## 6. MCP — M

**Goal:** attach external tools without touching Kiln's code.

- stdio JSON-RPC client, tool discovery into the existing registry with
  namespaced names (`server__tool`).
- Per-server permission policy: which tools are auto, ask, or denied.
- Tool schema translation both ways, and clear errors when a server dies.
- HTTP and SSE transports after stdio works.

**Done when:** a community MCP server (filesystem or git) is usable with the same
approval flow as native tools.

**Progress:** stdio and HTTP transports, initialize handshake, tool discovery
and tool calling are implemented in crates/mcp, wired into the CLI via kiln mcp list,
and verified end to end against a hand-written JSON-RPC server. Per-server trust
(allow/ask) maps to tool risk. SSE streaming responses and connectors remain.

## 7. Skills — S

**Goal:** reuse the skill ecosystem that already exists in markdown.

- Discover `SKILL.md` in `~/.kiln/skills/` and `<repo>/.kiln/skills/`.
- Progressive disclosure: only name and description in the prompt, body loaded
  on demand, to avoid paying tokens for skills that aren't used.
- Slash commands from skills and from `.kiln/commands/*.md`.
- Write down exactly which parts of the Claude Code format are supported, and
  which are ignored.

**Done when:** an existing Claude Code skill works unmodified, and the limits are
documented rather than assumed.

## 8. Sub-agents — L

**Goal:** a big investigation should not consume the parent's context.

- Spawn with its own model, tool subset, permission mode and step budget.
  Read-only by default.
- Parallel fan-out with a concurrency cap from config.
- Results returned as a bounded summary, not the child's whole transcript.
- A `delegate` tool the parent can call, so the model decides when it's worth it.

**Done when:** a repo-wide research task splits into parallel explorers whose
findings come back under a token cap.

## 9. Memory — M

**Goal:** stop rediscovering the same project facts every session.

- Project facts as readable markdown under `~/.kiln/memory/<repo>/`, not opaque
  embeddings.
- An explicit `remember` tool plus automatic capture of user preferences.
- Retrieval at session start, bounded by budget, with a `kiln memory` command to
  list, edit and delete entries.
- Conflicts resolved by newest-wins with the old value kept in history.

**Done when:** a fact saved in one session is retrieved and used in the next one,
and the user can see and delete it.

## 10. Plugins — L

**Goal:** extensions that cannot quietly escape the permission system.

- A host process per plugin, talking a capability-scoped RPC. No in-process
  execution, ever.
- Manifest declaring tools, commands and required capabilities.
- Plugin tools appear in the registry like MCP tools and obey the same policy.

**Done when:** a test proves a plugin cannot read a file outside the paths it was
granted, and cannot run a shell command it did not declare.

## 11. Packaging — M

**Goal:** install without a compiler.

- `cargo-dist` releases for Windows, Linux and macOS, plus checksums.
- Installers: Scoop, Homebrew tap, and a shell script.
- A Docker image for CI use.
- Finish the docs: provider setup per vendor, troubleshooting, and a security
  page covering what the permission modes do and do not guarantee.

**Done when:** a user with no Rust toolchain goes from download to a completed
task without following a build guide.

---

## Not planned

- Training or fine-tuning models.
- Hosting inference. Kiln is a client; Ollama and friends already do this well.
- A hosted service or account system. Config stays local.
- Telemetry. Logs stay on disk and are opt-in.

## How this changes

Milestones are reordered when reality disagrees with them, and that's expected.
The main known risks that could reorder things:

- Streaming tool-call assembly may turn out messier per provider than estimated,
  which would push the TUI back but not cancel it.
- MCP and skills may want a shared "extension" abstraction; if they converge too
  much, they get merged into one milestone rather than building both twice.
- Tree-sitter grammar maintenance across many languages is a long-term cost.
  v1 will likely ship a small set (Rust, TS/JS, Python, Go) and grow on demand.
