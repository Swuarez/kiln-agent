# Skills and slash commands

Kiln reads the Claude Code skill and command formats. This page records exactly
what is honoured, what is ignored, and what is different. If something is not on
the "supported" list, assume it does nothing.

## Where Kiln looks

Skills, in priority order (first match for a name wins):

1. `<repo>/.kiln/skills/<name>/SKILL.md`
2. `~/.kiln/skills/<name>/SKILL.md`

Commands:

1. `<repo>/.kiln/commands/<name>.md`
2. `~/.kiln/commands/<name>.md`

A project skill shadows a global skill with the same name. Run `kiln skills` to
see what was found and which scope it came from.

## Frontmatter

Kiln parses a deliberately small subset: `key: value` lines between `---` markers.
Not a YAML parser. Nested maps, anchors, multi-line scalars (`|`, `>`), typed
values and lists spanning several lines are not supported.

| Field | Supported | Notes |
| --- | --- | --- |
| `name` | yes | Falls back to the directory name |
| `description` | yes | **Required.** A skill without one is skipped |
| `allowed-tools` | parsed, **not enforced** | Shown by `kiln skills`, otherwise ignored |
| `argument-hint` | yes (commands) | Unused by the agent; kept for future completion |
| everything else | ignored | `license`, `version`, `model`, `metadata`, ... |

Quoted values and trailing ` # comments` are handled. `allowed-tools: Read, Grep`
and `allowed-tools: [Read, Grep]` both parse.

A missing or unterminated frontmatter block leaves the whole file as body rather
than failing.

## Progressive disclosure

This is the point of the format and it works as in Claude Code:

- The system prompt contains only `name: description` for each skill.
- The body is read from disk when the model calls the `use_skill` tool.
- `use_skill` is read-only and never prompts for approval.

The tool is only registered when at least one skill was discovered, so a project
with no skills pays nothing.

A skill whose body is empty reports an error instead of returning nothing, because
an empty tool result usually makes the model retry and waste a turn.

## Slash commands

Type `/name args` in the chat REPL. Supported substitutions:

| Placeholder | Replaced with |
| --- | --- |
| `$ARGUMENTS` | the entire argument string, verbatim |
| `$1` .. `$9` | whitespace-separated positional arguments; missing ones become empty |

`$ARGUMENTS` takes precedence: if a body contains it, positionals are left alone.
This avoids substituting inside text the user typed.

Unknown commands are rejected locally with a message instead of being sent to the
model, so a typo costs nothing.

## Deliberately not implemented

These are real gaps, not oversights:

- **`allowed-tools` is not enforced.** The model can call any registered tool from
  within a skill. Treat it as documentation, not a security boundary. Per-skill
  tool restriction needs the same machinery as sub-agents (Phase 8).
- **No skill scripts.** A skill that ships `scripts/foo.py` and asks the model to
  run it will only work if the `shell` tool is allowed and the model chooses to.
  Kiln does not execute skill scripts itself.
- **No auto-invocation.** Claude Code may decide to load a skill without being
  asked. Kiln relies on the model calling `use_skill`, which is the same decision
  but visible as a tool call in the transcript.
- **No `/command` tab completion** in the REPL. `/help` lists what exists.
- **No plugin-bundled skills.** Skills come from the two directories above only;
  Kiln does not scan installed Claude Code plugins. Code-executing plugins are
  deferred until there is process isolation (Phase 10).
- **No `--skill` flag** to force-load a skill from the command line. Use
  `kiln run "use the pdf-forms skill to fill form.pdf"` for now.

## A skill that works unmodified

```
.kiln/skills/pdf-forms/SKILL.md
---
name: pdf-forms
description: Fill and flatten PDF form fields with a small Python script
allowed-tools: Read, Write, Bash
---
# Filling PDF forms

1. Inspect the fields with `pdftk dump_data_fields`.
2. Write a fill script.
3. Flatten and verify the output.
```

`kiln skills` lists it, the model sees one line for it, and `use_skill` returns
the body when the task matches.

## Verifying

```sh
kiln skills                      # what was discovered, and from where
kiln run "use the pdf-forms skill to fill form.pdf"
```

The test suite covers discovery, shadowing, missing descriptions, frontmatter
edge cases, and argument substitution; see `crates/skills/src/`.
