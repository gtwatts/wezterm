//! Configuration bridge between elwood.toml and WezTerm's Lua config system.
//!
//! Merges Elwood-specific configuration (provider, model, permissions) with
//! WezTerm's native configuration. Elwood settings are accessible from both
//! Rust and Lua.
//!
//! ## Multi-Model Configuration
//!
//! Supports both legacy single-provider config and multi-model `[[models]]` arrays:
//!
//! ```toml
//! # Legacy (still works):
//! provider = "gemini"
//! model = "gemini-2.5-pro"
//!
//! # Multi-model:
//! [[models]]
//! name = "gemini-2.5-pro"
//! provider = "gemini"
//! default = true
//!
//! [[models]]
//! name = "claude-sonnet-4-6"
//! provider = "anthropic"
//! ```

use crate::model_router::{ModelConfig, ModelRouter};
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

    /// Optional multi-model configuration.
    /// When present, takes precedence over single `provider`/`model` fields.
    #[serde(default)]
    pub models: Vec<ModelConfig>,

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
            models: Vec::new(),
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

    /// Build a [`ModelRouter`] from the configuration.
    ///
    /// If `[[models]]` is present, uses those entries. Otherwise, creates a
    /// single-model router from the legacy `provider`/`model` fields.
    pub fn model_router(&self) -> ModelRouter {
        if self.models.is_empty() {
            ModelRouter::from_single(&self.provider, &self.model)
        } else {
            ModelRouter::new(self.models.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ElwoodConfig::default();
        assert_eq!(config.provider, "gemini");
        assert_eq!(config.model, "gemini-2.5-pro");
        assert!(config.models.is_empty());
    }

    #[test]
    fn test_parse_legacy_config() {
        let toml_str = r#"
            provider = "anthropic"
            model = "claude-sonnet-4-6"
        "#;
        let config: ElwoodConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.provider, "anthropic");
        assert_eq!(config.model, "claude-sonnet-4-6");
        assert!(config.models.is_empty());
    }

    #[test]
    fn test_parse_multi_model_config() {
        let toml_str = r#"
            provider = "gemini"
            model = "gemini-2.5-pro"

            [[models]]
            name = "gemini-2.5-pro"
            provider = "gemini"
            display_name = "Gemini Pro"
            default = true

            [[models]]
            name = "gemini-2.5-flash"
            provider = "gemini"

            [[models]]
            name = "claude-sonnet-4-6"
            provider = "anthropic"
            display_name = "Claude Sonnet"
            cost_per_1k_input = 0.003
            cost_per_1k_output = 0.015
        "#;
        let config: ElwoodConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.models.len(), 3);
        assert_eq!(config.models[0].name, "gemini-2.5-pro");
        assert!(config.models[0].default);
        assert_eq!(config.models[2].display_name, "Claude Sonnet");
    }

    #[test]
    fn test_model_router_from_legacy() {
        let config = ElwoodConfig::default();
        let router = config.model_router();
        assert_eq!(router.model_count(), 1);
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
        assert_eq!(router.active_model().provider, "gemini");
    }

    #[test]
    fn test_model_router_from_multi_model() {
        let toml_str = r#"
            [[models]]
            name = "gemini-2.5-pro"
            provider = "gemini"
            default = true

            [[models]]
            name = "claude-sonnet-4-6"
            provider = "anthropic"
        "#;
        let config: ElwoodConfig = toml::from_str(toml_str).unwrap();
        let router = config.model_router();
        assert_eq!(router.model_count(), 2);
        assert_eq!(router.active_model().name, "gemini-2.5-pro");
    }
}
