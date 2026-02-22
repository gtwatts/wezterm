//! Slash command system for the Elwood pane.
//!
//! When the user types `/` at the start of input in Agent mode, the input is
//! routed here instead of being sent to the agent. Commands are parsed and
//! executed, returning a [`CommandResult`] that the pane renders.
//!
//! ## Supported Commands
//!
//! | Command   | Description                              |
//! |-----------|------------------------------------------|
//! | `/help`   | Show available commands                  |
//! | `/clear`  | Clear chat history                       |
//! | `/model`  | Show current model info                  |
//! | `/export` | Export session (md/html/json/share)     |
//! | `/import` | Import a session file                   |
//! | `/compact`| Summarize conversation history           |
//! | `/plan`   | Start plan mode                          |
//! | `/diff`   | Show git diff of working directory       |
//! | `/git`    | Git operations (status, stage, commit)  |

use crate::runtime::AgentRequest;

/// Metadata for a slash command (for help display and completion).
#[derive(Debug, Clone)]
pub struct SlashCommand {
    /// Command name without the leading `/`.
    pub name: &'static str,
    /// Short description shown in help and completion menu.
    pub description: &'static str,
    /// Usage example (e.g. `/export [path]`).
    pub usage: &'static str,
}

/// Result of executing a slash command.
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// Display a message in the chat area (informational output).
    ChatMessage(String),
    /// Send a request to the agent (e.g. /compact, /plan).
    AgentRequest(AgentRequest),
    /// Clear the chat history (scroll buffer).
    ClearChat,
    /// Export the session to the given path.
    ExportSession(String),
    /// Export session in a specific format.
    ExportFormatted {
        /// Target file path.
        path: String,
        /// Format: "md", "html", "json", or "share".
        format: String,
    },
    /// Import a session from a file path.
    ImportSession {
        /// Path to the session file.
        path: String,
    },
    /// Open the interactive diff viewer.
    OpenDiffViewer { staged: bool },
    /// Open the interactive staging view (`/git stage`).
    OpenStagingView,
    /// Open the commit flow (`/git commit`).
    OpenCommitFlow,
    /// Push to remote (`/git push`).
    GitPush,
    /// Show git log (`/git log`).
    GitLog { count: usize },
    /// Show git status (`/git status`).
    GitStatus,
    /// List sibling terminal panes (`/panes`).
    ListPanes,
    /// Switch to a named model (`/model <name>`).
    SwitchModel { model_name: String },
    /// List saved plans (`/plan list`).
    ListPlans,
    /// Resume a saved plan by ID prefix (`/plan resume <id>`).
    ResumePlan { id_prefix: String },
    /// Unknown or invalid command.
    Unknown(String),
}

/// Return the list of all available slash commands.
pub fn get_commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand {
            name: "help",
            description: "Show available commands",
            usage: "/help",
        },
        SlashCommand {
            name: "clear",
            description: "Clear chat history",
            usage: "/clear",
        },
        SlashCommand {
            name: "model",
            description: "Show/switch model (list, <name>)",
            usage: "/model [list|<name>]",
        },
        SlashCommand {
            name: "export",
            description: "Export session (md/html/json/share)",
            usage: "/export [md|html|json|share] [path]",
        },
        SlashCommand {
            name: "import",
            description: "Import a session file",
            usage: "/import <path>",
        },
        SlashCommand {
            name: "compact",
            description: "Summarize conversation history",
            usage: "/compact",
        },
        SlashCommand {
            name: "plan",
            description: "Create/manage implementation plans",
            usage: "/plan [list|resume <id>|<description>]",
        },
        SlashCommand {
            name: "diff",
            description: "Show interactive diff viewer",
            usage: "/diff [--staged]",
        },
        SlashCommand {
            name: "git",
            description: "Git operations (status/diff/stage/commit/push/log)",
            usage: "/git <subcommand>",
        },
        SlashCommand {
            name: "panes",
            description: "List sibling terminal panes and their content",
            usage: "/panes",
        },
    ]
}

