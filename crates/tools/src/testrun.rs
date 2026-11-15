use crate::{resolve_in_root, truncate_output, Tool, ToolCtx, ToolOutcome, ToolSpec};
use async_trait::async_trait;
use kiln_core::Risk;
use serde_json::{json, Value};
use std::process::Stdio;

#[derive(Clone, Copy)]
enum Project {
    Cargo,
    Npm,
    Pytest,
    Go,
}

fn detect(dir: &std::path::Path) -> Option<Project> {
    if dir.join("Cargo.toml").is_file() {
        return Some(Project::Cargo);
    }
    if dir.join("go.mod").is_file() {
        return Some(Project::Go);
    }
    if dir.join("package.json").is_file() {
        return Some(Project::Npm);
    }
    if dir.join("pytest.ini").is_file()
        || dir.join("pyproject.toml").is_file()
        || dir.join("setup.cfg").is_file()
        || dir.join("tests").is_dir()
        || dir.join("test").is_dir()
    {
        return Some(Project::Pytest);
    }
    None
}

async fn run_cmd(
    ctx: &ToolCtx,
    dir: &std::path::Path,
    program: &str,
    args: &[&str],
) -> ToolOutcome {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(ctx.shell_timeout, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return ToolOutcome::err(format!("failed to start {program}: {e}")),
        Err(_) => return ToolOutcome::err(format!("{program} timed out")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut body = String::new();
    if !stdout.trim().is_empty() {
        body.push_str(&stdout);
    }
    if !stderr.trim().is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("stderr:\n");
        body.push_str(&stderr);
    }
    if body.trim().is_empty() {
        body = "(no output)".into();
    }
    // Pull a one-line summary out of cargo/pytest output so the model doesn't
    // have to scan the whole log.
    let mut summary = String::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("test result:") || t.starts_with("FAILED") && t.contains("::") {
            if !summary.is_empty() {
                summary.push_str("; ");
            }
            summary.push_str(t);
            if summary.len() > 400 {
                break;
            }
        }
    }
    if !summary.is_empty() {
        body.push_str("\n\n[summary] ");
        body.push_str(&summary);
    }
    let body = truncate_output(&body, ctx.max_output_bytes);
    if out.status.success() {
        ToolOutcome::ok(body)
    } else {
        ToolOutcome::err(body)
    }
}

pub struct RunTests;
#[async_trait]
impl Tool for RunTests {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_tests".to_string(),
            description: "Run the project's test suite. Detects cargo, npm, pytest or go automatically. Returns failures parsed out of the output.".to_string(),
            parameters: json!({"type":"object","properties":{"cwd":{"type":"string","description":"Workspace-relative directory (default .)"}}}),
            risk: Risk::Exec,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let dir = match resolve_in_root(&ctx.root, cwd) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        match detect(&dir) {
            Some(Project::Cargo) => run_cmd(ctx, &dir, "cargo", &["test"]).await,
            Some(Project::Go) => run_cmd(ctx, &dir, "go", &["test", "./..."]).await,
            Some(Project::Npm) => run_cmd(ctx, &dir, "npm", &["test"]).await,
            Some(Project::Pytest) => run_cmd(ctx, &dir, "pytest", &["-q"]).await,
            None => ToolOutcome::err(
                "no recognized project (Cargo.toml, package.json, go.mod, or pytest files)",
            ),
        }
    }
}

pub struct Build;
#[async_trait]
impl Tool for Build {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "build".to_string(),
            description: "Build the project. Detects cargo, npm or go automatically.".to_string(),
            parameters: json!({"type":"object","properties":{"cwd":{"type":"string","description":"Workspace-relative directory (default .)"}}}),
            risk: Risk::Exec,
        }
    }
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome {
        let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or(".");
        let dir = match resolve_in_root(&ctx.root, cwd) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e),
        };
        match detect(&dir) {
            Some(Project::Cargo) => run_cmd(ctx, &dir, "cargo", &["build"]).await,
            Some(Project::Go) => run_cmd(ctx, &dir, "go", &["build", "./..."]).await,
            Some(Project::Npm) => run_cmd(ctx, &dir, "npm", &["run", "build"]).await,
            Some(Project::Pytest) => {
                ToolOutcome::err("python projects have no standard build step")
            }
            None => ToolOutcome::err("no recognized project"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCtx;
    use kiln_core::config::ToolsConfig;
    use std::process::Command;

    fn cargo_available() -> bool {
        Command::new("cargo")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn run_tests_on_this_repo() {
        if !cargo_available() {
            return;
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let root = root.canonicalize().unwrap();
        let ctx = ToolCtx::new(&root, &ToolsConfig::default()).unwrap();
        let out = RunTests.run(&json!({"cwd":"."}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("test result:"), "{}", out.content);
    }

    #[tokio::test]
    async fn build_on_this_repo() {
        if !cargo_available() {
            return;
        }
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let root = root.canonicalize().unwrap();
        let ctx = ToolCtx::new(&root, &ToolsConfig::default()).unwrap();
        let out = Build.run(&json!({"cwd":"."}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn run_tests_on_unknown_project() {
        let dir = std::env::temp_dir().join(format!("kiln-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolCtx::new(&dir, &ToolsConfig::default()).unwrap();
        let out = RunTests.run(&json!({"cwd":"."}), &ctx).await;
        assert!(out.is_error);
        assert!(out.content.contains("no recognized project"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
