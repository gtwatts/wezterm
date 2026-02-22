//! MCP server configuration.
//!
//! Parses `[mcp]` and `[mcp.servers.*]` sections from `elwood.toml`.
//! Supports environment variable expansion in command args and env values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level MCP configuration block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// Whether the MCP client is enabled (consuming external servers).
    #[serde(default = "default_true")]
    pub client_enabled: bool,

    /// Whether the MCP server is enabled (exposing terminal capabilities).
    #[serde(default)]
    pub server_enabled: bool,

    /// Configured MCP servers to connect to (client side).
    #[serde(default)]
    pub servers: HashMap<String, McpServerConfig>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            client_enabled: true,
            server_enabled: false,
            servers: HashMap::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to launch the server process (stdio transport).
    pub command: String,

    /// Arguments to the command.
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables to set for the server process.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Transport type. Defaults to "stdio".
    #[serde(default = "default_transport")]
    pub transport: String,

    /// Trust level for this server.
    #[serde(default)]
    pub permissions: McpServerPermissions,
}

fn default_transport() -> String {
    "stdio".to_string()
}

/// Per-server permission configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerPermissions {
    /// Trust level: "trusted", "untrusted", or "sandbox".
    #[serde(default = "default_trust_level")]
    pub trust_level: String,
}

impl Default for McpServerPermissions {
    fn default() -> Self {
        Self {
            trust_level: default_trust_level(),
        }
    }
}

fn default_trust_level() -> String {
    "untrusted".to_string()
}

/// Expand `${VAR_NAME}` patterns in a string using environment variables.
///
/// If a variable is not set, the `${VAR_NAME}` token is left as-is.
pub fn expand_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                var_name.push(c);
            }
            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => {
                    // Leave the original token for unresolved variables
                    result.push_str(&format!("${{{var_name}}}"));
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Expand environment variables in a server config's args and env values.
pub fn expand_server_config(config: &McpServerConfig) -> McpServerConfig {
    McpServerConfig {
        command: expand_env_vars(&config.command),
        args: config.args.iter().map(|a| expand_env_vars(a)).collect(),
        env: config
            .env
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect(),
        transport: config.transport.clone(),
        permissions: config.permissions.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = McpConfig::default();
        assert!(config.client_enabled);
        assert!(!config.server_enabled);
        assert!(config.servers.is_empty());
    }

    #[test]
    fn test_deserialize_config() {
        let toml_str = r#"
            client_enabled = true

            [servers.filesystem]
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

            [servers.github]
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-github"]
            env = { GITHUB_TOKEN = "abc123" }
        "#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert!(config.client_enabled);
        assert_eq!(config.servers.len(), 2);
        assert!(config.servers.contains_key("filesystem"));
        assert!(config.servers.contains_key("github"));

        let fs = &config.servers["filesystem"];
        assert_eq!(fs.command, "npx");
        assert_eq!(fs.args.len(), 3);
        assert_eq!(fs.transport, "stdio");

        let gh = &config.servers["github"];
        assert_eq!(gh.env.get("GITHUB_TOKEN").unwrap(), "abc123");
    }

    #[test]
    fn test_expand_env_vars() {
        std::env::set_var("_TEST_MCP_VAR", "hello");
        assert_eq!(expand_env_vars("${_TEST_MCP_VAR}"), "hello");
        assert_eq!(expand_env_vars("prefix_${_TEST_MCP_VAR}_suffix"), "prefix_hello_suffix");
        assert_eq!(expand_env_vars("no_vars_here"), "no_vars_here");
        // Unset variable left as-is
        assert_eq!(
            expand_env_vars("${_TEST_MCP_UNSET_VAR_12345}"),
            "${_TEST_MCP_UNSET_VAR_12345}"
        );
        std::env::remove_var("_TEST_MCP_VAR");
    }

    #[test]
    fn test_expand_server_config() {
        std::env::set_var("_TEST_MCP_CMD", "node");
        let config = McpServerConfig {
            command: "${_TEST_MCP_CMD}".to_string(),
            args: vec!["${_TEST_MCP_CMD}".to_string(), "literal".to_string()],
            env: HashMap::from([("KEY".to_string(), "${_TEST_MCP_CMD}".to_string())]),
            transport: "stdio".to_string(),
            permissions: McpServerPermissions::default(),
        };
        let expanded = expand_server_config(&config);
        assert_eq!(expanded.command, "node");
        assert_eq!(expanded.args[0], "node");
        assert_eq!(expanded.args[1], "literal");
        assert_eq!(expanded.env.get("KEY").unwrap(), "node");
        std::env::remove_var("_TEST_MCP_CMD");
    }

    #[test]
    fn test_server_permissions_default() {
        let perms = McpServerPermissions::default();
        assert_eq!(perms.trust_level, "untrusted");
    }

    #[test]
    fn test_deserialize_server_enabled() {
        let toml_str = r#"
            client_enabled = true
            server_enabled = true
        "#;
        let config: McpConfig = toml::from_str(toml_str).unwrap();
        assert!(config.client_enabled);
        assert!(config.server_enabled);
    }

    #[test]
    fn test_deserialize_with_permissions() {
        let toml_str = r#"
            command = "npx"
            args = ["-y", "some-server"]

            [permissions]
            trust_level = "trusted"
        "#;
        let config: McpServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.permissions.trust_level, "trusted");
    }
}
