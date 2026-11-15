use crate::{resolve_in_root, truncate_output, Tool, ToolCtx, ToolOutcome, ToolSpec};
use async_trait::async_trait;
use kiln_core::Risk;
use serde_json::{json, Value};
use std::process::Stdio;

async fn run_git(ctx: &ToolCtx, cwd: &str, args: &[&str]) -> ToolOutcome {
    let dir = match resolve_in_root(&ctx.root, cwd) {
        Ok(p) => p,
        Err(e) => return ToolOutcome::err(e),
    };
    if !dir.is_dir() {
        return ToolOutcome::err(format!("cwd is not a directory: {cwd}"));
    }
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(&dir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(ctx.shell_timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return ToolOutcome::err(format!("failed to start git: {e}")),
        Err(_) => return ToolOutcome::err("git timed out".to_string()),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut body = String::new();
    if !stdout.trim().is_empty() { body.push_str(&stdout); }
    if !stderr.trim().is_empty() {
        if !body.is_empty() { body.push('\n'); }
        body.push_str("stderr:\n");
        body.push_str(&stderr);
    }
    if body.trim().is_empty() { body = "(no output)".into(); }
    let body = truncate_output(&body, ctx.max_output_bytes);
    if out.status.success() { ToolOutcome::ok(body) } else { ToolOutcome::err(body) }
}

pub struct GitStatus;
#[async_trait]
impl Tool for GitStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".to_string(),
            description: "Show the working tree status: branch, staged, modified and untracked files.".to_string(),
            parameters: json!({"type":"object","properties":{"cwd":{"type":"string","description":"Workspace-relative directory (default .)"}}}),
            risk: Risk::Read,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        run_git(ctx, cwd, &["status", "--porcelain=v1", "-b"]).await
    }
}

pub struct GitDiff;
#[async_trait]
impl Tool for GitDiff {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".to_string(),
            description: "Show the diff of uncommitted changes. Set staged=true for the index.".to_string(),
            parameters: json!({"type":"object","properties":{
                "cwd":{"type":"string"},
                "staged":{"type":"boolean"},
                "path":{"type":"string","description":"Limit to one file"}
            }}),
            risk: Risk::Read,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
        let path = args.get("path").and_then(Value::as_str);
        let mut a: Vec<&str> = vec!["diff"];
        if staged { a.push("--cached"); }
        if let Some(p) = path { a.push("--"); a.push(p); }
        run_git(ctx, cwd, &a).await
    }
}

pub struct GitLog;
#[async_trait]
impl Tool for GitLog {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_log".to_string(),
            description: "Show recent commits, one line each.".to_string(),
            parameters: json!({"type":"object","properties":{
                "cwd":{"type":"string"},
                "count":{"type":"integer","description":"Number of commits (default 15)"}
            }}),
            risk: Risk::Read,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let count = args.get("count").and_then(Value::as_u64).unwrap_or(15).clamp(1, 100);
        let n = count.to_string();
        run_git(ctx, cwd, &["log", "--oneline", "-n", &n]).await
    }
}

pub struct GitCommit;
#[async_trait]
impl Tool for GitCommit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_commit".to_string(),
            description: "Stage the given files (or all tracked changes if none) and create a commit. Requires approval.".to_string(),
            parameters: json!({"type":"object","properties":{
                "cwd":{"type":"string"},
                "message":{"type":"string"},
                "files":{"type":"array","items":{"type":"string"},"description":"Files to stage (default: git add -A)"}
            },"required":["message"]}),
            risk: Risk::Write,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let dir = match resolve_in_root(&ctx.root, cwd) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        let message = match args.get("message").and_then(Value::as_str) {
            Some(m) if !m.trim().is_empty() => m,
            _ => return ToolOutcome::err("missing required argument 'message'"),
        };
        // Stage.
        if let Some(files) = args.get("files").and_then(Value::as_array) {
            // (files provided individually below)
            let mut names: Vec<String> = Vec::new();
            for f in files {
                if let Some(s) = f.as_str() { names.push(s.to_string()); }
            }
            if names.is_empty() {
                return ToolOutcome::err("files array is empty");
            }
            let mut cmd = tokio::process::Command::new("git");
            cmd.arg("add").arg("--").args(&names).current_dir(&dir)
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let out = match cmd.output().await {
                Ok(o) => o,
                Err(e) => return ToolOutcome::err(format!("git add failed: {e}")),
            };
            if !out.status.success() {
                return ToolOutcome::err(format!("git add failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
        } else {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["add", "-A"]).current_dir(&dir)
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let out = match cmd.output().await {
                Ok(o) => o,
                Err(e) => return ToolOutcome::err(format!("git add failed: {e}")),
            };
            if !out.status.success() {
                return ToolOutcome::err(format!("git add failed: {}", String::from_utf8_lossy(&out.stderr)));
            }
        }
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(["commit", "-m", message]).current_dir(&dir)
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let out = match cmd.output().await {
            Ok(o) => o,
            Err(e) => return ToolOutcome::err(format!("git commit failed: {e}")),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut body = String::new();
        if !stdout.trim().is_empty() { body.push_str(&stdout); }
        if !stderr.trim().is_empty() { if !body.is_empty() { body.push('\n'); } body.push_str(&stderr); }
        if body.trim().is_empty() { body = "(no output)".into(); }
        if out.status.success() { ToolOutcome::ok(truncate_output(&body, ctx.max_output_bytes)) }
        else { ToolOutcome::err(truncate_output(&body, ctx.max_output_bytes)) }
    }
}

#[cfg(test)]
mod tests {
    // Smoke test only: git may not be installed on every machine, so these
    // tests skip gracefully when `git --version` fails.
    use super::*;
    use crate::ToolCtx;
    use kiln_core::config::ToolsConfig;
    use std::process::Command;

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn ctx(dir: &std::path::Path) -> ToolCtx {
        ToolCtx::new(dir, &ToolsConfig::default()).unwrap()
    }

    #[tokio::test]
    async fn git_status_on_a_fresh_repo() {
        if !git_available() { return; }
        let dir = std::env::temp_dir().join(format!("kiln-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = Command::new("git").args(["init", "-q"]).current_dir(&dir).output();
        let _ = std::fs::write(dir.join("a.txt"), "x");
        let c = ctx(&dir);
        let out = GitStatus.run(&json!({"cwd":"."}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("a.txt") || out.content.contains("##"), "{}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn git_log_empty_repo_is_handled() {
        if !git_available() { return; }
        let dir = std::env::temp_dir().join(format!("kiln-git2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = Command::new("git").args(["init", "-q"]).current_dir(&dir).output();
        let c = ctx(&dir);
        let out = GitLog.run(&json!({"cwd":".","count":5}), &c).await;
        // Empty repo: git log exits non-zero; we report it as an error with output.
        assert!(out.content.contains("does not have any commits") || out.is_error, "{}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
