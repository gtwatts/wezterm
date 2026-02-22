//! ANSI formatter for agent output.
//!
//! Converts `AgentResponse` variants into ANSI escape sequences that can be
//! written to the virtual terminal inside `ElwoodPane`. WezTerm's existing
//! rendering pipeline handles all the rich text display.

use crate::runtime::AgentResponse;

/// ANSI color codes for consistent styling.
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const ITALIC: &str = "\x1b[3m";
    pub const CYAN: &str = "\x1b[36m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const BLUE: &str = "\x1b[34m";
    pub const GRAY: &str = "\x1b[90m";
}

/// Format an `AgentResponse` as ANSI-escaped text for the virtual terminal.
pub fn format_response(response: &AgentResponse) -> String {
    match response {
        AgentResponse::ContentDelta(text) => {
            // Content is streamed directly — no prefix needed
            text.clone()
        }

        AgentResponse::ToolStart {
            tool_name,
            tool_id: _,
            input_preview,
        } => {
            format!(
                "\r\n{}{}{} {}{}{}\r\n",
                ansi::YELLOW,
                ansi::BOLD,
                tool_name,
                ansi::RESET,
                ansi::DIM,
                truncate(input_preview, 120),
            )
        }

        AgentResponse::ToolEnd {
            tool_id: _,
            success,
            output_preview,
        } => {
            let (icon, color) = if *success {
                ("OK", ansi::GREEN)
            } else {
                ("FAIL", ansi::RED)
            };
            format!(
                "{}{}[{}]{} {}{}\r\n",
                color,
                ansi::BOLD,
                icon,
                ansi::RESET,
                truncate(output_preview, 200),
                ansi::RESET,
            )
        }

        AgentResponse::PermissionRequest {
            request_id: _,
            tool_name,
            description,
        } => {
            format!(
                concat!(
                    "\r\n",
                    "{}{}",
                    "╭─ Permission Required ──────────────────────────────╮{}\r\n",
                    "{}│{} {}{}{} {}\r\n",
                    "{}│{} {}\r\n",
                    "{}{}",
                    "╰────────────────── [y] approve  [n] deny ──────────╯{}\r\n",
                ),
                ansi::BLUE, ansi::BOLD, ansi::RESET,
                ansi::BLUE, ansi::RESET, ansi::YELLOW, ansi::BOLD, tool_name, ansi::RESET,
                ansi::BLUE, ansi::RESET, description,
                ansi::BLUE, ansi::BOLD, ansi::RESET,
            )
        }

        AgentResponse::TurnComplete { summary } => {
            let suffix = summary
                .as_deref()
                .map(|s| format!(" — {}", truncate(s, 80)))
                .unwrap_or_default();
            format!(
                "\r\n{}{}Done{}{}\r\n\r\n{}elwood>{} ",
                ansi::GREEN,
                ansi::BOLD,
                ansi::RESET,
                suffix,
                ansi::CYAN,
                ansi::RESET,
            )
        }

        AgentResponse::Error(msg) => {
            format!(
                "\r\n{}{}Error:{} {}\r\n",
                ansi::RED,
                ansi::BOLD,
                ansi::RESET,
                msg,
            )
        }

        AgentResponse::Shutdown => {
            format!(
                "\r\n{}{}Agent session ended.{}\r\n",
                ansi::GRAY,
                ansi::ITALIC,
                ansi::RESET,
            )
        }
    }
}

/// Format the initial prompt display.
pub fn format_prompt_banner() -> String {
    format!(
        "{}{}Elwood Agent{} — terminal-native AI coding assistant\r\n\r\n{}elwood>{} ",
        ansi::CYAN,
        ansi::BOLD,
        ansi::RESET,
        ansi::CYAN,
        ansi::RESET,
    )
}

/// Format a permission approval message.
pub fn format_permission_granted(tool_name: &str) -> String {
    format!(
        "{}{}Approved:{} {}\r\n",
        ansi::GREEN, ansi::BOLD, ansi::RESET, tool_name,
    )
}

/// Format a permission denial message.
pub fn format_permission_denied(tool_name: &str) -> String {
    format!(
        "{}{}Denied:{} {}\r\n",
        ansi::RED, ansi::BOLD, ansi::RESET, tool_name,
    )
}

/// Format a status line showing agent state.
pub fn format_status(model: &str, state: &str, tokens: Option<usize>) -> String {
    let token_str = tokens
        .map(|t| format!(" | {}tok", t))
        .unwrap_or_default();
    format!(
        "{}{}{} | {}{}{}",
        ansi::DIM, model, ansi::RESET,
        ansi::DIM, state, token_str,
    )
}

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a char boundary
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_content_delta() {
        let resp = AgentResponse::ContentDelta("hello world".into());
        assert_eq!(format_response(&resp), "hello world");
    }

    #[test]
    fn test_format_tool_start() {
        let resp = AgentResponse::ToolStart {
            tool_name: "ReadFile".into(),
            tool_id: "t1".into(),
            input_preview: "/src/main.rs".into(),
        };
        let output = format_response(&resp);
        assert!(output.contains("ReadFile"));
        assert!(output.contains("/src/main.rs"));
    }

    #[test]
    fn test_truncate_multibyte() {
        // Should not panic on multibyte chars
        let s = "hello 世界 world";
        let t = truncate(s, 8);
        assert!(t.len() <= 8);
        assert!(t.is_char_boundary(t.len()));
    }
}
