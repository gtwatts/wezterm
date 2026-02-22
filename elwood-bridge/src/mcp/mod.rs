//! MCP (Model Context Protocol) integration for Elwood Terminal.
//!
//! Elwood acts as **both** MCP client and server:
//!
//! - **Client**: Consume external MCP servers and expose their tools to the agent
//!   via the existing `Tool` trait.
//! - **Server**: Expose terminal capabilities (pane content, session log, git status)
//!   as MCP resources and tools that external clients can access.
//!
//! ## Architecture
//!
//! ```text
//! MCP Client (consuming external servers):
//!   McpClientManager
//!     ├── McpClient("filesystem")  ← stdio subprocess
//!     │     └── tools: [read_file, write_file, ...]
//!     ├── McpClient("github")      ← stdio subprocess
//!     │     └── tools: [create_issue, get_pr, ...]
//!     └── ...
//!   Each tool → McpToolAdapter → registered in ToolRegistry
//!   Agent calls mcp__filesystem__read_file → adapter → JSON-RPC → server
//!
//! MCP Server (exposing terminal capabilities):
//!   McpServer (stdio)
//!     ├── resources: terminal://pane/content, elwood://session/log, ...
//!     ├── tools: terminal_execute, terminal_read_screen, agent_send_message
//!     └── pane_query channel → fulfilled by ElwoodPane/domain
//! ```

pub mod adapter;
pub mod client;
pub mod config;
pub mod protocol;
pub mod resources;
pub mod server;

pub use adapter::McpToolAdapter;
pub use client::{McpClient, McpClientManager, McpError};
pub use config::McpConfig;
pub use server::McpServer;