/// Parse a raw input string into (command_name, args).
///
/// Returns `None` if the input doesn't start with `/` or is just `/`.
pub fn parse_command(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') || trimmed.len() < 2 {
        return None;
    }
    let without_slash = &trimmed[1..];
    match without_slash.split_once(char::is_whitespace) {
        Some((cmd, args)) => Some((cmd, args.trim())),
        None => Some((without_slash, "")),
    }
}

/// Return commands whose name starts with the given prefix (for completion).
pub fn complete_command(prefix: &str) -> Vec<&'static SlashCommand> {
    static COMMANDS: std::sync::OnceLock<Vec<SlashCommand>> = std::sync::OnceLock::new();
    let commands = COMMANDS.get_or_init(get_commands);
    commands
        .iter()
        .filter(|c| c.name.starts_with(prefix))
        .collect()
}

/// Execute a parsed slash command, returning the result.
pub fn execute_command(name: &str, args: &str, model_name: &str) -> CommandResult {
    match name {
        "help" => execute_help(),
        "clear" => CommandResult::ClearChat,
        "model" => execute_model(args, model_name),
        "export" => execute_export(args),
        "import" => execute_import(args),
        "compact" => execute_compact(),
        "plan" => execute_plan(args),
        "diff" => execute_diff(args),
        "git" => execute_git(args),
        "panes" => CommandResult::ListPanes,
        _ => CommandResult::Unknown(name.to_string()),
    }
}

/// `/help` — build a formatted help message listing all commands.
fn execute_help() -> CommandResult {
    let commands = get_commands();
    let mut msg = String::from("Available commands:\n\n");
    for cmd in &commands {
        msg.push_str(&format!(
            "  {:<12} {}\n",
            format!("/{}", cmd.name),
            cmd.description,
        ));
    }
    msg.push_str("\nKeyboard shortcuts:\n\n");
    msg.push_str("  Ctrl+T       Toggle Agent/Terminal mode\n");
    msg.push_str("  Ctrl+F       Quick-fix last error\n");
    msg.push_str("  Ctrl+Up/Down Navigate blocks\n");
    msg.push_str("  Shift+Enter  New line in input\n");
    msg.push_str("  !command     Run shell command (Agent mode)\n");
    msg.push_str("  @file        Attach file context to prompt\n");
    CommandResult::ChatMessage(msg)
}

/// `/model [list|<name>]` — show current model, list models, or switch.
fn execute_model(args: &str, model_name: &str) -> CommandResult {
    let args = args.trim();
    if args.is_empty() {
        // No args: show current model
        let display = if model_name.is_empty() {
            "No model configured".to_string()
        } else {
            format!("Current model: {model_name}")
        };
        CommandResult::ChatMessage(display)
    } else if args == "list" {
        // `/model list` — show all models (rendered by the pane from the model router)
        CommandResult::ChatMessage("__MODEL_LIST__".to_string())
    } else {
        // `/model <name>` — switch to named model
        CommandResult::SwitchModel {
            model_name: args.to_string(),
        }
    }
}

/// `/export [format] [path]` — export chat session in various formats.
///
/// Formats: `md` (default), `html`, `json`, `share` (encrypted).
fn execute_export(args: &str) -> CommandResult {
    let (format, rest) = match args.split_once(char::is_whitespace) {
        Some((f, r)) => (f.trim(), r.trim()),
        None => (args.trim(), ""),
    };

    match format {
        "html" | "json" | "share" => {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let ext = match format {
                "share" => "elwood-session",
                other => other,
            };
            let path = if rest.is_empty() {
                format!("elwood_chat_{timestamp}.{ext}")
            } else {
                rest.to_string()
            };
            CommandResult::ExportFormatted {
                path,
                format: format.to_string(),
            }
        }
        "md" => {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let path = if rest.is_empty() {
                format!("elwood_chat_{timestamp}.md")
            } else {
                rest.to_string()
            };
            CommandResult::ExportSession(path)
        }
        "" => {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            CommandResult::ExportSession(format!("elwood_chat_{timestamp}.md"))
        }
        _ => {
            // Treat as a path (backward compat with `/export /tmp/file.md`)
            CommandResult::ExportSession(args.to_string())
        }
    }
}

