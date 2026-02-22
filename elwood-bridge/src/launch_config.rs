//! Launch configuration system — save and restore terminal setups.
//!
//! Users can save their current terminal state (working directory, environment
//! variables, startup commands, model preferences) as named configurations and
//! restore them later. Configurations are stored as TOML files in
//! `~/.elwood/launch_configs/`.
//!
//! ## Storage
//!
//! Each configuration is a single TOML file named `{slug}.toml`.
//!
//! ## Example
//!
//! ```toml
//! [config]
//! name = "elwood-dev"
//! description = "Elwood development environment"
//! working_dir = "/Users/gordon/Projects/elwood-pro"
//! model = "gemini-2.5-pro"
//! tags = ["dev", "rust"]
//!
//! [config.env_vars]
//! RUST_LOG = "debug"
//! CARGO_INCREMENTAL = "1"
//!
//! [[config.startup_commands]]
//! command = "git status"
//!
//! [[config.startup_commands]]
//! command = "cargo check --workspace"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─── Types ──────────────────────────────────────────────────────────────────

/// A startup command to run when applying a launch config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartupCommand {
    /// The shell command to execute.
    pub command: String,
}

/// A saved terminal launch configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LaunchConfig {
    /// Configuration name (used as filename slug).
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Starting working directory.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Environment variables to set on launch.
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    /// Commands to run sequentially on launch.
    #[serde(default)]
    pub startup_commands: Vec<StartupCommand>,
    /// Preferred LLM model name.
    #[serde(default)]
    pub model: Option<String>,
    /// Initial agent system prompt or context.
    #[serde(default)]
    pub agent_prompt: Option<String>,
    /// Tags for categorization and search.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Wrapper for TOML serialization — `[config]` table.
#[derive(Debug, Serialize, Deserialize)]
struct LaunchConfigFile {
    config: LaunchConfig,
}

/// Snapshot of current pane state, used to create a config from the live session.
#[derive(Debug, Clone, Default)]
pub struct PaneState {
    /// Current working directory of the pane.
    pub working_dir: Option<String>,
    /// Current model name.
    pub model: Option<String>,
    /// Environment variables set during this session.
    pub env_vars: HashMap<String, String>,
}

/// Structured data returned when applying a launch config.
///
/// The pane uses this to set working directory, apply env vars, and run
/// startup commands sequentially.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    /// Directory to change to (if any).
    pub working_dir: Option<String>,
    /// Environment variables to set.
    pub env_vars: HashMap<String, String>,
    /// Commands to run sequentially.
    pub commands: Vec<String>,
    /// Model to switch to (if any).
    pub model: Option<String>,
    /// Agent prompt to set (if any).
    pub agent_prompt: Option<String>,
}

// ─── Launch Config Manager ──────────────────────────────────────────────────

/// Manages CRUD operations on launch configurations stored as TOML files.
pub struct LaunchConfigManager {
    /// Directory where config TOML files are stored.
    dir: PathBuf,
}

