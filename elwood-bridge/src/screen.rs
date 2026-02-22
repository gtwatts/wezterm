//! Full-screen layout renderer for the Elwood pane.
//!
//! Uses ANSI cursor positioning to create a proper TUI layout inside the
//! WezTerm virtual terminal — matching the visual hierarchy of elwood-cli's
//! ratatui-based compositor: header bar, chat area, input box, status bar.
//!
//! ## Layout
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐  Row 0
//! │  Elwood Pro / project    1:chat  2:tools   22:14 │  Header
//! ├──────────────────────────────────────────────────┤
//! │                                                  │
//! │  Elwood:  I will help you with...                │  Chat area
//! │  ⚙ ReadFile /src/main.rs                         │  (scrolling)
//! │  ✔ OK — 200 lines                               │
//! │                                                  │
//! ├──────────────────────────────────────────────────┤
//! │ ╭─ Message (Enter send, Esc cancel) ───────────╮ │  Row H-5
//! │ │ Type a message...                            │ │  Input area
//! │ │                                              │ │
//! │ ╰──────────────────────────────────────────────╯ │  Row H-2
//! │  Thinking · gemini-2.5-pro · 5.2K tok · 12s     │  Status bar
//! └──────────────────────────────────────────────────┘  Row H-1
//! ```

use crate::runtime::InputMode;
use std::time::Instant;

// ─── TokyoNight Color Palette ────────────────────────────────────────────

/// True-color foreground.
fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// True-color background.
fn bg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";

// Palette tuples: (R, G, B)
const BG: (u8, u8, u8) = (26, 27, 38);
const FG: (u8, u8, u8) = (192, 202, 245);
const ACCENT: (u8, u8, u8) = (122, 162, 247);
const SUCCESS: (u8, u8, u8) = (158, 206, 106);
const ERROR: (u8, u8, u8) = (247, 118, 142);
const WARNING: (u8, u8, u8) = (224, 175, 104);
const INFO: (u8, u8, u8) = (125, 207, 255);
const MUTED: (u8, u8, u8) = (86, 95, 137);
const BORDER: (u8, u8, u8) = (59, 66, 97);
const HEADER_BG: (u8, u8, u8) = (26, 27, 38);
const STATUS_BG: (u8, u8, u8) = (26, 27, 38);
const SELECTION: (u8, u8, u8) = (40, 44, 66);
const WHITE: (u8, u8, u8) = (220, 225, 252);

// Tab colors
const TAB_CHAT: (u8, u8, u8) = (122, 162, 247);   // blue
const TAB_TOOLS: (u8, u8, u8) = (224, 175, 104);   // yellow
const TAB_FILES: (u8, u8, u8) = (158, 206, 106);   // green
const TAB_AGENTS: (u8, u8, u8) = (187, 154, 247);   // magenta

fn fgc(c: (u8, u8, u8)) -> String { fg(c.0, c.1, c.2) }
fn bgc(c: (u8, u8, u8)) -> String { bg(c.0, c.1, c.2) }

/// Blend color 35% toward background.
fn muted_tab(tab: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        ((tab.0 as u16 * 35 + BG.0 as u16 * 65) / 100) as u8,
        ((tab.1 as u16 * 35 + BG.1 as u16 * 65) / 100) as u8,
        ((tab.2 as u16 * 35 + BG.2 as u16 * 65) / 100) as u8,
    )
}

// Box chars
const BOX_TL: char = '╭';
const BOX_TR: char = '╮';
const BOX_BL: char = '╰';
const BOX_BR: char = '╯';
const BOX_H: char = '─';
const BOX_V: char = '│';
const DOUBLE_H: char = '═';
const CHECK: &str = "✔";
const GEAR: &str = "⚙";
const CROSS: &str = "✗";
const ARROW: &str = "▸";

// ─── ANSI Cursor Control ────────────────────────────────────────────────

/// Move cursor to (row, col) — 1-based.
fn goto(row: u16, col: u16) -> String {
    format!("\x1b[{};{}H", row, col)
}

/// Clear to end of line.
const CLEAR_EOL: &str = "\x1b[K";

/// Set scrolling region (1-based, inclusive).
fn set_scroll_region(top: u16, bottom: u16) -> String {
    format!("\x1b[{};{}r", top, bottom)
}