/// `/import <path>` — import a session file.
fn execute_import(args: &str) -> CommandResult {
    let path = args.trim();
    if path.is_empty() {
        return CommandResult::ChatMessage(
            "Usage: /import <path>\n\nSupported formats: .json, .elwood-session".to_string(),
        );
    }
    CommandResult::ImportSession {
        path: path.to_string(),
    }
}

/// `/compact` — ask the agent to summarize conversation history.
fn execute_compact() -> CommandResult {
    CommandResult::AgentRequest(AgentRequest::SendMessage {
        content: "Please provide a concise summary of our conversation so far, \
                  highlighting the key decisions, changes made, and current state."
            .to_string(),
    })
}

/// `/plan [list|resume <id>|<description>]` — plan mode with subcommands.
fn execute_plan(args: &str) -> CommandResult {
    let (subcmd, sub_args) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "list" => CommandResult::ListPlans,
        "resume" => {
            if sub_args.is_empty() {
                CommandResult::ChatMessage(
                    "Usage: /plan resume <id-prefix>\nUse /plan list to see saved plans."
                        .to_string(),
                )
            } else {
                CommandResult::ResumePlan {
                    id_prefix: sub_args.to_string(),
                }
            }
        }
        "" => {
            // No args: generate plan for current task
            CommandResult::AgentRequest(AgentRequest::GeneratePlan {
                description: "Analyze the current state and create a step-by-step plan \
                              for completing the task at hand."
                    .to_string(),
            })
        }
        _ => {
            // Treat entire args string as the plan description
            CommandResult::AgentRequest(AgentRequest::GeneratePlan {
                description: args.to_string(),
            })
        }
    }
}

/// `/diff [--staged]` — open the interactive diff viewer.
fn execute_diff(args: &str) -> CommandResult {
    let staged = args.contains("--staged");
    CommandResult::OpenDiffViewer { staged }
}

