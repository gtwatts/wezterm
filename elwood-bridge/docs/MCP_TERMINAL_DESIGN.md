# MCP Terminal Integration Design

> Model Context Protocol (MCP) integration for Elwood Terminal
>
> Protocol version: **2025-11-25** | Spec: https://modelcontextprotocol.io/specification/2025-11-25

## Table of Contents

1. [Protocol Overview](#1-protocol-overview)
2. [Transport Selection](#2-transport-selection)
3. [Elwood as MCP Server](#3-elwood-as-mcp-server)
4. [Elwood as MCP Client](#4-elwood-as-mcp-client)
5. [Integration Architecture](#5-integration-architecture)
6. [Module Structure](#6-module-structure)
7. [Configuration Format](#7-configuration-format)
8. [Permission Model](#8-permission-model)
9. [Implementation Plan](#9-implementation-plan)
10. [Dependency Recommendations](#10-dependency-recommendations)

---

## 1. Protocol Overview

MCP is a JSON-RPC 2.0 protocol that standardizes communication between LLM applications
(hosts/clients) and external services (servers). It enables tool discovery, resource
access, and prompt templating through a stateful, capability-negotiated connection.

### 1.1 Roles

```
Host (Elwood Terminal)
  └── Client (MCP connector inside elwood-bridge)
        ├── Server A (filesystem MCP server, stdio subprocess)
        ├── Server B (database MCP server, stdio subprocess)
        └── Server C (remote API MCP server, streamable HTTP)
```

- **Host**: The application that contains the LLM integration (Elwood Terminal).
- **Client**: Protocol connector that talks to MCP servers. One client per server.
- **Server**: Process that exposes tools, resources, and prompts.

Elwood acts as **both**:
- **MCP Client** — consuming external MCP servers (filesystem, database, GitHub, etc.)
- **MCP Server** — exposing terminal-native capabilities to external MCP clients

### 1.2 Primitives

| Primitive | Direction | Description |
|-----------|-----------|-------------|
| **Tools** | Server -> Model | Functions the LLM can invoke (model-controlled) |
| **Resources** | Server -> App | Data/context exposed via URI (application-controlled) |
| **Prompts** | Server -> User | Templated message workflows (user-controlled) |
| **Sampling** | Server -> Client | Server requests LLM completion from client |
| **Roots** | Client -> Server | Client tells server which filesystem roots to use |
| **Elicitation** | Server -> Client | Server requests additional info from user |

### 1.3 Lifecycle

```
Client                              Server
  │                                   │
  │─── initialize ──────────────────>│  Phase 1: Initialization
  │<── InitializeResult ────────────│  (capability negotiation, version agreement)
  │─── notifications/initialized ──>│
  │                                   │
  │<── tools/list ──────────────────>│  Phase 2: Operation
  │<── resources/read ──────────────>│  (normal protocol communication)
  │<── tools/call ──────────────────>│
  │                                   │
  │─── [close connection] ─────────>│  Phase 3: Shutdown
  │                                   │  (transport-level disconnect)
```

#### Initialize Request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "2025-11-25",
    "capabilities": {
      "roots": { "listChanged": true },
      "sampling": {},
      "elicitation": {}
    },
    "clientInfo": {
      "name": "elwood-terminal",
      "version": "0.3.1"
    }
  }
}
```

#### Initialize Response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2025-11-25",
    "capabilities": {
      "tools": { "listChanged": true },
      "resources": { "subscribe": true, "listChanged": true },
      "prompts": { "listChanged": true },
      "logging": {}
    },
    "serverInfo": {
      "name": "example-server",
      "version": "1.0.0"
    }
  }
}
```

### 1.4 Capability Negotiation

| Category | Capability | Description |
|----------|-----------|-------------|
| Client | `roots` | Filesystem roots the server can operate in |
| Client | `sampling` | Server can request LLM completions |
| Client | `elicitation` | Server can request user input |
| Server | `tools` | Exposes callable tools |
| Server | `resources` | Exposes readable resources |
| Server | `prompts` | Offers prompt templates |
| Server | `logging` | Emits structured log messages |
| Server | `completions` | Supports argument autocompletion |

### 1.5 Tool Schema

```json
{
  "name": "read_file",
  "title": "Read File",
  "description": "Read the contents of a file",
  "inputSchema": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "File path to read" }
    },
    "required": ["path"]
  },
  "annotations": {
    "readOnlyHint": true,
    "destructiveHint": false,
    "idempotentHint": true,
    "openWorldHint": false
  }
}
```

Tool annotations inform the client about tool behavior characteristics:
- `readOnlyHint` — tool does not modify state
- `destructiveHint` — tool may perform destructive operations
- `idempotentHint` — calling repeatedly with same args gives same result
- `openWorldHint` — tool interacts with the outside world

### 1.6 Resource Schema

```json
{
  "uri": "terminal://pane/1/content",
  "name": "Terminal Pane 1 Content",
  "description": "Current visible content of terminal pane 1",
  "mimeType": "text/plain",
  "annotations": {
    "audience": ["assistant"],
    "priority": 0.8
  }
}
```

Resources use URI schemes. Elwood defines custom `terminal://` and `elwood://` schemes.

---

## 2. Transport Selection

MCP defines two standard transports:

### 2.1 stdio (Subprocess)

```
Elwood (parent process)
  │
  ├── spawn child process (MCP server)
  │     stdin  ← JSON-RPC messages from Elwood
  │     stdout → JSON-RPC messages to Elwood
  │     stderr → logs (captured/ignored)
  │
  └── newline-delimited JSON-RPC messages
```

- Client spawns server as subprocess
- Communication over stdin/stdout
- Newline-delimited JSON-RPC messages (no embedded newlines)
- Server stderr is for logging only
- **Best for**: Local MCP servers (filesystem, git, database)
- **Shutdown**: Close stdin, SIGTERM, then SIGKILL

### 2.2 Streamable HTTP

```
Elwood (HTTP client)
  │
  ├── POST /mcp          → Send JSON-RPC requests/notifications
  │   Accept: application/json, text/event-stream
  │
  ├── GET /mcp            → Open SSE stream for server-initiated messages
  │   Accept: text/event-stream
  │
  └── Headers:
        MCP-Session-Id: <uuid>
        MCP-Protocol-Version: 2025-11-25
```

- Server runs as independent HTTP service
- POST for client->server messages, GET for SSE stream
- Session management via `MCP-Session-Id` header
- Connection resumability via `Last-Event-ID`
- **Best for**: Remote MCP servers, shared services
- **Shutdown**: HTTP DELETE with session ID, or close connections

### 2.3 Recommendation for Elwood Terminal

| Use Case | Transport | Rationale |
|----------|-----------|-----------|
| **MCP Client (consuming servers)** | stdio (primary) | Same as Claude Desktop/Code. Local servers are most common. Zero network config. |
| **MCP Client (remote servers)** | Streamable HTTP | For cloud-hosted MCP servers. |
| **MCP Server (exposing terminal)** | Streamable HTTP | Allows external clients (Claude Desktop, other editors) to connect to Elwood. |
| **MCP Server (local pipe)** | stdio | When Elwood is launched as a subprocess by another tool. |

**Primary transport for Phase 1**: stdio for client-side, streamable HTTP for server-side.

---

## 3. Elwood as MCP Server

Elwood Terminal exposes terminal-native capabilities as MCP primitives.

### 3.1 Resources

#### Terminal Pane Content

```
URI: terminal://pane/{pane_id}/content
MIME: text/plain
Description: Current visible text content of a terminal pane
```

Reading this resource returns the current visible lines from the virtual terminal
(`wezterm_term::Terminal`) via `terminal_get_lines()`.

#### Terminal Pane Scrollback

```
URI: terminal://pane/{pane_id}/scrollback?lines={n}
MIME: text/plain
Description: Last N lines from terminal scrollback buffer
```

#### Shell Command History

```
URI: elwood://history/commands?limit={n}
MIME: application/json
Description: Recent shell commands executed in the terminal
```

Returns entries from `HistorySearch` / `CompletionEngine`.

#### Session Log

```
URI: elwood://session/{session_id}/log
MIME: text/markdown
Description: Full session log (agent messages, tool calls, commands)
```

Returns the markdown export from `SessionLog`.

#### Working Directory

```
URI: elwood://cwd
MIME: text/plain
Description: Current working directory of the terminal
```

#### Git Context

```
URI: elwood://git/status
MIME: application/json
Description: Current git branch, modified files, staged changes
```

Returns data from `git_info::get_git_context()`.

#### Agent Block Output

```
URI: elwood://block/{block_id}/output
MIME: text/plain
Description: Output from a specific agent response or command block
```

Returns content from `BlockManager`.

### 3.2 Resource Templates

```json
{
  "uriTemplate": "terminal://pane/{pane_id}/content",
  "name": "Terminal Pane Content",
  "description": "Visible content of a terminal pane by ID"
}
```

```json
{
  "uriTemplate": "elwood://file/{path}",
  "name": "Project File",
  "description": "Read a file relative to the working directory"
}
```

### 3.3 Tools

Elwood exposes tools that let external MCP clients interact with the terminal:

| Tool | Description | Annotations |
|------|-------------|-------------|
| `terminal_execute` | Run a shell command in the terminal | destructive, not readonly |
| `terminal_send_keys` | Send keystrokes to the active pane | destructive, not readonly |
| `terminal_read_screen` | Read current screen content | readonly, idempotent |
| `terminal_get_panes` | List active panes with IDs and titles | readonly, idempotent |
| `terminal_resize_pane` | Resize a pane | not destructive |
| `agent_send_message` | Send a message to the Elwood agent | not destructive |
| `agent_get_status` | Get current agent state (idle/running) | readonly, idempotent |

### 3.4 Prompts

```json
{
  "name": "fix_error",
  "description": "Analyze and fix the last command error",
  "arguments": [
    {
      "name": "command",
      "description": "The command that failed",
      "required": false
    }
  ]
}
```

Prompts map to Elwood's existing command palette and quick-fix features.

---

## 4. Elwood as MCP Client

Elwood consumes external MCP servers to extend the agent's capabilities.

### 4.1 Server Lifecycle Management

```
                     ┌────────────────────┐
                     │   McpClientManager  │
                     │                     │
                     │  servers: HashMap   │
                     │    name -> McpConn  │
                     └────────┬───────────┘
                              │
              ┌───────────────┼───────────────┐
              │               │               │
        ┌─────┴─────┐  ┌─────┴─────┐  ┌─────┴─────┐
        │  StdioConn │  │  StdioConn │  │  HttpConn  │
        │  (fs-mcp)  │  │  (git-mcp) │  │ (cloud)    │
        └─────┬─────┘  └─────┬─────┘  └─────┬─────┘
              │               │               │
         child proc      child proc      HTTP/SSE
```

For each configured MCP server:
1. **Spawn**: Launch subprocess (stdio) or establish HTTP connection
2. **Initialize**: Send `initialize` request, negotiate capabilities
3. **Discover**: Call `tools/list` and `resources/list`
4. **Register**: Add discovered tools to `ToolRegistry`
5. **Operate**: Route tool calls from the agent through the MCP client
6. **Monitor**: Watch for `tools/list_changed` notifications
7. **Shutdown**: Close stdin (stdio) or DELETE session (HTTP)

### 4.2 Tool Discovery and Registration

When an MCP server is connected, its tools are discovered via `tools/list` and
wrapped in an adapter that implements the existing `Tool` trait from elwood-core:

```rust
/// Adapter that wraps an MCP server tool as an elwood-core Tool.
pub struct McpToolAdapter {
    /// The MCP client connection to route calls through.
    client: Arc<McpClient>,
    /// Tool name from the MCP server.
    tool_name: String,
    /// Tool description from the MCP server.
    tool_description: String,
    /// JSON Schema for input parameters.
    input_schema: serde_json::Value,
    /// Tool annotations from the MCP server.
    annotations: McpToolAnnotations,
    /// The MCP server name (for namespacing).
    server_name: String,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        // Namespaced: "mcp__servername__toolname"
        // (matches Claude Code convention)
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn category(&self) -> ToolCategory {
        if self.annotations.read_only_hint {
            ToolCategory::Read
        } else if self.annotations.destructive_hint {
            ToolCategory::Write
        } else {
            ToolCategory::External
        }
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<ToolResult> {
        // Route the call through the MCP client
        let result = self.client.call_tool(&self.tool_name, arguments).await?;
        // Convert MCP tool result to elwood-core ToolResult
        Ok(convert_mcp_result(result))
    }

    fn risk_level(&self) -> RiskLevel {
        if self.annotations.destructive_hint {
            RiskLevel::High
        } else if self.annotations.read_only_hint {
            RiskLevel::Low
        } else {
            RiskLevel::Moderate
        }
    }
}
```

Tool names are namespaced as `mcp__{server_name}__{tool_name}` to avoid collisions
with built-in tools and between servers (matching Claude Code's convention).

### 4.3 Resource Access

MCP resources are surfaced to the agent through:

1. **@ context attachment**: `@mcp:servername/resource-name` in the input box
2. **Command palette**: Browse and select MCP resources
3. **Agent context**: Auto-include high-priority resources in system prompt

### 4.4 Configuration Format

See [Section 7](#7-configuration-format) for the full `~/.elwood/elwood.toml` format.

---

## 5. Integration Architecture

### 5.1 Where MCP Fits in the RuntimeBridge

```
┌─────────────────────────────────────────────────────────────────────┐
│                        WezTerm (smol runtime)                       │
│                                                                     │
│  ElwoodPane                                                         │
│    ├── poll_responses()  ← AgentResponse from flume channel         │
│    ├── key_down()        → AgentRequest into flume channel          │
│    └── MCP Server (Streamable HTTP)  ← external clients connect     │
│          └── Reads pane content, blocks, session log                │
│                                                                     │
└───────────────────────────┬─────────────────────────────────────────┘
                            │ flume channels (AgentRequest/AgentResponse)
┌───────────────────────────┴─────────────────────────────────────────┐
│                     tokio runtime thread                            │
│                                                                     │
│  agent_runtime_loop()                                               │
│    ├── CoreAgent + ToolRegistry                                     │
│    │     └── McpToolAdapter instances (registered at startup)       │
│    │                                                                │
│    └── McpClientManager (owns MCP client connections)               │
│          ├── StdioConnection("filesystem")                          │
│          ├── StdioConnection("github")                              │
│          └── HttpConnection("remote-api")                           │
│                                                                     │
│  MCP Client Manager Lifecycle:                                      │
│    1. Load config from ~/.elwood/elwood.toml                        │
│    2. Spawn/connect MCP servers                                     │
│    3. Initialize + discover tools                                   │
│    4. Register McpToolAdapter into ToolRegistry                     │
│    5. Monitor for list_changed notifications                        │
│    6. Shutdown on AgentRequest::Shutdown                            │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### 5.2 MCP Messages Through AgentRequest/AgentResponse

MCP tool calls flow naturally through the existing bridge because `McpToolAdapter`
implements the `Tool` trait. When the LLM calls an MCP tool:

1. LLM returns a function call for `mcp__github__create_issue`
2. `CoreAgent` looks up the tool in `ToolRegistry`
3. `ToolRegistry` finds `McpToolAdapter` for `github/create_issue`
4. `McpToolAdapter::execute()` sends `tools/call` JSON-RPC to the MCP server
5. MCP server returns the result
6. Result flows back through `AgentEvent::ToolCallCompleted`
7. `translate_event()` converts to `AgentResponse::ToolEnd`
8. ElwoodPane renders the result in the chat area

No new `AgentRequest`/`AgentResponse` variants are needed for MCP tool calls.

### 5.3 New Bridge Messages (MCP Server Only)

For the MCP *server* (Elwood exposing capabilities), we need to read pane state.
This is handled internally by the server having an `Arc<Mutex<Terminal>>` reference
or by routing through existing mechanisms:

```rust
// New AgentRequest variant for MCP server resource reads
AgentRequest::McpResourceRead {
    uri: String,
    request_id: String,
}

// New AgentResponse variant for MCP server resource results
AgentResponse::McpResourceResult {
    request_id: String,
    content: String,
    mime_type: String,
}
```

However, since the MCP server runs on the smol side (inside WezTerm's event loop where
it has access to the ElwoodPane), it can read pane content directly without going through
the bridge. The MCP server component lives alongside ElwoodPane, not in the tokio thread.

### 5.4 Sequence Diagram — MCP Client Tool Call

```
User            ElwoodPane        Bridge        CoreAgent       McpClient     MCP Server
  │                │                │               │               │             │
  │ "create issue" │                │               │               │             │
  │───────────────>│                │               │               │             │
  │                │ AgentRequest   │               │               │             │
  │                │───────────────>│               │               │             │
  │                │                │ execute()     │               │             │
  │                │                │──────────────>│               │             │
  │                │                │               │ tool_call     │             │
  │                │                │               │──────────────>│             │
  │                │                │               │               │ tools/call  │
  │                │                │               │               │────────────>│
  │                │                │               │               │<────────────│
  │                │                │               │<──────────────│             │
  │                │                │ AgentEvent    │               │             │
  │                │ AgentResponse  │<──────────────│               │             │
  │                │<───────────────│               │               │             │
  │ rendered result│                │               │               │             │
  │<───────────────│                │               │               │             │
```

### 5.5 Sequence Diagram — MCP Server Resource Read

```
External Client        Elwood MCP Server        ElwoodPane
      │                       │                      │
      │ resources/read        │                      │
      │ terminal://pane/1     │                      │
      │──────────────────────>│                      │
      │                       │ get_lines(0..rows)   │
      │                       │─────────────────────>│
      │                       │<─────────────────────│
      │                       │                      │
      │ { text: "..." }       │                      │
      │<──────────────────────│                      │
```

---

## 6. Module Structure

### 6.1 File Layout

```
elwood-bridge/
├── src/
│   ├── lib.rs              # (existing) crate root
│   ├── runtime.rs          # (existing) RuntimeBridge, AgentRequest/Response
│   ├── domain.rs           # (existing) ElwoodDomain, agent_runtime_loop
│   ├── pane.rs             # (existing) ElwoodPane
│   │
│   ├── mcp/                # NEW: MCP integration module
│   │   ├── mod.rs          # Re-exports
│   │   │
│   │   ├── client/         # MCP client (consuming external servers)
│   │   │   ├── mod.rs
│   │   │   ├── manager.rs  # McpClientManager — lifecycle management
│   │   │   ├── connection.rs # McpConnection trait + StdioConn, HttpConn
│   │   │   ├── adapter.rs  # McpToolAdapter (Tool trait impl)
│   │   │   └── protocol.rs # JSON-RPC message types, serialize/deserialize
│   │   │
│   │   ├── server/         # MCP server (exposing terminal capabilities)
│   │   │   ├── mod.rs
│   │   │   ├── handler.rs  # McpServerHandler — request routing
│   │   │   ├── resources.rs # Terminal resource providers
│   │   │   ├── tools.rs    # Terminal tool implementations
│   │   │   └── transport.rs # Streamable HTTP server (axum/hyper)
│   │   │
│   │   ├── config.rs       # MCP configuration parsing
│   │   └── types.rs        # Shared types (capabilities, errors, etc.)
│   │
│   └── ...
```

### 6.2 Key Types

#### MCP Client Types

```rust
/// Manages all MCP server connections.
pub struct McpClientManager {
    /// Active connections, keyed by server name.
    connections: HashMap<String, McpConnection>,
    /// Configuration for MCP servers.
    config: McpConfig,
}

/// A single MCP server connection (either stdio or HTTP).
pub enum McpConnection {
    Stdio(StdioConnection),
    Http(HttpConnection),
}

/// stdio transport — manages a child process.
pub struct StdioConnection {
    /// Child process handle.
    child: tokio::process::Child,
    /// JSON-RPC message writer (to child stdin).
    writer: tokio::io::BufWriter<tokio::process::ChildStdin>,
    /// JSON-RPC message reader (from child stdout).
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    /// Server capabilities (from initialize response).
    capabilities: ServerCapabilities,
    /// Server info.
    server_info: ServerInfo,
    /// Next JSON-RPC request ID.
    next_id: AtomicU64,
    /// Pending requests awaiting responses.
    pending: HashMap<u64, oneshot::Sender<JsonRpcResponse>>,
}

/// Streamable HTTP transport — HTTP client with SSE.
pub struct HttpConnection {
    /// HTTP client.
    client: reqwest::Client,
    /// MCP endpoint URL.
    endpoint: String,
    /// Session ID from server.
    session_id: Option<String>,
    /// Server capabilities.
    capabilities: ServerCapabilities,
    /// Next JSON-RPC request ID.
    next_id: AtomicU64,
}

/// Tool annotations from MCP server.
pub struct McpToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}
```

#### MCP Server Types

```rust
/// Elwood's MCP server — exposes terminal capabilities.
pub struct ElwoodMcpServer {
    /// Reference to the terminal for reading pane content.
    terminal: Arc<Mutex<Terminal>>,
    /// Reference to session log.
    session_log: Arc<Mutex<SessionLog>>,
    /// Reference to block manager.
    block_manager: Arc<Mutex<BlockManager>>,
    /// Reference to history search.
    history: Arc<Mutex<HistorySearch>>,
    /// Server capabilities.
    capabilities: ServerCapabilities,
    /// Active sessions (for streamable HTTP).
    sessions: HashMap<String, McpSession>,
}

/// Handles incoming JSON-RPC requests.
pub struct McpServerHandler {
    server: Arc<ElwoodMcpServer>,
}

/// A terminal resource provider.
pub trait ResourceProvider: Send + Sync {
    /// URI scheme this provider handles.
    fn scheme(&self) -> &str;
    /// List available resources.
    fn list(&self) -> Vec<ResourceDescriptor>;
    /// Read a resource by URI.
    fn read(&self, uri: &str) -> Result<ResourceContent>;
    /// Subscribe to changes (optional).
    fn subscribe(&self, uri: &str) -> Option<ResourceWatcher>;
}
```

#### JSON-RPC Protocol Types

```rust
/// JSON-RPC 2.0 request.
#[derive(Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String, // always "2.0"
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 notification (no id).
#[derive(Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC error object.
#[derive(Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

/// MCP standard error codes (in addition to JSON-RPC codes).
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const RESOURCE_NOT_FOUND: i64 = -32002;
}
```

---

## 7. Configuration Format

### 7.1 elwood.toml MCP Section

```toml
# ~/.elwood/elwood.toml

[mcp]
# Enable MCP client (consuming external servers)
client_enabled = true
# Enable MCP server (exposing terminal capabilities)
server_enabled = false
# Server listen address (when server_enabled = true)
server_address = "127.0.0.1:9315"

# MCP server definitions (consumed by client)
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/project"]
# Optional environment variables
env = { NODE_ENV = "production" }

[mcp.servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_TOKEN}" }

[mcp.servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
env = { POSTGRES_URL = "postgresql://localhost:5432/mydb" }

# Remote MCP server (streamable HTTP)
[mcp.servers.cloud-api]
url = "https://api.example.com/mcp"
transport = "http"
headers = { Authorization = "Bearer ${API_TOKEN}" }

# Per-server permission overrides
[mcp.servers.filesystem.permissions]
trust_level = "trusted"  # trusted | untrusted | sandbox

[mcp.servers.cloud-api.permissions]
trust_level = "untrusted"  # all tools require user approval
```

### 7.2 Project-Level Overrides

```toml
# .elwood/settings.json (project root)
{
  "mcp": {
    "servers": {
      "project-db": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-postgres"],
        "env": { "POSTGRES_URL": "postgresql://localhost:5432/project_db" }
      }
    }
  }
}
```

Project-level MCP servers merge with global config. Project settings can add servers
but cannot override trust levels set in global config (security constraint).

### 7.3 Environment Variable Expansion

Configuration supports `${VAR_NAME}` syntax for environment variable expansion,
matching Claude Desktop's approach. Variables are resolved at server spawn time.

---

## 8. Permission Model

### 8.1 Trust Levels

| Level | Description | Behavior |
|-------|-------------|----------|
| `trusted` | Vetted server (built-in, user-installed) | Tools execute without prompting |
| `untrusted` | Unknown/remote server | All tool calls require user approval |
| `sandbox` | Restricted execution | Tools run in Seatbelt sandbox (macOS) |

### 8.2 Permission Flow

```
MCP tool call arrives
        │
        ▼
┌───────────────────┐
│ Check trust level │
└───────┬───────────┘
        │
   ┌────┴────┐
   │trusted? │──yes──> Execute directly
   └────┬────┘
        │ no
        ▼
┌─────────────────────────┐
│ Check permission rules  │
│ (pattern-based matching │
│  from elwood settings)  │
└───────────┬─────────────┘
            │
    ┌───────┴───────┐
    │ Rule matches? │──allow──> Execute
    └───────┬───────┘
            │ ask (or no rule)
            ▼
┌─────────────────────────┐
│ Prompt user (via bridge │
│ PermissionRequest)      │
└───────────┬─────────────┘
            │
    ┌───────┴───────┐
    │ User grants?  │──yes──> Execute
    └───────┬───────┘
            │ no
            ▼
        Return error
```

### 8.3 Tool Annotation Trust

MCP tool annotations (`readOnlyHint`, `destructiveHint`) are **untrusted by default**.
They are used for UI display (showing risk indicators) but NOT for automatic
permission decisions. The server could lie about annotations.

Only annotations from `trusted` servers are used for permission shortcuts
(e.g., `readOnlyHint: true` on a trusted server skips confirmation).

### 8.4 Integration with PermissionManager

MCP tools are registered with the existing `PermissionManager` from elwood-core.
Permission rules can reference MCP tools by their namespaced name:

```json
{
  "permissions": {
    "allow": [
      "mcp__filesystem__read_file",
      "mcp__filesystem__list_directory",
      "mcp__github__get_pull_request"
    ],
    "deny": [
      "mcp__*__delete_*"
    ]
  }
}
```

---

## 9. Implementation Plan

### Phase 1: MCP Client Core (Priority: High)

**Goal**: Elwood can consume external MCP servers and expose their tools to the agent.

1. **Protocol layer** (`mcp/types.rs`, `mcp/client/protocol.rs`)
   - JSON-RPC 2.0 message types (request, response, notification)
   - Serialize/deserialize with serde
   - Error types and codes

2. **stdio transport** (`mcp/client/connection.rs`)
   - Spawn child process with `tokio::process::Command`
   - Newline-delimited JSON-RPC over stdin/stdout
   - Stderr capture/logging
   - Graceful shutdown (close stdin, SIGTERM, SIGKILL)

3. **Client manager** (`mcp/client/manager.rs`)
   - Load config from `~/.elwood/elwood.toml`
   - Spawn and initialize servers on startup
   - `tools/list` discovery
   - Handle `tools/list_changed` notifications
   - Shutdown all servers on exit

4. **Tool adapter** (`mcp/client/adapter.rs`)
   - `McpToolAdapter` implementing `Tool` trait
   - Namespaced tool names (`mcp__server__tool`)
   - JSON-RPC `tools/call` execution
   - Error mapping (MCP errors -> elwood-core errors)

5. **Wire into agent_runtime_loop** (`domain.rs`)
   - Create `McpClientManager` at startup
   - Register discovered tools into `ToolRegistry`
   - Shutdown MCP connections on `AgentRequest::Shutdown`

### Phase 2: MCP Client HTTP Transport

**Goal**: Support remote MCP servers via Streamable HTTP.

1. **HTTP transport** (`mcp/client/connection.rs`)
   - `reqwest` client with SSE support
   - `MCP-Session-Id` header management
   - `MCP-Protocol-Version` header
   - Connection resumability via `Last-Event-ID`
   - POST for requests, GET for SSE stream

2. **Config**: `transport = "http"` in server config
   - URL, headers, TLS options

### Phase 3: MCP Server (Terminal Exposure)

**Goal**: External MCP clients can connect to Elwood and access terminal capabilities.

1. **Resource providers** (`mcp/server/resources.rs`)
   - `TerminalPaneProvider` — read visible content, scrollback
   - `SessionLogProvider` — read session log
   - `GitContextProvider` — git status, branch info
   - `HistoryProvider` — command history

2. **Tool implementations** (`mcp/server/tools.rs`)
   - `terminal_execute` — run shell command
   - `terminal_read_screen` — read pane content
   - `terminal_send_keys` — send keystrokes
   - `agent_send_message` — send message to agent

3. **Streamable HTTP server** (`mcp/server/transport.rs`)
   - Lightweight HTTP server (axum or hyper)
   - JSON-RPC routing
   - SSE streaming for notifications
   - Session management
   - Origin validation (DNS rebinding protection)

4. **Wire into ElwoodDomain** (`domain.rs`)
   - Start MCP server on `attach()`
   - Stop on `detach()`
   - Share pane references with server

### Phase 4: Enhanced Features

1. **Resource subscriptions** — notify clients when terminal content changes
2. **Sampling** — let MCP servers request LLM completions through Elwood
3. **Prompts** — expose command palette entries as MCP prompts
4. **Elicitation** — handle server requests for user input via the input box
5. **@ context integration** — `@mcp:server/resource` syntax in input
6. **Command palette** — browse MCP resources and tools
7. **Tasks** — support async/long-running MCP tool calls (2025-11-25 feature)

---

## 10. Dependency Recommendations

### 10.1 Primary Choice: Official `rmcp` Crate

**Crate**: `rmcp` (https://github.com/modelcontextprotocol/rust-sdk)
**Version**: 0.8.0+
**License**: Apache-2.0

The official Rust SDK for MCP, maintained by the MCP project. Recommended because:

- **Official**: Maintained alongside the specification
- **tokio-native**: Uses tokio async runtime (matches elwood-core)
- **Full protocol**: Client + server, stdio + streamable HTTP
- **Macro support**: `#[tool]`, `#[tool_handler]`, `#[prompt]` proc macros
- **Type-safe**: Strongly typed protocol messages
- **Active**: 140+ contributors, regular releases

```toml
[dependencies]
rmcp = { version = "0.8", features = [
    "client",
    "server",
    "macros",
    "transport-io",
    "transport-child-process",
    "transport-streamable-http-client",
    "transport-streamable-http-server",
] }
```

### 10.2 Alternative: Hand-Rolled Protocol

If `rmcp` proves too heavy or its API doesn't fit the bridge architecture cleanly,
a minimal hand-rolled implementation is viable because:

- JSON-RPC 2.0 is simple (serde + serde_json)
- stdio transport is trivial (newline-delimited JSON over stdin/stdout)
- We only need a subset of MCP (tools + resources, not the full spec)

Additional dependencies for hand-rolled:

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["process", "io-util", "time"] }
reqwest = { version = "0.12", features = ["stream"], optional = true }
```

### 10.3 Decision Matrix

| Factor | `rmcp` | Hand-rolled |
|--------|--------|-------------|
| Spec compliance | Full, automatic | Partial, manual effort |
| API ergonomics | Good (macros, typed) | Custom (full control) |
| Binary size | ~500KB added | Minimal |
| Compile time | Moderate (proc macros) | Fast |
| Maintenance | Upstream tracks spec | Manual spec tracking |
| Transport support | All three built-in | Build as needed |

**Recommendation**: Start with `rmcp` for Phase 1. If binary size or compile time
becomes an issue, extract only the protocol types and implement transports manually.
The `rmcp` crate's `model` module can be used standalone for just the type definitions
without the full service/transport stack.

### 10.4 Additional Dependencies

| Crate | Purpose | Phase |
|-------|---------|-------|
| `rmcp` | MCP protocol (primary) | 1 |
| `serde_json` | JSON-RPC message handling | 1 (already in workspace) |
| `tokio` | Async runtime, process spawning | 1 (already in workspace) |
| `reqwest` | HTTP client for streamable HTTP | 2 (already in workspace) |
| `axum` or `hyper` | HTTP server for MCP server | 3 |
| `tokio-tungstenite` | WebSocket (if needed for SSE) | 3 (optional) |
| `eventsource-stream` | SSE client parsing | 2 (or use rmcp's built-in) |

---

## Appendix A: Existing Implementations Reference

### Claude Desktop

- **Transport**: stdio only (spawns MCP servers as subprocesses)
- **Config**: `~/.config/claude/claude_desktop_config.json` with `mcpServers` object
- **Process isolation**: New session per server, `start_new_session=True` on Unix
- **Shutdown**: Close stdin, wait, SIGTERM, SIGKILL
- **Tool naming**: `mcp__{server_name}__{tool_name}`

### Claude Code

- **Transport**: stdio (90% of servers), streamable HTTP for remote
- **Config**: `claude mcp add <name> -- <command> [args]` or `.mcp.json`
- **Client-server model**: Each connection spawns separate process, no state sharing
- **Tool naming**: `mcp__{server_name}__{tool_name}`
- **Over 3,000 community servers** in the MCP registry (as of 2025)

### Cursor IDE

- **Transport**: stdio for local, gateway for discovery
- **MCP Gateway**: Unified access point for multiple servers
- **Tool discovery**: FAISS vector similarity search across registered servers
- **Chaining**: Multiple servers compose into autonomous dev workflows

### VS Code (GitHub Copilot)

- **Transport**: stdio
- **Config**: `settings.json` with `github.copilot.mcp.servers`
- **Extension-based**: MCP servers can be bundled as VS Code extensions

### Key Patterns Across Implementations

1. **stdio is dominant** — all implementations support stdio; HTTP is secondary
2. **Subprocess lifecycle** — spawn on startup, shutdown on exit
3. **Namespaced tools** — prefix tool names with server name to avoid collisions
4. **Config-driven** — JSON/TOML file lists servers to connect
5. **Permission per server** — trust levels vary by server source
6. **Lazy initialization** — connect to servers on first use, not at startup

---

## Appendix B: MCP 2025-11-25 New Features

The latest spec revision (2025-11-25) adds:

1. **Tasks** — Any request can return a task handle for async "call-now, fetch-later"
   - `tasks/get` — check task status
   - `tasks/result` — fetch completed result
   - `tasks/cancel` — cancel running task
   - Enables long-running tool calls without blocking

2. **Canonical tool names** — Single format for display, sort, reference

3. **Structured content** — Tools can return typed JSON via `structuredContent` field
   alongside traditional `content` array (backward compatible)

4. **Output schema** — Tools can declare `outputSchema` for structured result validation

5. **Elicitation** — Servers can request additional input from users via forms

6. **Connection resumability** — SSE streams support `Last-Event-ID` for reconnection

These features should be supported in Phase 4 of the implementation plan.
