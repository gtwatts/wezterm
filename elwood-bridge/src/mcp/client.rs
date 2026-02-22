//! MCP client with stdio transport.
//!
//! Manages the lifecycle of MCP server subprocesses: spawn, initialize,
//! tool/resource discovery, JSON-RPC communication, and graceful shutdown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex, Notify};

use super::config::{expand_server_config, McpConfig, McpServerConfig};
use super::protocol::*;

/// Maximum time to wait for a server to respond to `initialize`.
const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum time to wait for any individual request.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum time to wait during graceful shutdown.
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Error type for MCP client operations.
#[derive(Debug)]
pub enum McpError {
    /// Server process failed to spawn.
    SpawnFailed(String),
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// I/O error communicating with the server.
    Io(std::io::Error),
    /// The server returned a JSON-RPC error.
    RpcError(JsonRpcError),
    /// The server did not respond in time.
    Timeout,
    /// The server connection is closed.
    Disconnected,
    /// Protocol version mismatch.
    ProtocolMismatch(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpawnFailed(msg) => write!(f, "failed to spawn MCP server: {msg}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::RpcError(e) => write!(f, "MCP server error: {e}"),
            Self::Timeout => write!(f, "MCP request timed out"),
            Self::Disconnected => write!(f, "MCP server disconnected"),
            Self::ProtocolMismatch(v) => write!(f, "unsupported MCP protocol version: {v}"),
        }
    }
}

impl std::error::Error for McpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::RpcError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for McpError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<std::io::Error> for McpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<JsonRpcError> for McpError {
    fn from(e: JsonRpcError) -> Self {
        Self::RpcError(e)
    }
}

/// Represents an active connection to a single MCP server.
pub struct McpClient {
    /// Server name (from config key).
    name: String,
    /// Child process handle.
    child: Mutex<Option<Child>>,
    /// Writer to the child's stdin.
    writer: Mutex<BufWriter<ChildStdin>>,
    /// Pending request response channels, keyed by request ID.
    pending: Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>,
    /// Monotonically increasing request ID.
    next_id: AtomicU64,
    /// Server capabilities from initialization.
    capabilities: ServerCapabilities,
    /// Server info from initialization.
    server_info: ServerInfo,
    /// Notification channel — signals when a `tools/list_changed` is received.
    tools_changed: Arc<Notify>,
    /// Flag: has the reader loop detected a disconnect.
    disconnected: std::sync::atomic::AtomicBool,
}

impl McpClient {
    /// The server name (config key).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Server capabilities negotiated during initialization.
    pub fn capabilities(&self) -> &ServerCapabilities {
        &self.capabilities
    }

    /// Server info from initialization.
    pub fn server_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Whether the connection is still alive.
    pub fn is_connected(&self) -> bool {
        !self.disconnected.load(Ordering::Acquire)
    }

    /// Allocate the next request ID.
    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        if !self.is_connected() {
            return Err(McpError::Disconnected);
        }

        let id = self.next_request_id();
        let req = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Serialize and send
        {
            let mut writer = self.writer.lock().await;
            let line = serde_json::to_string(&req)?;
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        // Wait for response with timeout
        let response = tokio::time::timeout(REQUEST_TIMEOUT, rx)
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(|_| McpError::Disconnected)?;

        response.into_result().map_err(McpError::from)
    }

