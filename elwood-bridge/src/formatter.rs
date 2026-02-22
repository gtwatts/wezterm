//! ANSI formatter for agent output — Elwood Pro visual identity (legacy).
//!
//! Superseded by `screen.rs` which provides a full-screen TUI layout.
//! Kept for reference and its test suite.
#![allow(dead_code)]

use crate::runtime::AgentResponse;

// ─── TokyoNight Color Palette (24-bit true color) ────────────────────────

/// True-color ANSI escape helpers using Elwood's TokyoNight palette.
#[allow(dead_code)]
mod tc {
    /// Set foreground to RGB.
    pub fn fg(r: u8, g: u8, b: u8) -> String {
        format!("\x1b[38;2;{r};{g};{b}m")
    }

    /// Set background to RGB.
    pub fn bg(r: u8, g: u8, b: u8) -> String {
        format!("\x1b[48;2;{r};{g};{b}m")
    }

    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const ITALIC: &str = "\x1b[3m";
    pub const UNDERLINE: &str = "\x1b[4m";

    // TokyoNight palette
    pub const BG: (u8, u8, u8) = (26, 27, 38);       // #1a1b26
    pub const FG: (u8, u8, u8) = (192, 202, 245);     // #c0caf5
    pub const ACCENT: (u8, u8, u8) = (122, 162, 247);  // #7aa2f7
    pub const SUCCESS: (u8, u8, u8) = (158, 206, 106); // #9ece6a
    pub const ERROR: (u8, u8, u8) = (247, 118, 142);   // #f7768e
    pub const WARNING: (u8, u8, u8) = (224, 175, 104);  // #e0af68
    pub const INFO: (u8, u8, u8) = (125, 207, 255);    // #7dcfff
    pub const MUTED: (u8, u8, u8) = (86, 95, 137);     // #565f89
    pub const BORDER: (u8, u8, u8) = (59, 66, 97);     // #3b4261
    pub const CODE_BG: (u8, u8, u8) = (36, 40, 59);    // #24283b
    pub const MAGENTA: (u8, u8, u8) = (187, 154, 247);  // #bb9af7
    pub const CYAN: (u8, u8, u8) = (125, 207, 255);    // #7dcfff
    pub const GREEN: (u8, u8, u8) = (158, 206, 106);   // #9ece6a
    pub const YELLOW: (u8, u8, u8) = (224, 175, 104);   // #e0af68
    pub const RED: (u8, u8, u8) = (247, 118, 142);     // #f7768e
    pub const WHITE: (u8, u8, u8) = (220, 225, 252);   // #dce1fc

    /// Convenience: foreground from palette tuple.
    pub fn fgp(c: (u8, u8, u8)) -> String {
        fg(c.0, c.1, c.2)
    }

    /// Convenience: background from palette tuple.
    pub fn bgp(c: (u8, u8, u8)) -> String {
        bg(c.0, c.1, c.2)
    }
}

// ─── Box Drawing Characters ──────────────────────────────────────────────

const BOX_TL: char = '╭';  // top-left rounded
const BOX_TR: char = '╮';  // top-right rounded
const BOX_BL: char = '╰';  // bottom-left rounded
const BOX_BR: char = '╯';  // bottom-right rounded
const BOX_H: char = '─';   // horizontal
const BOX_V: char = '│';   // vertical
const DOUBLE_H: char = '═'; // double horizontal (for completion)
const CHECK: &str = "✔";
const GEAR: &str = "⚙";
const CROSS: &str = "✗";
#[allow(dead_code)]
const WARN: &str = "⚠";
const ARROW: &str = "▸";   // right-pointing small triangle
#[allow(dead_code)]
const THINK_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ─── Formatting Functions ────────────────────────────────────────────────

/// Format an `AgentResponse` as rich ANSI-escaped text for the virtual terminal.
pub fn format_response(response: &AgentResponse) -> String {
    match response {
        AgentResponse::ContentDelta(text) => {
            // Stream content with foreground color
            format!("{}{}{}", tc::fgp(tc::FG), text, tc::RESET)
        }

        AgentResponse::ToolStart {
            tool_name,
            tool_id: _,
            input_preview,
        } => format_tool_start(tool_name, input_preview),

        AgentResponse::ToolEnd {
            tool_id: _,
            success,
            output_preview,
        } => format_tool_end(*success, output_preview),

        AgentResponse::PermissionRequest {
            request_id: _,
            tool_name,
            description,
        } => format_permission_request(tool_name, description),

        AgentResponse::TurnComplete { summary } => format_turn_complete(summary.as_deref()),

        AgentResponse::CommandOutput {
            command,
            stdout,
            stderr,
            exit_code,
        } => crate::screen::format_command_output(command, stdout, stderr, *exit_code),

        AgentResponse::Error(msg) => format_error(msg),

        AgentResponse::Shutdown => format_shutdown(),
    }
}

