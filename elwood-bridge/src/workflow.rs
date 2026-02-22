//! Workflow system — saved, parameterized, replayable command sequences.
//!
//! Users can define multi-step workflows with `{{param}}` template placeholders,
//! save them as TOML files, and replay them with parameter prompts. This is
//! Elwood Terminal's answer to Warp's workflow feature.
//!
//! ## Storage
//!
//! Workflows are stored as TOML files in `~/.elwood/workflows/`. Each file
//! contains a single workflow definition.
//!
//! ## Example
//!
//! ```toml
//! [workflow]
//! name = "deploy"
//! description = "Deploy to production"
//! tags = ["deploy", "ci"]
//!
//! [[workflow.parameters]]
//! name = "branch"
//! default = "main"
//! description = "Branch to deploy"
//!
//! [[workflow.steps]]
//! command = "git checkout {{branch}}"
//! description = "Switch to deploy branch"
//! continue_on_error = false
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─── Types ──────────────────────────────────────────────────────────────────

/// A single parameter definition for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowParameter {
    /// Parameter name (used in `{{name}}` placeholders).
    pub name: String,
    /// Default value (empty string if none).
    #[serde(default)]
    pub default: String,
    /// Human-readable description shown during prompting.
    #[serde(default)]
    pub description: String,
}

/// A single step in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowStep {
    /// Command template with `{{param}}` placeholders.
    pub command: String,
    /// Optional human-readable description of this step.
    #[serde(default)]
    pub description: String,
    /// Whether to continue executing subsequent steps if this one fails.
    #[serde(default)]
    pub continue_on_error: bool,
}

/// A complete workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workflow {
    /// Unique name (used as filename slug).
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Tags for categorization and search.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Parameter definitions.
    #[serde(default)]
    pub parameters: Vec<WorkflowParameter>,
    /// Ordered list of steps to execute.
    pub steps: Vec<WorkflowStep>,
}

/// Wrapper for TOML serialization — `[workflow]` table.
#[derive(Debug, Serialize, Deserialize)]
struct WorkflowFile {
    workflow: Workflow,
}

/// Result of executing a single workflow step.
#[derive(Debug, Clone)]
pub struct StepResult {
    /// 0-based step index.
    pub index: usize,
    /// The resolved command (after parameter substitution).
    pub command: String,
    /// Step description.
    pub description: String,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
    /// Process exit code (`None` on timeout or spawn failure).
    pub exit_code: Option<i32>,
}

impl StepResult {
    /// Whether this step succeeded (exit code 0).
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

// ─── Parameter Substitution ─────────────────────────────────────────────────

/// Substitute `{{param_name}}` placeholders in a command template.
///
/// Unknown placeholders are left as-is (not replaced).
///
/// # Examples
///
/// ```
/// use elwood_bridge::workflow::substitute_params;
/// use std::collections::HashMap;
///
/// let mut params = HashMap::new();
/// params.insert("branch".to_string(), "main".to_string());
/// params.insert("env".to_string(), "prod".to_string());
///
/// assert_eq!(
///     substitute_params("git checkout {{branch}} && deploy --env={{env}}", &params),
///     "git checkout main && deploy --env=prod",
/// );
/// ```
pub fn substitute_params(template: &str, params: &HashMap<String, String>) -> String {
    let mut result = template.to_string();
    for (key, value) in params {
        let placeholder = format!("{{{{{key}}}}}");
        result = result.replace(&placeholder, value);
    }
    result
}

/// Build a parameter map from workflow definitions and user overrides.
///
/// For each parameter, uses the override if provided, otherwise falls back
/// to the default value.
pub fn build_param_map(
    definitions: &[WorkflowParameter],
    overrides: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for param in definitions {
        let value = overrides
            .get(&param.name)
            .cloned()
            .unwrap_or_else(|| param.default.clone());
        map.insert(param.name.clone(), value);
    }
    map
}

/// Resolve all steps in a workflow, substituting parameters.
pub fn resolve_steps(
    workflow: &Workflow,
    params: &HashMap<String, String>,
) -> Vec<(String, String, bool)> {
    workflow
        .steps
        .iter()
        .map(|step| {
            let command = substitute_params(&step.command, params);
            (command, step.description.clone(), step.continue_on_error)
        })
        .collect()
}

// ─── Workflow Manager ───────────────────────────────────────────────────────

/// Manages CRUD operations on workflows stored as TOML files.
pub struct WorkflowManager {
    /// Directory where workflow TOML files are stored.
    dir: PathBuf,
}

impl WorkflowManager {
    /// Create a new manager pointing at the default `~/.elwood/workflows/` directory.
    pub fn new() -> Self {
        let dir = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".elwood")
            .join("workflows");
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

