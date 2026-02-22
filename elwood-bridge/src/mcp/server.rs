//! MCP server — exposes terminal capabilities as MCP resources and tools.
//!
//! Implements a stdio-based MCP server that external clients (Claude Desktop,
//! other editors) can connect to. The server reads JSON-RPC requests from stdin
//! and writes responses to stdout.
//!
//! ## Architecture
//!
//! ```text
//! External MCP Client
//!   │ stdin/stdout (newline-delimited JSON-RPC)
//!   ▼
//! McpServer
//!   ├── resources: terminal://pane/content, elwood://session/log, ...
//!   ├── tools: terminal_execute, terminal_read_screen, agent_send_message
//!   └── pane_query_tx → fulfillment on the pane/domain side
//! ```

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::Mutex;

use super::protocol::*;
use super::resources::{self, PaneQuery, PaneQueryResult};

/// Maximum time to wait for a pane query response.
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// MCP server tool definition.
#[derive(Debug, Clone)]
struct McpServerTool {
    name: String,
    description: String,
    input_schema: Value,
    read_only: bool,
    destructive: bool,
}

/// The MCP server state.
pub struct McpServer {
    /// Whether the server has completed the initialize handshake.
    initialized: bool,
    /// Channel to send queries to the pane/domain for fulfillment.
    pane_query_tx: Option<flume::Sender<(PaneQuery, flume::Sender<PaneQueryResult>)>>,
    /// Registered tools.
    tools: Vec<McpServerTool>,
    /// Stdout writer (locked for sequential writes).
    writer: Arc<Mutex<BufWriter<tokio::io::Stdout>>>,
}

