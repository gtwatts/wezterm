//! MCP tool adapter — wraps an MCP server tool as an elwood-core `Tool`.
//!
//! Each discovered MCP tool is wrapped in an `McpToolAdapter` that implements
//! the `Tool` trait from elwood-core. The agent interacts with MCP tools
//! identically to built-in tools.

use std::sync::Arc;

use async_trait::async_trait;
use elwood_core::tools::{RiskLevel, Tool, ToolCategory, ToolResult};

use super::client::McpClient;
use super::protocol::{McpToolAnnotations, McpToolDef, ToolCallContent};

/// Wraps a single MCP server tool as an elwood-core `Tool`.
///
/// Tool names are namespaced as `mcp__{server_name}__{tool_name}` to avoid
/// collisions between servers and with built-in tools. This matches the
/// Claude Code / Claude Desktop convention.
pub struct McpToolAdapter {
    /// Namespaced tool name: `mcp__{server}__{tool}`.
    namespaced_name: String,
    /// The original tool name on the MCP server.
    remote_tool_name: String,
    /// Tool description from the MCP server.
    tool_description: String,
    /// JSON Schema for input parameters.
    input_schema: serde_json::Value,
    /// Tool annotations from the MCP server.
    annotations: McpToolAnnotations,
    /// The MCP client connection to route calls through.
    client: Arc<McpClient>,
    /// The MCP server name (for error messages).
    server_name: String,
}

impl McpToolAdapter {
    /// Create a new adapter from an MCP tool definition.
    pub fn new(server_name: &str, tool_def: &McpToolDef, client: Arc<McpClient>) -> Self {
        let namespaced_name = format!("mcp__{server_name}__{}", tool_def.name);
        Self {
            namespaced_name,
            remote_tool_name: tool_def.name.clone(),
            tool_description: tool_def
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool: {}", tool_def.name)),
            input_schema: tool_def.input_schema.clone(),
            annotations: tool_def.annotations.clone().unwrap_or_default(),
            client,
            server_name: server_name.to_string(),
        }
    }