    /// Sanitize a workflow name into a safe filename slug.
    fn slug(name: &str) -> String {
        name.chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect::<String>()
            .to_lowercase()
    }

    /// Path to the TOML file for a given workflow name.
    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.toml", Self::slug(name)))
    }

    /// Save a workflow to disk.
    pub fn save(&self, workflow: &Workflow) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let file = WorkflowFile {
            workflow: workflow.clone(),
        };
        let toml_str = toml::to_string_pretty(&file)?;
        let path = self.path_for(&workflow.name);
        std::fs::write(&path, toml_str)?;
        Ok(())
    }

    /// Load a workflow by name.
    pub fn load(&self, name: &str) -> anyhow::Result<Workflow> {
        let path = self.path_for(name);
        if !path.exists() {
            anyhow::bail!("Workflow not found: {name}");
        }
        let content = std::fs::read_to_string(&path)?;
        let file: WorkflowFile = toml::from_str(&content)?;
        Ok(file.workflow)
    }

    /// Delete a workflow by name.
    pub fn delete(&self, name: &str) -> anyhow::Result<()> {
        let path = self.path_for(name);
        if !path.exists() {
            anyhow::bail!("Workflow not found: {name}");
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// List all saved workflows (name + description).
    pub fn list(&self) -> anyhow::Result<Vec<Workflow>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut workflows = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "toml") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match toml::from_str::<WorkflowFile>(&content) {
                        Ok(file) => workflows.push(file.workflow),
                        Err(e) => {
                            tracing::warn!("Skipping malformed workflow {}: {e}", path.display());
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Cannot read {}: {e}", path.display());
                    }
                }
            }
        }
        workflows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(workflows)
    }

    /// Check if a workflow with the given name exists.
    pub fn exists(&self, name: &str) -> bool {
        self.path_for(name).exists()
    }

    /// Find workflows matching a tag.
    pub fn find_by_tag(&self, tag: &str) -> anyhow::Result<Vec<Workflow>> {
        let all = self.list()?;
        let tag_lower = tag.to_lowercase();
        Ok(all
            .into_iter()
            .filter(|w| w.tags.iter().any(|t| t.to_lowercase() == tag_lower))
            .collect())
    }

    /// Search workflows by name or description substring (case-insensitive).
    pub fn search(&self, query: &str) -> anyhow::Result<Vec<Workflow>> {
        let all = self.list()?;
        let q = query.to_lowercase();
        Ok(all
            .into_iter()
            .filter(|w| {
                w.name.to_lowercase().contains(&q)
                    || w.description.to_lowercase().contains(&q)
                    || w.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .collect())
    }
}

impl Default for WorkflowManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Slash Command Execution ────────────────────────────────────────────────

/// Execute a `/workflow` slash command and return formatted output.
///
/// Subcommands:
/// - `list` — list all saved workflows
/// - `save <name> [description]` — save a new empty workflow
/// - `show <name>` — show workflow details
/// - `delete <name>` — delete a workflow
/// - `run <name> [param=value ...]` — run a workflow (returns steps to execute)
/// - (empty) — show help
pub fn execute_workflow_command(args: &str) -> WorkflowCommandResult {
    let args = args.trim();
    let (subcmd, rest) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "" | "help" => WorkflowCommandResult::ChatMessage(workflow_help()),
        "list" | "ls" => execute_list(),
        "save" | "create" | "new" => execute_save(rest),
        "show" | "info" => execute_show(rest),
        "delete" | "rm" | "remove" => execute_delete(rest),
        "run" | "exec" => execute_run(rest),
        _ => {
            // Maybe it's a direct workflow name to run
            if WorkflowManager::new().exists(subcmd) {
                execute_run(args)
            } else {
                WorkflowCommandResult::ChatMessage(format!(
                    "Unknown subcommand: {subcmd}\n\n{}",
                    workflow_help()
                ))
            }
        }
    }
}

