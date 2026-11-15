use crate::{display_path, resolve_in_root, truncate_output, Tool, ToolCtx, ToolOutcome, ToolSpec};
use async_trait::async_trait;
use kiln_core::Risk;
use serde_json::{json, Value};

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn io_error(path: &str, e: std::io::Error) -> ToolOutcome {
    ToolOutcome::err(format!("{path}: {e}"))
}

fn missing(key: &str) -> ToolOutcome {
    ToolOutcome::err(format!("missing required argument '{key}'"))
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file",
            description: "Read a UTF-8 text file from the workspace. Use offset and limit for large files instead of reading everything.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path"},
                    "offset": {"type": "integer", "description": "1-based first line (default 1)"},
                    "limit": {"type": "integer", "description": "Maximum lines (default 2000)"}
                },
                "required": ["path"]
            }),
            risk: Risk::Read,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(path) = arg_str(args, "path") else {
            return missing("path");
        };
        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        let rel = display_path(&ctx.root, &resolved);

        let bytes = match tokio::fs::read(&resolved).await {
            Ok(b) => b,
            Err(e) => return io_error(&rel, e),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return ToolOutcome::err(format!("{rel}: not valid UTF-8"));
        };

        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return ToolOutcome::ok(format!("{rel}: (empty file)"));
        }
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(2000)
            .max(1) as usize;

        if offset > lines.len() {
            return ToolOutcome::ok(format!(
                "{rel}: only {} lines, offset {offset} is past the end",
                lines.len()
            ));
        }
        let start = offset - 1;
        let end = (start + limit).min(lines.len());
        let mut out = truncate_output(&lines[start..end].join("\n"), ctx.max_output_bytes);
        if end < lines.len() {
            out.push_str(&format!(
                "\n[showing lines {}-{end} of {}]",
                start + 1,
                lines.len()
            ));
        }
        ToolOutcome::ok(out)
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file",
            description: "Create or overwrite a file with the full given content. For changing part of an existing file prefer edit_file.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path"},
                    "content": {"type": "string", "description": "Complete file content"}
                },
                "required": ["path", "content"]
            }),
            risk: Risk::Write,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(path) = arg_str(args, "path") else {
            return missing("path");
        };
        let Some(content) = arg_str(args, "content") else {
            return missing("content");
        };
        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        let rel = display_path(&ctx.root, &resolved);

        if let Ok(existing) = tokio::fs::read_to_string(&resolved).await {
            if existing == content {
                return ToolOutcome::ok(format!("{rel}: unchanged, content already matches"));
            }
        }
        if let Some(parent) = resolved.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return io_error(&rel, e);
            }
        }
        if let Err(e) = tokio::fs::write(&resolved, content).await {
            return io_error(&rel, e);
        }
        ToolOutcome::ok(format!("wrote {rel} ({} bytes)", content.len()))
    }
}

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file",
            description: "Replace an exact string in an existing file. old_string must appear exactly once unless replace_all is true. Read the file first so the text matches byte for byte.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative path"},
                    "old_string": {"type": "string", "description": "Exact text to replace, including indentation"},
                    "new_string": {"type": "string", "description": "Replacement text"},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
            risk: Risk::Write,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let (Some(path), Some(old), Some(new)) = (
            arg_str(args, "path"),
            arg_str(args, "old_string"),
            arg_str(args, "new_string"),
        ) else {
            return missing("path, old_string, new_string");
        };
        if old.is_empty() {
            return ToolOutcome::err("old_string must not be empty");
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        let rel = display_path(&ctx.root, &resolved);
        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return io_error(&rel, e),
        };

        let (old, new) = match_line_endings(&content, old, new);
        let hits = match_lines(&content, &old);
        match hits.len() {
            0 => {
                return ToolOutcome::err(format!(
                    "{rel}: old_string not found. Read the file and copy the exact text, including indentation."
                ))
            }
            1 => {}
            n if !replace_all => {
                return ToolOutcome::err(format!(
                    "{rel}: old_string appears {n} times (lines {:?}). Add surrounding context to make it unique, or set replace_all.",
                    &hits[..hits.len().min(5)]
                ))
            }
            _ => {}
        }

        let updated = if replace_all {
            content.replace(old.as_str(), new.as_str())
        } else {
            content.replacen(old.as_str(), new.as_str(), 1)
        };
        if let Err(e) = tokio::fs::write(&resolved, &updated).await {
            return io_error(&rel, e);
        }
        ToolOutcome::ok(format!(
            "{rel}: replaced {} occurrence(s), first at line {}",
            if replace_all { hits.len() } else { 1 },
            hits[0]
        ))
    }
}

/// Models often emit `\n` for a CRLF file. Retry with platform endings before
/// reporting "not found", which would otherwise loop forever.
fn match_line_endings(content: &str, old: &str, new: &str) -> (String, String) {
    let needs_crlf = content.contains("\r\n")
        && old.contains('\n')
        && !old.contains('\r')
        && !content.contains(old);
    if needs_crlf {
        return (old.replace('\n', "\r\n"), new.replace('\n', "\r\n"));
    }
    (old.to_string(), new.to_string())
}

