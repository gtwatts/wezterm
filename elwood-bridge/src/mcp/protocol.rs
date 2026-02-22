//! JSON-RPC 2.0 protocol types for MCP communication.
//!
//! Implements the minimal set of JSON-RPC types needed for the MCP client:
//! requests, responses, notifications, and MCP-specific error codes.

use serde::{Deserialize, Serialize};

/// JSON-RPC protocol version constant.
pub const JSONRPC_VERSION: &str = "2.0";

/// MCP protocol version we support.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 core types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request (has an `id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Create a new request with the given method and parameters.
    pub fn new(id: u64, method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: serde_json::Value::Number(id.into()),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 notification (no `id` — fire and forget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcNotification {
    /// Create a notification with the given method.
    pub fn new(method: impl Into<String>, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Returns `true` if this response indicates an error.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Extract the result, returning an error if the response was an error.
    pub fn into_result(self) -> Result<serde_json::Value, JsonRpcError> {
        if let Some(err) = self.error {
            Err(err)
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for JsonRpcError {}

// ---------------------------------------------------------------------------
// A message that can be either a response or notification from the server
// ---------------------------------------------------------------------------

/// An incoming message from an MCP server (can be response or notification).
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// A response to a client request (has `id`).
    Response(JsonRpcResponse),
    /// A server-initiated notification (has `method`, no `id`).
    Notification(JsonRpcNotification),
}

impl ServerMessage {
    /// Parse a raw JSON line into a `ServerMessage`.
    ///
    /// Uses a heuristic: if the message has a "method" field and no "id" (or
    /// "id" is null), it's a notification. Otherwise it's a response.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        let raw: serde_json::Value = serde_json::from_str(line)?;

        let has_method = raw.get("method").is_some();
        let has_id = raw
            .get("id")
            .map_or(false, |v| !v.is_null());

        if has_method && !has_id {
            let notif: JsonRpcNotification = serde_json::from_value(raw)?;
            Ok(Self::Notification(notif))
        } else {
            let resp: JsonRpcResponse = serde_json::from_value(raw)?;
            Ok(Self::Response(resp))
        }
    }
}

// ---------------------------------------------------------------------------
// Standard JSON-RPC error codes
// ---------------------------------------------------------------------------

/// Standard JSON-RPC error codes and MCP-specific codes.
pub mod error_codes {
    /// Parse error: invalid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Invalid request: not a valid JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;

    // MCP-specific
    /// Resource not found.
    pub const RESOURCE_NOT_FOUND: i64 = -32002;
}

// ---------------------------------------------------------------------------
// MCP-specific types
// ---------------------------------------------------------------------------

/// Client info sent during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Server info received during initialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// Client capabilities advertised during initialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<serde_json::Value>,
}

/// Server capabilities received during initialization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(default)]
    pub tools: Option<ToolsCapability>,
    #[serde(default)]
    pub resources: Option<ResourcesCapability>,
    #[serde(default)]
    pub prompts: Option<serde_json::Value>,
    #[serde(default)]
    pub logging: Option<serde_json::Value>,
}

/// Tools capability descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

/// Resources capability descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    #[serde(default)]
    pub subscribe: bool,
    #[serde(default)]
    pub list_changed: bool,
}

/// Parameters for the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

/// Result of the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// An MCP tool definition from `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub annotations: Option<McpToolAnnotations>,
}

/// Tool behavior annotations from the MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default)]
    pub read_only_hint: bool,
    #[serde(default)]
    pub destructive_hint: bool,
    #[serde(default)]
    pub idempotent_hint: bool,
    #[serde(default)]
    pub open_world_hint: bool,
}

/// Result of `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<McpToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Result of `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    #[serde(default)]
    pub content: Vec<ToolCallContent>,
    #[serde(default)]
    pub is_error: bool,
}

/// Content item in a tool call result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolCallContent {
    #[serde(rename = "text")]
    Text {
        text: String,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource {
        resource: ResourceContent,
    },
}