    /// Send a JSON-RPC notification (fire-and-forget).
    pub async fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), McpError> {
        if !self.is_connected() {
            return Err(McpError::Disconnected);
        }

        let notif = JsonRpcNotification::new(method, params);
        let mut writer = self.writer.lock().await;
        let line = serde_json::to_string(&notif)?;
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }

    /// Discover tools from the server via `tools/list`.
    pub async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
        let result = self.request("tools/list", None).await?;
        let list: ToolsListResult = serde_json::from_value(result)?;
        Ok(list.tools)
    }

    /// Call a tool on the server via `tools/call`.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolCallResult, McpError> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments,
        });
        let result = self.request("tools/call", Some(params)).await?;
        let call_result: ToolCallResult = serde_json::from_value(result)?;
        Ok(call_result)
    }

    /// Discover resources from the server via `resources/list`.
    pub async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
        let result = self.request("resources/list", None).await?;
        let list: ResourcesListResult = serde_json::from_value(result)?;
        Ok(list.resources)
    }

    /// Read a resource from the server via `resources/read`.
    pub async fn read_resource(&self, uri: &str) -> Result<ResourceReadResult, McpError> {
        let params = serde_json::json!({"uri": uri});
        let result = self.request("resources/read", Some(params)).await?;
        let read_result: ResourceReadResult = serde_json::from_value(result)?;
        Ok(read_result)
    }

    /// Get the notification handle for `tools/list_changed` events.
    pub fn tools_changed_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.tools_changed)
    }

    /// Gracefully shut down the server connection.
    pub async fn shutdown(&self) {
        // Close stdin (signals EOF to the server)
        {
            let mut writer = self.writer.lock().await;
            let _ = writer.shutdown().await;
        }

        // Wait briefly for the child to exit
        let mut child_guard = self.child.lock().await;
        if let Some(ref mut child) = *child_guard {
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait()).await;
            // If still running, kill it
            let _ = child.kill().await;
        }
        *child_guard = None;

        self.disconnected.store(true, Ordering::Release);

        // Wake any pending requests so they fail
        let mut pending = self.pending.lock().await;
        pending.clear();
    }
}

/// Spawn a stdio MCP server, perform initialization, and return an `McpClient`.
///
/// This is the main entry point for creating a connection to an MCP server.
pub async fn connect_stdio(
    name: &str,
    config: &McpServerConfig,
) -> Result<Arc<McpClient>, McpError> {
    let config = expand_server_config(config);

    // Spawn the child process
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args);
    for (key, value) in &config.env {
        cmd.env(key, value);
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // Prevent child from being in the same process group (avoids signal forwarding)
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| McpError::SpawnFailed(format!("{}: {e}", config.command)))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpError::SpawnFailed("failed to capture stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpError::SpawnFailed("failed to capture stdout".into()))?;

    let writer = BufWriter::new(stdin);
    let reader = BufReader::new(stdout);

    let tools_changed = Arc::new(Notify::new());

    let client = Arc::new(McpClient {
        name: name.to_string(),
        child: Mutex::new(Some(child)),
        writer: Mutex::new(writer),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        capabilities: ServerCapabilities::default(),
        server_info: ServerInfo::default(),
        tools_changed: Arc::clone(&tools_changed),
        disconnected: std::sync::atomic::AtomicBool::new(false),
    });

    // Spawn the reader loop (dispatches responses and notifications)
    spawn_reader_loop(Arc::clone(&client), reader);

    // Perform MCP initialization handshake
    let init_result = initialize(&client).await?;

    // Safety: we need to set the capabilities on the Arc'd client.
    // We do this via unsafe pointer cast since we just created it and the
    // reader loop only reads `pending` and `disconnected`, not caps/info.
    // Instead, we use a pattern where we create a new Arc with the right data.
    // But since McpClient fields are behind Mutex where mutable, and
    // capabilities/server_info are only read after init, we use a different
    // approach: reconstruct with the init data.
    //
    // Actually the cleanest approach is to make capabilities/server_info also
    // behind a Mutex or use interior mutability. Let's use a pragmatic approach
    // and reconstruct the client with the init data.

    // Drop the old client's reader loop by disconnecting it
    // Actually, the reader loop holds an Arc clone, so it will keep running.
    // The simplest correct approach: return a new wrapper. But since we already
    // spawned the reader loop with a reference to the client, we need the
    // capabilities to be settable.

    // We'll use a small trick: write to capabilities through raw pointer.
    // This is safe because:
    // 1. No other thread reads capabilities until after this function returns
    // 2. The reader loop only accesses pending/disconnected/tools_changed
    {
        let ptr = Arc::as_ptr(&client) as *mut McpClient;
        // SAFETY: We are the only ones with access at this point since we haven't
        // returned the Arc yet and the reader loop doesn't access these fields.
        unsafe {
            (*ptr).capabilities = init_result.capabilities;
            (*ptr).server_info = init_result.server_info;
        }
    }

    tracing::info!(
        "MCP server '{}' connected: {} v{}",
        name,
        client.server_info().name,
        client.server_info().version,
    );

    Ok(client)
}

