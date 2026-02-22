//! Multi-agent pane management for the Elwood terminal.
//!
//! Provides an [`AgentRegistry`] that tracks multiple agent instances, each
//! potentially running in its own WezTerm pane. Includes a dashboard renderer
//! for visual status overview and command parsing for agent lifecycle operations.
//!
//! ## Slash Commands
//!
//! | Command | Description |
//! |---------|-------------|
//! | `/agent spawn [name] [--model name]` | Spawn a new agent pane |
//! | `/agent list` or `/agents` | Show agent dashboard |
//! | `/agent kill <name\|id>` | Terminate an agent |
//! | `/agent focus <name\|id>` | Switch to agent's pane |
//! | `/tell <name> <message>` | Send message to agent |
//!
//! ## Architecture
//!
//! The registry is designed to be wrapped in `Arc<Mutex<AgentRegistry>>` and
//! shared between the domain (which spawns panes) and the command layer (which
//! parses user input). Actual pane creation via WezTerm's Mux will be wired
//! in the domain layer later.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Local, Utc};

// ─── Agent Status ───────────────────────────────────────────────────────────

/// Current status of an agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent is idle, waiting for input.
    Idle,
    /// Agent is actively working on a task.
    Working,
    /// Agent encountered an error.
    Error,
    /// Agent is starting up (not yet ready).
    Starting,
    /// Agent has been shut down.
    Stopped,
}

impl AgentStatus {
    /// ANSI-colored status indicator for dashboard rendering.
    ///
    /// - Idle: green `●`
    /// - Working: yellow `◉`
    /// - Error: red `✗`
    /// - Starting: cyan `○`
    /// - Stopped: dim `◌`
    pub fn indicator(&self) -> &'static str {
        match self {
            Self::Idle => "\x1b[32m●\x1b[0m",    // green
            Self::Working => "\x1b[33m◉\x1b[0m",  // yellow
            Self::Error => "\x1b[31m✗\x1b[0m",    // red
            Self::Starting => "\x1b[36m○\x1b[0m",  // cyan
            Self::Stopped => "\x1b[2m◌\x1b[0m",    // dim
        }
    }

    /// Plain-text label (no ANSI).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Error => "error",
            Self::Starting => "starting",
            Self::Stopped => "stopped",
        }
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ─── Agent Instance ─────────────────────────────────────────────────────────

/// A single agent instance tracked by the registry.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    /// Unique numeric identifier.
    pub id: u32,
    /// Human-readable name (e.g. "agent-1", "backend-dev").
    pub name: String,
    /// WezTerm pane ID, set once the pane is created.
    pub pane_id: Option<usize>,
    /// Current agent status.
    pub status: AgentStatus,
    /// LLM model this agent is using.
    pub model: String,
    /// What the agent is currently working on.
    pub current_task: Option<String>,
    /// Total tokens consumed by this agent.
    pub token_count: u64,
    /// Total cost in USD for this agent.
    pub cost_usd: f64,
    /// When this agent was created.
    pub created_at: DateTime<Utc>,
}

impl AgentInstance {
    /// How long this agent has been alive.
    pub fn uptime(&self) -> chrono::Duration {
        Utc::now() - self.created_at
    }

    /// Format uptime as a human-readable string (e.g. "2h 15m", "45s").
    pub fn uptime_display(&self) -> String {
        let dur = self.uptime();
        let total_secs = dur.num_seconds().max(0);
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = total_secs % 60;

        if hours > 0 {
            format!("{hours}h {minutes}m")
        } else if minutes > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{seconds}s")
        }
    }

    /// Format cost as a display string (e.g. "$0.0023").
    pub fn cost_display(&self) -> String {
        if self.cost_usd < 0.01 {
            format!("${:.4}", self.cost_usd)
        } else {
            format!("${:.2}", self.cost_usd)
        }
    }
}

// ─── Agent Registry ─────────────────────────────────────────────────────────

/// Central registry tracking all agent instances.
///
/// Thread-safe access should be provided by wrapping in `Arc<Mutex<AgentRegistry>>`.
#[derive(Debug)]
pub struct AgentRegistry {
    agents: HashMap<u32, AgentInstance>,
    next_id: u32,
}