/// An MCP resource descriptor from `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceDef {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// Result of `resources/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesListResult {
    pub resources: Vec<McpResourceDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Content of a read resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContent {
    pub uri: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob: Option<String>,
}

/// Result of `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReadResult {
    pub contents: Vec<ResourceContent>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new(
            1,
            "initialize",
            Some(serde_json::json!({"protocolVersion": "2025-11-25"})),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn test_notification_serialization() {
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"method\":\"notifications/initialized\""));
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn test_response_deserialization_success() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"protocolVersion": "2025-11-25"}
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.is_error());
        assert_eq!(resp.id, serde_json::json!(1));
        let result = resp.into_result().unwrap();
        assert_eq!(result["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn test_response_deserialization_error() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32601, "message": "Method not found"}
        }"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_error());
        let err = resp.into_result().unwrap_err();
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn test_server_message_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let msg = ServerMessage::from_line(json).unwrap();
        assert!(matches!(msg, ServerMessage::Response(_)));
    }

    #[test]
    fn test_server_message_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
        let msg = ServerMessage::from_line(json).unwrap();
        assert!(matches!(msg, ServerMessage::Notification(_)));
    }

    #[test]
    fn test_initialize_params_serialization() {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "elwood-terminal".to_string(),
                version: "0.3.1".to_string(),
            },
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(json["clientInfo"]["name"], "elwood-terminal");
    }

    #[test]
    fn test_initialize_result_deserialization() {
        let json = r#"{
            "protocolVersion": "2025-11-25",
            "capabilities": {
                "tools": {"listChanged": true},
                "resources": {"subscribe": true, "listChanged": true}
            },
            "serverInfo": {"name": "test-server", "version": "1.0.0"}
        }"#;
        let result: InitializeResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.server_info.name, "test-server");
        assert!(result.capabilities.tools.as_ref().unwrap().list_changed);
        assert!(result.capabilities.resources.as_ref().unwrap().subscribe);
    }

    #[test]
    fn test_tool_def_deserialization() {
        let json = r#"{
            "name": "read_file",
            "description": "Read a file",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false
            }
        }"#;
        let tool: McpToolDef = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "read_file");
        assert!(tool.annotations.as_ref().unwrap().read_only_hint);
        assert!(!tool.annotations.as_ref().unwrap().destructive_hint);
    }

    #[test]
    fn test_tools_list_result() {
        let json = r#"{
            "tools": [
                {"name": "a", "inputSchema": {}},
                {"name": "b", "inputSchema": {}}
            ]
        }"#;
        let result: ToolsListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 2);
    }

    #[test]
    fn test_tool_call_result_text() {
        let json = r#"{
            "content": [{"type": "text", "text": "hello world"}],
            "isError": false
        }"#;
        let result: ToolCallResult = serde_json::from_str(json).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ToolCallContent::Text { text } => assert_eq!(text, "hello world"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_tool_call_result_error() {
        let json = r#"{
            "content": [{"type": "text", "text": "file not found"}],
            "isError": true
        }"#;
        let result: ToolCallResult = serde_json::from_str(json).unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn test_resource_def_deserialization() {
        let json = r#"{
            "uri": "file:///path/to/file",
            "name": "My File",
            "description": "A file resource",
            "mimeType": "text/plain"
        }"#;
        let resource: McpResourceDef = serde_json::from_str(json).unwrap();
        assert_eq!(resource.uri, "file:///path/to/file");
        assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn test_resource_read_result() {
        let json = r#"{
            "contents": [{
                "uri": "file:///test",
                "text": "hello",
                "mimeType": "text/plain"
            }]
        }"#;
        let result: ResourceReadResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn test_jsonrpc_error_display() {
        let err = JsonRpcError {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        };
        assert_eq!(err.to_string(), "JSON-RPC error -32601: Method not found");
    }
}