impl LaunchConfigManager {
    /// Create a new manager pointing at the default `~/.elwood/launch_configs/` directory.
    pub fn new() -> Self {
        let dir = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".elwood")
            .join("launch_configs");
        Self { dir }
    }

    /// Create a manager with a custom storage directory (for testing).
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Return the storage directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Ensure the storage directory exists.
    fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Sanitize a config name into a safe filename slug.
    fn slug(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .to_lowercase()
    }

    /// Path to the TOML file for a given config name.
    pub fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.toml", Self::slug(name)))
    }

    /// Save a launch config to disk.
    pub fn save(&self, config: &LaunchConfig) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let file = LaunchConfigFile {
            config: config.clone(),
        };
        let toml_str = toml::to_string_pretty(&file)?;
        let path = self.path_for(&config.name);
        std::fs::write(&path, toml_str)?;
        Ok(())
    }

    /// Load a launch config by name.
    pub fn load(&self, name: &str) -> anyhow::Result<LaunchConfig> {
        let path = self.path_for(name);
        if !path.exists() {
            anyhow::bail!("Launch config not found: {name}");
        }
        let content = std::fs::read_to_string(&path)?;
        let file: LaunchConfigFile = toml::from_str(&content)?;
        Ok(file.config)
    }

    /// Delete a launch config by name.
    pub fn delete(&self, name: &str) -> anyhow::Result<()> {
        let path = self.path_for(name);
        if !path.exists() {
            anyhow::bail!("Launch config not found: {name}");
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// List all saved launch configs (sorted by name).
    pub fn list(&self) -> anyhow::Result<Vec<LaunchConfig>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut configs = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match toml::from_str::<LaunchConfigFile>(&content) {
                        Ok(file) => configs.push(file.config),
                        Err(e) => {
                            tracing::warn!(
                                "Skipping malformed launch config {}: {e}",
                                path.display()
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Cannot read {}: {e}", path.display());
                    }
                }
            }
        }
        configs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(configs)
    }

    /// Check if a config with the given name exists.
    pub fn exists(&self, name: &str) -> bool {
        self.path_for(name).exists()
    }

    /// Apply a launch config, returning structured data for the pane to execute.
    pub fn apply(&self, config: &LaunchConfig) -> ApplyResult {
        let commands = config
            .startup_commands
            .iter()
            .map(|sc| sc.command.clone())
            .collect();
        ApplyResult {
            working_dir: config.working_dir.clone(),
            env_vars: config.env_vars.clone(),
            commands,
            model: config.model.clone(),
            agent_prompt: config.agent_prompt.clone(),
        }
    }

    /// Create a new launch config by snapshotting the current pane state.
    pub fn create_from_current(&self, name: &str, pane_state: &PaneState) -> LaunchConfig {
        LaunchConfig {
            name: name.to_string(),
            description: None,
            working_dir: pane_state.working_dir.clone(),
            env_vars: pane_state.env_vars.clone(),
            startup_commands: Vec::new(),
            model: pane_state.model.clone(),
            agent_prompt: None,
            tags: Vec::new(),
        }
    }
}

impl Default for LaunchConfigManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Slash Command Execution ────────────────────────────────────────────────

/// Result of a `/launch` subcommand.
#[derive(Debug, Clone)]
pub enum LaunchCommandResult {
    /// Display informational text in the chat area.
    ChatMessage(String),
    /// Apply a launch config (pane should handle this).
    Apply(ApplyResult),
}

/// Execute a `/launch` slash command and return the result.
///
/// Subcommands:
/// - `list` — list saved configs
/// - `apply <name>` — apply a config
/// - `save <name>` — save current state
/// - `delete <name>` — delete a config
/// - `show <name>` — show config details
/// - `edit <name>` — show TOML file path for editing
/// - (empty) — show help
pub fn execute_launch_command(args: &str, pane_state: &PaneState) -> LaunchCommandResult {
    let args = args.trim();
    let (subcmd, rest) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "" | "help" => LaunchCommandResult::ChatMessage(launch_help()),
        "list" | "ls" => execute_list(),
        "apply" | "use" | "load" => execute_apply(rest),
        "save" | "create" | "new" => execute_save(rest, pane_state),
        "delete" | "rm" | "remove" => execute_delete(rest),
        "show" | "info" => execute_show(rest),
        "edit" => execute_edit(rest),
        _ => {
            // Maybe it's a direct config name to apply
            let mgr = LaunchConfigManager::new();
            if mgr.exists(subcmd) {
                execute_apply(subcmd)
            } else {
                LaunchCommandResult::ChatMessage(format!(
                    "Unknown subcommand: {subcmd}\n\n{}",
                    launch_help()
                ))
            }
        }
    }
}

fn launch_help() -> String {
    "\
Launch configuration commands:

  /launch list               List saved configurations
  /launch apply <name>       Apply a configuration (cd, env, run commands)
  /launch save <name>        Save current state as a configuration
  /launch show <name>        Show configuration details
  /launch edit <name>        Show TOML file path for manual editing
  /launch delete <name>      Delete a configuration

Alias: /lc

Configs are stored in ~/.elwood/launch_configs/ as TOML files."
        .to_string()
}

