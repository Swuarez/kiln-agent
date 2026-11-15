use crate::{display_path, resolve_in_root, Tool, ToolCtx, ToolOutcome, ToolSpec};
use async_trait::async_trait;
use ignore::overrides::OverrideBuilder;
use kiln_core::Risk;
use regex::RegexBuilder;
use serde_json::{json, Value};

/// Files larger than this are skipped: grepping a minified bundle or a binary
/// dump wastes context and time without helping the task.
const MAX_FILE_BYTES: u64 = 2_000_000;

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".to_string(),
            description: "Search file contents with a regular expression. Respects .gitignore, skips binary and very large files. Returns path:line:text matches.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Rust regex syntax"},
                    "path": {"type": "string", "description": "Directory or file to search (default .)"},
                    "glob": {"type": "string", "description": "Only search matching files, e.g. *.rs"},
                    "case_sensitive": {"type": "boolean", "description": "Default true"},
                    "max_results": {"type": "integer", "description": "Cap on returned matches"}
                },
                "required": ["pattern"]
            }),
            risk: Risk::Read,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolOutcome::err("missing required argument 'pattern'");
        };
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let regex = match RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolOutcome::err(format!("invalid regex '{pattern}': {e}")),
        };

        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let resolved = match resolve_in_root(&ctx.root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };

        let limit = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(ctx.max_grep_results)
            .clamp(1, ctx.max_grep_results);

        let mut builder = ignore::WalkBuilder::new(&resolved);
        builder.standard_filters(true).require_git(false);
        if let Some(glob) = args.get("glob").and_then(Value::as_str) {
            let mut overrides = OverrideBuilder::new(&ctx.root);
            if let Err(e) = overrides.add(glob) {
                return ToolOutcome::err(format!("invalid glob '{glob}': {e}"));
            }
            match overrides.build() {
                Ok(o) => {
                    builder.overrides(o);
                }
                Err(e) => return ToolOutcome::err(format!("invalid glob '{glob}': {e}")),
            }
        }

        let mut matches: Vec<String> = Vec::new();
        let mut skipped_binary = 0usize;
        let mut skipped_large = 0usize;

        for entry in builder.build() {
            let Ok(entry) = entry else { continue };
            let is_file = entry.file_type().is_some_and(|t| t.is_file());
            if !is_file {
                continue;
            }
            if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                skipped_large += 1;
                continue;
            }
            let Ok(bytes) = tokio::fs::read(entry.path()).await else {
                continue;
            };
            if bytes.iter().take(1024).any(|b| *b == 0) {
                skipped_binary += 1;
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let rel = display_path(&ctx.root, entry.path());
            for (index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    let trimmed: String = line.trim().chars().take(400).collect();
                    matches.push(format!("{rel}:{}: {trimmed}", index + 1));
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
            if matches.len() >= limit {
                break;
            }
        }

        if matches.is_empty() {
            return ToolOutcome::ok(format!("no matches for '{pattern}'"));
        }

        let mut out = matches.join("\n");
        if matches.len() >= limit {
            out.push_str(&format!("\n[stopped at {limit} matches]"));
        }
        if skipped_binary + skipped_large > 0 {
            out.push_str(&format!(
                "\n[skipped {skipped_binary} binary, {skipped_large} oversized files]"
            ));
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
            "kiln-grep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {\n    let needle = 1;\n}\n").unwrap();
        std::fs::write(dir.join("notes.md"), "needle in a haystack\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 3, 0]).unwrap();
        let ctx = ToolCtx::new(&dir, &ToolsConfig::default()).unwrap();
        (ctx, dir)
    }

    #[tokio::test]
    async fn finds_matches_with_line_numbers_across_files() {
        let (ctx, _dir) = ctx();
        let out = Grep.run(&json!({"pattern": "needle"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("main.rs:2:"), "{}", out.content);
        assert!(out.content.contains("notes.md:1:"), "{}", out.content);
    }

    #[tokio::test]
    async fn glob_limits_which_files_are_searched() {
        let (ctx, _dir) = ctx();
        let out = Grep
            .run(&json!({"pattern": "needle", "glob": "*.md"}), &ctx)
            .await;
        assert!(out.content.contains("notes.md"));
        assert!(!out.content.contains("main.rs"));
    }

    #[tokio::test]
    async fn binary_files_are_skipped() {
        let (ctx, _dir) = ctx();
        let out = Grep.run(&json!({"pattern": "\\u{0}"}), &ctx).await;
        assert!(out.content.contains("skipped") || out.content.contains("no matches"));
    }

    #[tokio::test]
    async fn invalid_regex_is_a_clear_error() {
        let (ctx, _dir) = ctx();
        let out = Grep.run(&json!({"pattern": "([unclosed"}), &ctx).await;
        assert!(out.is_error);
        assert!(out.content.contains("invalid regex"));
    }

    #[tokio::test]
    async fn result_limit_is_respected() {
        let (ctx, dir) = ctx();
        std::fs::write(dir.join("many.txt"), "needle\n".repeat(50)).unwrap();
        let out = Grep
            .run(
                &json!({"pattern": "needle", "path": "many.txt", "max_results": 5}),
                &ctx,
            )
            .await;
        assert_eq!(
            out.content
                .lines()
                .filter(|l| l.contains("many.txt"))
                .count(),
            5
        );
    }
}