/// Render a styled tool-start block with box drawing.
///
/// ```text
/// ╭─ ⚙ ReadFile ─────────────────────────────╮
/// │  /src/main.rs                              │
/// ```
fn format_tool_start(tool_name: &str, input_preview: &str) -> String {
    let border = tc::fgp(tc::BORDER);
    let warn = tc::fgp(tc::WARNING);
    let muted = tc::fgp(tc::MUTED);
    let r = tc::RESET;

    // Top border with tool name
    let title = format!(" {GEAR} {tool_name} ");
    let fill_len = 52usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "\r\n{border}{BOX_TL}{BOX_H}{r}{warn}{}{title}{r}{border}{fill}{BOX_TR}{r}\r\n",
        tc::BOLD,
    ));

    // Input preview line
    let preview = truncate(input_preview, 48);
    if !preview.is_empty() {
        out.push_str(&format!(
            "{border}{BOX_V}{r}  {muted}{preview}{r}\r\n",
        ));
    }

    out
}

/// Render a tool-end block with status.
///
/// ```text
/// │  ✔ OK (output preview...)                  │
/// ╰─────────────────────────────────────────────╯
/// ```
fn format_tool_end(success: bool, output_preview: &str) -> String {
    let border = tc::fgp(tc::BORDER);
    let r = tc::RESET;

    let (icon, color) = if success {
        (CHECK, tc::fgp(tc::SUCCESS))
    } else {
        (CROSS, tc::fgp(tc::ERROR))
    };

    let status = if success { "OK" } else { "FAIL" };
    let preview = truncate(output_preview, 44);

    let mut out = String::new();
    out.push_str(&format!(
        "{border}{BOX_V}{r}  {color}{}{icon} {status}{r}",
        tc::BOLD,
    ));
    if !preview.is_empty() {
        let muted = tc::fgp(tc::MUTED);
        out.push_str(&format!(" {muted}{preview}{r}"));
    }
    out.push_str("\r\n");

    // Bottom border
    let fill: String = std::iter::repeat(BOX_H).take(52).collect();
    out.push_str(&format!("{border}{BOX_BL}{fill}{BOX_BR}{r}\r\n"));

    out
}

/// Render a permission request with a prominent box.
fn format_permission_request(tool_name: &str, description: &str) -> String {
    let border = tc::fgp(tc::ACCENT);
    let warn = tc::fgp(tc::WARNING);
    let muted = tc::fgp(tc::MUTED);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    let title = " Permission Required ";
    let title_fill: String = std::iter::repeat(BOX_H).take(52 - title.len() - 2).collect();

    let desc_trunc = truncate(description, 48);

    let key_style = format!("{}{}", tc::bgp(tc::ACCENT), tc::fg(tc::BG.0, tc::BG.1, tc::BG.2));

    format!(
        concat!(
            "\r\n",
            "{border}{b}{tl}{h} {warn}{b}{title}{r}{border}{b}{fill}{tr}{r}\r\n",
            "{border}{v}{r}  {warn}{b}{tool}{r}\r\n",
            "{border}{v}{r}  {fg}{desc}{r}\r\n",
            "{border}{v}{r}\r\n",
            "{border}{v}{r}  {ks} y {r} {muted}approve   {ks} n {r} {muted}deny{r}\r\n",
            "{border}{b}{bl}{bottom}{br}{r}\r\n",
        ),
        border = border,
        b = b,
        tl = BOX_TL,
        tr = BOX_TR,
        bl = BOX_BL,
        br = BOX_BR,
        h = BOX_H,
        v = BOX_V,
        warn = warn,
        muted = muted,
        fg = fg,
        r = r,
        title = title,
        fill = title_fill,
        tool = tool_name,
        desc = desc_trunc,
        ks = key_style,
        bottom = std::iter::repeat(BOX_H).take(52).collect::<String>(),
    )
}