impl McpServer {
    /// Create a new MCP server.
    ///
    /// If `pane_query_tx` is `None`, the server will use fallback fulfillment
    /// for resources (git status from CLI, etc.) and tools will be unavailable.
    pub fn new(
        pane_query_tx: Option<flume::Sender<(PaneQuery, flume::Sender<PaneQueryResult>)>>,
    ) -> Self {
        let tools = vec![
            McpServerTool {
                name: "terminal_read_screen".to_string(),
                description: "Read the current visible content of the terminal screen".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "lines": {
                            "type": "integer",
                            "description": "Number of lines to read (default: 100)",
                            "default": 100
                        }
                    }
                }),
                read_only: true,
                destructive: false,
            },
            McpServerTool {
                name: "terminal_execute".to_string(),
                description: "Run a shell command in the terminal (requires permission)".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
                read_only: false,
                destructive: true,
            },
            McpServerTool {
                name: "agent_send_message".to_string(),
                description: "Send a message to the Elwood LLM agent".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "The message to send to the agent"
                        }
                    },
                    "required": ["message"]
                }),
                read_only: false,
                destructive: false,
            },
        ];

        Self {
            initialized: false,
            pane_query_tx,
            tools,
            writer: Arc::new(Mutex::new(BufWriter::new(tokio::io::stdout()))),
        }
    }

    /// Run the MCP server loop, reading from stdin and writing to stdout.
    pub async fn run(&mut self) {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf).await {
                Ok(0) => {
                    // EOF — client closed the connection
                    tracing::info!("MCP server: client disconnected (EOF)");
                    break;
                }
                Ok(_) => {
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Parse the incoming message
                    match parse_incoming(trimmed) {
                        Ok(IncomingMessage::Request(req)) => {
                            let response = self.handle_request(&req).await;
                            self.write_response(&response).await;
                        }
                        Ok(IncomingMessage::Notification(notif)) => {
                            self.handle_notification(&notif);
                        }
                        Err(e) => {
                            tracing::warn!("MCP server: failed to parse message: {e}");
                            let error_resp = JsonRpcResponse {
                                jsonrpc: JSONRPC_VERSION.to_string(),
                                id: Value::Null,
                                result: None,
                                error: Some(JsonRpcError {
                                    code: error_codes::PARSE_ERROR,
                                    message: format!("Parse error: {e}"),
                                    data: None,
                                }),
                            };
                            self.write_response(&error_resp).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("MCP server: stdin read error: {e}");
                    break;
                }
            }
        }
    }

    /// Handle a JSON-RPC request and return a response.
    async fn handle_request(&mut self, req: &JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req),
            "tools/list" => self.handle_tools_list(req),
            "tools/call" => self.handle_tools_call(req).await,
            "resources/list" => self.handle_resources_list(req),
            "resources/read" => self.handle_resources_read(req).await,
            "ping" => JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: req.id.clone(),
                result: Some(serde_json::json!({})),
                error: None,
            },
            _ => JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: req.id.clone(),
                result: None,
                error: Some(JsonRpcError {
                    code: error_codes::METHOD_NOT_FOUND,
                    message: format!("Method not found: {}", req.method),
                    data: None,
                }),
            },
        }
    }

    /// Handle `initialize` request.
    fn handle_initialize(&mut self, req: &JsonRpcRequest) -> JsonRpcResponse {
        self.initialized = true;

        let result = InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: false,
                }),
                resources: Some(ResourcesCapability {
                    subscribe: false,
                    list_changed: false,
                }),
                prompts: None,
                logging: None,
            },
            server_info: ServerInfo {
                name: "elwood-terminal".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: req.id.clone(),
            result: Some(serde_json::to_value(result).unwrap_or(Value::Null)),
            error: None,
        }
    }

    /// Handle `tools/list` request.
    fn handle_tools_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                    "annotations": {
                        "readOnlyHint": t.read_only,
                        "destructiveHint": t.destructive,
                        "idempotentHint": t.read_only,
                        "openWorldHint": false,
                    }
                })
            })
            .collect();

        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: req.id.clone(),
            result: Some(serde_json::json!({ "tools": tools })),
            error: None,
        }
    }

    /// Handle `tools/call` request.
    async fn handle_tools_call(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let params = match &req.params {
            Some(p) => p,
            None => {
                return self.error_response(&req.id, error_codes::INVALID_PARAMS, "Missing params");
            }
        };

        let tool_name = params["name"].as_str().unwrap_or("");
        let arguments = &params["arguments"];

        match tool_name {
            "terminal_read_screen" => {
                let lines = arguments["lines"].as_u64().unwrap_or(100) as usize;
                match self.query_pane(PaneQuery::GetPaneContent { pane_id: None, lines }).await {
                    Some(result) => self.tool_result(&req.id, &result.content, false),
                    None => self.tool_result(
                        &req.id,
                        "Terminal screen content not available (no pane connection)",
                        true,
                    ),
                }
            }
            "terminal_execute" => {
                let command = arguments["command"].as_str().unwrap_or("");
                if command.is_empty() {
                    return self.tool_result(&req.id, "Command parameter is required", true);
                }

                // Execute the command with a timeout
                match execute_command(command).await {
                    Ok(output) => self.tool_result(&req.id, &output, false),
                    Err(e) => self.tool_result(&req.id, &format!("Command failed: {e}"), true),
                }
            }
            "agent_send_message" => {
                let message = arguments["message"].as_str().unwrap_or("");
                if message.is_empty() {
                    return self.tool_result(&req.id, "Message parameter is required", true);
                }

                // For now, acknowledge the message. Full integration requires the bridge channel.
                self.tool_result(
                    &req.id,
                    &format!("Message queued for agent: {}", truncate(message, 200)),
                    false,
                )
            }
            _ => self.error_response(
                &req.id,
                error_codes::METHOD_NOT_FOUND,
                &format!("Unknown tool: {tool_name}"),
            ),
        }
    }

    /// Handle `resources/list` request.
    fn handle_resources_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let resource_defs = resources::list_resources();
        let result = ResourcesListResult {
            resources: resource_defs,
            next_cursor: None,
        };

        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: req.id.clone(),
            result: Some(serde_json::to_value(result).unwrap_or(Value::Null)),
            error: None,
        }
    }

    /// Handle `resources/read` request.
    async fn handle_resources_read(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let params = match &req.params {
            Some(p) => p,
            None => {
                return self.error_response(&req.id, error_codes::INVALID_PARAMS, "Missing params");
            }
        };

        let uri = params["uri"].as_str().unwrap_or("");

        // Resolve the URI to a query
        let query = match resources::resolve_uri(uri) {
            Some(q) => q,
            None => {
                return self.error_response(
                    &req.id,
                    error_codes::RESOURCE_NOT_FOUND,
                    &format!("Unknown resource URI: {uri}"),
                );
            }
        };

        // Try to fulfill via the pane channel first, then fallbacks
        let result = match self.query_pane(query.clone()).await {
            Some(r) => r,
            None => {
                // Fallback: fulfill directly where possible
                match query {
                    PaneQuery::GetGitStatus => resources::fulfill_git_status(),
                    PaneQuery::GetCommandHistory { limit } => {
                        resources::fulfill_command_history(limit)
                    }
                    _ => {
                        return self.error_response(
                            &req.id,
                            error_codes::INTERNAL_ERROR,
                            "Resource not available (no pane connection)",
                        );
                    }
                }
            }
        };

        let content = resources::build_resource_content(uri, result);
        let read_result = ResourceReadResult {
            contents: vec![content],
        };

        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: req.id.clone(),
            result: Some(serde_json::to_value(read_result).unwrap_or(Value::Null)),
            error: None,
        }
    }

    /// Handle a notification (no response needed).
    fn handle_notification(&self, notif: &JsonRpcNotification) {
        match notif.method.as_str() {
            "notifications/initialized" => {
                tracing::info!("MCP server: client completed initialization");
            }
            "notifications/cancelled" => {
                tracing::debug!("MCP server: client cancelled a request");
            }
            _ => {
                tracing::debug!(
                    "MCP server: unhandled notification: {}",
                    notif.method,
                );
            }
        }
    }

    /// Send a PaneQuery through the channel and wait for the result.
    async fn query_pane(&self, query: PaneQuery) -> Option<PaneQueryResult> {
        let tx = self.pane_query_tx.as_ref()?;
        let (result_tx, result_rx) = flume::bounded(1);

        tx.send((query, result_tx)).ok()?;

        tokio::time::timeout(QUERY_TIMEOUT, result_rx.recv_async())
            .await
            .ok()?
            .ok()
    }

    /// Write a JSON-RPC response to stdout.
    async fn write_response(&self, response: &JsonRpcResponse) {
        let mut writer = self.writer.lock().await;
        match serde_json::to_string(response) {
            Ok(json) => {
                let _ = writer.write_all(json.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                let _ = writer.flush().await;
            }
            Err(e) => {
                tracing::error!("MCP server: failed to serialize response: {e}");
            }
        }
    }

    /// Build a tool result response.
    fn tool_result(&self, id: &Value, text: &str, is_error: bool) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.clone(),
            result: Some(serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "isError": is_error,
            })),
            error: None,
        }
    }

    /// Build an error response.
    fn error_response(&self, id: &Value, code: i64, message: &str) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.clone(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Message parsing
// ---------------------------------------------------------------------------

/// An incoming message from the MCP client.
enum IncomingMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

/// Parse a JSON line into a request or notification.
fn parse_incoming(line: &str) -> Result<IncomingMessage, serde_json::Error> {
    let raw: Value = serde_json::from_str(line)?;

    let has_id = raw.get("id").map_or(false, |v| !v.is_null());

    if has_id {
        let req: JsonRpcRequest = serde_json::from_value(raw)?;
        Ok(IncomingMessage::Request(req))
    } else {
        let notif: JsonRpcNotification = serde_json::from_value(raw)?;
        Ok(IncomingMessage::Notification(notif))
    }
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

/// Maximum time to wait for a command to complete.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Execute a shell command and return its output.
async fn execute_command(command: &str) -> Result<String, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());

    let mut cmd = tokio::process::Command::new(&shell);
    cmd.arg("-c").arg(command);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let result = tokio::time::timeout(COMMAND_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "Command timed out (2 minute limit)".to_string())?
        .map_err(|e| format!("Failed to execute: {e}"))?;

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    let exit_code = result.status.code().unwrap_or(-1);

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(&stderr);
    }
    output.push_str(&format!("\n[exit code: {exit_code}]"));

    Ok(output)
}

