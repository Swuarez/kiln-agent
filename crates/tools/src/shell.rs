use crate::{display_path, resolve_in_root, truncate_output, Tool, ToolCtx, ToolOutcome, ToolSpec};
use async_trait::async_trait;
use kiln_core::Risk;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Runs a shell command inside the workspace. The binary is chosen per platform
/// so the same prompt works on cmd.exe and sh; PowerShell-only syntax is not
/// portable and the prompt tells the model to stay POSIX-ish.
pub struct Shell;

impl Shell {
    fn command_line(command: &str) -> tokio::process::Command {
        #[cfg(windows)]
        {
            let mut cmd = tokio::process::Command::new("cmd");
            cmd.arg("/C").arg(command);
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(command);
            cmd
        }
    }
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell",
            description: "Run a shell command in the workspace and return its output. Prefer dedicated tools for reading and editing files. Long-running commands need an explicit timeout_secs.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command line to execute"},
                    "cwd": {"type": "string", "description": "Workspace-relative directory (default .)"},
                    "timeout_secs": {"type": "integer", "description": "Seconds before the command is killed (default from config)"}
                },
                "required": ["command"]
            }),
            risk: Risk::Exec,
        }
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolOutcome::err("missing required argument 'command'");
        };
        if command.trim().is_empty() {
            return ToolOutcome::err("command is empty");
        }

        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let dir = match resolve_in_root(&ctx.root, cwd) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        if !dir.is_dir() {
            return ToolOutcome::err(format!("cwd is not a directory: {cwd}"));
        }

        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .map(|s| Duration::from_secs(s.clamp(1, ctx.shell_timeout.as_secs())))
            .unwrap_or(ctx.shell_timeout);

        let mut cmd = Self::command_line(command);
        cmd.current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let started = Instant::now();
        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return ToolOutcome::err(format!("failed to start command: {e}")),
            Err(_) => {
                // kill_on_drop reaps the child when the future is dropped.
                return ToolOutcome::err(format!(
                    "command timed out after {}s and was killed: {command}",
                    timeout.as_secs()
                ));
            }
        };
        let elapsed = started.elapsed();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut body = format!(
            "cwd: {}\nexit: {:?} ({:.1}s)\n",
            display_path(&ctx.root, &dir),
            output.status.code(),
            elapsed.as_secs_f64()
        );
        if !stdout.trim().is_empty() {
            body.push_str("stdout:\n");
            body.push_str(&stdout);
            if !stdout.ends_with('\n') {
                body.push('\n');
            }
        }
        if !stderr.trim().is_empty() {
            body.push_str("stderr:\n");
            body.push_str(&stderr);
            if !stderr.ends_with('\n') {
                body.push('\n');
            }
        }
        let body = truncate_output(&body, ctx.max_output_bytes);
        if output.status.success() {
            ToolOutcome::ok(body)
        } else {
            ToolOutcome::err(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::config::ToolsConfig;
    use std::path::PathBuf;

    fn ctx() -> (ToolCtx, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "kiln-shell-{}-{}",
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
    async fn runs_in_the_workspace_and_reports_exit_code() {
        let (ctx, _dir) = ctx();
        let out = Shell.run(&json!({"command": "echo kiln-ok"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("kiln-ok"), "{}", out.content);
        assert!(out.content.contains("exit: Some(0)"), "{}", out.content);
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error_with_output_preserved() {
        let (ctx, _dir) = ctx();
        let out = Shell
            .run(&json!({"command": "echo boom && exit 3"}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("boom"), "{}", out.content);
        assert!(out.content.contains("Some(3)"), "{}", out.content);
    }

    #[tokio::test]
    async fn cwd_outside_workspace_is_refused() {
        let (ctx, _dir) = ctx();
        let out = Shell
            .run(&json!({"command": "echo hi", "cwd": "../.."}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("outside the workspace"));
    }

    #[tokio::test]
    async fn output_is_truncated_for_huge_logs() {
        let (ctx, _dir) = ctx();
        let cmd = if cfg!(windows) {
            r#"for /L %i in (1,1,20000) do @echo ============================================================"#
        } else {
            "seq 1 20000"
        };
        let out = Shell.run(&json!({"command": cmd}), &ctx).await;
        assert!(
            out.content.len() <= ctx.max_output_bytes + 200,
            "len {}",
            out.content.len()
        );
        assert!(out.content.contains("bytes omitted"));
    }
}