/// Reset scrolling region to full screen.
#[allow(dead_code)]
fn reset_scroll_region() -> String {
    "\x1b[r".to_string()
}

/// Hide cursor.
const HIDE_CURSOR: &str = "\x1b[?25l";

/// Show cursor.
const SHOW_CURSOR: &str = "\x1b[?25h";

// ─── Screen State ───────────────────────────────────────────────────────

/// Tracks the state of the full-screen layout.
pub struct ScreenState {
    pub width: u16,
    pub height: u16,
    pub project_name: String,
    pub model_name: String,
    pub status: String,
    pub tokens_used: usize,
    pub context_used: usize,
    pub context_max: usize,
    pub cost: f64,
    pub active_tool: Option<String>,
    pub tool_start: Option<Instant>,
    pub task_start: Option<Instant>,
    pub task_elapsed_frozen: Option<u64>,
    pub input_text: String,
    /// Current input mode (Agent or Terminal).
    pub input_mode: InputMode,
    /// True when agent is running (for status display).
    pub is_running: bool,
    /// True when awaiting permission.
    pub awaiting_permission: bool,
}

impl Default for ScreenState {
    fn default() -> Self {
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "project".to_string());
        Self {
            width: 80,
            height: 24,
            project_name,
            model_name: String::new(),
            status: "Ready".to_string(),
            tokens_used: 0,
            context_used: 0,
            context_max: 0,
            cost: 0.0,
            active_tool: None,
            tool_start: None,
            task_start: None,
            task_elapsed_frozen: None,
            input_text: String::new(),
            input_mode: InputMode::default(),
            is_running: false,
            awaiting_permission: false,
        }
    }
}

impl ScreenState {
    /// Rows reserved for header (1) + input area (4) + status bar (1).
    pub fn chrome_height(&self) -> u16 {
        6
    }

    /// First row of the chat area (1-based).
    pub fn chat_top(&self) -> u16 {
        2 // Row 1 = header, row 2+ = chat
    }

    /// Last row of the chat area (1-based).
    pub fn chat_bottom(&self) -> u16 {
        self.height.saturating_sub(5) // Leave 4 for input + 1 for status
    }

    /// First row of the input area (1-based).
    pub fn input_top(&self) -> u16 {
        self.height.saturating_sub(4)
    }

    /// Status bar row (1-based).
    pub fn status_row(&self) -> u16 {
        self.height
    }
}

// ─── Rendering Functions ────────────────────────────────────────────────

/// Render the full initial screen (header + empty chat + input + status).
pub fn render_full_screen(state: &ScreenState) -> String {
    let mut out = String::with_capacity(4096);
    // Hide cursor during render
    out.push_str(HIDE_CURSOR);
    // Clear entire screen
    out.push_str("\x1b[2J");
    // Move cursor to home
    out.push_str(&goto(1, 1));
    // Draw fixed chrome at absolute positions
    out.push_str(&render_header(state));
    out.push_str(&render_input_box(state));
    out.push_str(&render_status_bar(state));
    // Write welcome text at absolute positions in the chat area
    // (before scroll region is set, so no interference)
    out.push_str(&render_welcome_at(state));
    // Now set scroll region so future content scrolls within the chat area
    out.push_str(&set_scroll_region(state.chat_top(), state.chat_bottom()));
    // Position cursor at the next available line in the chat area
    // (after the welcome text — about 8 lines down from chat_top)
    let cursor_row = state.chat_top() + 8;
    let cursor_row = cursor_row.min(state.chat_bottom());
    out.push_str(&goto(cursor_row, 1));
    out.push_str(SHOW_CURSOR);
    out
}