impl AgentRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            next_id: 1,
        }
    }

    /// Register a new agent, returning its assigned ID.
    ///
    /// If `name` is `None`, a default name like "agent-1" is generated.
    pub fn register(&mut self, name: Option<&str>, model: &str) -> u32 {
        let id = self.next_id;
        self.next_id += 1;

        let name = match name {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => format!("agent-{id}"),
        };

        let instance = AgentInstance {
            id,
            name,
            pane_id: None,
            status: AgentStatus::Starting,
            model: model.to_string(),
            current_task: None,
            token_count: 0,
            cost_usd: 0.0,
            created_at: Utc::now(),
        };

        self.agents.insert(id, instance);
        id
    }

    /// Remove an agent from the registry.
    ///
    /// Returns the removed instance, or `None` if not found.
    pub fn unregister(&mut self, id: u32) -> Option<AgentInstance> {
        self.agents.remove(&id)
    }

    /// Get an agent by ID.
    pub fn get(&self, id: u32) -> Option<&AgentInstance> {
        self.agents.get(&id)
    }

    /// Get a mutable reference to an agent by ID.
    pub fn get_mut(&mut self, id: u32) -> Option<&mut AgentInstance> {
        self.agents.get_mut(&id)
    }

    /// Find an agent by name (case-insensitive).
    pub fn find_by_name(&self, name: &str) -> Option<&AgentInstance> {
        let lower = name.to_lowercase();
        self.agents
            .values()
            .find(|a| a.name.to_lowercase() == lower)
    }

    /// Resolve a name-or-id string to an agent ID.
    ///
    /// Tries parsing as `u32` first, then falls back to name lookup.
    pub fn resolve(&self, name_or_id: &str) -> Option<u32> {
        if let Ok(id) = name_or_id.parse::<u32>() {
            if self.agents.contains_key(&id) {
                return Some(id);
            }
        }
        self.find_by_name(name_or_id).map(|a| a.id)
    }

    /// List all agents (sorted by ID).
    pub fn list(&self) -> Vec<&AgentInstance> {
        let mut agents: Vec<&AgentInstance> = self.agents.values().collect();
        agents.sort_by_key(|a| a.id);
        agents
    }

    /// List only active agents (not Stopped).
    pub fn active(&self) -> Vec<&AgentInstance> {
        let mut agents: Vec<&AgentInstance> = self
            .agents
            .values()
            .filter(|a| a.status != AgentStatus::Stopped)
            .collect();
        agents.sort_by_key(|a| a.id);
        agents
    }

    /// Number of registered agents.
    pub fn count(&self) -> usize {
        self.agents.len()
    }

    /// Number of active (non-stopped) agents.
    pub fn active_count(&self) -> usize {
        self.agents
            .values()
            .filter(|a| a.status != AgentStatus::Stopped)
            .count()
    }

    /// Update the status of an agent.
    ///
    /// Returns `true` if the agent was found and updated.
    pub fn update_status(&mut self, id: u32, status: AgentStatus) -> bool {
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.status = status;
            true
        } else {
            false
        }
    }

    /// Set the pane ID for an agent (once the pane is created by Mux).
    pub fn set_pane_id(&mut self, agent_id: u32, pane_id: usize) -> bool {
        if let Some(agent) = self.agents.get_mut(&agent_id) {
            agent.pane_id = Some(pane_id);
            true
        } else {
            false
        }
    }

    /// Update token count and cost for an agent.
    pub fn update_cost(&mut self, id: u32, tokens: u64, cost_usd: f64) -> bool {
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.token_count += tokens;
            agent.cost_usd += cost_usd;
            true
        } else {
            false
        }
    }

    /// Set the current task description for an agent.
    pub fn set_task(&mut self, id: u32, task: Option<String>) -> bool {
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.current_task = task;
            true
        } else {
            false
        }
    }

    /// Total cost across all agents.
    pub fn total_cost(&self) -> f64 {
        self.agents.values().map(|a| a.cost_usd).sum()
    }

    /// Total tokens across all agents.
    pub fn total_tokens(&self) -> u64 {
        self.agents.values().map(|a| a.token_count).sum()
    }

    /// Check if a name is already taken.
    pub fn name_exists(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        self.agents
            .values()
            .any(|a| a.name.to_lowercase() == lower)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Dashboard Rendering ────────────────────────────────────────────────────

/// Maximum characters for task description in dashboard display.
const TASK_TRUNCATE_LEN: usize = 40;

/// Render the agent dashboard as ANSI-formatted text.
///
/// Produces a multi-line string suitable for display in a terminal pane,
/// showing all active agents with their status, model, task, tokens, cost,
/// and uptime.
pub fn render_dashboard(registry: &AgentRegistry) -> String {
    let agents = registry.list();
    if agents.is_empty() {
        return "\x1b[2mNo agents running. Use /agent spawn to create one.\x1b[0m\n".to_string();
    }

    let mut out = String::new();

    // Header
    out.push_str("\x1b[1;36m");
    out.push_str("  Agent Dashboard");
    out.push_str("\x1b[0m\n");
    out.push_str(&"\x1b[2m─\x1b[0m".repeat(60));
    out.push('\n');

    // Column headers
    out.push_str(&format!(
        "  \x1b[2m{:<4} {:<2} {:<14} {:<16} {:<10} {:<10} {}\x1b[0m\n",
        "ID", "", "Name", "Model", "Tokens", "Cost", "Uptime"
    ));

    for agent in &agents {
        let task_line = match &agent.current_task {
            Some(t) if !t.is_empty() => {
                let truncated = truncate_str(t, TASK_TRUNCATE_LEN);
                format!("  \x1b[2m   └─ {truncated}\x1b[0m\n")
            }
            _ => String::new(),
        };

        let tokens_display = format_tokens(agent.token_count);

        out.push_str(&format!(
            "  {:<4} {} {:<14} {:<16} {:<10} {:<10} {}\n",
            agent.id,
            agent.status.indicator(),
            agent.name,
            truncate_str(&agent.model, 15),
            tokens_display,
            agent.cost_display(),
            agent.uptime_display(),
        ));

        if !task_line.is_empty() {
            out.push_str(&task_line);
        }
    }

    // Footer: totals
    out.push_str(&"\x1b[2m─\x1b[0m".repeat(60));
    out.push('\n');

    let total_active = registry.active_count();
    let total_cost = registry.total_cost();
    let total_tokens = registry.total_tokens();

    let cost_str = if total_cost < 0.01 {
        format!("${:.4}", total_cost)
    } else {
        format!("${:.2}", total_cost)
    };

    out.push_str(&format!(
        "  \x1b[1m{} active\x1b[0m  |  {} tokens  |  {} total\n",
        total_active,
        format_tokens(total_tokens),
        cost_str,
    ));

    out
}

/// Render a compact status-bar fragment showing agent count.
///
/// Returns something like "3 agents" or empty if only one or zero agents.
pub fn status_bar_fragment(registry: &AgentRegistry) -> Option<String> {
    let count = registry.active_count();
    if count >= 2 {
        Some(format!("{count} agents"))
    } else {
        None
    }
}

// ─── Command Parsing ────────────────────────────────────────────────────────

/// Parsed result of an agent-related slash command.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentCommand {
    /// Spawn a new agent: `/agent spawn [name] [--model name]`.
    Spawn {
        name: Option<String>,
        model: Option<String>,
    },
    /// List agents: `/agent list` or `/agents`.
    List,
    /// Kill an agent: `/agent kill <name|id>`.
    Kill { target: String },
    /// Focus an agent's pane: `/agent focus <name|id>`.
    Focus { target: String },
    /// Send a message to an agent: `/tell <name> <message>`.
    Tell { target: String, message: String },
    /// Show help for agent commands.
    Help,
}

