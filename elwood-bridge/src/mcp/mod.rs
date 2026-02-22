//! MCP (Model Context Protocol) integration for Elwood Terminal.
//!
//! Phase 1: MCP Client — consume external MCP servers and expose their tools
//! to the agent via the existing `Tool` trait.
//!
//! ## Architecture
//!
//! ```text
//! McpClientManager
//!   ├── McpClient("filesystem")  ← stdio subprocess
//!   │     └── tools: [read_file, write_file, ...]
//!   ├── McpClient("github")      ← stdio subprocess
//!   │     └── tools: [create_issue, get_pr, ...]
//!   └── ...
//!
//! Each tool → McpToolAdapter → registered in ToolRegistry
//! Agent calls mcp__filesystem__read_file → adapter → JSON-RPC → server
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! let config = McpConfig { ... };
//! let mut manager = McpClientManager::new();
//! manager.connect_all(&config).await;
//!
//! // Register discovered tools into the agent's ToolRegistry
//! for (server_name, tool_def) in manager.discovered_tools() {
//!     let client = manager.get_client(server_name).unwrap();
//!     let adapter = McpToolAdapter::new(server_name, tool_def, Arc::clone(client));
//!     registry.register(Arc::new(adapter));
//! }
//! ```

pub mod adapter;
pub mod client;
pub mod config;
pub mod protocol;

pub use adapter::McpToolAdapter;
pub use client::{McpClient, McpClientManager, McpError};
pub use config::McpConfig;