/// Render a turn completion banner.
///
/// ```text
/// ══════════════════════════════════
/// ✔ Task Complete — Completed in 3 steps (2 tool calls)
/// ══════════════════════════════════
///
/// elwood ▸
/// ```
fn format_turn_complete(summary: Option<&str>) -> String {
    let success = tc::fgp(tc::SUCCESS);
    let muted = tc::fgp(tc::MUTED);
    let accent = tc::fgp(tc::ACCENT);
    let r = tc::RESET;
    let b = tc::BOLD;

    let separator: String = std::iter::repeat(DOUBLE_H).take(40).collect();

    let suffix = summary
        .map(|s| format!(" {muted}{ARROW} {}{r}", truncate(s, 60)))
        .unwrap_or_default();

    format!(
        "\r\n{muted}{sep}{r}\r\n{success}{b}{CHECK} Done{r}{suffix}\r\n{muted}{sep}{r}\r\n\r\n{accent}{b}elwood{r} {muted}{ARROW}{r} ",
        sep = separator,
    )
}

/// Render an error message.
fn format_error(msg: &str) -> String {
    let err = tc::fgp(tc::ERROR);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("\r\n{err}{b}{CROSS} Error:{r} {fg}{msg}{r}\r\n")
}

/// Render a shutdown message.
fn format_shutdown() -> String {
    let muted = tc::fgp(tc::MUTED);
    let r = tc::RESET;

    format!("\r\n{muted}{}Agent session ended.{r}\r\n", tc::ITALIC)
}

// ─── Public Helpers ──────────────────────────────────────────────────────

/// Format the initial prompt banner displayed when the pane opens.
///
/// ```text
/// ╭──────────────────────────────────────────────╮
/// │  Elwood Agent — terminal-native AI assistant │
/// │  Model: gemini-2.5-pro · Provider: gemini    │
/// ╰──────────────────────────────────────────────╯
///
/// elwood ▸
/// ```
pub fn format_prompt_banner() -> String {
    let border = tc::fgp(tc::BORDER);
    let accent = tc::fgp(tc::ACCENT);
    let muted = tc::fgp(tc::MUTED);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    let w = 50;
    let top: String = std::iter::repeat(BOX_H).take(w).collect();
    let bot: String = std::iter::repeat(BOX_H).take(w).collect();

    let title = "Elwood Agent";
    let subtitle = "terminal-native AI coding assistant";

    format!(
        concat!(
            "{border}{tl}{top}{tr}{r}\r\n",
            "{border}{v}{r}  {accent}{b}{title}{r} {muted}{ARROW}{r} {fg}{sub}{r}\r\n",
            "{border}{bl}{bot}{br}{r}\r\n",
            "\r\n",
            "{accent}{b}elwood{r} {muted}{ARROW}{r} ",
        ),
        border = border,
        tl = BOX_TL,
        tr = BOX_TR,
        bl = BOX_BL,
        br = BOX_BR,
        v = BOX_V,
        top = top,
        bot = bot,
        accent = accent,
        b = b,
        r = r,
        muted = muted,
        fg = fg,
        title = title,
        sub = subtitle,
        ARROW = ARROW,
    )
}

/// Format the user's submitted prompt (echo with styling).
pub fn format_user_prompt(text: &str) -> String {
    let accent = tc::fgp(tc::ACCENT);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("\r\n{accent}{b}You{r}  {fg}{text}{r}\r\n\r\n")
}

/// Format the "Elwood" prefix shown before agent content starts streaming.
pub fn format_assistant_prefix() -> String {
    let success = tc::fgp(tc::SUCCESS);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("{success}{b}Elwood{r}  ")
}

/// Format a permission approval message.
pub fn format_permission_granted(tool_name: &str) -> String {
    let success = tc::fgp(tc::SUCCESS);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("{success}{b}{CHECK} Approved:{r} {fg}{tool_name}{r}\r\n")
}

/// Format a permission denial message.
pub fn format_permission_denied(tool_name: &str) -> String {
    let err = tc::fgp(tc::ERROR);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("{err}{b}{CROSS} Denied:{r} {fg}{tool_name}{r}\r\n")
}

