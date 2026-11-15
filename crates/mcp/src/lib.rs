//! MCP (Model Context Protocol) client. Kiln only implements the stdio transport
//! for now; HTTP/SSE comes later. Each server is a child process that speaks
//! newline-delimited JSON-RPC 2.0.

pub mod client;
pub mod tool;

pub use client::{McpClient, McpError, McpToolSpec};
pub use tool::McpTool;

use kiln_core::config::McpServerConfig;
use kiln_tools::Risk;
use kiln_tools::Tool;
use std::sync::Arc;

/// Spawn every configured server, run its handshake and tool discovery, and
/// return the discovered tools. Servers that fail to start are reported as
/// warnings rather than aborting the whole run, matching provider behaviour.
pub async fn discover(
    servers: &std::collections::BTreeMap<String, McpServerConfig>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (name, cfg) in servers {
        match McpClient::connect(cfg).await {
            Ok(client) => {
                if let Err(e) = client.initialize().await {
                    warnings.push(format!("mcp '{name}': initialize failed: {e}"));
                    continue;
                }
                match client.list_tools().await {
                    Ok(specs) => {
                        for spec in specs {
                            let risk = match cfg.trust {
                                kiln_core::permission::Mode::Allow => Risk::Read,
                                kiln_core::permission::Mode::Readonly => Risk::Read,
                                kiln_core::permission::Mode::Ask => Risk::Write,
                            };
                            tools.push(Arc::new(McpTool {
                                server: name.clone(),
                                tool_name: spec.name,
                                description: spec.description,
                                schema: spec.input_schema,
                                client: client.clone(),
                                risk,
                            }));
                        }
                    }
                    Err(e) => warnings.push(format!("mcp '{name}': tools/list failed: {e}")),
                }
            }
            Err(e) => warnings.push(format!("mcp '{name}': failed to start: {e}")),
        }
    }
    (tools, warnings)
}