/// Result of a `/workflow` subcommand.
#[derive(Debug, Clone)]
pub enum WorkflowCommandResult {
    /// Display informational text in the chat area.
    ChatMessage(String),
    /// Run these resolved commands as a workflow sequence.
    RunSteps {
        /// Workflow name.
        name: String,
        /// Resolved (command, description, continue_on_error) triples.
        steps: Vec<(String, String, bool)>,
    },
}

fn workflow_help() -> String {
    "\
Workflow commands:

  /workflow list               List saved workflows
  /workflow save <name> [desc] Save a new workflow (edit TOML afterwards)
  /workflow show <name>        Show workflow steps and parameters
  /workflow run <name> [p=v..] Run a workflow with optional parameters
  /workflow delete <name>      Delete a saved workflow

Workflows are stored in ~/.elwood/workflows/ as TOML files.
Use {{param}} placeholders in commands for parameterization."
        .to_string()
}

fn execute_list() -> WorkflowCommandResult {
    let mgr = WorkflowManager::new();
    match mgr.list() {
        Ok(workflows) if workflows.is_empty() => {
            WorkflowCommandResult::ChatMessage(
                "No workflows saved yet.\n\nCreate one with: /workflow save <name> <description>\nOr create a TOML file in ~/.elwood/workflows/".to_string(),
            )
        }
        Ok(workflows) => {
            let mut msg = String::from("Saved workflows:\n\n");
            for w in &workflows {
                let tags = if w.tags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", w.tags.join(", "))
                };
                let steps = w.steps.len();
                msg.push_str(&format!(
                    "  {:<16} {}{} ({steps} step{})\n",
                    w.name,
                    if w.description.is_empty() { "(no description)" } else { &w.description },
                    tags,
                    if steps == 1 { "" } else { "s" },
                ));
            }
            msg.push_str("\nRun with: /workflow run <name>");
            WorkflowCommandResult::ChatMessage(msg)
        }
        Err(e) => WorkflowCommandResult::ChatMessage(format!("Error listing workflows: {e}")),
    }
}

fn execute_save(args: &str) -> WorkflowCommandResult {
    if args.is_empty() {
        return WorkflowCommandResult::ChatMessage(
            "Usage: /workflow save <name> [description]".to_string(),
        );
    }

    let (name, description) = match args.split_once(char::is_whitespace) {
        Some((n, d)) => (n.trim(), d.trim()),
        None => (args.trim(), ""),
    };

    let mgr = WorkflowManager::new();
    if mgr.exists(name) {
        return WorkflowCommandResult::ChatMessage(format!(
            "Workflow '{name}' already exists. Delete it first or choose a different name.",
        ));
    }

    let workflow = Workflow {
        name: name.to_string(),
        description: description.to_string(),
        tags: Vec::new(),
        parameters: Vec::new(),
        steps: vec![WorkflowStep {
            command: "echo 'Replace this with your command'".to_string(),
            description: "Placeholder step".to_string(),
            continue_on_error: false,
        }],
    };

    match mgr.save(&workflow) {
        Ok(()) => {
            let path = mgr.path_for(name);
            WorkflowCommandResult::ChatMessage(format!(
                "Workflow '{name}' created.\n\nEdit the TOML file to add steps and parameters:\n  {}\n\nRun with: /workflow run {name}",
                path.display(),
            ))
        }
        Err(e) => WorkflowCommandResult::ChatMessage(format!("Error saving workflow: {e}")),
    }
}

