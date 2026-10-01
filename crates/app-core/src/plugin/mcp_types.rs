//! App-facing descriptions of ready compiled MCP exports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One MCP tool published by a ready compiled plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginMcpTool {
    pub plugin_id: String,
    pub tool_name: String,
    pub description: String,
    pub input_schema: Value,
}

impl PluginMcpTool {
    /// Returns the complete signed input schema.
    pub fn mcp_schema(&self) -> Value {
        self.input_schema.clone()
    }
}
