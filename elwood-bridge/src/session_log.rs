//! Session logging and markdown export for Elwood terminal sessions.
//!
//! Tracks all chat messages, commands, tool uses, and agent responses during
//! a session, then exports them as a readable markdown document.

use std::path::{Path, PathBuf};

/// Type of session log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryType {
    /// User message sent to the agent.
    User,
    /// Agent response text.
    Agent,
    /// Shell command executed.
    Command,
    /// Command output (stdout/stderr).
    CommandOutput,
    /// Tool execution (start or end).
    Tool,
    /// System message (errors, status, etc.).
    System,
}

/// A single entry in the session log.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Type of this entry.
    pub entry_type: EntryType,
    /// Content of the entry.
    pub content: String,
}

/// Accumulated log of a terminal session.
#[derive(Debug, Clone)]
pub struct SessionLog {
    /// All entries in chronological order.
    pub entries: Vec<SessionEntry>,
    /// When the session started (ISO 8601).
    pub started_at: String,
    /// Working directory at session start.
    pub working_dir: PathBuf,
}

impl SessionLog {
    /// Create a new empty session log.
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            entries: Vec::new(),
            started_at: now_iso8601(),
            working_dir,
        }
    }

    /// Add an entry to the log.
    pub fn add(&mut self, entry_type: EntryType, content: impl Into<String>) {
        self.entries.push(SessionEntry {
            timestamp: now_iso8601(),
            entry_type,
            content: content.into(),
        });
    }

    /// Add a user message.
    pub fn log_user(&mut self, content: &str) {
        self.add(EntryType::User, content);
    }

    /// Add agent response content.
    pub fn log_agent(&mut self, content: &str) {
        self.add(EntryType::Agent, content);
    }

    /// Add a command execution.
    pub fn log_command(&mut self, command: &str) {
        self.add(EntryType::Command, command);
    }

    /// Add command output.
    pub fn log_command_output(&mut self, stdout: &str, stderr: &str, exit_code: Option<i32>) {
        let mut content = String::new();
        if !stdout.is_empty() {
            content.push_str(stdout);
        }
        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("stderr: ");
            content.push_str(stderr);
        }
        let code = exit_code.unwrap_or(-1);
        content.push_str(&format!("\nexit code: {code}"));
        self.add(EntryType::CommandOutput, content);
    }

    /// Add a tool event.
    pub fn log_tool(&mut self, tool_name: &str, detail: &str) {
        self.add(EntryType::Tool, format!("{tool_name}: {detail}"));
    }

    /// Add a system message.
    pub fn log_system(&mut self, message: &str) {
        self.add(EntryType::System, message);
    }

    /// Export the session as a markdown document.
    pub fn export_markdown(&self) -> String {
        let mut md = String::with_capacity(4096);

        md.push_str(&format!("# Elwood Session — {}\n\n", self.started_at));
        md.push_str(&format!(
            "**Working directory:** `{}`\n\n",
            self.working_dir.display()
        ));
        md.push_str("---\n\n");

        for entry in &self.entries {
            match entry.entry_type {
                EntryType::User => {
                    md.push_str(&format!("### You ({})\n\n", entry.timestamp));
                    md.push_str(&entry.content);
                    md.push_str("\n\n");
                }
                EntryType::Agent => {
                    md.push_str(&format!("### Elwood ({})\n\n", entry.timestamp));
                    md.push_str(&entry.content);
                    md.push_str("\n\n");
                }
                EntryType::Command => {
                    md.push_str(&format!("#### Command ({})\n\n", entry.timestamp));
                    md.push_str(&format!("```bash\n$ {}\n```\n\n", entry.content));
                }
                EntryType::CommandOutput => {
                    md.push_str("```\n");
                    md.push_str(&entry.content);
                    md.push_str("\n```\n\n");
                }
                EntryType::Tool => {
                    md.push_str(&format!(
                        "> **Tool** ({}): {}\n\n",
                        entry.timestamp, entry.content
                    ));
                }
                EntryType::System => {
                    md.push_str(&format!(
                        "> _System ({}): {}_\n\n",
                        entry.timestamp, entry.content
                    ));
                }
            }
        }

        md
    }

    /// Export the session to a file.
    ///
    /// Writes to `~/.elwood/sessions/elwood-{timestamp}.md`.
    /// Creates the directory if it doesn't exist.
    /// Returns the path of the written file.
    pub fn export_to_file(&self) -> std::io::Result<PathBuf> {
        let dir = default_session_dir();
        std::fs::create_dir_all(&dir)?;

        let filename = format!(
            "elwood-{}.md",
            self.started_at.replace(':', "-").replace(' ', "_")
        );
        let path = dir.join(filename);

        let markdown = self.export_markdown();
        std::fs::write(&path, markdown)?;

        Ok(path)
    }
}