/// Parse the arguments to `/agent <subcommand> [args...]`.
///
/// Returns `None` for unrecognized subcommands.
pub fn parse_agent_command(args: &str) -> Option<AgentCommand> {
    let args = args.trim();
    if args.is_empty() {
        return Some(AgentCommand::Help);
    }

    let (subcmd, rest) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "spawn" | "new" | "create" => Some(parse_spawn_args(rest)),
        "list" | "ls" => Some(AgentCommand::List),
        "kill" | "stop" | "rm" => {
            if rest.is_empty() {
                Some(AgentCommand::Help)
            } else {
                Some(AgentCommand::Kill {
                    target: rest.to_string(),
                })
            }
        }
        "focus" | "switch" => {
            if rest.is_empty() {
                Some(AgentCommand::Help)
            } else {
                Some(AgentCommand::Focus {
                    target: rest.to_string(),
                })
            }
        }
        "help" => Some(AgentCommand::Help),
        _ => None,
    }
}

/// Parse `/tell <target> <message>`.
///
/// Returns `None` if the input doesn't have both a target and a message.
pub fn parse_tell_command(args: &str) -> Option<AgentCommand> {
    let args = args.trim();
    let (target, message) = args.split_once(char::is_whitespace)?;
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    Some(AgentCommand::Tell {
        target: target.to_string(),
        message: message.to_string(),
    })
}

