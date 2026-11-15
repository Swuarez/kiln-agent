//! Tool abstraction plus the built-in coding tools.
//!
//! Every tool receives an already-confined [`ToolCtx`]. MCP servers, connector
//! tools and plugin tools will implement this same trait; nothing in the agent
//! loop needs to know where a tool came from.

pub mod fs;
pub mod search;
pub mod shell;

use async_trait::async_trait;
use kiln_core::{config::ToolsConfig, Result, Risk};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ToolCtx {
    /// Canonical workspace root. All paths are resolved and checked against it.
    pub root: PathBuf,
    pub max_output_bytes: usize,
    pub shell_timeout: Duration,
    pub max_grep_results: usize,
}

impl ToolCtx {
    pub fn new(root: &Path, cfg: &ToolsConfig) -> Result<Self> {
        let root = root
            .canonicalize()
            .map_err(|e| kiln_core::Error::io(root, e))?;
        Ok(Self {
            root,
            max_output_bytes: cfg.max_output_bytes.max(1_000),
            shell_timeout: Duration::from_secs(cfg.shell_timeout_secs.clamp(1, 3600)),
            max_grep_results: cfg.max_grep_results.clamp(1, 5_000),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// A tool's public face: name, description, JSON schema and how risky it is.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub risk: Risk,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutcome;
}

pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            tools: tools
                .into_iter()
                .map(|t| (t.spec().name, t))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    /// The v1 toolset: enough to reproduce normal coding work.
    pub fn coding() -> Self {
        Self::new(vec![
            Arc::new(fs::ReadFile),
            Arc::new(fs::WriteFile),
            Arc::new(fs::EditFile),
            Arc::new(fs::ListDir),
            Arc::new(search::Grep),
            Arc::new(shell::Shell),
        ])
    }

    pub fn with(self, tool: Arc<dyn Tool>) -> Self {
        let mut tools = self.tools;
        tools.insert(tool.spec().name, tool);
        Self { tools }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

/// Resolves a user/model supplied path and proves it stays inside `root`.
///
/// Lexical `..` resolution happens through canonicalization of the deepest
/// existing ancestor, which also catches symlinked directories pointing outside.
/// Race conditions between check and use remain (TOCTOU); closing them needs
/// capability handles, which is out of scope for v1.
pub fn resolve_in_root(root: &Path, requested: &str) -> std::result::Result<PathBuf, String> {
    if requested.trim().is_empty() {
        return Err("path is empty".into());
    }
    let raw = Path::new(requested);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };

    let mut probe = joined.clone();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match probe.canonicalize() {
            Ok(mut resolved) => {
                for part in suffix.iter().rev() {
                    resolved.push(part);
                }
                if !resolved.starts_with(root) {
                    return Err(format!(
                        "'{requested}' resolves outside the workspace ({})",
                        root.display()
                    ));
                }
                return Ok(resolved);
            }
            Err(_) => {
                let Some(name) = probe.file_name().map(|n| n.to_owned()) else {
                    return Err(format!("cannot resolve '{requested}'"));
                };
                suffix.push(name);
                if !probe.pop() {
                    return Err(format!("cannot resolve '{requested}'"));
                }
            }
        }
    }
}

/// Relative, forward-slash display path. Stable across platforms and readable
/// by the model regardless of where Kiln runs.
pub(crate) fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Keeps the head and the tail, because both build commands and test failures
/// put their useful lines at the extremes.
pub(crate) fn truncate_output(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let head_len = limit * 6 / 10;
    let tail_len = limit - head_len;
    let head_end = floor_char_boundary(text, head_len);
    let tail_start = ceil_char_boundary(text, text.len() - tail_len);
    format!(
        "{}\n[… {} bytes omitted …]\n{}",
        &text[..head_end],
        text.len() - head_end - (text.len() - tail_start),
        &text[tail_start..]
    )
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kiln-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn relative_paths_resolve_inside_root() {
        let root = temp_root();
        let p = resolve_in_root(&root, "src/main.rs").unwrap();
        assert!(p.starts_with(&root));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let root = temp_root();
        let err = resolve_in_root(&root, "../outside.txt").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn nested_traversal_that_exists_is_rejected() {
        let root = temp_root();
        std::fs::create_dir_all(root.join("a")).unwrap();
        let err = resolve_in_root(&root, "a/../../outside.txt").unwrap_err();
        assert!(err.contains("outside"), "{err}");
    }

    #[test]
    fn truncation_keeps_head_and_tail() {
        let text = format!("{}{}", "a".repeat(1000), "z".repeat(1000));
        let out = truncate_output(&text, 400);
        assert!(out.starts_with("aaaa"));
        assert!(out.ends_with("zzzz"));
        assert!(out.contains("bytes omitted"));
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let text = "é".repeat(500);
        let out = truncate_output(&text, 101);
        assert!(out.contains("bytes omitted"));
    }
}