/// Render the welcome message at absolute positions in the chat area.
fn render_welcome_at(state: &ScreenState) -> String {
    let accent = fgc(ACCENT);
    let success = fgc(SUCCESS);
    let fg_main = fgc(FG);
    let info = fgc(INFO);
    let white = fgc(WHITE);
    let top = state.chat_top();

    let mut out = String::new();
    // Row top: blank line
    out.push_str(&goto(top, 1));
    out.push_str(CLEAR_EOL);
    // Row top+1: title
    out.push_str(&goto(top + 1, 1));
    out.push_str(&format!(
        "  {accent}{BOLD}Elwood Agent{RESET} {white}{ARROW} terminal-native AI coding assistant{RESET}",
    ));
    out.push_str(CLEAR_EOL);
    // Row top+2: blank
    out.push_str(&goto(top + 2, 1));
    out.push_str(CLEAR_EOL);
    // Row top+3: instruction
    out.push_str(&goto(top + 3, 1));
    out.push_str(&format!(
        "  {fg_main}Type a message below to get started.{RESET}",
    ));
    out.push_str(CLEAR_EOL);
    // Row top+4: blank
    out.push_str(&goto(top + 4, 1));
    out.push_str(CLEAR_EOL);
    // Row top+5: tip line 1
    out.push_str(&goto(top + 5, 1));
    out.push_str(&format!(
        "  {success}{BOLD}Tip:{RESET} {info}Press Ctrl+T to toggle Terminal mode, or prefix with ! to run commands.{RESET}",
    ));
    out.push_str(CLEAR_EOL);
    // Row top+6: tip line 2
    out.push_str(&goto(top + 6, 1));
    out.push_str(&format!(
        "       {info}The agent can read panes, edit files, and navigate your codebase.{RESET}",
    ));
    out.push_str(CLEAR_EOL);
    out
}

/// Render the header bar (row 1).
///
/// Layout: ` Elwood Pro / project   [1:chat] [2:tools] [3:files] [4:agents]  HH:MM `
pub fn render_header(state: &ScreenState) -> String {
    let w = state.width as usize;
    let mut out = String::new();
    out.push_str(&goto(1, 1));

    let hbg = bgc(HEADER_BG);

    // Breadcrumb: " Elwood Pro / project "
    let accent_fg = fgc(ACCENT);
    let muted_fg = fgc(MUTED);
    let white_fg = fgc(WHITE);
    let project = &state.project_name;
    let breadcrumb = format!(
        "{hbg} {accent_fg}{BOLD}Elwood Pro{RESET}{hbg} {muted_fg}/{RESET}{hbg} {white_fg}{project}{RESET}{hbg} ",
    );

    // Tab pills
    struct Tab {
        num: u8,
        label: &'static str,
        color: (u8, u8, u8),
        active: bool,
    }
    let tabs = [
        Tab { num: 1, label: "chat", color: TAB_CHAT, active: true },
        Tab { num: 2, label: "tools", color: TAB_TOOLS, active: false },
        Tab { num: 3, label: "files", color: TAB_FILES, active: false },
        Tab { num: 4, label: "agents", color: TAB_AGENTS, active: false },
    ];

    let mut tab_str = String::new();
    for tab in &tabs {
        if tab.active {
            tab_str.push_str(&format!(
                "{}{} {}:{} {RESET} ",
                bgc(tab.color), fg(BG.0, BG.1, BG.2), tab.num, tab.label,
            ));
        } else {
            let m = muted_tab(tab.color);
            tab_str.push_str(&format!(
                "{}{} {}:{} {RESET}{hbg} ",
                bgc(m), fgc(MUTED), tab.num, tab.label,
            ));
        }
    }

    // Clock
    let now = chrono::Local::now();
    let clock = format!("{}{} {} {RESET}", hbg, fgc(MUTED), now.format("%H:%M"));

    // Assemble: breadcrumb + gap + tabs + gap + clock
    // Use simplified approach: render breadcrumb, then tabs centered-ish, then clock right
    out.push_str(&breadcrumb);
    out.push_str(&tab_str);

    // Fill remaining space to push clock to right edge
    // Calculate visible widths (approximate — ANSI codes have zero width)
    // Instead of exact counting, just position clock at far right
    let clock_col = w.saturating_sub(7); // "HH:MM " is ~7 chars
    out.push_str(&hbg);
    out.push_str(CLEAR_EOL); // Fill rest with header bg
    out.push_str(&goto(1, clock_col as u16));
    out.push_str(&clock);

    out
}