fn match_lines(content: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let mut start = 0;
    while let Some(pos) = content[start..].find(needle) {
        let abs = start + pos;
        out.push(content[..abs].matches('\n').count() + 1);
        start = abs + needle.len();
    }
    out
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir",
            description: "List files under a directory, honouring .gitignore and skipping hidden entries. Use it to orient yourself in an unfamiliar repository.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workspace-relative directory (default .)"},
                    "depth": {"type": "integer", "description": "How many levels deep (default 2)"}
                }
            }),
            risk: Risk::Read,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let path = arg_str(args, "path").unwrap_or(".");
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 10) as usize;
        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };

        let mut entries: Vec<String> = Vec::new();
        let mut truncated = false;
        let walk = ignore::WalkBuilder::new(&resolved)
            .max_depth(Some(depth))
            .standard_filters(true)
            .require_git(false)
            .build();

        for entry in walk {
            let Ok(entry) = entry else { continue };
            if entry.depth() == 0 {
                continue;
            }
            if entries.len() >= 500 {
                truncated = true;
                break;
            }
            let mut rel = display_path(&ctx.root, entry.path());
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                rel.push('/');
            }
            entries.push(rel);
        }
        entries.sort();

        if entries.is_empty() {
            return ToolOutcome::ok(format!(
                "{}: no visible entries",
                display_path(&ctx.root, &resolved)
            ));
        }
        let mut out = entries.join("\n");
        if truncated {
            out.push_str("\n[stopped at 500 entries; narrow the path or reduce depth]");
        }
        ToolOutcome::ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::config::ToolsConfig;
    use std::path::PathBuf;

    fn ctx() -> (ToolCtx, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "kiln-fs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx::new(&dir, &ToolsConfig::default()).unwrap();
        (ctx, dir)
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let (ctx, _dir) = ctx();
        let w = WriteFile
            .run(&json!({"path": "a/b.txt", "content": "hello\nworld"}), &ctx)
            .await;
        assert!(!w.is_error, "{}", w.content);
        let r = ReadFile.run(&json!({"path": "a/b.txt"}), &ctx).await;
        assert_eq!(r.content, "hello\nworld");
    }

    #[tokio::test]
    async fn read_reports_line_windows() {
        let (ctx, _dir) = ctx();
        WriteFile
            .run(&json!({"path": "l.txt", "content": "1\n2\n3\n4\n"}), &ctx)
            .await;
        let r = ReadFile
            .run(&json!({"path": "l.txt", "offset": 2, "limit": 2}), &ctx)
            .await;
        assert!(r.content.starts_with("2\n3"), "{}", r.content);
        assert!(r.content.contains("of 4"));
    }

    #[tokio::test]
    async fn edit_requires_a_unique_match() {
        let (ctx, _dir) = ctx();
        WriteFile
            .run(&json!({"path": "e.txt", "content": "x = 1\nx = 2\n"}), &ctx)
            .await;
        let ambiguous = EditFile
            .run(
                &json!({"path": "e.txt", "old_string": "x", "new_string": "y"}),
                &ctx,
            )
            .await;
        assert!(ambiguous.is_error);
        assert!(ambiguous.content.contains("appears 2 times"));

        let unique = EditFile
            .run(
                &json!({"path": "e.txt", "old_string": "x = 2", "new_string": "x = 3"}),
                &ctx,
            )
            .await;
        assert!(!unique.is_error, "{}", unique.content);
        let content = std::fs::read_to_string(ctx.root.join("e.txt")).unwrap();
        assert_eq!(content, "x = 1\nx = 3\n");
    }

    #[tokio::test]
    async fn edit_rejects_missing_text_with_actionable_error() {
        let (ctx, _dir) = ctx();
        WriteFile
            .run(&json!({"path": "m.txt", "content": "alpha"}), &ctx)
            .await;
        let out = EditFile
            .run(
                &json!({"path": "m.txt", "old_string": "beta", "new_string": "z"}),
                &ctx,
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("old_string not found"));
    }

    #[tokio::test]
    async fn write_refuses_paths_outside_the_workspace() {
        let (ctx, _dir) = ctx();
        let out = WriteFile
            .run(&json!({"path": "../escape.txt", "content": "no"}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("outside the workspace"));
    }

    #[tokio::test]
    async fn identical_write_is_reported_as_noop() {
        let (ctx, _dir) = ctx();
        WriteFile
            .run(&json!({"path": "s.txt", "content": "same"}), &ctx)
            .await;
        let again = WriteFile
            .run(&json!({"path": "s.txt", "content": "same"}), &ctx)
            .await;
        assert!(again.content.contains("unchanged"));
    }

    #[tokio::test]
    async fn list_dir_skips_gitignored_entries() {
        let (ctx, dir) = ctx();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/junk.bin"), "x").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        let out = ListDir.run(&json!({"path": ".", "depth": 2}), &ctx).await;
        assert!(out.content.contains("main.rs"), "{}", out.content);
        assert!(!out.content.contains("junk.bin"), "{}", out.content);
    }
}