fn execute_list() -> LaunchCommandResult {
    let mgr = LaunchConfigManager::new();
    match mgr.list() {
        Ok(configs) if configs.is_empty() => LaunchCommandResult::ChatMessage(
            "No launch configs saved yet.\n\n\
             Create one with: /launch save <name>\n\
             Or create a TOML file in ~/.elwood/launch_configs/"
                .to_string(),
        ),
        Ok(configs) => {
            let mut msg = String::from("Saved launch configurations:\n\n");
            for c in &configs {
                let desc = c
                    .description
                    .as_deref()
                    .unwrap_or("(no description)");
                let tags = if c.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", c.tags.join(", "))
                };
                let dir = c
                    .working_dir
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                msg.push_str(&format!("  {:<20} {}{}{}\n", c.name, desc, tags, dir));
            }
            msg.push_str("\nApply with: /launch apply <name>");
            LaunchCommandResult::ChatMessage(msg)
        }
        Err(e) => {
            LaunchCommandResult::ChatMessage(format!("Error listing launch configs: {e}"))
        }
    }
}

fn execute_apply(args: &str) -> LaunchCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return LaunchCommandResult::ChatMessage(
            "Usage: /launch apply <name>".to_string(),
        );
    }

    let mgr = LaunchConfigManager::new();
    match mgr.load(name) {
        Ok(config) => {
            let result = mgr.apply(&config);
            LaunchCommandResult::Apply(result)
        }
        Err(e) => LaunchCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

fn execute_save(args: &str, pane_state: &PaneState) -> LaunchCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return LaunchCommandResult::ChatMessage(
            "Usage: /launch save <name>".to_string(),
        );
    }

    let mgr = LaunchConfigManager::new();
    if mgr.exists(name) {
        return LaunchCommandResult::ChatMessage(format!(
            "Launch config '{name}' already exists. Delete it first or choose a different name.",
        ));
    }

    let config = mgr.create_from_current(name, pane_state);
    match mgr.save(&config) {
        Ok(()) => {
            let path = mgr.path_for(name);
            LaunchCommandResult::ChatMessage(format!(
                "Launch config '{name}' saved.\n\n\
                 Edit to add startup commands, env vars, etc:\n  {}\n\n\
                 Apply with: /launch apply {name}",
                path.display(),
            ))
        }
        Err(e) => LaunchCommandResult::ChatMessage(format!("Error saving config: {e}")),
    }
}