/// Render the input box (rows: input_top to input_top+3).
///
/// ```text
/// ╭─ Message (Enter send, Esc cancel) ────────────╮
/// │ Type a message...                              │
/// │                                                │
/// ╰────────────────────────────────────────────────╯
/// ```
pub fn render_input_box(state: &ScreenState) -> String {
    let w = state.width as usize;
    let top = state.input_top();
    let r = RESET;

    // Mode-dependent styling
    let (border_color, title, placeholder) = match state.input_mode {
        InputMode::Agent => (ACCENT, " Message (Enter send, Esc cancel) ", "Type a message..."),
        InputMode::Terminal => (WARNING, " Command (Enter run, Ctrl+T agent) ", "Type a command..."),
    };
    let border = fgc(border_color);

    let title_len = title.len();
    let fill_len = w.saturating_sub(title_len + 3);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();

    // Top border with title
    out.push_str(&goto(top, 1));
    out.push_str(&format!("{border}{BOX_TL}{BOX_H}{r}{border}{BOLD}{title}{r}{border}{fill}{BOX_TR}{r}"));

    // Content line 1 (input text or placeholder)
    out.push_str(&goto(top + 1, 1));
    let inner_w = w.saturating_sub(4);
    let placeholder_len = placeholder.len();
    if state.input_text.is_empty() {
        out.push_str(&format!(
            "{border}{BOX_V}{r} {}{DIM}{placeholder}{r}{}",
            fgc(MUTED),
            " ".repeat(inner_w.saturating_sub(placeholder_len)),
        ));
    } else {
        let display: String = state.input_text.chars().take(inner_w).collect();
        let pad = inner_w.saturating_sub(display.len());
        out.push_str(&format!(
            "{border}{BOX_V}{r} {}{display}{}",
            fgc(FG), " ".repeat(pad),
        ));
    }
    out.push_str(&format!(" {border}{BOX_V}{r}"));

    // Content line 2 (empty)
    out.push_str(&goto(top + 2, 1));
    out.push_str(&format!(
        "{border}{BOX_V}{r}{}  {border}{BOX_V}{r}",
        " ".repeat(w.saturating_sub(4)),
    ));

    // Bottom border
    out.push_str(&goto(top + 3, 1));
    let bot_fill: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
    out.push_str(&format!("{border}{BOX_BL}{bot_fill}{BOX_BR}{r}"));

    out
}

/// Render the status bar (last row).
///
/// Layout: ` [/help] cmds  [^C] quit    Status · model · 5.2K tok · 12s `
pub fn render_status_bar(state: &ScreenState) -> String {
    let w = state.width as usize;
    let row = state.status_row();
    let sbg = bgc(STATUS_BG);

    let mut out = String::new();
    out.push_str(&goto(row, 1));
    out.push_str(&sbg);
    out.push_str(CLEAR_EOL);

    // Left: key hints
    let key_bg = bgc(SELECTION);
    let key_fg = fgc(FG);
    let label_fg = fgc(MUTED);

    // Mode pill badge
    let (mode_label, mode_color) = match state.input_mode {
        InputMode::Agent => (" Agent ", ACCENT),
        InputMode::Terminal => (" Term ", WARNING),
    };
    let mode_bg = bgc(mode_color);
    let mode_fg = fg(BG.0, BG.1, BG.2);

    out.push_str(&goto(row, 1));
    out.push_str(&format!(
        "{sbg} {mode_bg}{mode_fg}{BOLD}{mode_label}{RESET}{sbg}  \
         {key_bg}{key_fg}{BOLD} /help {RESET}{sbg} {label_fg}cmds{RESET}{sbg}  \
         {key_bg}{key_fg}{BOLD} ^C {RESET}{sbg} {label_fg}quit{RESET}{sbg}  \
         {key_bg}{key_fg}{BOLD} ^T {RESET}{sbg} {label_fg}mode{RESET}{sbg}",
    ));

    // Right: status · model · tokens · elapsed
    let mut right_parts: Vec<String> = Vec::new();

    // Status
    if state.is_running {
        if let Some(ref tool) = state.active_tool {
            let elapsed = state.tool_start
                .map(|s| format!(" ({:.1}s)", s.elapsed().as_secs_f64()))
                .unwrap_or_default();
            right_parts.push(format!(
                "{}Running {tool}...{elapsed}{RESET}",
                fgc(INFO),
            ));
        } else {
            right_parts.push(format!("{}Thinking{RESET}", fgc(INFO)));
        }
    } else if state.awaiting_permission {
        right_parts.push(format!(
            "{}{BOLD}Permission needed{RESET}",
            fgc(WARNING),
        ));
    } else {
        right_parts.push(format!("{}Ready{RESET}", fgc(MUTED)));
    }

    // Model
    if !state.model_name.is_empty() {
        right_parts.push(format!("{}{}{RESET}", fgc(MUTED), state.model_name));
    }

    // Cost
    if state.cost > 0.0 {
        right_parts.push(format!("{}${:.4}{RESET}", fgc(MUTED), state.cost));
    }

    // Tokens
    if state.tokens_used > 0 {
        right_parts.push(format!("{}{}{RESET}", fgc(MUTED), format_tokens(state.tokens_used)));
    }

    // Context budget
    if state.context_max > 0 {
        let pct = (state.context_used as f64 / state.context_max as f64 * 100.0) as u8;
        let color = if pct >= 90 { ERROR } else if pct >= 70 { WARNING } else { MUTED };
        right_parts.push(format!(
            "{}{}/{}({pct}%){RESET}",
            fgc(color),
            format_tokens(state.context_used),
            format_tokens(state.context_max),
        ));
    }

    // Elapsed
    let elapsed_secs = if let Some(start) = state.task_start {
        Some(start.elapsed().as_secs())
    } else {
        state.task_elapsed_frozen
    };
    if let Some(secs) = elapsed_secs {
        right_parts.push(format!("{}{}{RESET}", fgc(MUTED), format_elapsed(secs)));
    }

    // Join with " · " separators
    let sep = format!(" {}·{RESET} ", fgc(MUTED));
    let right_str = right_parts.join(&sep);

    // Position right-aligned (approximate — just put it toward the right)
    // Count visible chars roughly: strip ANSI codes
    let visible_len = strip_ansi_len(&right_str);
    let right_col = w.saturating_sub(visible_len + 2);
    out.push_str(&goto(row, right_col as u16));
    out.push_str(&right_str);
    out.push_str(&format!(" {RESET}"));

    out
}