fn execute_show(args: &str) -> WorkflowCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return WorkflowCommandResult::ChatMessage(
            "Usage: /workflow show <name>".to_string(),
        );
    }

    let mgr = WorkflowManager::new();
    match mgr.load(name) {
        Ok(w) => {
            let mut msg = format!("Workflow: {}\n", w.name);
            if !w.description.is_empty() {
                msg.push_str(&format!("  {}\n", w.description));
            }
            if !w.tags.is_empty() {
                msg.push_str(&format!("  Tags: {}\n", w.tags.join(", ")));
            }

            if !w.parameters.is_empty() {
                msg.push_str("\nParameters:\n");
                for p in &w.parameters {
                    let default = if p.default.is_empty() {
                        "(required)".to_string()
                    } else {
                        format!("default: {}", p.default)
                    };
                    msg.push_str(&format!("  {:<16} {} [{}]\n", p.name, p.description, default));
                }
            }

            msg.push_str(&format!("\nSteps ({}):\n", w.steps.len()));
            for (i, step) in w.steps.iter().enumerate() {
                let flag = if step.continue_on_error { " (continue on error)" } else { "" };
                msg.push_str(&format!("  {}. {}{}\n", i + 1, step.command, flag));
                if !step.description.is_empty() {
                    msg.push_str(&format!("     {}\n", step.description));
                }
            }
            WorkflowCommandResult::ChatMessage(msg)
        }
        Err(e) => WorkflowCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

fn execute_delete(args: &str) -> WorkflowCommandResult {
    let name = args.trim();
    if name.is_empty() {
        return WorkflowCommandResult::ChatMessage(
            "Usage: /workflow delete <name>".to_string(),
        );
    }

    let mgr = WorkflowManager::new();
    match mgr.delete(name) {
        Ok(()) => {
            WorkflowCommandResult::ChatMessage(format!("Workflow '{name}' deleted."))
        }
        Err(e) => WorkflowCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

fn execute_run(args: &str) -> WorkflowCommandResult {
    if args.is_empty() {
        return WorkflowCommandResult::ChatMessage(
            "Usage: /workflow run <name> [param=value ...]".to_string(),
        );
    }

    let parts: Vec<&str> = args.split_whitespace().collect();
    let name = parts[0];

    // Parse param=value pairs from remaining args
    let mut overrides = HashMap::new();
    for part in &parts[1..] {
        if let Some((key, value)) = part.split_once('=') {
            overrides.insert(key.to_string(), value.to_string());
        }
    }

    let mgr = WorkflowManager::new();
    match mgr.load(name) {
        Ok(w) => {
            let params = build_param_map(&w.parameters, &overrides);
            let steps = resolve_steps(&w, &params);
            if steps.is_empty() {
                return WorkflowCommandResult::ChatMessage(format!(
                    "Workflow '{name}' has no steps.",
                ));
            }
            WorkflowCommandResult::RunSteps {
                name: w.name.clone(),
                steps,
            }
        }
        Err(e) => WorkflowCommandResult::ChatMessage(format!("Error: {e}")),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_workflow() -> Workflow {
        Workflow {
            name: "test-deploy".to_string(),
            description: "Test deployment workflow".to_string(),
            tags: vec!["deploy".to_string(), "test".to_string()],
            parameters: vec![
                WorkflowParameter {
                    name: "branch".to_string(),
                    default: "main".to_string(),
                    description: "Branch to deploy".to_string(),
                },
                WorkflowParameter {
                    name: "env".to_string(),
                    default: "staging".to_string(),
                    description: "Target environment".to_string(),
                },
            ],
            steps: vec![
                WorkflowStep {
                    command: "git checkout {{branch}}".to_string(),
                    description: "Switch branch".to_string(),
                    continue_on_error: false,
                },
                WorkflowStep {
                    command: "cargo test --workspace".to_string(),
                    description: "Run tests".to_string(),
                    continue_on_error: false,
                },
                WorkflowStep {
                    command: "deploy --env={{env}} --branch={{branch}}".to_string(),
                    description: "Deploy to target".to_string(),
                    continue_on_error: false,
                },
            ],
        }
    }

    #[test]
    fn test_substitute_params() {
        let mut params = HashMap::new();
        params.insert("branch".to_string(), "develop".to_string());
        params.insert("env".to_string(), "prod".to_string());

        assert_eq!(
            substitute_params("git checkout {{branch}}", &params),
            "git checkout develop",
        );
        assert_eq!(
            substitute_params("deploy --env={{env}} --branch={{branch}}", &params),
            "deploy --env=prod --branch=develop",
        );
        // Unknown placeholders left as-is
        assert_eq!(
            substitute_params("echo {{unknown}}", &params),
            "echo {{unknown}}",
        );
        // No placeholders
        assert_eq!(
            substitute_params("echo hello", &params),
            "echo hello",
        );
    }

    #[test]
    fn test_substitute_params_empty_value() {
        let mut params = HashMap::new();
        params.insert("tag".to_string(), String::new());
        assert_eq!(
            substitute_params("git tag {{tag}}", &params),
            "git tag ",
        );
    }

    #[test]
    fn test_build_param_map_defaults() {
        let defs = vec![
            WorkflowParameter {
                name: "a".to_string(),
                default: "1".to_string(),
                description: String::new(),
            },
            WorkflowParameter {
                name: "b".to_string(),
                default: "2".to_string(),
                description: String::new(),
            },
        ];
        let overrides = HashMap::new();
        let map = build_param_map(&defs, &overrides);
        assert_eq!(map.get("a").unwrap(), "1");
        assert_eq!(map.get("b").unwrap(), "2");
    }

    #[test]
    fn test_build_param_map_overrides() {
        let defs = vec![WorkflowParameter {
            name: "branch".to_string(),
            default: "main".to_string(),
            description: String::new(),
        }];
        let mut overrides = HashMap::new();
        overrides.insert("branch".to_string(), "develop".to_string());
        let map = build_param_map(&defs, &overrides);
        assert_eq!(map.get("branch").unwrap(), "develop");
    }

    #[test]
    fn test_resolve_steps() {
        let w = sample_workflow();
        let mut params = HashMap::new();
        params.insert("branch".to_string(), "feat/x".to_string());
        params.insert("env".to_string(), "prod".to_string());

        let steps = resolve_steps(&w, &params);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].0, "git checkout feat/x");
        assert_eq!(steps[1].0, "cargo test --workspace");
        assert_eq!(steps[2].0, "deploy --env=prod --branch=feat/x");
    }

    #[test]
    fn test_workflow_toml_roundtrip() {
        let w = sample_workflow();
        let file = WorkflowFile { workflow: w.clone() };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: WorkflowFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.workflow, w);
    }

    #[test]
    fn test_workflow_toml_format() {
        let w = sample_workflow();
        let file = WorkflowFile { workflow: w };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        assert!(toml_str.contains("[workflow]"));
        assert!(toml_str.contains("name = \"test-deploy\""));
        assert!(toml_str.contains("[[workflow.parameters]]"));
        assert!(toml_str.contains("[[workflow.steps]]"));
    }

    #[test]
    fn test_manager_save_load() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());
        let w = sample_workflow();

        mgr.save(&w).unwrap();
        assert!(mgr.exists("test-deploy"));

        let loaded = mgr.load("test-deploy").unwrap();
        assert_eq!(loaded, w);
    }

    #[test]
    fn test_manager_delete() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());
        let w = sample_workflow();

        mgr.save(&w).unwrap();
        assert!(mgr.exists("test-deploy"));

        mgr.delete("test-deploy").unwrap();
        assert!(!mgr.exists("test-deploy"));
    }

    #[test]
    fn test_manager_delete_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());
        assert!(mgr.delete("nope").is_err());
    }

    #[test]
    fn test_manager_load_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());
        assert!(mgr.load("nope").is_err());
    }

    #[test]
    fn test_manager_list() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());

        // Empty list
        assert_eq!(mgr.list().unwrap().len(), 0);

        // Add two workflows
        let mut w1 = sample_workflow();
        w1.name = "alpha".to_string();
        mgr.save(&w1).unwrap();

        let mut w2 = sample_workflow();
        w2.name = "beta".to_string();
        mgr.save(&w2).unwrap();

        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 2);
        // Sorted by name
        assert_eq!(list[0].name, "alpha");
        assert_eq!(list[1].name, "beta");
    }

    #[test]
    fn test_manager_list_skips_malformed() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());

        // Save a valid workflow
        mgr.save(&sample_workflow()).unwrap();

        // Write a malformed TOML file
        std::fs::write(tmp.path().join("bad.toml"), "not valid toml [[[").unwrap();

        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-deploy");
    }

    #[test]
    fn test_manager_find_by_tag() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());

        let mut w1 = sample_workflow();
        w1.name = "deploy-staging".to_string();
        w1.tags = vec!["deploy".to_string(), "staging".to_string()];
        mgr.save(&w1).unwrap();

        let mut w2 = sample_workflow();
        w2.name = "run-tests".to_string();
        w2.tags = vec!["test".to_string(), "ci".to_string()];
        mgr.save(&w2).unwrap();

        let deploy = mgr.find_by_tag("deploy").unwrap();
        assert_eq!(deploy.len(), 1);
        assert_eq!(deploy[0].name, "deploy-staging");

        let ci = mgr.find_by_tag("CI").unwrap(); // case-insensitive
        assert_eq!(ci.len(), 1);
        assert_eq!(ci[0].name, "run-tests");
    }

    #[test]
    fn test_manager_search() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());

        let mut w1 = Workflow {
            name: "deploy-prod".to_string(),
            description: "Production deployment".to_string(),
            tags: vec!["deploy".to_string(), "ci".to_string()],
            parameters: Vec::new(),
            steps: vec![WorkflowStep {
                command: "deploy.sh".to_string(),
                description: String::new(),
                continue_on_error: false,
            }],
        };
        mgr.save(&w1).unwrap();

        let w2 = Workflow {
            name: "run-tests".to_string(),
            description: "Run test suite".to_string(),
            tags: vec!["test".to_string(), "ci".to_string()],
            parameters: Vec::new(),
            steps: vec![WorkflowStep {
                command: "cargo test".to_string(),
                description: String::new(),
                continue_on_error: false,
            }],
        };
        mgr.save(&w2).unwrap();

        let results = mgr.search("deploy").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "deploy-prod");

        let results = mgr.search("test").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "run-tests");

        // Search by tag
        let results = mgr.search("ci").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_slug_sanitization() {
        assert_eq!(WorkflowManager::slug("my workflow!"), "my-workflow-");
        assert_eq!(WorkflowManager::slug("Deploy-Prod"), "deploy-prod");
        assert_eq!(WorkflowManager::slug("test_123"), "test_123");
    }

    #[test]
    fn test_manager_overwrite_save() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkflowManager::with_dir(tmp.path().to_path_buf());

        let mut w = sample_workflow();
        mgr.save(&w).unwrap();

        w.description = "Updated description".to_string();
        mgr.save(&w).unwrap();

        let loaded = mgr.load("test-deploy").unwrap();
        assert_eq!(loaded.description, "Updated description");
    }

    #[test]
    fn test_step_result_success() {
        let result = StepResult {
            index: 0,
            command: "echo hi".to_string(),
            description: "test".to_string(),
            stdout: "hi\n".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
        };
        assert!(result.success());

        let failed = StepResult {
            exit_code: Some(1),
            ..result.clone()
        };
        assert!(!failed.success());

        let timeout = StepResult {
            exit_code: None,
            ..result
        };
        assert!(!timeout.success());
    }

    #[test]
    fn test_workflow_command_help() {
        let result = execute_workflow_command("");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("/workflow list"));
                assert!(msg.contains("/workflow run"));
                assert!(msg.contains("/workflow save"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_workflow_command_save_empty() {
        let result = execute_workflow_command("save");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_workflow_command_show_empty() {
        let result = execute_workflow_command("show");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_workflow_command_delete_empty() {
        let result = execute_workflow_command("delete");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_workflow_command_run_empty() {
        let result = execute_workflow_command("run");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_workflow_command_unknown() {
        let result = execute_workflow_command("foobar");
        match result {
            WorkflowCommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Unknown subcommand") || msg.contains("Error"));
            }
            _ => panic!("Expected ChatMessage"),
        }
    }

    #[test]
    fn test_minimal_workflow_deserialize() {
        let toml_str = r#"
[workflow]
name = "minimal"
steps = [{ command = "echo hello" }]
"#;
        let file: WorkflowFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.workflow.name, "minimal");
        assert_eq!(file.workflow.steps.len(), 1);
        assert!(file.workflow.description.is_empty());
        assert!(file.workflow.tags.is_empty());
        assert!(file.workflow.parameters.is_empty());
        assert!(!file.workflow.steps[0].continue_on_error);
    }
}
