# Architecture

Kiln is a workspace of small crates with one-way dependencies. The agent loop
does not know which vendor answered, and providers do not know the loop exists.

```
kiln (cli)
   │  parsing, printing, prompts
   ▼
kiln-agent ──► kiln-router ──► kiln-core::config
   │                (health, ordering)
   ├──► kiln-provider ──► HTTP
   ├──► kiln-tools ──► filesystem, shell
   └──► kiln-core (messages, sessions, permissions)
```

## Crates

| crate | responsibility | does not |
|-------|----------------|----------|
| `kiln-core` | config, message model, sessions, permission policy, errors | talk to the network or the terminal |
| `kiln-provider` | `Provider` trait, OpenAI-compatible + Anthropic adapters, registry | decide which model to use |
| `kiln-router` | logical models to ordered targets, health/cooldown tracking | perform I/O |
| `kiln-tools` | `Tool` trait, `ToolCtx` confinement, coding tools | decide whether a tool is allowed |
| `kiln-agent` | turn loop, approvals, context budgeting, system prompt | know a vendor's wire format |
| `kiln-cli` | clap commands, event printing, interactive approver | contain business logic |

Adding a provider = implement `Provider` + register it in `Registry`. Adding a
tool = implement `Tool` and hand it to `ToolRegistry::with`. Adding a routing
strategy = extend `Strategy` and `Router::order`. None of these touch the loop.

## Decisions

**Provider protocol, not vendor SDKs.** One OpenAI-compatible adapter covers most
of the market (OpenAI, DeepSeek, GLM, NIM, OpenRouter, Groq, Ollama, llama.cpp,
vLLM, LM Studio). Anthropic gets a real native adapter because its wire format
differs structurally (system prompt is a top-level field, tool results are user
turns). Vendor SDKs would multiply dependencies for no gain.

**Routing is configuration.** A logical model names targets in a file. Fallback
is therefore a config edit, not a code path. The runtime tries targets in the
router's order and records the result; nothing else has vendor knowledge.

**Health is observed, not declared.** Published rate limits are unreliable,
especially on free tiers. Kiln records latency, failures and 429s per
`provider/model`, cools down limited targets (honouring `Retry-After`), and keeps
them last rather than removing them — if everything is limited, the least
recently limited target is still the best bet.

**Permissions are one pure function.** `permission::decide(mode, risk, danger)`
returns allow / ask / deny. It is unit-tested directly, so the policy can be
audited without spawning an agent or a model. The agent only translates the
decision into prompts and tool results.

**Confine by canonicalization.** Every path is resolved and checked against the
canonical workspace root. Non-existent paths are resolved by canonicalizing the
deepest existing ancestor, which also catches symlinked directories pointing
outside. Residual risk: TOCTOU races between check and use.

**Context budgeting is honest.** Three escalating steps (compact stale tool
output, drop whole turns, hard-truncate the largest body) with one invariant:
an assistant tool call is never separated from its results. Token counts are
`chars / 4` estimates used for budgeting, not billing.

**Sessions are files.** One JSON file per session, written via temp-file +
rename, saved after every step. Inspectable, copyable, deletable, no database
until search or memory actually needs one.

**Errors are typed where they cross boundaries** (`kiln_core::Error`,
`ProviderError`) and stringly only at the CLI edge. Provider errors keep status
code, a truncated response body, and retry metadata, and classify into
retryable/terminal for the router.

## Testing

| layer | approach |
|-------|----------|
| core | config parsing/merging/validation, permission table, session round-trip |
| provider | request-shape assertions and response fixtures; no network |
| router | ordering per strategy, cooldown after 429, recovery on success |
| tools | temp-dir filesystem tests, grep/glob/binary handling, shell exit codes, escape rejection |
| agent | scripted `MockProvider`: tool execution, denial, read-only, fallback, unknown tool, step limit |

No test calls a paid API. The only slow test truncates 20k lines of shell output.

## Open questions

- Streaming: the `Provider` trait returns a complete turn. Streaming needs an
  event channel in the trait plus incremental tool-call assembly in both adapters.
- Persisted health: currently per-process, so each run starts optimistic.
- Code-executing plugins: needs a process host with a capability boundary before
  anything ships; in-process plugins would be a fake sandbox.
- Concurrency: tool calls within a turn run sequentially. Parallel execution
  needs per-tool declarations of side effects so two writers cannot race.