/// Render content that goes into the chat area (scrolling region).
/// This positions the cursor at the cursor row inside the scroll region
/// so new content scrolls naturally.
pub fn render_chat_content(text: &str) -> String {
    // Content goes into the scroll region — just output it directly
    // The terminal handles scrolling within the set scroll region
    text.to_string()
}

/// Write a styled user prompt line into the chat area.
pub fn format_user_prompt(text: &str) -> String {
    format!(
        "\r\n{}{BOLD}You{RESET}  {}{text}{RESET}\r\n\r\n",
        fgc(ACCENT), fgc(FG),
    )
}

/// Write the "Elwood" prefix before streaming starts.
pub fn format_assistant_prefix() -> String {
    format!("{}{BOLD}Elwood:{RESET}  ", fgc(SUCCESS))
}

/// Format a content delta (streaming text).
pub fn format_content(text: &str) -> String {
    format!("{}{text}{RESET}", fgc(FG))
}

/// Format a tool start event.
pub fn format_tool_start(tool_name: &str, preview: &str) -> String {
    let border = fgc(BORDER);
    let warn = fgc(WARNING);
    let muted = fgc(MUTED);

    let title = format!(" {GEAR} {tool_name} ");
    let fill_len = 50usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "\r\n{border}{BOX_TL}{BOX_H}{RESET}{warn}{BOLD}{title}{RESET}{border}{fill}{BOX_TR}{RESET}\r\n",
    ));
    if !preview.is_empty() {
        let p = truncate(preview, 46);
        out.push_str(&format!("{border}{BOX_V}{RESET}  {muted}{p}{RESET}\r\n"));
    }
    out
}

/// Format a tool end event.
pub fn format_tool_end(success: bool, preview: &str) -> String {
    let border = fgc(BORDER);
    let (icon, color) = if success {
        (CHECK, fgc(SUCCESS))
    } else {
        (CROSS, fgc(ERROR))
    };
    let status = if success { "OK" } else { "FAIL" };
    let p = truncate(preview, 42);

    let mut out = String::new();
    out.push_str(&format!(
        "{border}{BOX_V}{RESET}  {color}{BOLD}{icon} {status}{RESET}",
    ));
    if !p.is_empty() {
        let muted = fgc(MUTED);
        out.push_str(&format!(" {muted}{p}{RESET}"));
    }
    out.push_str("\r\n");

    let fill: String = std::iter::repeat(BOX_H).take(50).collect();
    out.push_str(&format!("{border}{BOX_BL}{fill}{BOX_BR}{RESET}\r\n"));
    out
}

