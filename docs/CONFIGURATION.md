# Configuration reference

Files, in load order (later files override matching keys):

1. `~/.kiln/config.toml` — global
2. `<workspace>/.kiln/config.toml` — project
3. `$KILN_CONFIG` — replaces both when set (for CI and tests)

Unknown keys are an error. A typo that silently disables a provider is worse than
a startup failure.

## Top level

| key | type | meaning |
|-----|------|---------|
| `default_model` | string | logical model used when `--model` is absent |
| `providers` | table | where to send requests |
| `models` | table | logical models and their fallback targets |
| `permissions` | table | how cautious the agent is |
| `agent` | table | loop and context limits |
| `tools` | table | tool limits |

## `[providers.<id>]`

| key | required | meaning |
|-----|----------|---------|
| `base_url` | yes | `http(s)://…`; OpenAI-compatible bases usually end in `/v1` |
| `protocol` | no | `openai-compatible` (default) or `anthropic` |
| `api_key_env` | no | environment variable holding the key |
| `api_key` | no | inline key; only for local servers, warned about |
| `headers` | no | extra headers, e.g. OpenRouter attribution |
| `timeout_secs` | no | per-request timeout, default 180 |

Providers whose key is missing are disabled with a warning instead of failing the
whole run, so a fallback chain keeps working.

## `[models.<name>]`

| key | meaning |
|-----|---------|
| `strategy` | `priority` (default), `round-robin`, `auto` |
| `targets` | ordered list of `{ provider, model, weight }` |

```toml
[models.default]
strategy = "priority"

[[models.default.targets]]
provider = "nvidia"
model = "deepseek-ai/deepseek-v3"

[[models.default.targets]]
provider = "openrouter"
model = "deepseek/deepseek-chat"

[[models.default.targets]]
provider = "ollama"
model = "qwen2.5-coder:14b"
```

Every target must name a configured provider. `weight` only affects
`round-robin`.

## `[permissions]`

| mode | reads | writes | shell | destructive shell |
|------|-------|--------|-------|-------------------|
| `ask` (default) | auto | confirm | confirm | confirm |
| `allow` | auto | auto | auto | confirm |
| `readonly` | auto | denied | denied | denied |

Destructive patterns include `rm -rf /`, `rm -rf ~`, `mkfs`, `diskpart`,
`format c:`, raw device writes, `git push --force`, `git reset --hard`,
`git clean -fd`, `chmod -R 777 /`, and piping remote content into a shell.

Approval in headless mode is `--yes` (approve all) or deny. The agent receives
denials as tool errors and is told not to retry.

## `[agent]`

| key | default | meaning |
|-----|---------|---------|
| `max_steps` | 40 | hard stop for the tool loop |
| `context_budget_tokens` | 60000 | prompt budget used before sending |
| `temperature` | 0.2 | sampling temperature |

## `[tools]`

| key | default | meaning |
|-----|---------|---------|
| `shell_timeout_secs` | 120 | default and maximum shell runtime |
| `max_output_bytes` | 20000 | per tool result, head and tail kept |
| `max_grep_results` | 200 | match cap |

## Environment

| variable | effect |
|----------|--------|
| `KILN_CONFIG` | use this config file instead of the default pair |
| `RUST_LOG` | tracing filter, e.g. `kiln=debug` |
| standard provider keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, …) | read via `api_key_env` |

Secrets are never written to config by Kiln, never logged, and are redacted by
`kiln config show`.