/// Default directory for session exports.
fn default_session_dir() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".elwood")
        .join("sessions")
}

/// Resolve the export directory, accepting an optional override.
pub fn session_export_dir(override_dir: Option<&Path>) -> PathBuf {
    override_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_session_dir)
}

/// Current time in ISO 8601 format.
fn now_iso8601() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_log_new() {
        let log = SessionLog::new(PathBuf::from("/tmp/project"));
        assert!(log.entries.is_empty());
        assert_eq!(log.working_dir, PathBuf::from("/tmp/project"));
        assert!(!log.started_at.is_empty());
    }

    #[test]
    fn test_add_entries() {
        let mut log = SessionLog::new(PathBuf::from("/tmp"));
        log.log_user("Hello agent");
        log.log_agent("Hi! How can I help?");
        log.log_command("ls -la");
        log.log_command_output("file.txt\n", "", Some(0));
        log.log_tool("ReadFile", "src/main.rs");
        log.log_system("Session started");

        assert_eq!(log.entries.len(), 6);
        assert_eq!(log.entries[0].entry_type, EntryType::User);
        assert_eq!(log.entries[1].entry_type, EntryType::Agent);
        assert_eq!(log.entries[2].entry_type, EntryType::Command);
        assert_eq!(log.entries[3].entry_type, EntryType::CommandOutput);
        assert_eq!(log.entries[4].entry_type, EntryType::Tool);
        assert_eq!(log.entries[5].entry_type, EntryType::System);
    }

    #[test]
    fn test_export_markdown_structure() {
        let mut log = SessionLog::new(PathBuf::from("/home/user/project"));
        log.log_user("Fix the bug in main.rs");
        log.log_agent("I'll look at the file and fix the issue.");
        log.log_tool("ReadFile", "src/main.rs (200 lines)");
        log.log_command("cargo test");
        log.log_command_output("test result: ok. 5 passed\n", "", Some(0));

        let md = log.export_markdown();

        // Check header
        assert!(md.contains("# Elwood Session"));
        assert!(md.contains("**Working directory:** `/home/user/project`"));

        // Check user message
        assert!(md.contains("### You"));
        assert!(md.contains("Fix the bug in main.rs"));

        // Check agent message
        assert!(md.contains("### Elwood"));
        assert!(md.contains("look at the file"));

        // Check tool
        assert!(md.contains("> **Tool**"));
        assert!(md.contains("ReadFile"));

        // Check command
        assert!(md.contains("```bash\n$ cargo test\n```"));

        // Check command output
        assert!(md.contains("test result: ok"));
    }

    #[test]
    fn test_export_markdown_empty_session() {
        let log = SessionLog::new(PathBuf::from("/tmp"));
        let md = log.export_markdown();
        assert!(md.contains("# Elwood Session"));
        assert!(md.contains("---"));
    }

    #[test]
    fn test_command_output_with_stderr() {
        let mut log = SessionLog::new(PathBuf::from("/tmp"));
        log.log_command_output("", "error: file not found", Some(1));

        let entry = &log.entries[0];
        assert!(entry.content.contains("stderr: error: file not found"));
        assert!(entry.content.contains("exit code: 1"));
    }

    #[test]
    fn test_command_output_combined() {
        let mut log = SessionLog::new(PathBuf::from("/tmp"));
        log.log_command_output("output line", "warning line", Some(0));

        let entry = &log.entries[0];
        assert!(entry.content.contains("output line"));
        assert!(entry.content.contains("stderr: warning line"));
        assert!(entry.content.contains("exit code: 0"));
    }

    #[test]
    fn test_session_export_dir_default() {
        let dir = session_export_dir(None);
        assert!(dir.to_string_lossy().contains(".elwood"));
        assert!(dir.to_string_lossy().contains("sessions"));
    }

    #[test]
    fn test_session_export_dir_override() {
        let dir = session_export_dir(Some(Path::new("/custom/path")));
        assert_eq!(dir, PathBuf::from("/custom/path"));
    }
}