/// Format a turn completion banner.
pub fn format_turn_complete(summary: Option<&str>) -> String {
    let success = fgc(SUCCESS);
    let muted = fgc(MUTED);
    let sep: String = std::iter::repeat(DOUBLE_H).take(38).collect();

    let suffix = summary
        .map(|s| format!(" {muted}{ARROW} {}{RESET}", truncate(s, 55)))
        .unwrap_or_default();

    format!(
        "\r\n{muted}{sep}{RESET}\r\n{success}{BOLD}{CHECK} Done{RESET}{suffix}\r\n{muted}{sep}{RESET}\r\n",
    )
}

/// Format a permission request box.
pub fn format_permission_request(tool_name: &str, description: &str) -> String {
    let border = fgc(ACCENT);
    let warn = fgc(WARNING);
    let muted = fgc(MUTED);
    let fgv = fgc(FG);
    let key_bg = bgc(ACCENT);
    let key_fg = fg(BG.0, BG.1, BG.2);

    let title = " Permission Required ";
    let fill_len = 50usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();
    let bot: String = std::iter::repeat(BOX_H).take(50).collect();
    let desc = truncate(description, 46);

    format!(
        concat!(
            "\r\n{b}{bold}{tl}{h} {w}{bold}{title}{r}{b}{bold}{fill}{tr}{r}\r\n",
            "{b}{v}{r}  {w}{bold}{tool}{r}\r\n",
            "{b}{v}{r}  {fg}{desc}{r}\r\n",
            "{b}{v}{r}\r\n",
            "{b}{v}{r}  {kb}{kf} y {r} {m}approve   {kb}{kf} n {r} {m}deny{r}\r\n",
            "{b}{bold}{bl}{bot}{br}{r}\r\n",
        ),
        b = border, bold = BOLD, r = RESET,
        tl = BOX_TL, tr = BOX_TR, bl = BOX_BL, br = BOX_BR,
        h = BOX_H, v = BOX_V,
        w = warn, m = muted, fg = fgv,
        title = title, fill = fill, bot = bot,
        tool = tool_name, desc = desc,
        kb = key_bg, kf = key_fg,
    )
}

/// Format the welcome message shown in the chat area on first open.
pub fn format_welcome() -> String {
    let accent = fgc(ACCENT);
    let success = fgc(SUCCESS);
    let fg_main = fgc(FG);
    let info = fgc(INFO);
    let white = fgc(WHITE);

    // Use explicit \n (not \r\n) since cursor is already at column 1
    // and the scroll region handles line breaks
    let mut out = String::new();
    out.push_str(&format!(
        "  {accent}{BOLD}Elwood Agent{RESET} {white}{ARROW} terminal-native AI coding assistant{RESET}\n",
    ));
    out.push('\n');
    out.push_str(&format!(
        "  {fg_main}Type a message below to get started.{RESET}\n",
    ));
    out.push('\n');
    out.push_str(&format!(
        "  {success}{BOLD}Tip:{RESET} {info}Press Ctrl+T to toggle Terminal mode, or prefix with ! to run commands.{RESET}\n",
    ));
    out.push_str(&format!(
        "       {info}The agent can read panes, edit files, and navigate your codebase.{RESET}\n",
    ));
    out.push('\n');
    out
}

/// Format an error message.
pub fn format_error(msg: &str) -> String {
    format!("\r\n{}{BOLD}{CROSS} Error:{RESET} {}{msg}{RESET}\r\n", fgc(ERROR), fgc(FG))
}

/// Format a `$ command` prompt line in the chat area.
pub fn format_command_prompt(command: &str) -> String {
    format!(
        "\r\n{}{BOLD}${RESET} {}{command}{RESET}\r\n",
        fgc(WARNING), fgc(FG),
    )
}