    /// Build the namespaced tool name from server and tool names.
    pub fn namespaced_name(server_name: &str, tool_name: &str) -> String {
        format!("mcp__{server_name}__{tool_name}")
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn category(&self) -> ToolCategory {
        if self.annotations.read_only_hint {
            ToolCategory::ReadOnly
        } else {
            // MCP tools that aren't read-only are treated as network/external tools
            ToolCategory::Network
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, arguments: serde_json::Value) -> elwood_core::error::Result<ToolResult> {
        if !self.client.is_connected() {
            return Ok(ToolResult::error(format!(
                "MCP server '{}' is disconnected",
                self.server_name,
            )));
        }

        match self
            .client
            .call_tool(&self.remote_tool_name, arguments)
            .await
        {
            Ok(result) => {
                let content = format_tool_result(&result.content);
                if result.is_error {
                    Ok(ToolResult::error(content))
                } else {
                    Ok(ToolResult::success(content))
                }
            }
            Err(e) => Ok(ToolResult::error(format!(
                "MCP tool call failed (server '{}'): {e}",
                self.server_name,
            ))),
        }
    }

    fn risk_level(&self) -> RiskLevel {
        if self.annotations.destructive_hint {
            RiskLevel::Dangerous
        } else if self.annotations.read_only_hint {
            RiskLevel::Safe
        } else {
            RiskLevel::Moderate
        }
    }

    fn usage_example(&self) -> &str {
        "" // MCP tools are dynamic; no static examples
    }
}

/// Format MCP tool result content into a single string.
fn format_tool_result(content: &[ToolCallContent]) -> String {
    let mut parts = Vec::new();
    for item in content {
        match item {
            ToolCallContent::Text { text } => {
                parts.push(text.clone());
            }
            ToolCallContent::Image { mime_type, .. } => {
                parts.push(format!("[Image: {mime_type}]"));
            }
            ToolCallContent::Resource { resource } => {
                if let Some(text) = &resource.text {
                    parts.push(text.clone());
                } else if let Some(blob) = &resource.blob {
                    parts.push(format!("[Resource blob: {} bytes]", blob.len()));
                } else {
                    parts.push(format!("[Resource: {}]", resource.uri));
                }
            }
        }
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::ResourceContent;

    #[test]
    fn test_namespaced_name() {
        assert_eq!(
            McpToolAdapter::namespaced_name("filesystem", "read_file"),
            "mcp__filesystem__read_file"
        );
        assert_eq!(
            McpToolAdapter::namespaced_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
    }

    #[test]
    fn test_format_tool_result_text() {
        let content = vec![ToolCallContent::Text {
            text: "hello world".to_string(),
        }];
        assert_eq!(format_tool_result(&content), "hello world");
    }

    #[test]
    fn test_format_tool_result_multiple() {
        let content = vec![
            ToolCallContent::Text {
                text: "line 1".to_string(),
            },
            ToolCallContent::Text {
                text: "line 2".to_string(),
            },
        ];
        assert_eq!(format_tool_result(&content), "line 1\nline 2");
    }

    #[test]
    fn test_format_tool_result_image() {
        let content = vec![ToolCallContent::Image {
            data: "base64data".to_string(),
            mime_type: "image/png".to_string(),
        }];
        assert_eq!(format_tool_result(&content), "[Image: image/png]");
    }

    #[test]
    fn test_format_tool_result_resource_text() {
        let content = vec![ToolCallContent::Resource {
            resource: ResourceContent {
                uri: "file:///test".to_string(),
                mime_type: Some("text/plain".to_string()),
                text: Some("file contents".to_string()),
                blob: None,
            },
        }];
        assert_eq!(format_tool_result(&content), "file contents");
    }

    #[test]
    fn test_format_tool_result_resource_blob() {
        let content = vec![ToolCallContent::Resource {
            resource: ResourceContent {
                uri: "file:///test".to_string(),
                mime_type: None,
                text: None,
                blob: Some("AQID".to_string()),
            },
        }];
        assert_eq!(format_tool_result(&content), "[Resource blob: 4 bytes]");
    }

    #[test]
    fn test_format_tool_result_empty() {
        let content: Vec<ToolCallContent> = vec![];
        assert_eq!(format_tool_result(&content), "");
    }

    #[test]
    fn test_category_from_annotations() {
        // Helper to check category inference
        fn check(read_only: bool, destructive: bool) -> ToolCategory {
            let annotations = McpToolAnnotations {
                read_only_hint: read_only,
                destructive_hint: destructive,
                idempotent_hint: false,
                open_world_hint: false,
            };
            if annotations.read_only_hint {
                ToolCategory::ReadOnly
            } else {
                ToolCategory::Network
            }
        }

        assert_eq!(check(true, false), ToolCategory::ReadOnly);
        assert_eq!(check(false, true), ToolCategory::Network);
        assert_eq!(check(false, false), ToolCategory::Network);
    }

    #[test]
    fn test_risk_level_from_annotations() {
        fn check(read_only: bool, destructive: bool) -> RiskLevel {
            let annotations = McpToolAnnotations {
                read_only_hint: read_only,
                destructive_hint: destructive,
                idempotent_hint: false,
                open_world_hint: false,
            };
            if annotations.destructive_hint {
                RiskLevel::Dangerous
            } else if annotations.read_only_hint {
                RiskLevel::Safe
            } else {
                RiskLevel::Moderate
            }
        }

        assert_eq!(check(true, false), RiskLevel::Safe);
        assert_eq!(check(false, true), RiskLevel::Dangerous);
        assert_eq!(check(false, false), RiskLevel::Moderate);
        // destructive takes priority over read_only
        assert_eq!(check(true, true), RiskLevel::Dangerous);
    }
}