/// Parse spawn arguments: `[name] [--model model_name]`.
fn parse_spawn_args(args: &str) -> AgentCommand {
    let args = args.trim();
    if args.is_empty() {
        return AgentCommand::Spawn {
            name: None,
            model: None,
        };
    }

    let mut name: Option<String> = None;
    let mut model: Option<String> = None;
    let mut tokens = args.split_whitespace().peekable();

    while let Some(tok) = tokens.next() {
        if tok == "--model" || tok == "-m" {
            if let Some(model_val) = tokens.next() {
                model = Some(model_val.to_string());
            }
        } else if name.is_none() {
            name = Some(tok.to_string());
        }
    }

    AgentCommand::Spawn { name, model }
}

/// Build the help text for agent commands.
pub fn agent_help_text() -> String {
    "\
Agent commands:\n\
\n\
  /agent spawn [name] [--model name]  Spawn a new agent pane\n\
  /agent list                         List all agents (or /agents)\n\
  /agent kill <name|id>               Terminate an agent\n\
  /agent focus <name|id>              Switch to agent's pane\n\
  /tell <name> <message>              Send message to an agent\n\
\n\
Examples:\n\
  /agent spawn backend --model gemini-2.5-flash\n\
  /agent spawn                        (auto-named, default model)\n\
  /tell backend write tests for auth\n\
  /agent kill backend"
        .to_string()
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    // Find a safe char boundary
    let mut end = max_len.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Format a token count with K/M suffixes.
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

// ─── Inter-Agent Messages ───────────────────────────────────────────────────

/// A message routed between agents.
#[derive(Debug, Clone)]
pub struct InterAgentMessage {
    /// Source agent ID.
    pub from_id: u32,
    /// Source agent name.
    pub from_name: String,
    /// Target agent ID.
    pub to_id: u32,
    /// The message content.
    pub content: String,
    /// When the message was sent.
    pub timestamp: DateTime<Utc>,
}

impl InterAgentMessage {
    /// Create a new inter-agent message.
    pub fn new(from_id: u32, from_name: &str, to_id: u32, content: &str) -> Self {
        Self {
            from_id,
            from_name: from_name.to_string(),
            to_id,
            content: content.to_string(),
            timestamp: Utc::now(),
        }
    }

    /// Format the message as a system-prompt injection for the receiving agent.
    pub fn as_system_prompt(&self) -> String {
        let ts = self
            .timestamp
            .with_timezone(&Local)
            .format("%H:%M:%S");
        format!(
            "[{ts}] Message from agent \"{}\": {}",
            self.from_name, self.content
        )
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Registry CRUD ───────────────────────────────────────────────────

    #[test]
    fn test_register_auto_name() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "gemini-2.5-pro");
        assert_eq!(id, 1);

        let agent = reg.get(id).unwrap();
        assert_eq!(agent.name, "agent-1");
        assert_eq!(agent.model, "gemini-2.5-pro");
        assert_eq!(agent.status, AgentStatus::Starting);
        assert_eq!(agent.token_count, 0);
        assert_eq!(agent.cost_usd, 0.0);
        assert!(agent.pane_id.is_none());
        assert!(agent.current_task.is_none());
    }

    #[test]
    fn test_register_custom_name() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some("backend"), "gemini-2.5-flash");
        let agent = reg.get(id).unwrap();
        assert_eq!(agent.name, "backend");
        assert_eq!(agent.model, "gemini-2.5-flash");
    }

    #[test]
    fn test_register_empty_name_gets_auto() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some(""), "model");
        let agent = reg.get(id).unwrap();
        assert_eq!(agent.name, "agent-1");
    }

    #[test]
    fn test_register_increments_ids() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        let id3 = reg.register(None, "m3");
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_unregister() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some("test"), "m1");
        assert_eq!(reg.count(), 1);

        let removed = reg.unregister(id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "test");
        assert_eq!(reg.count(), 0);
        assert!(reg.get(id).is_none());
    }

    #[test]
    fn test_unregister_nonexistent() {
        let mut reg = AgentRegistry::new();
        assert!(reg.unregister(99).is_none());
    }

    #[test]
    fn test_get_mut() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some("test"), "m1");
        reg.get_mut(id).unwrap().name = "renamed".to_string();
        assert_eq!(reg.get(id).unwrap().name, "renamed");
    }

    // ── Name lookup ─────────────────────────────────────────────────────

    #[test]
    fn test_find_by_name() {
        let mut reg = AgentRegistry::new();
        reg.register(Some("backend"), "m1");
        reg.register(Some("frontend"), "m2");

        assert!(reg.find_by_name("backend").is_some());
        assert_eq!(reg.find_by_name("backend").unwrap().name, "backend");
        assert!(reg.find_by_name("BACKEND").is_some()); // case-insensitive
        assert!(reg.find_by_name("missing").is_none());
    }

    #[test]
    fn test_resolve_by_id() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some("test"), "m1");
        assert_eq!(reg.resolve("1"), Some(id));
    }

    #[test]
    fn test_resolve_by_name() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(Some("backend"), "m1");
        assert_eq!(reg.resolve("backend"), Some(id));
    }

    #[test]
    fn test_resolve_nonexistent() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.resolve("99"), None);
        assert_eq!(reg.resolve("missing"), None);
    }

    #[test]
    fn test_name_exists() {
        let mut reg = AgentRegistry::new();
        reg.register(Some("backend"), "m1");
        assert!(reg.name_exists("backend"));
        assert!(reg.name_exists("Backend")); // case-insensitive
        assert!(!reg.name_exists("frontend"));
    }

    // ── Listing ─────────────────────────────────────────────────────────

    #[test]
    fn test_list_sorted_by_id() {
        let mut reg = AgentRegistry::new();
        reg.register(Some("charlie"), "m1");
        reg.register(Some("alpha"), "m1");
        reg.register(Some("bravo"), "m1");

        let list = reg.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].name, "charlie"); // id=1
        assert_eq!(list[1].name, "alpha");   // id=2
        assert_eq!(list[2].name, "bravo");   // id=3
    }

    #[test]
    fn test_active_filters_stopped() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(Some("a1"), "m1");
        let id2 = reg.register(Some("a2"), "m1");
        reg.update_status(id1, AgentStatus::Idle);
        reg.update_status(id2, AgentStatus::Stopped);

        let active = reg.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "a1");
    }

    #[test]
    fn test_active_count() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m1");
        let id3 = reg.register(None, "m1");
        reg.update_status(id1, AgentStatus::Idle);
        reg.update_status(id2, AgentStatus::Working);
        reg.update_status(id3, AgentStatus::Stopped);

        assert_eq!(reg.count(), 3);
        assert_eq!(reg.active_count(), 2);
    }

    // ── Status updates ──────────────────────────────────────────────────

    #[test]
    fn test_update_status() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");
        assert_eq!(reg.get(id).unwrap().status, AgentStatus::Starting);

        assert!(reg.update_status(id, AgentStatus::Idle));
        assert_eq!(reg.get(id).unwrap().status, AgentStatus::Idle);

        assert!(reg.update_status(id, AgentStatus::Working));
        assert_eq!(reg.get(id).unwrap().status, AgentStatus::Working);

        assert!(reg.update_status(id, AgentStatus::Error));
        assert_eq!(reg.get(id).unwrap().status, AgentStatus::Error);
    }

    #[test]
    fn test_update_status_nonexistent() {
        let mut reg = AgentRegistry::new();
        assert!(!reg.update_status(99, AgentStatus::Idle));
    }

    #[test]
    fn test_set_pane_id() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");
        assert!(reg.get(id).unwrap().pane_id.is_none());

        assert!(reg.set_pane_id(id, 42));
        assert_eq!(reg.get(id).unwrap().pane_id, Some(42));
    }

    #[test]
    fn test_set_pane_id_nonexistent() {
        let mut reg = AgentRegistry::new();
        assert!(!reg.set_pane_id(99, 42));
    }

    // ── Cost tracking ───────────────────────────────────────────────────

    #[test]
    fn test_update_cost() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");

        assert!(reg.update_cost(id, 1000, 0.05));
        assert_eq!(reg.get(id).unwrap().token_count, 1000);
        assert!((reg.get(id).unwrap().cost_usd - 0.05).abs() < f64::EPSILON);

        assert!(reg.update_cost(id, 500, 0.02));
        assert_eq!(reg.get(id).unwrap().token_count, 1500);
        assert!((reg.get(id).unwrap().cost_usd - 0.07).abs() < f64::EPSILON);
    }

    #[test]
    fn test_update_cost_nonexistent() {
        let mut reg = AgentRegistry::new();
        assert!(!reg.update_cost(99, 100, 0.01));
    }

    #[test]
    fn test_total_cost() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        reg.update_cost(id1, 1000, 0.05);
        reg.update_cost(id2, 2000, 0.10);

        assert!((reg.total_cost() - 0.15).abs() < f64::EPSILON);
    }

    #[test]
    fn test_total_tokens() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        reg.update_cost(id1, 1000, 0.05);
        reg.update_cost(id2, 2000, 0.10);

        assert_eq!(reg.total_tokens(), 3000);
    }

    #[test]
    fn test_total_cost_empty() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.total_cost(), 0.0);
        assert_eq!(reg.total_tokens(), 0);
    }

    // ── Task tracking ───────────────────────────────────────────────────

    #[test]
    fn test_set_task() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");

        assert!(reg.set_task(id, Some("writing tests".to_string())));
        assert_eq!(
            reg.get(id).unwrap().current_task.as_deref(),
            Some("writing tests")
        );

        assert!(reg.set_task(id, None));
        assert!(reg.get(id).unwrap().current_task.is_none());
    }

    #[test]
    fn test_set_task_nonexistent() {
        let mut reg = AgentRegistry::new();
        assert!(!reg.set_task(99, Some("task".to_string())));
    }

    // ── Display helpers ─────────────────────────────────────────────────

    #[test]
    fn test_status_labels() {
        assert_eq!(AgentStatus::Idle.label(), "idle");
        assert_eq!(AgentStatus::Working.label(), "working");
        assert_eq!(AgentStatus::Error.label(), "error");
        assert_eq!(AgentStatus::Starting.label(), "starting");
        assert_eq!(AgentStatus::Stopped.label(), "stopped");
    }

    #[test]
    fn test_status_display() {
        assert_eq!(format!("{}", AgentStatus::Idle), "idle");
        assert_eq!(format!("{}", AgentStatus::Working), "working");
    }

    #[test]
    fn test_status_indicators_contain_ansi() {
        // Each indicator should contain escape codes
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Error,
            AgentStatus::Starting,
            AgentStatus::Stopped,
        ] {
            assert!(
                status.indicator().contains("\x1b["),
                "{status:?} indicator missing ANSI"
            );
        }
    }

    #[test]
    fn test_cost_display() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");

        // Small cost: 4 decimal places
        reg.update_cost(id, 100, 0.0023);
        assert_eq!(reg.get(id).unwrap().cost_display(), "$0.0023");

        // Reset and set larger cost
        reg.get_mut(id).unwrap().cost_usd = 1.50;
        assert_eq!(reg.get(id).unwrap().cost_display(), "$1.50");
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5K");
        assert_eq!(format_tokens(150_000), "150.0K");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("", 5), "");
    }

    #[test]
    fn test_truncate_str_multibyte() {
        // Ensure we don't panic on multi-byte chars
        let s = "hello\u{1F600}world"; // emoji in the middle
        let result = truncate_str(s, 8);
        assert!(result.ends_with("..."));
    }

    // ── Dashboard rendering ─────────────────────────────────────────────

    #[test]
    fn test_dashboard_empty() {
        let reg = AgentRegistry::new();
        let output = render_dashboard(&reg);
        assert!(output.contains("No agents running"));
        assert!(output.contains("/agent spawn"));
    }

    #[test]
    fn test_dashboard_with_agents() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(Some("backend"), "gemini-2.5-pro");
        let id2 = reg.register(Some("frontend"), "gemini-2.5-flash");

        reg.update_status(id1, AgentStatus::Working);
        reg.update_status(id2, AgentStatus::Idle);
        reg.set_task(id1, Some("implementing auth module".to_string()));
        reg.update_cost(id1, 5000, 0.12);
        reg.update_cost(id2, 2000, 0.04);

        let output = render_dashboard(&reg);
        assert!(output.contains("Agent Dashboard"));
        assert!(output.contains("backend"));
        assert!(output.contains("frontend"));
        assert!(output.contains("gemini-2.5-pro"));
        assert!(output.contains("implementing auth module"));
        assert!(output.contains("2 active"));
    }

    #[test]
    fn test_dashboard_totals() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        reg.update_status(id1, AgentStatus::Idle);
        reg.update_status(id2, AgentStatus::Idle);
        reg.update_cost(id1, 10000, 0.50);
        reg.update_cost(id2, 20000, 1.00);

        let output = render_dashboard(&reg);
        assert!(output.contains("2 active"));
        assert!(output.contains("30.0K"));
        assert!(output.contains("$1.50"));
    }

    // ── Status bar fragment ─────────────────────────────────────────────

    #[test]
    fn test_status_bar_single_agent() {
        let mut reg = AgentRegistry::new();
        reg.register(None, "m1");
        reg.update_status(1, AgentStatus::Idle);
        assert!(status_bar_fragment(&reg).is_none());
    }

    #[test]
    fn test_status_bar_multiple_agents() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        let id3 = reg.register(None, "m3");
        reg.update_status(id1, AgentStatus::Idle);
        reg.update_status(id2, AgentStatus::Working);
        reg.update_status(id3, AgentStatus::Idle);

        let fragment = status_bar_fragment(&reg).unwrap();
        assert_eq!(fragment, "3 agents");
    }

    #[test]
    fn test_status_bar_with_stopped() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register(None, "m1");
        let id2 = reg.register(None, "m2");
        let id3 = reg.register(None, "m3");
        reg.update_status(id1, AgentStatus::Idle);
        reg.update_status(id2, AgentStatus::Idle);
        reg.update_status(id3, AgentStatus::Stopped);

        let fragment = status_bar_fragment(&reg).unwrap();
        assert_eq!(fragment, "2 agents");
    }

    // ── Command parsing ─────────────────────────────────────────────────

    #[test]
    fn test_parse_agent_empty() {
        assert_eq!(parse_agent_command(""), Some(AgentCommand::Help));
    }

    #[test]
    fn test_parse_agent_spawn_no_args() {
        assert_eq!(
            parse_agent_command("spawn"),
            Some(AgentCommand::Spawn {
                name: None,
                model: None,
            })
        );
    }

    #[test]
    fn test_parse_agent_spawn_with_name() {
        assert_eq!(
            parse_agent_command("spawn backend"),
            Some(AgentCommand::Spawn {
                name: Some("backend".to_string()),
                model: None,
            })
        );
    }

    #[test]
    fn test_parse_agent_spawn_with_model() {
        assert_eq!(
            parse_agent_command("spawn --model gemini-2.5-flash"),
            Some(AgentCommand::Spawn {
                name: None,
                model: Some("gemini-2.5-flash".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_agent_spawn_with_name_and_model() {
        assert_eq!(
            parse_agent_command("spawn backend --model gemini-2.5-flash"),
            Some(AgentCommand::Spawn {
                name: Some("backend".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_agent_spawn_short_model_flag() {
        assert_eq!(
            parse_agent_command("spawn -m gemini-2.5-flash"),
            Some(AgentCommand::Spawn {
                name: None,
                model: Some("gemini-2.5-flash".to_string()),
            })
        );
    }

    #[test]
    fn test_parse_agent_spawn_aliases() {
        // "new" and "create" should also work
        assert_eq!(
            parse_agent_command("new backend"),
            Some(AgentCommand::Spawn {
                name: Some("backend".to_string()),
                model: None,
            })
        );
        assert_eq!(
            parse_agent_command("create backend"),
            Some(AgentCommand::Spawn {
                name: Some("backend".to_string()),
                model: None,
            })
        );
    }

    #[test]
    fn test_parse_agent_list() {
        assert_eq!(parse_agent_command("list"), Some(AgentCommand::List));
        assert_eq!(parse_agent_command("ls"), Some(AgentCommand::List));
    }

    #[test]
    fn test_parse_agent_kill() {
        assert_eq!(
            parse_agent_command("kill backend"),
            Some(AgentCommand::Kill {
                target: "backend".to_string(),
            })
        );
        assert_eq!(
            parse_agent_command("stop 1"),
            Some(AgentCommand::Kill {
                target: "1".to_string(),
            })
        );
        assert_eq!(
            parse_agent_command("rm backend"),
            Some(AgentCommand::Kill {
                target: "backend".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_agent_kill_no_target() {
        assert_eq!(parse_agent_command("kill"), Some(AgentCommand::Help));
    }

    #[test]
    fn test_parse_agent_focus() {
        assert_eq!(
            parse_agent_command("focus backend"),
            Some(AgentCommand::Focus {
                target: "backend".to_string(),
            })
        );
        assert_eq!(
            parse_agent_command("switch 2"),
            Some(AgentCommand::Focus {
                target: "2".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_agent_focus_no_target() {
        assert_eq!(parse_agent_command("focus"), Some(AgentCommand::Help));
    }

    #[test]
    fn test_parse_agent_help() {
        assert_eq!(parse_agent_command("help"), Some(AgentCommand::Help));
    }

    #[test]
    fn test_parse_agent_unknown() {
        assert_eq!(parse_agent_command("foobar"), None);
    }

    // ── Tell command parsing ────────────────────────────────────────────

    #[test]
    fn test_parse_tell() {
        assert_eq!(
            parse_tell_command("backend write tests for auth"),
            Some(AgentCommand::Tell {
                target: "backend".to_string(),
                message: "write tests for auth".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_tell_no_message() {
        assert_eq!(parse_tell_command("backend"), None);
    }

    #[test]
    fn test_parse_tell_empty() {
        assert_eq!(parse_tell_command(""), None);
    }

    #[test]
    fn test_parse_tell_whitespace_message() {
        assert_eq!(parse_tell_command("backend   "), None);
    }

    // ── Inter-agent messages ────────────────────────────────────────────

    #[test]
    fn test_inter_agent_message() {
        let msg = InterAgentMessage::new(1, "backend", 2, "auth module is ready");
        assert_eq!(msg.from_id, 1);
        assert_eq!(msg.from_name, "backend");
        assert_eq!(msg.to_id, 2);
        assert_eq!(msg.content, "auth module is ready");
    }

    #[test]
    fn test_inter_agent_message_system_prompt() {
        let msg = InterAgentMessage::new(1, "backend", 2, "tests are passing");
        let prompt = msg.as_system_prompt();
        assert!(prompt.contains("backend"));
        assert!(prompt.contains("tests are passing"));
        assert!(prompt.contains("Message from agent"));
    }

    // ── Agent help text ─────────────────────────────────────────────────

    #[test]
    fn test_agent_help_text() {
        let help = agent_help_text();
        assert!(help.contains("/agent spawn"));
        assert!(help.contains("/agent list"));
        assert!(help.contains("/agent kill"));
        assert!(help.contains("/agent focus"));
        assert!(help.contains("/tell"));
    }

    // ── Default trait ───────────────────────────────────────────────────

    #[test]
    fn test_registry_default() {
        let reg = AgentRegistry::default();
        assert_eq!(reg.count(), 0);
        assert_eq!(reg.total_cost(), 0.0);
    }

    // ── Uptime display ──────────────────────────────────────────────────

    #[test]
    fn test_uptime_display_format() {
        let mut reg = AgentRegistry::new();
        let id = reg.register(None, "m1");
        // Just created, so uptime should be very short
        let uptime = reg.get(id).unwrap().uptime_display();
        assert!(
            uptime.ends_with('s'),
            "Expected uptime to end with 's', got: {uptime}"
        );
    }
}
