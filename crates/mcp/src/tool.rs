use crate::McpClient;
use async_trait::async_trait;
use kiln_core::Risk;
use kiln_tools::{Tool, ToolCtx, ToolOutcome, ToolSpec};
use serde_json::Value;

pub struct McpTool {
    pub server: String,
    pub tool_name: String,
    pub description: String,
    pub schema: Value,
    pub client: McpClient,
    pub risk: Risk,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: format!("{}_{}", self.server, self.tool_name),
            description: self.description.clone(),
            parameters: self.schema.clone(),
            risk: self.risk,
        }
    }

    async fn run(&self, args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        match self.client.call_tool(&self.tool_name, args).await {
            Ok(text) => ToolOutcome::ok(text),
            Err(e) => ToolOutcome::err(e.to_string()),
        }
    }
}