/// `/git <subcommand>` — git operations.
fn execute_git(args: &str) -> CommandResult {
    let (subcmd, sub_args) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "status" | "st" => CommandResult::GitStatus,
        "diff" => {
            let staged = sub_args.contains("--staged") || sub_args.contains("--cached");
            CommandResult::OpenDiffViewer { staged }
        }
        "stage" | "add" => CommandResult::OpenStagingView,
        "commit" | "ci" => CommandResult::OpenCommitFlow,
        "push" => CommandResult::GitPush,
        "log" => {
            let count = sub_args.parse::<usize>().unwrap_or(10);
            CommandResult::GitLog { count }
        }
        "" => {
            let help = "\
/git status    Show detailed file status\n\
/git diff      Open interactive diff viewer\n\
/git stage     Interactive file staging\n\
/git commit    Stage + AI commit message + commit\n\
/git push      Push to remote\n\
/git log [N]   Show recent N commits (default 10)";
            CommandResult::ChatMessage(help.to_string())
        }
        other => CommandResult::ChatMessage(format!(
            "Unknown git subcommand: {other}\nType /git for available subcommands."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_basic() {
        assert_eq!(parse_command("/help"), Some(("help", "")));
        assert_eq!(parse_command("/model"), Some(("model", "")));
    }

    #[test]
    fn test_parse_command_with_args() {
        assert_eq!(
            parse_command("/export /tmp/chat.md"),
            Some(("export", "/tmp/chat.md"))
        );
        assert_eq!(
            parse_command("/plan build a REST API"),
            Some(("plan", "build a REST API"))
        );
    }

    #[test]
    fn test_parse_command_empty_or_invalid() {
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("/"), None);
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("  /help  "), Some(("help", "")));
    }

    #[test]
    fn test_execute_help() {
        let result = execute_command("help", "", "");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("/help"));
                assert!(msg.contains("/clear"));
                assert!(msg.contains("/model"));
                assert!(msg.contains("/diff"));
                assert!(msg.contains("Ctrl+T"));
                assert!(msg.contains("@file"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_clear() {
        let result = execute_command("clear", "", "");
        assert!(matches!(result, CommandResult::ClearChat));
    }

    #[test]
    fn test_execute_model_empty() {
        let result = execute_command("model", "", "");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("No model configured"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_model_with_name() {
        let result = execute_command("model", "", "gemini-2.5-pro");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("gemini-2.5-pro"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_model_list() {
        let result = execute_command("model", "list", "gemini-2.5-pro");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert_eq!(msg, "__MODEL_LIST__");
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_model_switch() {
        let result = execute_command("model", "claude-sonnet-4-6", "");
        match result {
            CommandResult::SwitchModel { model_name } => {
                assert_eq!(model_name, "claude-sonnet-4-6");
            }
            other => panic!("expected SwitchModel, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_default_path() {
        let result = execute_command("export", "", "");
        match result {
            CommandResult::ExportSession(path) => {
                assert!(path.starts_with("elwood_chat_"));
                assert!(path.ends_with(".md"));
            }
            other => panic!("expected ExportSession, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_custom_path() {
        let result = execute_command("export", "/tmp/my_chat.md", "");
        match result {
            CommandResult::ExportSession(path) => {
                assert_eq!(path, "/tmp/my_chat.md");
            }
            other => panic!("expected ExportSession, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_compact() {
        let result = execute_command("compact", "", "");
        assert!(matches!(result, CommandResult::AgentRequest(_)));
    }

    #[test]
    fn test_execute_plan_no_args() {
        let result = execute_command("plan", "", "");
        match result {
            CommandResult::AgentRequest(AgentRequest::GeneratePlan { description }) => {
                assert!(description.contains("step-by-step plan"));
            }
            other => panic!("expected AgentRequest::GeneratePlan, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_plan_with_args() {
        let result = execute_command("plan", "build a REST API", "");
        match result {
            CommandResult::AgentRequest(AgentRequest::GeneratePlan { description }) => {
                assert!(description.contains("build a REST API"));
            }
            other => panic!("expected AgentRequest::GeneratePlan, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_plan_list() {
        let result = execute_command("plan", "list", "");
        assert!(matches!(result, CommandResult::ListPlans));
    }

    #[test]
    fn test_execute_plan_resume() {
        let result = execute_command("plan", "resume 20260222", "");
        match result {
            CommandResult::ResumePlan { id_prefix } => {
                assert_eq!(id_prefix, "20260222");
            }
            other => panic!("expected ResumePlan, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_plan_resume_no_id() {
        let result = execute_command("plan", "resume", "");
        assert!(matches!(result, CommandResult::ChatMessage(_)));
    }

    #[test]
    fn test_execute_unknown_command() {
        let result = execute_command("foobar", "", "");
        match result {
            CommandResult::Unknown(name) => assert_eq!(name, "foobar"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn test_complete_command() {
        let matches = complete_command("he");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "help");

        let matches = complete_command("cl");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "clear");

        let matches = complete_command("");
        assert_eq!(matches.len(), 10); // all commands
    }

    #[test]
    fn test_complete_command_no_match() {
        let matches = complete_command("zzz");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_get_commands_has_all() {
        let commands = get_commands();
        let names: Vec<&str> = commands.iter().map(|c| c.name).collect();
        assert!(names.contains(&"help"));
        assert!(names.contains(&"clear"));
        assert!(names.contains(&"model"));
        assert!(names.contains(&"export"));
        assert!(names.contains(&"compact"));
        assert!(names.contains(&"plan"));
        assert!(names.contains(&"diff"));
        assert!(names.contains(&"git"));
    }

    #[test]
    fn test_execute_diff() {
        let result = execute_command("diff", "", "");
        assert!(matches!(result, CommandResult::OpenDiffViewer { staged: false }));
    }

    #[test]
    fn test_execute_diff_staged() {
        let result = execute_command("diff", "--staged", "");
        assert!(matches!(result, CommandResult::OpenDiffViewer { staged: true }));
    }

    #[test]
    fn test_execute_git_status() {
        let result = execute_command("git", "status", "");
        assert!(matches!(result, CommandResult::GitStatus));
    }

    #[test]
    fn test_execute_git_status_alias() {
        let result = execute_command("git", "st", "");
        assert!(matches!(result, CommandResult::GitStatus));
    }

    #[test]
    fn test_execute_git_stage() {
        let result = execute_command("git", "stage", "");
        assert!(matches!(result, CommandResult::OpenStagingView));
    }

    #[test]
    fn test_execute_git_add_alias() {
        let result = execute_command("git", "add", "");
        assert!(matches!(result, CommandResult::OpenStagingView));
    }

    #[test]
    fn test_execute_git_commit() {
        let result = execute_command("git", "commit", "");
        assert!(matches!(result, CommandResult::OpenCommitFlow));
    }

    #[test]
    fn test_execute_git_push() {
        let result = execute_command("git", "push", "");
        assert!(matches!(result, CommandResult::GitPush));
    }

    #[test]
    fn test_execute_git_log_default() {
        let result = execute_command("git", "log", "");
        assert!(matches!(result, CommandResult::GitLog { count: 10 }));
    }

    #[test]
    fn test_execute_git_log_with_count() {
        let result = execute_command("git", "log 5", "");
        assert!(matches!(result, CommandResult::GitLog { count: 5 }));
    }

    #[test]
    fn test_execute_git_diff() {
        let result = execute_command("git", "diff", "");
        assert!(matches!(result, CommandResult::OpenDiffViewer { staged: false }));
    }

    #[test]
    fn test_execute_git_diff_staged() {
        let result = execute_command("git", "diff --staged", "");
        assert!(matches!(result, CommandResult::OpenDiffViewer { staged: true }));
    }

    #[test]
    fn test_execute_git_no_subcommand() {
        let result = execute_command("git", "", "");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("/git status"));
                assert!(msg.contains("/git commit"));
                assert!(msg.contains("/git push"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_git_unknown_subcommand() {
        let result = execute_command("git", "rebase", "");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Unknown git subcommand: rebase"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_html() {
        let result = execute_command("export", "html", "");
        match result {
            CommandResult::ExportFormatted { path, format } => {
                assert!(path.ends_with(".html"));
                assert_eq!(format, "html");
            }
            other => panic!("expected ExportFormatted, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_json() {
        let result = execute_command("export", "json", "");
        match result {
            CommandResult::ExportFormatted { path, format } => {
                assert!(path.ends_with(".json"));
                assert_eq!(format, "json");
            }
            other => panic!("expected ExportFormatted, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_share() {
        let result = execute_command("export", "share", "");
        match result {
            CommandResult::ExportFormatted { path, format } => {
                assert!(path.ends_with(".elwood-session"));
                assert_eq!(format, "share");
            }
            other => panic!("expected ExportFormatted, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_html_with_path() {
        let result = execute_command("export", "html /tmp/session.html", "");
        match result {
            CommandResult::ExportFormatted { path, format } => {
                assert_eq!(path, "/tmp/session.html");
                assert_eq!(format, "html");
            }
            other => panic!("expected ExportFormatted, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_export_md_explicit() {
        let result = execute_command("export", "md", "");
        match result {
            CommandResult::ExportSession(path) => {
                assert!(path.ends_with(".md"));
            }
            other => panic!("expected ExportSession, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_import_with_path() {
        let result = execute_command("import", "/tmp/session.json", "");
        match result {
            CommandResult::ImportSession { path } => {
                assert_eq!(path, "/tmp/session.json");
            }
            other => panic!("expected ImportSession, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_import_no_args() {
        let result = execute_command("import", "", "");
        match result {
            CommandResult::ChatMessage(msg) => {
                assert!(msg.contains("Usage: /import"));
            }
            other => panic!("expected ChatMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_get_commands_has_import() {
        let commands = get_commands();
        let names: Vec<&str> = commands.iter().map(|c| c.name).collect();
        assert!(names.contains(&"import"));
    }
}
