//! Configuration bridge between elwood.toml and WezTerm's Lua config system.
//!
//! Merges Elwood-specific configuration (provider, model, permissions) with
//! WezTerm's native configuration. Elwood settings are accessible from both
//! Rust and Lua.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Elwood-specific configuration that extends WezTerm's config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElwoodConfig {
    /// LLM provider (e.g., "gemini", "anthropic").
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Model name (e.g., "gemini-2.5-pro").
    #[serde(default = "default_model")]
    pub model: String,

    /// Path to the Elwood config file.
    #[serde(default = "default_config_path")]
    pub config_path: PathBuf,

    /// Whether to auto-open the agent pane on startup.
    #[serde(default)]
    pub auto_open: bool,

    /// Default permission mode for the agent.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,

    /// Scrollback size for the Elwood pane's virtual terminal.
    #[serde(default = "default_scrollback")]
    pub scrollback_size: usize,

    /// Working directory for the agent session.
    /// Defaults to the current working directory.
    pub working_dir: Option<String>,

    /// MCP (Model Context Protocol) configuration.
    #[serde(default)]
    pub mcp: crate::mcp::McpConfig,
}

impl Default for ElwoodConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            config_path: default_config_path(),
            auto_open: false,
            permission_mode: default_permission_mode(),
            scrollback_size: default_scrollback(),
            working_dir: None,
            mcp: crate::mcp::McpConfig::default(),
        }
    }
}

fn default_provider() -> String {
    "gemini".into()
}

fn default_model() -> String {
    "gemini-2.5-pro".into()
}

fn default_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".elwood")
        .join("elwood.toml")
}

fn default_permission_mode() -> String {
    "default".into()
}

fn default_scrollback() -> usize {
    10_000
}

impl ElwoodConfig {
    /// Load configuration from the default config file.
    pub fn load() -> Self {
        Self::load_from(&default_config_path())
    }

    /// Load configuration from a specific path.
    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}