fn execute_delete(args: &str) -> LaunchCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return LaunchCommandResult::ChatMessage(
            "Usage: /launch delete <name>".to_string(),
        );
    }

    let mgr = LaunchConfigManager::new();
    match mgr.delete(name) {
        Ok(()) => LaunchCommandResult::ChatMessage(format!("Launch config '{name}' deleted.")),
        Err(e) => LaunchCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

fn execute_show(args: &str) -> LaunchCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return LaunchCommandResult::ChatMessage(
            "Usage: /launch show <name>".to_string(),
        );
    }

    let mgr = LaunchConfigManager::new();
    match mgr.load(name) {
        Ok(c) => {
            let mut msg = format!("Launch config: {}\n", c.name);
            if let Some(ref desc) = c.description {
                msg.push_str(&format!("  {desc}\n"));
            }
            if !c.tags.is_empty() {
                msg.push_str(&format!("  Tags: {}\n", c.tags.join(", ")));
            }
            if let Some(ref dir) = c.working_dir {
                msg.push_str(&format!("\nWorking directory: {dir}\n"));
            }
            if let Some(ref model) = c.model {
                msg.push_str(&format!("Model: {model}\n"));
            }
            if let Some(ref prompt) = c.agent_prompt {
                let preview = if prompt.len() > 80 {
                    let mut end = 80;
                    while end > 0 && !prompt.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...", &prompt[..end])
                } else {
                    prompt.clone()
                };
                msg.push_str(&format!("Agent prompt: {preview}\n"));
            }
            if !c.env_vars.is_empty() {
                msg.push_str("\nEnvironment variables:\n");
                let mut vars: Vec<_> = c.env_vars.iter().collect();
                vars.sort_by_key(|(k, _)| *k);
                for (key, value) in vars {
                    msg.push_str(&format!("  {key}={value}\n"));
                }
            }
            if !c.startup_commands.is_empty() {
                msg.push_str(&format!(
                    "\nStartup commands ({}):\n",
                    c.startup_commands.len()
                ));
                for (i, cmd) in c.startup_commands.iter().enumerate() {
                    msg.push_str(&format!("  {}. {}\n", i + 1, cmd.command));
                }
            }
            LaunchCommandResult::ChatMessage(msg)
        }
        Err(e) => LaunchCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

fn execute_edit(args: &str) -> LaunchCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return LaunchCommandResult::ChatMessage(
            "Usage: /launch edit <name>".to_string(),
        );
    }

    let mgr = LaunchConfigManager::new();
    let path = mgr.path_for(name);
    if path.exists() {
        LaunchCommandResult::ChatMessage(format!(
            "Edit this file:\n  {}\n\n\
             After editing, apply with: /launch apply {name}",
            path.display(),
        ))
    } else {
        LaunchCommandResult::ChatMessage(format!(
            "Launch config '{name}' not found.\n\
             Create it first with: /launch save {name}",
        ))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_config() -> LaunchConfig {
        LaunchConfig {
            name: "elwood-dev".to_string(),
            description: Some("Elwood development environment".to_string()),
            working_dir: Some("/home/user/projects/elwood".to_string()),
            env_vars: HashMap::from([
                ("RUST_LOG".to_string(), "debug".to_string()),
                ("CARGO_INCREMENTAL".to_string(), "1".to_string()),
            ]),
            startup_commands: vec![
                StartupCommand {
                    command: "git status".to_string(),
                },
                StartupCommand {
                    command: "cargo check --workspace".to_string(),
                },
            ],
            model: Some("gemini-2.5-pro".to_string()),
            agent_prompt: None,
            tags: vec!["dev".to_string(), "rust".to_string()],
        }
    }

    fn sample_pane_state() -> PaneState {
        PaneState {
            working_dir: Some("/tmp/test-project".to_string()),
            model: Some("gemini-2.5-flash".to_string()),
            env_vars: HashMap::from([("FOO".to_string(), "bar".to_string())]),
        }
    }

    // ── TOML round-trip ─────────────────────────────────────────────────

    #[test]
    fn test_toml_roundtrip() {
        let config = sample_config();
        let file = LaunchConfigFile {
            config: config.clone(),
        };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: LaunchConfigFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.config, config);
    }

    #[test]
    fn test_toml_format() {
        let config = sample_config();
        let file = LaunchConfigFile { config };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        assert!(toml_str.contains("[config]"));
        assert!(toml_str.contains("name = \"elwood-dev\""));
        assert!(toml_str.contains("[config.env_vars]"));
        assert!(toml_str.contains("[[config.startup_commands]]"));
    }

    #[test]
    fn test_toml_minimal_deserialize() {
        let toml_str = r#"
[config]
name = "minimal"
"#;
        let file: LaunchConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.config.name, "minimal");
        assert!(file.config.description.is_none());
        assert!(file.config.working_dir.is_none());
        assert!(file.config.env_vars.is_empty());
        assert!(file.config.startup_commands.is_empty());
        assert!(file.config.model.is_none());
        assert!(file.config.agent_prompt.is_none());
        assert!(file.config.tags.is_empty());
    }

    #[test]
    fn test_toml_with_env_vars_only() {
        let toml_str = r#"
[config]
name = "env-test"

[config.env_vars]
PATH = "/usr/local/bin"
EDITOR = "vim"
"#;
        let file: LaunchConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.config.env_vars.len(), 2);
        assert_eq!(file.config.env_vars.get("PATH").unwrap(), "/usr/local/bin");
        assert_eq!(file.config.env_vars.get("EDITOR").unwrap(), "vim");
    }

    #[test]
    fn test_toml_with_startup_commands() {
        let toml_str = r#"
[config]
name = "cmd-test"

[[config.startup_commands]]
command = "echo hello"

[[config.startup_commands]]
command = "ls -la"
"#;
        let file: LaunchConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.config.startup_commands.len(), 2);
        assert_eq!(file.config.startup_commands[0].command, "echo hello");
        assert_eq!(file.config.startup_commands[1].command, "ls -la");
    }

    // ── CRUD operations ─────────────────────────────────────────────────

    #[test]
    fn test_manager_save_load() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        let config = sample_config();

        mgr.save(&config).unwrap();
        assert!(mgr.exists("elwood-dev"));

        let loaded = mgr.load("elwood-dev").unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn test_manager_delete() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        let config = sample_config();

        mgr.save(&config).unwrap();
        assert!(mgr.exists("elwood-dev"));

        mgr.delete("elwood-dev").unwrap();
        assert!(!mgr.exists("elwood-dev"));
    }

    #[test]
    fn test_manager_delete_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        assert!(mgr.delete("nope").is_err());
    }

    #[test]
    fn test_manager_load_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        assert!(mgr.load("nope").is_err());
    }

    #[test]
    fn test_manager_list_empty() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        assert_eq!(mgr.list().unwrap().len(), 0);
    }

    #[test]
    fn test_manager_list() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());

        let mut c1 = sample_config();
        c1.name = "beta".to_string();
        mgr.save(&c1).unwrap();

        let mut c2 = sample_config();
        c2.name = "alpha".to_string();
        mgr.save(&c2).unwrap();

        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 2);
        // Sorted by name
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[1].name, "beta");
    }

    #[test]
    fn test_manager_list_skips_malformed() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());

        mgr.save(&sample_config()).unwrap();
        std::fs::write(tmp.path().join("bad.toml"), "not valid toml [[[").unwrap();

        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "elwood-dev");
    }

    #[test]
    fn test_manager_overwrite() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());

        let mut config = sample_config();
        mgr.save(&config).unwrap();

        config.description = Some("Updated".to_string());
        mgr.save(&config).unwrap();

        let loaded = mgr.load("elwood-dev").unwrap();
        assert_eq!(loaded.description, Some("Updated".to_string()));
    }

    #[test]
    fn test_manager_list_nonexistent_dir() {
        let mgr = LaunchConfigManager::with_dir(PathBuf::from("/nonexistent/path/xyz"));
        assert_eq!(mgr.list().unwrap().len(), 0);
    }

    // ── Slug sanitization ───────────────────────────────────────────────

    #[test]
    fn test_slug_sanitization() {
        assert_eq!(LaunchConfigManager::slug("my config!"), "my-config-");
        assert_eq!(LaunchConfigManager::slug("Elwood-Dev"), "elwood-dev");
        assert_eq!(LaunchConfigManager::slug("test_123"), "test_123");
        assert_eq!(LaunchConfigManager::slug("a b/c"), "a-b-c");
    }

    // ── env var handling ────────────────────────────────────────────────

    #[test]
    fn test_env_vars_preserved_on_save_load() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());

        let mut config = sample_config();
        config.env_vars.insert("SPECIAL_CHARS".to_string(), "val=with=equals".to_string());
        config
            .env_vars
            .insert("EMPTY".to_string(), String::new());

        mgr.save(&config).unwrap();
        let loaded = mgr.load("elwood-dev").unwrap();
        assert_eq!(loaded.env_vars.get("SPECIAL_CHARS").unwrap(), "val=with=equals");
        assert_eq!(loaded.env_vars.get("EMPTY").unwrap(), "");
        assert_eq!(loaded.env_vars.get("RUST_LOG").unwrap(), "debug");
    }

    // ── apply ───────────────────────────────────────────────────────────

    #[test]
    fn test_apply() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        let config = sample_config();

        let result = mgr.apply(&config);
        assert_eq!(
            result.working_dir,
            Some("/home/user/projects/elwood".to_string())
        );
        assert_eq!(result.env_vars.len(), 2);
        assert_eq!(result.commands.len(), 2);
        assert_eq!(result.commands[0], "git status");
        assert_eq!(result.commands[1], "cargo check --workspace");
        assert_eq!(result.model, Some("gemini-2.5-pro".to_string()));
        assert!(result.agent_prompt.is_none());
    }

    #[test]
    fn test_apply_empty_config() {
        let mgr = LaunchConfigManager::with_dir(PathBuf::from("/tmp"));
        let config = LaunchConfig {
            name: "empty".to_string(),
            description: None,
            working_dir: None,
            env_vars: HashMap::new(),
            startup_commands: Vec::new(),
            model: None,
            agent_prompt: None,
            tags: Vec::new(),
        };

        let result = mgr.apply(&config);
        assert!(result.working_dir.is_none());
        assert!(result.env_vars.is_empty());
        assert!(result.commands.is_empty());
        assert!(result.model.is_none());
        assert!(result.agent_prompt.is_none());
    }

    // ── create_from_current ─────────────────────────────────────────────

    #[test]
    fn test_create_from_current() {
        let tmp = TempDir::new().unwrap();
        let mgr = LaunchConfigManager::with_dir(tmp.path().to_path_buf());
        let state = sample_pane_state();

        let config = mgr.create_from_current("my-snapshot", &state);
        assert_eq!(config.name, "my-snapshot");
        assert_eq!(
            config.working_dir,
            Some("/tmp/test-project".to_string())
        );
        assert_eq!(config.model, Some("gemini-2.5-flash".to_string()));
        assert_eq!(config.env_vars.get("FOO").unwrap(), "bar");
        assert!(config.startup_commands.is_empty());
        assert!(config.description.is_none());
    }

    #[test]
    fn test_create_from_current_empty_state() {
        let mgr = LaunchConfigManager::with_dir(PathBuf::from("/tmp"));
        let state = PaneState::default();

        let config = mgr.create_from_current("blank", &state);
        assert_eq!(config.name, "blank");
        assert!(config.working_dir.is_none());
        assert!(config.model.is_none());
        assert!(config.env_vars.is_empty());
    }

    // ── Slash command execution ─────────────────────────────────────────

    #[test]
    fn test_command_help() {
        let state = PaneState::default();
        let result = execute_launch_command("", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("/launch list"));
                assert!(msg.contains("/launch apply"));
                assert!(msg.contains("/launch save"));
                assert!(msg.contains("/launch delete"));
                assert!(msg.contains("/launch edit"));
                assert!(msg.contains("/lc"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_help_explicit() {
        let state = PaneState::default();
        let result = execute_launch_command("help", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("/launch list"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_apply_empty() {
        let state = PaneState::default();
        let result = execute_launch_command("apply", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_save_empty() {
        let state = PaneState::default();
        let result = execute_launch_command("save", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_delete_empty() {
        let state = PaneState::default();
        let result = execute_launch_command("delete", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_show_empty() {
        let state = PaneState::default();
        let result = execute_launch_command("show", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_edit_empty() {
        let state = PaneState::default();
        let result = execute_launch_command("edit", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_unknown_subcommand() {
        let state = PaneState::default();
        let result = execute_launch_command("foobar", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Unknown subcommand"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_edit_nonexistent() {
        let state = PaneState::default();
        let result = execute_launch_command("edit nonexistent-config", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_command_apply_nonexistent() {
        let state = PaneState::default();
        let result = execute_launch_command("apply nonexistent-config", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Error"));
            }
            _ => panic!("Expected ChatMessage for nonexistent config"),
        }
    }

    #[test]
    fn test_command_delete_nonexistent() {
        let state = PaneState::default();
        let result = execute_launch_command("delete nonexistent-config", &state);
        match result {
            LaunchCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Error"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }
}
