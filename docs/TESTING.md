# Testing

## Unit and integration tests

```sh
cargo test --workspace          # all, fast, no network
cargo test -p kiln-provider    # provider only
cargo clippy --workspace --all-targets
```

Every test uses scripted in-memory providers. None calls a paid API, so the
suite stays fast and runnable in CI on every push.

## Live provider checks

`crates/provider/tests/live.rs` contains opt-in tests that talk to a real
endpoint. They are ignored by default and only run with `--ignored` and the
environment variables below.

```sh
KILN_LIVE_BASE_URL=https://api.openai.com/v1 \
KILN_LIVE_API_KEY_ENV=OPENAI_API_KEY \
KILN_LIVE_MODEL=gpt-4o-mini \
cargo test -p kiln-provider --test live -- --ignored openai_compatible
```

For Anthropic, set the base URL to `https://api.anthropic.com`, the key env to
`ANTHROPIC_API_KEY`, the model to something your account has, and run
`anthropic_roundtrip` instead.

Each live test checks two things: the provider's model listing works, and a
minimal chat round-trip returns non-empty text. Tool calling is not covered by
automated live tests because it depends on the model's willingness to call a
tool; that part is checked manually.

## Manual test matrix

Run a full task against each provider and record what happens. None of these
have been run yet, because no API keys were available when the milestone was
built. Contributions with filled-in results are welcome.

| Provider | Protocol | Setup | Text reply | Tool call | `kiln doctor` | Notes |
|---|---|---|---|---|---|---|
| Anthropic | anthropic | `ANTHROPIC_API_KEY` | not run | not run | not run | |
| OpenAI | openai-compatible | `OPENAI_API_KEY` | not run | not run | not run | |
| DeepSeek | openai-compatible | `DEEPSEEK_API_KEY` | not run | not run | not run | |
| NVIDIA NIM | openai-compatible | `NVIDIA_API_KEY` | not run | not run | not run | |
| OpenRouter | openai-compatible | `OPENROUTER_API_KEY` | not run | not run | not run | |
| Groq | openai-compatible | `GROQ_API_KEY` | not run | not run | not run | |
| Ollama (local) | openai-compatible | none | not run | not run | not run | |
| llama.cpp (local) | openai-compatible | none | not run | not run | not run | |

Things to watch for while running the matrix, because these are the places where
minimal servers usually differ from OpenAI:

- Whether `/models` exists at all. Kiln treats a 404 there as a warning, not a
  failure, but it means model-id typos cannot be caught.
- Whether tool calls arrive as `tool_calls` on the assistant message or as
  something else.
- Whether `max_tokens` is required or rejected.
- Whether the server sends `Retry-After` on a 429, or a non-standard header.
- Whether arguments come back as a JSON string or already as an object.