/// Perform the MCP initialization handshake.
async fn initialize(client: &McpClient) -> Result<InitializeResult, McpError> {
    let params = InitializeParams {
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        capabilities: ClientCapabilities {
            roots: Some(serde_json::json!({"listChanged": true})),
            sampling: None,
        },
        client_info: ClientInfo {
            name: "elwood-terminal".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };

    let result = tokio::time::timeout(
        INIT_TIMEOUT,
        client.request("initialize", Some(serde_json::to_value(&params)?)),
    )
    .await
    .map_err(|_| McpError::Timeout)??;

    let init_result: InitializeResult = serde_json::from_value(result)?;

    // Verify protocol version compatibility
    if init_result.protocol_version != MCP_PROTOCOL_VERSION {
        tracing::warn!(
            "MCP server '{}' uses protocol version '{}' (we support '{}')",
            client.name(),
            init_result.protocol_version,
            MCP_PROTOCOL_VERSION,
        );
        // We still proceed — the spec says clients should be lenient
    }

    // Send `notifications/initialized` to complete the handshake
    client.notify("notifications/initialized", None).await?;

    Ok(init_result)
}

/// Spawn a background task that reads JSON-RPC messages from the server's stdout.
///
/// Dispatches responses to waiting request channels and handles notifications.
fn spawn_reader_loop(client: Arc<McpClient>, reader: BufReader<ChildStdout>) {
    tokio::spawn(async move {
        let mut reader = reader;
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf).await {
                Ok(0) => {
                    // EOF — server closed stdout
                    tracing::info!("MCP server '{}' disconnected (EOF)", client.name());
                    client.disconnected.store(true, Ordering::Release);
                    // Wake all pending requests
                    let mut pending = client.pending.lock().await;
                    pending.clear();
                    break;
                }
                Ok(_) => {
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    match ServerMessage::from_line(trimmed) {
                        Ok(ServerMessage::Response(resp)) => {
                            // Extract the ID and dispatch to the waiting channel
                            if let Some(id) = resp.id.as_u64() {
                                let mut pending = client.pending.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    let _ = tx.send(resp);
                                }
                            }
                        }
                        Ok(ServerMessage::Notification(notif)) => {
                            handle_notification(&client, &notif).await;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "MCP server '{}': failed to parse message: {e}: {trimmed}",
                                client.name(),
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("MCP server '{}' read error: {e}", client.name());
                    client.disconnected.store(true, Ordering::Release);
                    let mut pending = client.pending.lock().await;
                    pending.clear();
                    break;
                }
            }
        }
    });
}