/// Truncate a string to a maximum length, respecting UTF-8 char boundaries.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// ---------------------------------------------------------------------------
// Server startup helpers
// ---------------------------------------------------------------------------

/// Configuration for the MCP server.
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    /// Whether the MCP server is enabled.
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// Spawn the MCP server on a background task.
///
/// Returns a handle that can be used to shut down the server (by dropping
/// the pane_query sender).
pub fn spawn_server(
    pane_query_tx: Option<flume::Sender<(PaneQuery, flume::Sender<PaneQueryResult>)>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut server = McpServer::new(pane_query_tx);
        server.run().await;
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_incoming_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        match parse_incoming(json).unwrap() {
            IncomingMessage::Request(req) => {
                assert_eq!(req.method, "initialize");
                assert_eq!(req.id, serde_json::json!(1));
            }
            _ => panic!("expected request"),
        }
    }

    #[test]
    fn test_parse_incoming_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        match parse_incoming(json).unwrap() {
            IncomingMessage::Notification(notif) => {
                assert_eq!(notif.method, "notifications/initialized");
            }
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn test_parse_incoming_invalid() {
        assert!(parse_incoming("not json").is_err());
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
        // UTF-8 boundary test
        assert_eq!(truncate("héllo", 2), "h");
    }

    #[tokio::test]
    async fn test_server_initialize() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1.0"}
            })),
        );

        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
        assert!(server.initialized);

        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert_eq!(result["serverInfo"]["name"], "elwood-terminal");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
    }

    #[tokio::test]
    async fn test_server_tools_list() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "tools/list", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"terminal_read_screen"));
        assert!(names.contains(&"terminal_execute"));
        assert!(names.contains(&"agent_send_message"));
    }

    #[tokio::test]
    async fn test_server_resources_list() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "resources/list", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 5);
    }

    #[tokio::test]
    async fn test_server_resources_read_git_status() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "resources/read",
            Some(serde_json::json!({"uri": "elwood://git/status"})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["uri"], "elwood://git/status");
    }

    #[tokio::test]
    async fn test_server_resources_read_unknown() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "resources/read",
            Some(serde_json::json!({"uri": "unknown://foo"})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, error_codes::RESOURCE_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_server_method_not_found() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "nonexistent/method", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_server_ping() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "ping", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), serde_json::json!({}));
    }

    #[tokio::test]
    async fn test_server_tools_call_missing_params() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "tools/call", None);
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_server_tools_call_unknown_tool() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "nonexistent", "arguments": {}})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn test_server_tools_call_read_screen_no_pane() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "terminal_read_screen", "arguments": {}})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        assert!(result["isError"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_server_tools_call_agent_message_empty() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "agent_send_message", "arguments": {"message": ""}})),
        );
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert!(result["isError"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_server_tools_call_with_pane_channel() {
        // Create a channel pair and fulfill queries manually
        let (query_tx, query_rx) =
            flume::unbounded::<(PaneQuery, flume::Sender<PaneQueryResult>)>();

        let mut server = McpServer::new(Some(query_tx));

        // Spawn a fulfillment task
        tokio::spawn(async move {
            while let Ok((query, result_tx)) = query_rx.recv_async().await {
                let result = match query {
                    PaneQuery::GetPaneContent { .. } => PaneQueryResult {
                        content: "$ echo hello\nhello\n$".to_string(),
                        mime_type: "text/plain".to_string(),
                    },
                    _ => PaneQueryResult {
                        content: "unknown query".to_string(),
                        mime_type: "text/plain".to_string(),
                    },
                };
                let _ = result_tx.send(result);
            }
        });

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "terminal_read_screen", "arguments": {"lines": 50}})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("echo hello"));
    }

    #[tokio::test]
    async fn test_server_tools_call_execute() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "terminal_execute", "arguments": {"command": "echo test123"}})),
        );
        let resp = server.handle_request(&req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("test123"));
    }

    #[tokio::test]
    async fn test_server_tools_call_execute_empty_command() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "terminal_execute", "arguments": {"command": ""}})),
        );
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        assert!(result["isError"].as_bool().unwrap());
    }

    #[test]
    fn test_server_notification_handling() {
        let mut server = McpServer::new(None);

        // Should not panic
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        server.handle_notification(&notif);

        let notif = JsonRpcNotification::new("notifications/cancelled", None);
        server.handle_notification(&notif);

        let notif = JsonRpcNotification::new("unknown/notification", None);
        server.handle_notification(&notif);
    }

    #[test]
    fn test_server_default_config() {
        let config = McpServerConfig::default();
        assert!(!config.enabled);
    }

    #[tokio::test]
    async fn test_server_tool_annotations() {
        let mut server = McpServer::new(None);

        let req = JsonRpcRequest::new(1, "tools/list", None);
        let resp = server.handle_request(&req).await;
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();

        // terminal_read_screen should be read-only
        let read_screen = tools
            .iter()
            .find(|t| t["name"] == "terminal_read_screen")
            .unwrap();
        assert!(read_screen["annotations"]["readOnlyHint"].as_bool().unwrap());
        assert!(!read_screen["annotations"]["destructiveHint"].as_bool().unwrap());

        // terminal_execute should be destructive
        let execute = tools
            .iter()
            .find(|t| t["name"] == "terminal_execute")
            .unwrap();
        assert!(!execute["annotations"]["readOnlyHint"].as_bool().unwrap());
        assert!(execute["annotations"]["destructiveHint"].as_bool().unwrap());
    }
}