/// Format shell command output as a boxed section with exit code.
pub fn format_command_output(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> String {
    let border = fgc(BORDER);
    let muted = fgc(MUTED);
    let fgv = fgc(FG);
    let r = RESET;

    let code = exit_code.unwrap_or(-1);
    let (icon, status_color) = if code == 0 {
        (CHECK, fgc(SUCCESS))
    } else {
        (CROSS, fgc(ERROR))
    };

    let title = format!(" Shell: {command} ");
    let title_display = truncate(&title, 46);
    let fill_len = 50usize.saturating_sub(title_display.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();

    // Top border
    out.push_str(&format!(
        "\r\n{border}{BOX_TL}{BOX_H}{r}{muted}{BOLD}{title_display}{r}{border}{fill}{BOX_TR}{r}\r\n",
    ));

    // stdout lines
    if !stdout.is_empty() {
        for line in stdout.lines().take(50) {
            out.push_str(&format!("{border}{BOX_V}{r}  {fgv}{line}{r}\r\n"));
        }
        if stdout.lines().count() > 50 {
            out.push_str(&format!("{border}{BOX_V}{r}  {muted}... (truncated){r}\r\n"));
        }
    }

    // stderr lines
    if !stderr.is_empty() {
        let err_color = fgc(ERROR);
        for line in stderr.lines().take(20) {
            out.push_str(&format!("{border}{BOX_V}{r}  {err_color}{line}{r}\r\n"));
        }
    }

    // Exit code footer
    out.push_str(&format!(
        "{border}{BOX_V}{r}  {status_color}{BOLD}{icon} exit {code}{r}\r\n",
    ));

    // Bottom border
    let bot: String = std::iter::repeat(BOX_H).take(50).collect();
    out.push_str(&format!("{border}{BOX_BL}{bot}{BOX_BR}{r}\r\n"));

    out
}

/// Format a shutdown message.
pub fn format_shutdown() -> String {
    format!("\r\n{}{ITALIC}Agent session ended.{RESET}\r\n", fgc(MUTED))
}

/// Render the prompt line in the chat area (after completion).
pub fn format_prompt() -> String {
    let accent = fgc(ACCENT);
    let muted = fgc(MUTED);
    format!("\r\n{accent}{BOLD}elwood{RESET} {muted}{ARROW}{RESET} ")
}

/// Format a permission approval.
pub fn format_permission_granted(tool_name: &str) -> String {
    format!("{}{BOLD}{CHECK} Approved:{RESET} {}{tool_name}{RESET}\r\n", fgc(SUCCESS), fgc(FG))
}

/// Format a permission denial.
pub fn format_permission_denied(tool_name: &str) -> String {
    format!("{}{BOLD}{CROSS} Denied:{RESET} {}{tool_name}{RESET}\r\n", fgc(ERROR), fgc(FG))
}

// ─── Helpers ────────────────────────────────────────────────────────────

fn format_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K tok", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens} tok")
    }
}

fn format_elapsed(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m{s:02}s")
    } else if mins > 0 {
        format!("{mins}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

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

/// Approximate visible length of a string (strip ANSI escape codes).
fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(500), "500 tok");
        assert_eq!(format_tokens(1_500), "1.5K tok");
        assert_eq!(format_tokens(2_500_000), "2.5M tok");
    }

    #[test]
    fn test_format_elapsed() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(45), "45s");
        assert_eq!(format_elapsed(65), "1m05s");
        assert_eq!(format_elapsed(3661), "1h01m01s");
    }

    #[test]
    fn test_truncate_multibyte() {
        let s = "hello 世界 world";
        let t = truncate(s, 8);
        assert!(t.len() <= 8);
    }

    #[test]
    fn test_strip_ansi_len() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(strip_ansi_len("\x1b[38;2;100;200;255mtext\x1b[0m"), 4);
    }

    #[test]
    fn test_render_header() {
        let state = ScreenState { width: 80, height: 24, ..Default::default() };
        let header = render_header(&state);
        assert!(header.contains("Elwood Pro"));
        assert!(header.contains("chat"));
    }

    #[test]
    fn test_render_input_box() {
        let state = ScreenState { width: 80, height: 24, ..Default::default() };
        let input = render_input_box(&state);
        assert!(input.contains("Message"));
        assert!(input.contains("Type a message"));
        assert!(input.contains("╭"));
        assert!(input.contains("╯"));
    }

    #[test]
    fn test_render_status_bar() {
        let mut state = ScreenState { width: 80, height: 24, ..Default::default() };
        state.model_name = "gemini-2.5-pro".to_string();
        state.tokens_used = 5000;
        let bar = render_status_bar(&state);
        assert!(bar.contains("/help"));
        assert!(bar.contains("gemini-2.5-pro"));
    }

    #[test]
    fn test_muted_tab_blends() {
        let m = muted_tab(TAB_CHAT);
        // Should be darker than original
        assert!(m.0 < TAB_CHAT.0);
        assert!(m.1 < TAB_CHAT.1);
    }
}
