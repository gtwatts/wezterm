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
//! | `/export` | Export chat as markdown                  |
//! | `/compact`| Summarize conversation history           |
//! | `/plan`   | Start plan mode                          |
//! | `/diff`   | Show git diff of working directory       |

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
    /// Open the interactive diff viewer.
    OpenDiffViewer { staged: bool },
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
            description: "Show current model info",
            usage: "/model",
        },
        SlashCommand {
            name: "export",
            description: "Export chat as markdown",
            usage: "/export [path]",
        },
        SlashCommand {
            name: "compact",
            description: "Summarize conversation history",
            usage: "/compact",
        },
        SlashCommand {
            name: "plan",
            description: "Start plan mode",
            usage: "/plan [goal]",
        },
        SlashCommand {
            name: "diff",
            description: "Show interactive diff viewer",
            usage: "/diff [--staged]",
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
        "model" => execute_model(model_name),
        "export" => execute_export(args),
        "compact" => execute_compact(),
        "plan" => execute_plan(args),
        "diff" => execute_diff(args),
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

/// `/model` — display current model info.
fn execute_model(model_name: &str) -> CommandResult {
    let display = if model_name.is_empty() {
        "No model configured".to_string()
    } else {
        format!("Current model: {model_name}")
    };
    CommandResult::ChatMessage(display)
}

/// `/export [path]` — export chat session.
fn execute_export(args: &str) -> CommandResult {
    let path = if args.is_empty() {
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        format!("elwood_chat_{timestamp}.md")
    } else {
        args.to_string()
    };
    CommandResult::ExportSession(path)
}

/// `/compact` — ask the agent to summarize conversation history.
fn execute_compact() -> CommandResult {
    CommandResult::AgentRequest(AgentRequest::SendMessage {
        content: "Please provide a concise summary of our conversation so far, \
                  highlighting the key decisions, changes made, and current state."
            .to_string(),
    })
}

/// `/plan [goal]` — start plan mode (send planning request to agent).
fn execute_plan(args: &str) -> CommandResult {
    let prompt = if args.is_empty() {
        "Please analyze the current state and create a step-by-step plan \
         for completing the task at hand."
            .to_string()
    } else {
        format!(
            "Please create a step-by-step plan for the following goal:\n\n{args}"
        )
    };
    CommandResult::AgentRequest(AgentRequest::SendMessage { content: prompt })
}

/// `/diff [--staged]` — open the interactive diff viewer.
fn execute_diff(args: &str) -> CommandResult {
    let staged = args.contains("--staged");
    CommandResult::OpenDiffViewer { staged }
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
            CommandResult::AgentRequest(AgentRequest::SendMessage { content }) => {
                assert!(content.contains("step-by-step plan"));
            }
            other => panic!("expected AgentRequest::SendMessage, got {other:?}"),
        }
    }

    #[test]
    fn test_execute_plan_with_args() {
        let result = execute_command("plan", "build a REST API", "");
        match result {
            CommandResult::AgentRequest(AgentRequest::SendMessage { content }) => {
                assert!(content.contains("build a REST API"));
            }
            other => panic!("expected AgentRequest::SendMessage, got {other:?}"),
        }
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
        assert_eq!(matches.len(), 7); // all commands
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
}