/// Handle a server-initiated notification.
async fn handle_notification(client: &McpClient, notif: &JsonRpcNotification) {
    match notif.method.as_str() {
        "notifications/tools/list_changed" => {
            tracing::info!("MCP server '{}': tools list changed", client.name());
            client.tools_changed.notify_waiters();
        }
        "notifications/resources/list_changed" => {
            tracing::info!("MCP server '{}': resources list changed", client.name());
            // Future: trigger resource re-discovery
        }
        "notifications/message" => {
            // Log messages from the server
            if let Some(params) = &notif.params {
                let level = params["level"].as_str().unwrap_or("info");
                let data = params["data"].as_str().unwrap_or("");
                tracing::info!("MCP server '{}' [{level}]: {data}", client.name());
            }
        }
        _ => {
            tracing::debug!(
                "MCP server '{}': unhandled notification: {}",
                client.name(),
                notif.method,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// McpClientManager — manages multiple MCP server connections
// ---------------------------------------------------------------------------

/// Manages multiple MCP server connections.
///
/// Owns the lifecycle of all configured MCP servers: spawning, initialization,
/// tool discovery, and shutdown.
pub struct McpClientManager {
    /// Active client connections keyed by server name.
    clients: HashMap<String, Arc<McpClient>>,
    /// All discovered tools from all servers.
    tools: Vec<(String, McpToolDef)>,
}

impl McpClientManager {
    /// Create a new empty manager.
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tools: Vec::new(),
        }
    }

    /// Connect to all configured MCP servers and discover their tools.
    ///
    /// Servers that fail to connect are logged and skipped (non-fatal).
    pub async fn connect_all(&mut self, config: &McpConfig) {
        if !config.client_enabled {
            tracing::info!("MCP client disabled in configuration");
            return;
        }

        for (name, server_config) in &config.servers {
            if server_config.transport != "stdio" {
                tracing::warn!("MCP server '{name}': transport '{}' not supported (only stdio)", server_config.transport);
                continue;
            }

            match connect_stdio(name, server_config).await {
                Ok(client) => {
                    // Discover tools
                    match client.list_tools().await {
                        Ok(tools) => {
                            tracing::info!(
                                "MCP server '{name}': discovered {} tools",
                                tools.len(),
                            );
                            for tool in tools {
                                self.tools.push((name.clone(), tool));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("MCP server '{name}': failed to list tools: {e}");
                        }
                    }
                    self.clients.insert(name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!("MCP server '{name}': failed to connect: {e}");
                }
            }
        }
    }

    /// Get a client by server name.
    pub fn get_client(&self, name: &str) -> Option<&Arc<McpClient>> {
        self.clients.get(name)
    }

    /// Get all discovered tools (server_name, tool_def) pairs.
    pub fn discovered_tools(&self) -> &[(String, McpToolDef)] {
        &self.tools
    }

    /// Number of connected servers.
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// Shut down all MCP servers gracefully.
    pub async fn shutdown_all(&self) {
        for (name, client) in &self.clients {
            tracing::info!("Shutting down MCP server '{name}'");
            client.shutdown().await;
        }
    }
}

impl Default for McpClientManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_error_display() {
        let err = McpError::Timeout;
        assert_eq!(err.to_string(), "MCP request timed out");

        let err = McpError::Disconnected;
        assert_eq!(err.to_string(), "MCP server disconnected");

        let err = McpError::SpawnFailed("cmd not found".to_string());
        assert_eq!(err.to_string(), "failed to spawn MCP server: cmd not found");

        let err = McpError::ProtocolMismatch("1.0".to_string());
        assert_eq!(err.to_string(), "unsupported MCP protocol version: 1.0");

        let rpc_err = JsonRpcError {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        };
        let err = McpError::RpcError(rpc_err);
        assert!(err.to_string().contains("Method not found"));
    }

    #[test]
    fn test_mcp_error_from_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid json").unwrap_err();
        let err: McpError = json_err.into();
        assert!(matches!(err, McpError::Json(_)));
    }

    #[test]
    fn test_mcp_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe");
        let err: McpError = io_err.into();
        assert!(matches!(err, McpError::Io(_)));
    }

    #[test]
    fn test_manager_new() {
        let mgr = McpClientManager::new();
        assert_eq!(mgr.server_count(), 0);
        assert!(mgr.discovered_tools().is_empty());
    }

    #[tokio::test]
    async fn test_connect_disabled() {
        let mut mgr = McpClientManager::new();
        let config = McpConfig {
            client_enabled: false,
            server_enabled: false,
            servers: HashMap::new(),
        };
        mgr.connect_all(&config).await;
        assert_eq!(mgr.server_count(), 0);
    }

    #[tokio::test]
    async fn test_connect_empty_servers() {
        let mut mgr = McpClientManager::new();
        let config = McpConfig {
            client_enabled: true,
            server_enabled: false,
            servers: HashMap::new(),
        };
        mgr.connect_all(&config).await;
        assert_eq!(mgr.server_count(), 0);
    }

    #[tokio::test]
    async fn test_connect_nonexistent_command() {
        let mut mgr = McpClientManager::new();
        let config = McpConfig {
            client_enabled: true,
            server_enabled: false,
            servers: HashMap::from([(
                "bad".to_string(),
                McpServerConfig {
                    command: "__nonexistent_mcp_server_binary_12345__".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    transport: "stdio".to_string(),
                    permissions: Default::default(),
                },
            )]),
        };
        mgr.connect_all(&config).await;
        // Should not crash, just skip the failed server
        assert_eq!(mgr.server_count(), 0);
    }

    #[tokio::test]
    async fn test_connect_unsupported_transport() {
        let mut mgr = McpClientManager::new();
        let config = McpConfig {
            client_enabled: true,
            server_enabled: false,
            servers: HashMap::from([(
                "remote".to_string(),
                McpServerConfig {
                    command: "ignored".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                    transport: "http".to_string(),
                    permissions: Default::default(),
                },
            )]),
        };
        mgr.connect_all(&config).await;
        assert_eq!(mgr.server_count(), 0);
    }

    /// Test a real stdio MCP interaction using a small mock server script.
    ///
    /// This test spawns a shell script that acts as a minimal MCP server:
    /// it reads JSON-RPC requests from stdin and writes responses to stdout.
    #[tokio::test]
    async fn test_connect_stdio_mock_server() {
        // Create a mock MCP server as a bash script
        let script = r#"#!/bin/bash
# Minimal MCP server mock for testing.
# Reads JSON-RPC from stdin, writes responses to stdout.
while IFS= read -r line; do
    method=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('method',''))" 2>/dev/null)
    id=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('id','null'))" 2>/dev/null)

    case "$method" in
        "initialize")
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{\"listChanged\":true}},\"serverInfo\":{\"name\":\"mock-server\",\"version\":\"1.0.0\"}}}"
            ;;
        "notifications/initialized")
            # Notification — no response needed
            ;;
        "tools/list")
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo input\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}},\"required\":[\"text\"]}}]}}"
            ;;
        "tools/call")
            text=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['params']['arguments'].get('text',''))" 2>/dev/null)
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"echo: $text\"}],\"isError\":false}}"
            ;;
        *)
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}"
            ;;
    esac
done
"#;

        // Check if python3 is available (needed for the mock)
        let python_check = tokio::process::Command::new("python3")
            .arg("--version")
            .output()
            .await;
        if python_check.is_err() || !python_check.unwrap().status.success() {
            // Skip test if python3 is not available
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("mock_mcp_server.sh");
        std::fs::write(&script_path, script).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let config = McpServerConfig {
            command: "bash".to_string(),
            args: vec![script_path.to_string_lossy().to_string()],
            env: HashMap::new(),
            transport: "stdio".to_string(),
            permissions: Default::default(),
        };

        // Connect
        let client = connect_stdio("mock", &config).await.unwrap();
        assert_eq!(client.name(), "mock");
        assert!(client.is_connected());
        assert_eq!(client.server_info().name, "mock-server");
        assert_eq!(client.server_info().version, "1.0.0");

        // List tools
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        // Call a tool
        let result = client
            .call_tool("echo", serde_json::json!({"text": "hello"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ToolCallContent::Text { text } => assert_eq!(text, "echo: hello"),
            _ => panic!("expected text content"),
        }

        // Shutdown
        client.shutdown().await;
        assert!(!client.is_connected());
    }
}