/// Format a status line showing agent state (for the bottom of the pane).
#[allow(dead_code)]
pub fn format_status(model: &str, state: &str, tokens: Option<usize>) -> String {
    let muted = tc::fgp(tc::MUTED);
    let info = tc::fgp(tc::INFO);
    let r = tc::RESET;

    let token_str = tokens
        .map(|t| {
            if t >= 1000 {
                format!(" {muted}\u{00b7}{r} {muted}{:.1}K tok{r}", t as f64 / 1000.0)
            } else {
                format!(" {muted}\u{00b7}{r} {muted}{t} tok{r}")
            }
        })
        .unwrap_or_default();

    format!(
        "{muted}{model}{r} {muted}\u{00b7}{r} {info}{state}{r}{token_str}",
    )
}

/// Format a thinking/reasoning indicator.
#[allow(dead_code)]
pub fn format_thinking(frame_idx: usize) -> String {
    let info = tc::fgp(tc::INFO);
    let muted = tc::fgp(tc::MUTED);
    let r = tc::RESET;

    let frame = THINK_FRAMES[frame_idx % THINK_FRAMES.len()];
    format!("{info}{frame}{r} {muted}thinking...{r}")
}

/// Format a warning message.
#[allow(dead_code)]
pub fn format_warning(msg: &str) -> String {
    let warn = tc::fgp(tc::WARNING);
    let fg = tc::fgp(tc::FG);
    let r = tc::RESET;
    let b = tc::BOLD;

    format!("{warn}{b}{WARN} Warning:{r} {fg}{msg}{r}\r\n")
}

// ─── Internal Helpers ────────────────────────────────────────────────────

fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
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
        let output = format_response(&resp);
        assert!(output.contains("hello world"));
        // Should include true-color codes
        assert!(output.contains("\x1b[38;2;"));
    }

    #[test]
    fn test_format_tool_start_has_box() {
        let resp = AgentResponse::ToolStart {
            tool_name: "ReadFile".into(),
            tool_id: "t1".into(),
            input_preview: "/src/main.rs".into(),
        };
        let output = format_response(&resp);
        assert!(output.contains("ReadFile"));
        assert!(output.contains("╭"));
        assert!(output.contains("⚙"));
    }

    #[test]
    fn test_format_tool_end_success() {
        let resp = AgentResponse::ToolEnd {
            tool_id: "t1".into(),
            success: true,
            output_preview: "200 lines".into(),
        };
        let output = format_response(&resp);
        assert!(output.contains("✔"));
        assert!(output.contains("OK"));
        assert!(output.contains("╯"));
    }

    #[test]
    fn test_format_tool_end_failure() {
        let resp = AgentResponse::ToolEnd {
            tool_id: "t1".into(),
            success: false,
            output_preview: "not found".into(),
        };
        let output = format_response(&resp);
        assert!(output.contains("✗"));
        assert!(output.contains("FAIL"));
    }

    #[test]
    fn test_format_turn_complete() {
        let resp = AgentResponse::TurnComplete {
            summary: Some("Completed in 3 steps".into()),
        };
        let output = format_response(&resp);
        assert!(output.contains("Done"));
        assert!(output.contains("═"));
        assert!(output.contains("elwood"));
        assert!(output.contains("▸"));
    }

    #[test]
    fn test_format_permission_request() {
        let resp = AgentResponse::PermissionRequest {
            request_id: "r1".into(),
            tool_name: "BashTool".into(),
            description: "rm -rf /tmp/test".into(),
        };
        let output = format_response(&resp);
        assert!(output.contains("Permission Required"));
        assert!(output.contains("BashTool"));
        assert!(output.contains("approve"));
        assert!(output.contains("deny"));
    }

    #[test]
    fn test_format_error() {
        let resp = AgentResponse::Error("connection lost".into());
        let output = format_response(&resp);
        assert!(output.contains("Error"));
        assert!(output.contains("connection lost"));
    }

    #[test]
    fn test_banner_has_box() {
        let banner = format_prompt_banner();
        assert!(banner.contains("╭"));
        assert!(banner.contains("╯"));
        assert!(banner.contains("Elwood Agent"));
        assert!(banner.contains("▸"));
    }

    #[test]
    fn test_truncate_multibyte() {
        let s = "hello 世界 world";
        let t = truncate(s, 8);
        assert!(t.len() <= 8);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn test_format_thinking() {
        let t = format_thinking(0);
        assert!(t.contains("⠋"));
        assert!(t.contains("thinking"));
    }
}
