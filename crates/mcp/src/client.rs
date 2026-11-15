use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("process exited before responding")]
    ProcessGone,
    #[error("rpc error: {0}")]
    Rpc(String),
    #[error("timed out waiting for a response")]
    Timeout,
    #[error("could not decode: {0}")]
    Decode(String),
}

pub struct McpClient {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<tokio::process::ChildStdin>,
    pending: Arc<Mutex<BTreeMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    _reader: JoinHandle<()>,
    _child: Mutex<Child>,
}

impl McpClient {
    pub async fn spawn(cfg: &kiln_core::config::McpServerConfig) -> Result<Self, McpError> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args);
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(McpError::ProcessGone)?;
        let stdout = child.stdout.take().ok_or(McpError::ProcessGone)?;

        let pending: Arc<Mutex<BTreeMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let reader_pending = pending.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    if let Some(tx) = reader_pending.lock().await.remove(&id) {
                        let _ = tx.send(value);
                    }
                }
            }
        });

        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(stdin),
                pending,
                next_id: AtomicU64::new(1),
                _reader: reader,
                _child: Mutex::new(child),
            }),
        })
    }

    pub async fn initialize(&self) -> Result<(), McpError> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "kiln", "version": "0.1.0" }
                }),
            )
            .await?;
        let notif = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let mut w = self.inner.writer.lock().await;
        w.write_all(notif.to_string().as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolSpec>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let arr = result
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .into_iter()
            .filter_map(|t| {
                Some(McpToolSpec {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect())
    }

    pub async fn call_tool(&self, name: &str, args: &Value) -> Result<String, McpError> {
        let result = self
            .request("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        let content = result
            .get("content")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = String::new();
        for c in content {
            if c.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(McpError::Rpc(out.trim().to_string()));
        }
        Ok(out)
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        {
            let mut w = self.inner.writer.lock().await;
            w.write_all(req.to_string().as_bytes()).await?;
            w.write_all(b"\n").await?;
            w.flush().await?;
        }
        let resp = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(|_| McpError::ProcessGone)?;
        if let Some(err) = resp.get("error") {
            return Err(McpError::Rpc(err.to_string()));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }
}

impl Clone for McpClient {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[derive(Clone)]
pub struct McpToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
