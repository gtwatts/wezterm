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

use crate::git_info::GitInfo;
use crate::runtime::InputMode;
use crate::theme;
use crate::vim_mode::VimState;
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
#[allow(dead_code)]
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
    /// Deprecated single-line input text (kept for backwards compat with existing rendering path).
    pub input_text: String,
    /// Multi-line input lines (from InputEditor).  If non-empty, takes precedence over `input_text`.
    pub input_lines: Vec<String>,
    /// Cursor position within the multi-line editor (row, col as visible char index).
    pub cursor_row: usize,
    pub cursor_col: usize,
    /// Current input mode (Agent or Terminal).
    pub input_mode: InputMode,
    /// True when agent is running (for status display).
    pub is_running: bool,
    /// True when awaiting permission.
    pub awaiting_permission: bool,
    /// Current git repository info (branch, dirty state, ahead/behind).
    pub git_info: Option<GitInfo>,
    /// Ghost text suggestion (dim suffix shown after input cursor).
    pub ghost_text: Option<String>,
    /// Whether terminal recording is active.
    pub recording_active: bool,
    /// Whether recording is paused.
    pub recording_paused: bool,
    /// Number of currently running background jobs.
    pub running_jobs: usize,
    /// Current vim mode state (None = vim off).
    pub vim_state: Option<VimState>,
    /// Vim command-line buffer (for `:` mode rendering).
    pub vim_command_buffer: String,
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
            input_lines: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            input_mode: InputMode::default(),
            is_running: false,
            awaiting_permission: false,
            git_info: None,
            ghost_text: None,
            recording_active: false,
            recording_paused: false,
            running_jobs: 0,
            vim_state: None,
            vim_command_buffer: String::new(),
        }
    }
}

impl ScreenState {
    /// Number of content lines currently in the input editor (at least 1).
    fn input_content_lines(&self) -> u16 {
        if self.input_lines.is_empty() {
            1
        } else {
            (self.input_lines.len() as u16).min(8)
        }
    }

    /// Total rows the input box occupies: top border + content lines + bottom border.
    pub fn input_box_height(&self) -> u16 {
        self.input_content_lines() + 2 // top border + content + bottom border
    }

    /// Rows reserved for header (1) + input box (variable) + status bar (1).
    pub fn chrome_height(&self) -> u16 {
        1 + self.input_box_height() + 1
    }

    /// First row of the chat area (1-based).
    pub fn chat_top(&self) -> u16 {
        2 // Row 1 = header, row 2+ = chat
    }

    /// Last row of the chat area (1-based).
    pub fn chat_bottom(&self) -> u16 {
        // Leave room for input box + status bar
        self.height.saturating_sub(self.input_box_height() + 1)
    }

    /// First row of the input area (1-based).
    pub fn input_top(&self) -> u16 {
        // Input starts after chat area; status bar is the last row
        self.height.saturating_sub(self.input_box_height())
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
///
/// Shows a visually appealing welcome screen with a decorative box header,
/// quick start hints, and keyboard shortcuts.
fn render_welcome_at(state: &ScreenState) -> String {
    let accent = fgc(ACCENT);
    let success = fgc(SUCCESS);
    let fg_main = fgc(FG);
    let info = fgc(INFO);
    let white = fgc(WHITE);
    let muted = fgc(MUTED);
    let border = fgc(BORDER);
    let top = state.chat_top();

    let mut out = String::new();
    let r = RESET;
    // Row top: blank line
    out.push_str(&goto(top, 1));
    out.push_str(CLEAR_EOL);

    // ── Decorative header box ──────────────────────────────────────
    let box_w = 43;
    let hline: String = std::iter::repeat(BOX_H).take(box_w).collect();

    // Row top+1: top border
    out.push_str(&goto(top + 1, 1));
    out.push_str(&format!("    {accent}{BOX_TL}{hline}{BOX_TR}{r}"));
    out.push_str(CLEAR_EOL);

    // Row top+2: title line
    out.push_str(&goto(top + 2, 1));
    out.push_str(&format!(
        "    {accent}{BOX_V}{r}       {accent}{BOLD}Elwood Terminal{r} {white}v0.1.0{r}            {accent}{BOX_V}{r}",
    ));
    out.push_str(CLEAR_EOL);

    // Row top+3: subtitle
    out.push_str(&goto(top + 3, 1));
    out.push_str(&format!(
        "    {accent}{BOX_V}{r}   {muted}AI-native {border}\u{00B7}{r} {muted}Open Source {border}\u{00B7}{r} {muted}Local{r}         {accent}{BOX_V}{r}",
    ));
    out.push_str(CLEAR_EOL);

    // Row top+4: bottom border
    out.push_str(&goto(top + 4, 1));
    out.push_str(&format!("    {accent}{BOX_BL}{hline}{BOX_BR}{r}"));
    out.push_str(CLEAR_EOL);

    // Row top+5: blank
    out.push_str(&goto(top + 5, 1));
    out.push_str(CLEAR_EOL);

    // ── Quick start section ────────────────────────────────────────
    out.push_str(&goto(top + 6, 1));
    out.push_str(&format!("    {success}{BOLD}Quick Start:{r}"));
    out.push_str(CLEAR_EOL);

    let hints: &[(&str, &str)] = &[
        ("Type a message", "to chat with AI"),
        ("Press Ctrl+T", "to switch to terminal mode"),
        ("Use ! prefix", "for quick commands"),
        ("Type @file.rs", "to attach context"),
        ("Press Ctrl+P", "for command palette"),
    ];

    for (i, (key, desc)) in hints.iter().enumerate() {
        let row = top + 7 + i as u16;
        if row >= state.chat_bottom() {
            break;
        }
        out.push_str(&goto(row, 1));
        out.push_str(&format!(
            "    {muted}{ARROW}{r} {info}{key}{r} {fg_main}{desc}{r}",
        ));
        out.push_str(CLEAR_EOL);
    }

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

/// Render the input box (rows: input_top .. input_top + input_box_height - 1).
///
/// Single-line example:
/// ```text
/// ╭─ Message (Enter send, Esc cancel) ────────────╮
/// │ Type a message...                              │
/// ╰────────────────────────────────────────────────╯
/// ```
///
/// Multi-line example (3 lines):
/// ```text
/// ╭─ Message (Shift+Enter newline, Enter send) ────╮
/// │ 1│ first line of the message                   │
/// │ 2│ second line                                 │
/// │ 3│ third line_                                 │
/// ╰────────────────────────────────────────────────╯
/// ```
pub fn render_input_box(state: &ScreenState) -> String {
    let w = state.width as usize;
    let top = state.input_top();
    let r = RESET;

    // Mode-dependent styling
    let (border_color, title_single, title_multi, placeholder) = match state.input_mode {
        InputMode::Agent => (
            ACCENT,
            " Message (Enter send, Esc cancel) ",
            " Message (Shift+Enter newline, Enter send) ",
            "Type a message...",
        ),
        InputMode::Terminal => (
            WARNING,
            " Command (Enter run, Ctrl+T agent) ",
            " Command (Shift+Enter newline, Enter run) ",
            "Type a command...",
        ),
    };
    let border = fgc(border_color);

    // Determine whether we are in multi-line mode
    let content_lines = state.input_content_lines() as usize;
    let is_multiline = content_lines > 1;
    let title = if is_multiline { title_multi } else { title_single };

    // Top border with title
    let title_len = title.len();
    let fill_len = w.saturating_sub(title_len + 3);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();
    out.push_str(&goto(top, 1));
    out.push_str(&format!("{border}{BOX_TL}{BOX_H}{r}{border}{BOLD}{title}{r}{border}{fill}{BOX_TR}{r}"));

    // Content rows
    let inner_w = w.saturating_sub(4); // "│ " + content + " │"

    if is_multiline {
        // Multi-line: show line numbers, highlight cursor line
        let lines = &state.input_lines;
        // Line number gutter width: enough for the digit count + "│ "
        let gutter = format!("{content_lines}").len(); // e.g. 2 for 10+ lines

        for (row_idx, line_text) in lines.iter().enumerate().take(8) {
            let is_cursor_row = row_idx == state.cursor_row;
            let row_num = row_idx + 1;

            // Gutter: " N│"
            let gutter_str = format!("{:>gutter$}", row_num);
            let gutter_color = if is_cursor_row { fgc(ACCENT) } else { fgc(MUTED) };
            let line_bg = if is_cursor_row {
                bgc(SELECTION)
            } else {
                String::new()
            };
            let line_bg_reset = if is_cursor_row { RESET } else { "" };

            // Available width for content after gutter (gutter + "│ " = gutter+2 chars)
            let content_w = inner_w.saturating_sub(gutter + 2);

            // Truncate display text to available width
            let display: String = line_text.chars().take(content_w).collect();
            let pad = content_w.saturating_sub(display.chars().count());

            out.push_str(&goto(top + 1 + row_idx as u16, 1));
            out.push_str(&format!(
                "{border}{BOX_V}{r}{line_bg} {gutter_color}{gutter_str}{r}{line_bg}{gutter_color}│{r} {line_bg}{}{display}{}{line_bg_reset} {border}{BOX_V}{r}",
                fgc(FG),
                " ".repeat(pad),
            ));
        }
    } else {
        // Single-line: show text or placeholder, with optional ghost text
        out.push_str(&goto(top + 1, 1));
        let use_text = if state.input_lines.is_empty() {
            &state.input_text
        } else {
            &state.input_lines[0]
        };
        let placeholder_len = placeholder.chars().count();
        if use_text.is_empty() {
            out.push_str(&format!(
                "{border}{BOX_V}{r} {}{DIM}{placeholder}{r}{}",
                fgc(MUTED),
                " ".repeat(inner_w.saturating_sub(placeholder_len)),
            ));
        } else {
            let display: String = use_text.chars().take(inner_w).collect();
            let display_len = display.chars().count();

            // Render ghost text (dim+italic) after the typed text
            let ghost_suffix = state.ghost_text.as_deref().unwrap_or("");
            let ghost_available = inner_w.saturating_sub(display_len);
            let ghost_display: String = ghost_suffix.chars().take(ghost_available).collect();
            let ghost_len = ghost_display.chars().count();

            let pad = inner_w.saturating_sub(display_len + ghost_len);
            out.push_str(&format!(
                "{border}{BOX_V}{r} {}{display}",
                fgc(FG),
            ));
            if !ghost_display.is_empty() {
                out.push_str(&format!(
                    "{}{DIM}{ITALIC}{ghost_display}{r}",
                    fgc(MUTED),
                ));
            }
            out.push_str(&" ".repeat(pad));
        }
        out.push_str(&format!(" {border}{BOX_V}{r}"));
    }

    // Bottom border
    let bot_row = top + 1 + content_lines as u16;
    out.push_str(&goto(bot_row, 1));
    let bot_fill: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
    out.push_str(&format!("{border}{BOX_BL}{bot_fill}{BOX_BR}{r}"));

    out
}

/// Render the status bar (last row).
///
/// Warp-style badges/chips layout:
/// ` [Agent] | main* | gemini-2.5-pro | 5.2K tok | $0.03 | 12s    ^T mode · ^P palette `
pub fn render_status_bar(state: &ScreenState) -> String {
    let w = state.width as usize;
    let row = state.status_row();
    let sbg = bgc(STATUS_BG);

    let mut out = String::new();
    out.push_str(&goto(row, 1));
    out.push_str(&sbg);
    out.push_str(CLEAR_EOL);

    // ── Left section: mode badge + git + status ─────────────────────

    // Mode pill badge with colored background
    let (mode_label, mode_color) = match state.input_mode {
        InputMode::Agent => (" Agent ", ACCENT),
        InputMode::Terminal => (" Term ", WARNING),
    };
    let mode_bg = bgc(mode_color);
    let mode_fg = fg(BG.0, BG.1, BG.2);

    // Git branch chip
    let git_chip = if let Some(ref gi) = state.git_info {
        let branch_fg = fgc(SUCCESS);
        let dirty_mark = if gi.is_dirty {
            format!("{}*{RESET}{sbg}", fgc(WARNING))
        } else {
            String::new()
        };
        let mut ab = String::new();
        if gi.ahead > 0 {
            ab.push_str(&format!(" {}\u{2191}{}{RESET}{sbg}", fgc(SUCCESS), gi.ahead));
        }
        if gi.behind > 0 {
            ab.push_str(&format!(" {}\u{2193}{}{RESET}{sbg}", fgc(ERROR), gi.behind));
        }
        format!(
            " {sep} {branch_fg}\u{E0A0} {}{RESET}{sbg}{dirty_mark}{ab}",
            gi.branch,
            sep = format!("{}·{RESET}{sbg}", fgc(MUTED)),
        )
    } else {
        String::new()
    };

    // Model chip
    let model_chip = if !state.model_name.is_empty() {
        let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
        format!(" {sep} {}{}{RESET}{sbg}", fgc(ACCENT), state.model_name)
    } else {
        String::new()
    };

    // Status indicator with spinner
    let status_chip = if state.is_running {
        let spinner = theme::spinner_frame(
            state.tool_start
                .map(|s| s.elapsed().as_millis() as usize / 80)
                .unwrap_or(0),
        );
        if let Some(ref tool) = state.active_tool {
            let elapsed = state.tool_start
                .map(|s| format!(" {:.1}s", s.elapsed().as_secs_f64()))
                .unwrap_or_default();
            format!(
                " {sep} {info}{spinner} {tool}{elapsed}{RESET}{sbg}",
                sep = format!("{}·{RESET}{sbg}", fgc(MUTED)),
                info = fgc(INFO),
            )
        } else {
            format!(
                " {sep} {info}{spinner} Thinking{RESET}{sbg}",
                sep = format!("{}·{RESET}{sbg}", fgc(MUTED)),
                info = fgc(INFO),
            )
        }
    } else if state.awaiting_permission {
        format!(
            " {sep} {warn}{BOLD}\u{26A0} Permission{RESET}{sbg}",
            sep = format!("{}·{RESET}{sbg}", fgc(MUTED)),
            warn = fgc(WARNING),
        )
    } else {
        String::new()
    };

    // Recording indicator
    let rec_chip = if state.recording_active {
        if state.recording_paused {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} {}{BOLD}\u{23F8} REC{RESET}{sbg}", fgc(WARNING))
        } else {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} \x1b[1;31m\u{25CF} REC\x1b[0m{sbg}")
        }
    } else {
        String::new()
    };

    // Background jobs indicator
    let jobs_chip = if state.running_jobs > 0 {
        let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
        if state.running_jobs == 1 {
            format!(" {sep} {}{BOLD}\u{2699} 1 job{RESET}{sbg}", fgc(INFO))
        } else {
            format!(" {sep} {}{BOLD}\u{2699} {} jobs{RESET}{sbg}", fgc(INFO), state.running_jobs)
        }
    } else {
        String::new()
    };

    // Vim mode indicator
    let vim_chip = match state.vim_state {
        Some(VimState::Normal) => {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} {}{BOLD}NORMAL{RESET}{sbg}", fgc(SUCCESS))
        }
        Some(VimState::Insert) => {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} {}{BOLD}INSERT{RESET}{sbg}", fgc(ACCENT))
        }
        Some(VimState::Visual) => {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} {}{BOLD}VISUAL{RESET}{sbg}", fgc(WARNING))
        }
        Some(VimState::Command) => {
            let sep = format!("{}·{RESET}{sbg}", fgc(MUTED));
            format!(" {sep} {}:{}{RESET}{sbg}", fgc(WARNING), state.vim_command_buffer)
        }
        None => String::new(),
    };

    // Assemble left section
    out.push_str(&goto(row, 1));
    out.push_str(&format!(
        "{sbg} {mode_bg}{mode_fg}{BOLD}{mode_label}{RESET}{sbg}{vim_chip}{rec_chip}{jobs_chip}{git_chip}{model_chip}{status_chip}",
    ));

    // ── Right section: tokens, cost, elapsed, keyboard hints ────────
    let mut right_parts: Vec<String> = Vec::new();

    // Token count
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

    // Cost
    if state.cost > 0.0 {
        right_parts.push(format!("{}${:.4}{RESET}", fgc(MUTED), state.cost));
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

    // Keyboard hints (rightmost)
    let hints = format!(
        "{key_bg}{key_fg} ^T {RESET}{sbg}{label} {key_bg}{key_fg} ^P {RESET}{sbg}{label2} {key_bg}{key_fg} F1 {RESET}{sbg}",
        key_bg = bgc(SELECTION),
        key_fg = fgc(FG),
        label = format!("{}mode{RESET}{sbg}", fgc(MUTED)),
        label2 = format!("{}palette{RESET}{sbg}", fgc(MUTED)),
    );
    right_parts.push(hints);

    // Join right parts with " · "
    let sep = format!(" {}·{RESET}{sbg} ", fgc(MUTED));
    let right_str = right_parts.join(&sep);

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
///
/// User messages get a green accent border:
/// ```text
///   You  Fix the failing test in src/auth.rs
/// ```
pub fn format_user_prompt(text: &str) -> String {
    let user_accent = fgc(SUCCESS);
    format!(
        "\r\n  {user_accent}{BOLD}You{RESET}  {}{text}{RESET}\r\n\r\n",
        fgc(FG),
    )
}

/// Write the "Elwood" prefix before streaming starts.
///
/// Agent messages get a blue accent with block chrome:
/// ```text
/// ╭─ Elwood ────────────────────────────────────────╮
/// │  Response text...
/// ```
pub fn format_assistant_prefix() -> String {
    let agent_accent = fgc(ACCENT);
    let fill: String = std::iter::repeat(BOX_H).take(42).collect();
    format!(
        "{agent_accent}{BOX_TL}{BOX_H}{RESET} {agent_accent}{BOLD}Elwood{RESET} {agent_accent}{fill}{BOX_TR}{RESET}\r\n{agent_accent}{BOX_V}{RESET}  ",
    )
}

/// Format a content delta (streaming text).
///
/// If the text contains markdown formatting, renders it as rich ANSI output.
/// Otherwise, applies plain foreground coloring.
pub fn format_content(text: &str) -> String {
    if crate::markdown::is_markdown(text) {
        crate::markdown::render_markdown(text)
    } else {
        format!("{}{text}{RESET}", fgc(FG))
    }
}

/// Format a tool start event.
///
/// Renders as a tool execution block with purple accent border:
/// ```text
/// ╭─ ⚙ ReadFile ──────────────────────────────────╮
/// │  src/main.rs                                    │
/// ```
pub fn format_tool_start(tool_name: &str, preview: &str) -> String {
    let tool_color = fgc((187, 154, 247)); // purple - tool accent
    let muted = fgc(MUTED);

    let title = format!(" {GEAR} {tool_name} ");
    let fill_len = 54usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "\r\n{tool_color}{BOX_TL}{BOX_H}{RESET}{tool_color}{BOLD}{title}{RESET}{tool_color}{fill}{BOX_TR}{RESET}\r\n",
    ));
    if !preview.is_empty() {
        let p = truncate(preview, 50);
        out.push_str(&format!("{tool_color}{BOX_V}{RESET}  {muted}{p}{RESET}\r\n"));
    }
    out
}

/// Format a tool end event.
///
/// Closes the tool block with exit status:
/// ```text
/// │  ✔ OK — 200 lines                              │
/// ╰────────────────────────────────────────────────╯
/// ```
pub fn format_tool_end(success: bool, preview: &str) -> String {
    let tool_color = fgc((187, 154, 247)); // purple - tool accent
    let (icon, color) = if success {
        (CHECK, fgc(SUCCESS))
    } else {
        (CROSS, fgc(ERROR))
    };
    let status = if success { "OK" } else { "FAIL" };
    let p = truncate(preview, 46);

    let mut out = String::new();
    out.push_str(&format!(
        "{tool_color}{BOX_V}{RESET}  {color}{BOLD}{icon} {status}{RESET}",
    ));
    if !p.is_empty() {
        let muted = fgc(MUTED);
        out.push_str(&format!(" {muted}{DIM}{BOX_H} {p}{RESET}"));
    }
    out.push_str("\r\n");

    let fill: String = std::iter::repeat(BOX_H).take(54).collect();
    out.push_str(&format!("{tool_color}{BOX_BL}{fill}{BOX_BR}{RESET}\r\n"));
    out
}

/// Format a turn completion banner.
///
/// Closes the agent block and shows a completion summary:
/// ```text
/// │
/// │  ✔ Done ▸ Completed in 3 steps
/// ╰──────────────────────────────────────────────────╯
/// ```
pub fn format_turn_complete(summary: Option<&str>) -> String {
    let agent_accent = fgc(ACCENT);
    let success = fgc(SUCCESS);
    let muted = fgc(MUTED);

    let suffix = summary
        .map(|s| format!(" {muted}{ARROW} {}{RESET}", truncate(s, 55)))
        .unwrap_or_default();

    let fill: String = std::iter::repeat(BOX_H).take(52).collect();

    format!(
        "\r\n{agent_accent}{BOX_V}{RESET}\r\n\
         {agent_accent}{BOX_V}{RESET}  {success}{BOLD}{CHECK} Done{RESET}{suffix}\r\n\
         {agent_accent}{BOX_BL}{fill}{BOX_BR}{RESET}\r\n",
    )
}

/// Format a permission request box.
///
/// Permission prompts use amber accent for high visibility:
/// ```text
/// ╭─ Permission Required ────────────────────────────╮
/// │  BashTool                                         │
/// │  rm -rf /tmp/test                                 │
/// │                                                   │
/// │  [y] approve   [n] deny   [a] always              │
/// ╰──────────────────────────────────────────────────╯
/// ```
pub fn format_permission_request(tool_name: &str, description: &str) -> String {
    let perm_accent = fgc(WARNING);
    let warn = fgc(WARNING);
    let muted = fgc(MUTED);
    let fgv = fgc(FG);
    let key_bg = bgc(WARNING);
    let key_fg = fg(BG.0, BG.1, BG.2);

    let title = " \u{26A0} Permission Required ";
    let fill_len = 54usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();
    let bot: String = std::iter::repeat(BOX_H).take(54).collect();
    let desc = truncate(description, 50);

    format!(
        concat!(
            "\r\n{b}{tl}{h} {w}{bold}{title}{r}{b}{fill}{tr}{r}\r\n",
            "{b}{v}{r}  {w}{bold}{tool}{r}\r\n",
            "{b}{v}{r}  {fg}{desc}{r}\r\n",
            "{b}{v}{r}\r\n",
            "{b}{v}{r}  {kb}{kf} y {r} {m}approve   {kb}{kf} n {r} {m}deny{r}\r\n",
            "{b}{bl}{bot}{br}{r}\r\n",
        ),
        b = perm_accent, bold = BOLD, r = RESET,
        tl = BOX_TL, tr = BOX_TR, bl = BOX_BL, br = BOX_BR,
        h = BOX_H, v = BOX_V,
        w = warn, m = muted, fg = fgv,
        title = title, fill = fill, bot = bot,
        tool = tool_name, desc = desc,
        kb = key_bg, kf = key_fg,
    )
}

/// Format the welcome message shown in the chat area on first open.
///
/// This version uses `\n` line endings (not `\r\n`) since the cursor is
/// already at column 1 inside the scroll region.
pub fn format_welcome() -> String {
    let accent = fgc(ACCENT);
    let success = fgc(SUCCESS);
    let fg_main = fgc(FG);
    let info = fgc(INFO);
    let white = fgc(WHITE);
    let muted = fgc(MUTED);
    let border = fgc(BORDER);

    let box_w = 43;
    let hline: String = std::iter::repeat(BOX_H).take(box_w).collect();

    let mut out = String::new();
    out.push('\n');

    // Decorative header box
    out.push_str(&format!("    {accent}{BOX_TL}{hline}{BOX_TR}{RESET}\n"));
    out.push_str(&format!(
        "    {accent}{BOX_V}{RESET}       {accent}{BOLD}Elwood Terminal{RESET} {white}v0.1.0{RESET}            {accent}{BOX_V}{RESET}\n",
    ));
    out.push_str(&format!(
        "    {accent}{BOX_V}{RESET}   {muted}AI-native {border}\u{00B7}{RESET} {muted}Open Source {border}\u{00B7}{RESET} {muted}Local{RESET}         {accent}{BOX_V}{RESET}\n",
    ));
    out.push_str(&format!("    {accent}{BOX_BL}{hline}{BOX_BR}{RESET}\n"));
    out.push('\n');

    // Quick start hints
    out.push_str(&format!("    {success}{BOLD}Quick Start:{RESET}\n"));
    let hints: &[(&str, &str)] = &[
        ("Type a message", "to chat with AI"),
        ("Press Ctrl+T", "to switch to terminal mode"),
        ("Use ! prefix", "for quick commands"),
        ("Type @file.rs", "to attach context"),
        ("Press Ctrl+P", "for command palette"),
    ];
    for (key, desc) in hints {
        out.push_str(&format!(
            "    {muted}{ARROW}{RESET} {info}{key}{RESET} {fg_main}{desc}{RESET}\n",
        ));
    }
    out.push('\n');
    out
}

/// Format a `$ command` prompt line in the chat area.
pub fn format_command_prompt(command: &str) -> String {
    format!(
        "\r\n{}{BOLD}${RESET} {}{command}{RESET}\r\n",
        fgc(WARNING), fgc(FG),
    )
}

/// Format shell command output as a boxed section with exit code.
///
/// Uses a muted border for command output to distinguish from agent/tool blocks:
/// ```text
/// ╭─ $ git status ──────────────────────────────────╮
/// │  On branch main                                   │
/// │  nothing to commit                                │
/// │  ✔ exit 0                                         │
/// ╰──────────────────────────────────────────────────╯
/// ```
pub fn format_command_output(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
) -> String {
    let cmd_color = fgc(MUTED);
    let muted = fgc(MUTED);
    let fgv = fgc(FG);
    let r = RESET;

    let code = exit_code.unwrap_or(-1);
    let (icon, status_color) = if code == 0 {
        (CHECK, fgc(SUCCESS))
    } else {
        (CROSS, fgc(ERROR))
    };

    let title = format!(" $ {command} ");
    let title_display = truncate(&title, 48);
    let fill_len = 54usize.saturating_sub(title_display.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();
    let bot: String = std::iter::repeat(BOX_H).take(54).collect();

    let mut out = String::new();

    // Top border
    out.push_str(&format!(
        "\r\n{cmd_color}{BOX_TL}{BOX_H}{r}{muted}{BOLD}{title_display}{r}{cmd_color}{fill}{BOX_TR}{r}\r\n",
    ));

    // stdout lines
    if !stdout.is_empty() {
        for line in stdout.lines().take(50) {
            out.push_str(&format!("{cmd_color}{BOX_V}{r}  {fgv}{line}{r}\r\n"));
        }
        if stdout.lines().count() > 50 {
            out.push_str(&format!(
                "{cmd_color}{BOX_V}{r}  {muted}{ARROW} {DIM}42 more lines hidden{r}\r\n",
            ));
        }
    }

    // stderr lines
    if !stderr.is_empty() {
        let err_color = fgc(ERROR);
        for line in stderr.lines().take(20) {
            out.push_str(&format!("{cmd_color}{BOX_V}{r}  {err_color}{line}{r}\r\n"));
        }
    }

    // Exit code footer
    out.push_str(&format!(
        "{cmd_color}{BOX_V}{r}  {status_color}{BOLD}{icon} exit {code}{r}\r\n",
    ));

    // Bottom border
    out.push_str(&format!("{cmd_color}{BOX_BL}{bot}{BOX_BR}{r}\r\n"));

    out
}

/// Format an Active AI suggestion line below command output.
///
/// Shown when the `ContentDetector` finds compiler errors, test failures, etc.
/// The user can press `Ctrl+F` to trigger a quick-fix from this suggestion.
///
/// # Example output
///
/// ```text
///   💡 Compiler error detected — press Ctrl+F to ask Elwood to fix it
/// ```
pub fn format_suggestion(content_type_label: &str) -> String {
    let info = fgc(INFO);
    let muted = fgc(MUTED);
    let key_bg = bgc(SELECTION);
    let key_fg = fgc(ACCENT);
    format!(
        "\r\n{info}  [!]{RESET} {muted}{content_type_label} detected \
         \u{2014} press {key_bg}{key_fg}{BOLD} Ctrl+F {RESET}{muted} to ask Elwood to fix it{RESET}\r\n",
    )
}

/// Format a next-command suggestion line after a successful command.
///
/// Shown when the command sequence heuristic fires (e.g. `git add` -> `git commit`).
pub fn format_next_command_suggestion(suggestion: &str) -> String {
    let success = fgc(SUCCESS);
    let muted = fgc(MUTED);
    format!(
        "{muted}  [>]{RESET} {success}Next:{RESET} {muted}{suggestion}{RESET}\r\n",
    )
}

/// Format an error message with a red accent block.
///
/// ```text
/// ╭─ ✗ Error ─────────────────────────────────────────╮
/// │  Error message text                                 │
/// ╰────────────────────────────────────────────────────╯
/// ```
pub fn format_error(msg: &str) -> String {
    let err_color = fgc(ERROR);
    let fgv = fgc(FG);

    let title = format!(" {CROSS} Error ");
    let fill_len = 54usize.saturating_sub(title.len() + 2);
    let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();
    let bot: String = std::iter::repeat(BOX_H).take(54).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "\r\n{err_color}{BOX_TL}{BOX_H}{RESET}{err_color}{BOLD}{title}{RESET}{err_color}{fill}{BOX_TR}{RESET}\r\n",
    ));
    // Wrap long messages across lines
    for line in msg.lines() {
        let truncated = truncate(line, 50);
        out.push_str(&format!("{err_color}{BOX_V}{RESET}  {fgv}{truncated}{RESET}\r\n"));
    }
    out.push_str(&format!("{err_color}{BOX_BL}{bot}{BOX_BR}{RESET}\r\n"));
    out
}

/// Render a keyboard shortcut overlay (cheat sheet).
///
/// Shown when the user presses F1 or Ctrl+?. Renders as an absolute-positioned
/// box in the center of the chat area.
///
/// ```text
/// ╭─ Keyboard Shortcuts ──────────────────────────╮
/// │                                                │
/// │  Ctrl+T     Toggle Agent/Terminal mode         │
/// │  Ctrl+P     Command palette                    │
/// │  Ctrl+F     Quick-fix suggestion               │
/// │  ...                                           │
/// │                                                │
/// │  Press F1 or Esc to close                      │
/// ╰────────────────────────────────────────────────╯
/// ```
pub fn render_shortcut_overlay(state: &ScreenState) -> String {
    let accent = fgc(ACCENT);
    let fgv = fgc(FG);
    let muted = fgc(MUTED);
    let key_bg = bgc(SELECTION);
    let key_fg = fgc(FG);
    let r = RESET;

    let box_w = 52usize;
    let hline: String = std::iter::repeat(BOX_H).take(box_w).collect();

    let title = " Keyboard Shortcuts ";
    let title_fill_len = box_w.saturating_sub(title.len() + 1);
    let title_fill: String = std::iter::repeat(BOX_H).take(title_fill_len).collect();

    // Center the overlay vertically in the chat area
    let chat_h = state.chat_bottom().saturating_sub(state.chat_top()) + 1;
    let overlay_height = 16u16; // lines including borders
    let start_row = state.chat_top() + chat_h.saturating_sub(overlay_height) / 2;

    // Shortcuts to display
    let shortcuts: &[(&str, &str)] = &[
        ("Ctrl+T", "Toggle Agent / Terminal mode"),
        ("Ctrl+P", "Command palette"),
        ("Ctrl+F", "Quick-fix from AI suggestion"),
        ("Ctrl+L", "Clear screen"),
        ("Ctrl+C", "Cancel current operation"),
        ("Ctrl+D", "Exit / close pane"),
        ("Shift+Enter", "New line in input"),
        ("Enter", "Send message / run command"),
        ("Tab", "Accept ghost text completion"),
        ("!command", "Run shell command directly"),
        ("@file.rs", "Attach file as context"),
        ("F1", "Toggle this overlay"),
    ];

    let mut out = String::new();
    // Save cursor
    out.push_str("\x1b[s");
    out.push_str(HIDE_CURSOR);

    // Indent from left edge
    let indent = 4u16;
    let col = indent;

    let mut row = start_row;

    // Top border
    out.push_str(&goto(row, col));
    out.push_str(&format!("{accent}{BOX_TL}{BOX_H}{r}{accent}{BOLD}{title}{r}{accent}{title_fill}{BOX_TR}{r}"));
    row += 1;

    // Blank line
    let inner_pad: String = " ".repeat(box_w);
    out.push_str(&goto(row, col));
    out.push_str(&format!("{accent}{BOX_V}{r}{inner_pad}{accent}{BOX_V}{r}"));
    row += 1;

    // Shortcut rows
    for (key, desc) in shortcuts {
        out.push_str(&goto(row, col));
        let key_str = format!("{key_bg}{key_fg} {key:<13}{r}");
        // Pad description to fill the box width
        let desc_padded = format!("{desc:<36}");
        out.push_str(&format!(
            "{accent}{BOX_V}{r}  {key_str} {fgv}{desc_padded}{r} {accent}{BOX_V}{r}",
        ));
        row += 1;
    }

    // Blank line
    out.push_str(&goto(row, col));
    out.push_str(&format!("{accent}{BOX_V}{r}{inner_pad}{accent}{BOX_V}{r}"));
    row += 1;

    // Footer hint
    out.push_str(&goto(row, col));
    let footer = format!("  {muted}Press F1 or Esc to close{r}");
    let footer_pad = box_w.saturating_sub(26);
    out.push_str(&format!(
        "{accent}{BOX_V}{r}{footer}{}{accent}{BOX_V}{r}",
        " ".repeat(footer_pad),
    ));
    row += 1;

    // Bottom border
    out.push_str(&goto(row, col));
    out.push_str(&format!("{accent}{BOX_BL}{hline}{BOX_BR}{r}"));

    // Restore cursor
    out.push_str(SHOW_CURSOR);
    out.push_str("\x1b[u");

    out
}

/// Render a suggestion overlay box in the chat area.
///
/// Displays the active suggestion from the [`SuggestionManager`] as an
/// absolute-positioned overlay at the bottom of the chat area. Shows the
/// error message, suggested fix, and keybinding hints.
///
/// ```text
/// ╭─ ⚡ Error Fix ───────────────────────────────────────╮
/// │  error[E0308]: mismatched types                       │
/// │                                                       │
/// │  Suggested fix: cargo add serde                       │
/// │                                                       │
/// │  [Enter] Apply   [Tab] Next (2 more)   [Esc] Dismiss │
/// ╰──────────────────────────────────────────────────────╯
/// ```
pub fn render_suggestion_overlay(
    state: &ScreenState,
    suggestion: &crate::suggestion_overlay::Suggestion,
    visible_count: usize,
) -> String {
    let err_fg = fgc(ERROR);
    let warn_fg = fgc(WARNING);
    let success_fg = fgc(SUCCESS);
    let fgv = fgc(FG);
    let muted = fgc(MUTED);
    let key_bg = bgc(SELECTION);
    let key_fg = fgc(FG);
    let r = RESET;

    let box_w = 56usize;
    let hline: String = std::iter::repeat(BOX_H).take(box_w).collect();

    // Pick the severity color for the title
    let severity_color = match suggestion.severity {
        crate::observer::Severity::Fatal => &err_fg,
        crate::observer::Severity::Error => &err_fg,
        crate::observer::Severity::Info => &warn_fg,
    };

    // Title based on error type
    let icon = match suggestion.error_type {
        crate::observer::ErrorType::Compile => " [!] Compile Error ",
        crate::observer::ErrorType::Runtime => " [!] Runtime Error ",
        crate::observer::ErrorType::Test => " [!] Test Failure ",
        crate::observer::ErrorType::Permission => " [!] Permission Error ",
        crate::observer::ErrorType::NotFound => " [?] Not Found ",
        crate::observer::ErrorType::Git => " [!] Git Error ",
        crate::observer::ErrorType::General => " [!] Error ",
    };

    let title_fill_len = box_w.saturating_sub(icon.len() + 1);
    let title_fill: String = std::iter::repeat(BOX_H).take(title_fill_len).collect();

    // Position: bottom of the chat area
    let chat_h = state.chat_bottom().saturating_sub(state.chat_top()) + 1;
    let overlay_height = 8u16; // total lines including borders
    let start_row = state.chat_top() + chat_h.saturating_sub(overlay_height);
    let indent = 4u16;
    let col = indent;

    let mut out = String::new();
    out.push_str("\x1b[s"); // save cursor
    out.push_str(HIDE_CURSOR);

    let mut row = start_row;

    // Top border
    out.push_str(&goto(row, col));
    out.push_str(&format!(
        "{severity_color}{BOX_TL}{BOX_H}{r}{severity_color}{BOLD}{icon}{r}{severity_color}{title_fill}{BOX_TR}{r}",
    ));
    row += 1;

    // Error message (truncated to fit)
    let msg_max = box_w.saturating_sub(4);
    let msg = truncate(&suggestion.message, msg_max);
    let msg_pad = box_w.saturating_sub(msg.len() + 2);
    out.push_str(&goto(row, col));
    out.push_str(&format!(
        "{severity_color}{BOX_V}{r}  {fgv}{msg}{r}{}{severity_color}{BOX_V}{r}",
        " ".repeat(msg_pad),
    ));
    row += 1;

    // Blank line
    let inner_pad: String = " ".repeat(box_w);
    out.push_str(&goto(row, col));
    out.push_str(&format!("{severity_color}{BOX_V}{r}{inner_pad}{severity_color}{BOX_V}{r}"));
    row += 1;

    // Suggested fix
    let fix_prefix = if suggestion.auto_fixable { "Run: " } else { "Fix: " };
    let fix_max = box_w.saturating_sub(fix_prefix.len() + 4);
    let fix_text = truncate(&suggestion.suggested_fix, fix_max);
    let fix_line = format!("{fix_prefix}{fix_text}");
    let fix_pad = box_w.saturating_sub(fix_line.len() + 2);
    out.push_str(&goto(row, col));
    out.push_str(&format!(
        "{severity_color}{BOX_V}{r}  {success_fg}{BOLD}{fix_prefix}{r}{fgv}{fix_text}{r}{}{severity_color}{BOX_V}{r}",
        " ".repeat(fix_pad),
    ));
    row += 1;

    // Blank line
    out.push_str(&goto(row, col));
    out.push_str(&format!("{severity_color}{BOX_V}{r}{inner_pad}{severity_color}{BOX_V}{r}"));
    row += 1;

    // Keybinding hints
    let more = if visible_count > 1 {
        format!(" ({} more)", visible_count - 1)
    } else {
        String::new()
    };
    let keys_line = format!(
        " {key_bg}{key_fg} Enter {r} {muted}Apply   {key_bg}{key_fg} Tab {r} {muted}Next{more}   {key_bg}{key_fg} Esc {r} {muted}Dismiss{r}",
    );
    // Calculate visible length for padding
    let keys_visible_len = 6 + 8 + 4 + 4 + more.len() + 9 + 4 + 7; // approximate
    let keys_pad = box_w.saturating_sub(keys_visible_len + 1);
    out.push_str(&goto(row, col));
    out.push_str(&format!(
        "{severity_color}{BOX_V}{r}{keys_line}{}{severity_color}{BOX_V}{r}",
        " ".repeat(keys_pad),
    ));
    row += 1;

    // Bottom border
    out.push_str(&goto(row, col));
    out.push_str(&format!("{severity_color}{BOX_BL}{hline}{BOX_BR}{r}"));

    // Restore cursor
    out.push_str(SHOW_CURSOR);
    out.push_str("\x1b[u");

    out
}

/// Render a compact header line for a block in a listing (e.g. `/bookmarks`).
///
/// Layout:
/// ```text
///  ▾ $ command   ✓ (1.2s) ★
/// ```
///
/// - Collapse indicator: `▸` collapsed, `▾` expanded
/// - Exit code badge: green `✓` for 0, red `✗ N` for non-zero
/// - Duration if available in dim
/// - `★` bookmark indicator
/// - Selected blocks get a highlight background
pub fn render_block_header(block: &crate::block::Block, selected: bool) -> String {
    let muted = fgc(MUTED);
    let border_col = fgc(BORDER);
    let r = RESET;

    let mut out = String::new();

    // Selection highlight background
    if selected {
        out.push_str(&bgc(SELECTION));
    }

    // Collapse indicator
    let collapse_icon = if block.collapsed { "\u{25b8}" } else { "\u{25be}" };
    out.push_str(&format!(" {muted}{collapse_icon}{r}"));

    // Restore bg after indicator if selected
    if selected {
        out.push_str(&bgc(SELECTION));
    }

    // Block ID
    out.push_str(&format!(" {border_col}#{}{r}", block.id));
    if selected {
        out.push_str(&bgc(SELECTION));
    }

    // Exit code badge
    match block.exit_code {
        Some(0) => {
            out.push_str(&format!("  {}{BOLD}\u{2713}{r}", fgc(SUCCESS)));
            if selected { out.push_str(&bgc(SELECTION)); }
        }
        Some(n) => {
            out.push_str(&format!("  {}{BOLD}\u{2717} {n}{r}", fgc(ERROR)));
            if selected { out.push_str(&bgc(SELECTION)); }
        }
        None => {}
    }

    // Duration
    if let Some(dur) = block.duration_secs() {
        out.push_str(&format!(" {muted}{DIM}({dur:.1}s){r}"));
        if selected { out.push_str(&bgc(SELECTION)); }
    }

    // Bookmark indicator
    if block.bookmarked {
        out.push_str(&format!(" {}\u{2605}{r}", fgc(WARNING)));
        if selected { out.push_str(&bgc(SELECTION)); }
    }

    // Reset at end
    out.push_str(r);
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

/// Format a slash command response (informational text block).
///
/// Used to render the output of commands like `/help`, `/model`, `/diff`.
pub fn format_command_response(text: &str) -> String {
    let muted = fgc(MUTED);
    let fg_main = fgc(FG);
    let mut out = String::new();
    out.push_str("\r\n");
    for line in text.lines() {
        out.push_str(&format!("  {fg_main}{line}{RESET}\r\n"));
    }
    out.push_str(&format!("{muted}{RESET}\r\n"));
    out
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
        assert!(bar.contains("Agent"));
        assert!(bar.contains("gemini-2.5-pro"));
        assert!(bar.contains("F1"));
    }

    #[test]
    fn test_format_suggestion_contains_key_hint() {
        let s = format_suggestion("Compiler error");
        assert!(s.contains("Compiler error"));
        assert!(s.contains("Ctrl+F"));
        assert!(s.contains("[!]"));
    }

    #[test]
    fn test_format_next_command_suggestion() {
        let s = format_next_command_suggestion("git commit -m \"message\"");
        assert!(s.contains("Next:"));
        assert!(s.contains("git commit"));
        assert!(s.contains("[>]"));
    }

    #[test]
    fn test_muted_tab_blends() {
        let m = muted_tab(TAB_CHAT);
        // Should be darker than original
        assert!(m.0 < TAB_CHAT.0);
        assert!(m.1 < TAB_CHAT.1);
    }

    #[test]
    fn test_render_input_box_multiline() {
        let mut state = ScreenState { width: 80, height: 24, ..Default::default() };
        state.input_lines = vec!["line one".into(), "line two".into()];
        state.cursor_row = 1;
        let input = render_input_box(&state);
        assert!(input.contains("line one"));
        assert!(input.contains("line two"));
        assert!(input.contains("╭"));
        assert!(input.contains("╰"));
    }

    #[test]
    fn test_input_box_height_grows_with_lines() {
        let mut state = ScreenState { width: 80, height: 24, ..Default::default() };
        assert_eq!(state.input_box_height(), 3); // 1 content + top + bottom borders

        state.input_lines = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(state.input_box_height(), 5); // 3 content + 2 borders
    }

    #[test]
    fn test_chat_bottom_adjusts_for_input_height() {
        let mut state = ScreenState { width: 80, height: 24, ..Default::default() };
        let single_bottom = state.chat_bottom();

        state.input_lines = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let multi_bottom = state.chat_bottom();

        // Chat area shrinks when input box is taller
        assert!(multi_bottom < single_bottom);
    }

    #[test]
    fn test_input_box_caps_at_8_lines() {
        let mut state = ScreenState { width: 80, height: 40, ..Default::default() };
        state.input_lines = (0..20).map(|i| format!("line {i}")).collect();
        // Height should be capped at 8 content lines + 2 borders = 10
        assert_eq!(state.input_box_height(), 10);
    }

    #[test]
    fn test_format_error_block_chrome() {
        let err = format_error("Something went wrong");
        assert!(err.contains("Error"));
        assert!(err.contains("Something went wrong"));
        assert!(err.contains("╭"));
        assert!(err.contains("╰"));
    }

    #[test]
    fn test_format_error_multiline() {
        let err = format_error("line one\nline two");
        assert!(err.contains("line one"));
        assert!(err.contains("line two"));
    }

    #[test]
    fn test_format_tool_start_block_chrome() {
        let s = format_tool_start("ReadFile", "src/main.rs");
        assert!(s.contains("ReadFile"));
        assert!(s.contains("src/main.rs"));
        assert!(s.contains("╭"));
    }

    #[test]
    fn test_format_tool_end_success() {
        let s = format_tool_end(true, "200 lines");
        assert!(s.contains("OK"));
        assert!(s.contains("200 lines"));
        assert!(s.contains("╰"));
    }

    #[test]
    fn test_format_tool_end_failure() {
        let s = format_tool_end(false, "timeout");
        assert!(s.contains("FAIL"));
        assert!(s.contains("timeout"));
    }

    #[test]
    fn test_format_assistant_prefix_block_chrome() {
        let s = format_assistant_prefix();
        assert!(s.contains("Elwood"));
        assert!(s.contains("╭"));
    }

    #[test]
    fn test_format_turn_complete_block_chrome() {
        let s = format_turn_complete(Some("Completed in 3 steps"));
        assert!(s.contains("Done"));
        assert!(s.contains("Completed in 3 steps"));
        assert!(s.contains("╰"));
    }

    #[test]
    fn test_format_permission_request_block_chrome() {
        let s = format_permission_request("BashTool", "rm -rf /tmp/test");
        assert!(s.contains("Permission Required"));
        assert!(s.contains("BashTool"));
        assert!(s.contains("rm -rf"));
        assert!(s.contains("╭"));
        assert!(s.contains("╰"));
    }

    #[test]
    fn test_format_command_output_block_chrome() {
        let s = format_command_output("git status", "On branch main", "", Some(0));
        assert!(s.contains("git status"));
        assert!(s.contains("On branch main"));
        assert!(s.contains("exit 0"));
        assert!(s.contains("╭"));
        assert!(s.contains("╰"));
    }

    #[test]
    fn test_format_command_output_with_stderr() {
        let s = format_command_output("cargo build", "", "error[E0308]", Some(1));
        assert!(s.contains("error[E0308]"));
        assert!(s.contains("exit 1"));
    }

    #[test]
    fn test_render_shortcut_overlay() {
        let state = ScreenState { width: 80, height: 40, ..Default::default() };
        let overlay = render_shortcut_overlay(&state);
        assert!(overlay.contains("Keyboard Shortcuts"));
        assert!(overlay.contains("Ctrl+T"));
        assert!(overlay.contains("Ctrl+P"));
        assert!(overlay.contains("F1"));
        assert!(overlay.contains("╭"));
        assert!(overlay.contains("╰"));
    }

    #[test]
    fn test_format_welcome_contains_hints() {
        let w = format_welcome();
        assert!(w.contains("Elwood Terminal"));
        assert!(w.contains("Quick Start"));
        assert!(w.contains("Ctrl+T"));
        assert!(w.contains("@file.rs"));
    }

    #[test]
    fn test_render_welcome_at_contains_hints() {
        let state = ScreenState { width: 80, height: 40, ..Default::default() };
        let w = render_welcome_at(&state);
        assert!(w.contains("Elwood Terminal"));
        assert!(w.contains("Quick Start"));
        assert!(w.contains("Ctrl+T"));
    }

    #[test]
    fn test_status_bar_with_git_info() {
        let mut state = ScreenState { width: 120, height: 24, ..Default::default() };
        state.git_info = Some(GitInfo {
            branch: "main".to_string(),
            is_dirty: true,
            ahead: 2,
            behind: 0,
        });
        let bar = render_status_bar(&state);
        assert!(bar.contains("main"));
        assert!(bar.contains("*"));
    }

    #[test]
    fn test_status_bar_running_with_tool() {
        let mut state = ScreenState { width: 120, height: 24, ..Default::default() };
        state.is_running = true;
        state.active_tool = Some("ReadFile".to_string());
        state.tool_start = Some(Instant::now());
        let bar = render_status_bar(&state);
        assert!(bar.contains("ReadFile"));
    }

    #[test]
    fn test_status_bar_terminal_mode() {
        let mut state = ScreenState { width: 80, height: 24, ..Default::default() };
        state.input_mode = InputMode::Terminal;
        let bar = render_status_bar(&state);
        assert!(bar.contains("Term"));
    }

    // ── Block header rendering tests ────────────────────────────────────

    #[test]
    fn test_render_block_header_basic() {
        let block = crate::block::Block {
            id: 5,
            prompt_zone: None,
            input_zone: None,
            output_zone: Some(crate::block::ZoneRange { start_y: 0, end_y: 10 }),
            exit_code: Some(0),
            start_time: None,
            end_time: None,
            collapsed: false,
            bookmarked: false,
        };
        let header = render_block_header(&block, false);
        // Should contain: expand indicator, block ID, success check
        assert!(header.contains("#5"));
        assert!(header.contains("\u{2713}")); // checkmark
        assert!(header.contains("\u{25be}")); // down-pointing triangle (expanded)
    }

    #[test]
    fn test_render_block_header_collapsed() {
        let block = crate::block::Block {
            id: 3,
            prompt_zone: None,
            input_zone: None,
            output_zone: Some(crate::block::ZoneRange { start_y: 0, end_y: 5 }),
            exit_code: None,
            start_time: None,
            end_time: None,
            collapsed: true,
            bookmarked: false,
        };
        let header = render_block_header(&block, false);
        assert!(header.contains("\u{25b8}")); // right-pointing triangle (collapsed)
    }

    #[test]
    fn test_render_block_header_bookmarked() {
        let block = crate::block::Block {
            id: 7,
            prompt_zone: None,
            input_zone: None,
            output_zone: Some(crate::block::ZoneRange { start_y: 0, end_y: 5 }),
            exit_code: None,
            start_time: None,
            end_time: None,
            collapsed: false,
            bookmarked: true,
        };
        let header = render_block_header(&block, false);
        assert!(header.contains("\u{2605}")); // star
    }

    #[test]
    fn test_render_block_header_error_exit() {
        let block = crate::block::Block {
            id: 1,
            prompt_zone: None,
            input_zone: None,
            output_zone: Some(crate::block::ZoneRange { start_y: 0, end_y: 5 }),
            exit_code: Some(127),
            start_time: None,
            end_time: None,
            collapsed: false,
            bookmarked: false,
        };
        let header = render_block_header(&block, false);
        assert!(header.contains("\u{2717}")); // cross
        assert!(header.contains("127"));
    }

    #[test]
    fn test_render_block_header_selected() {
        let block = crate::block::Block {
            id: 2,
            prompt_zone: None,
            input_zone: None,
            output_zone: Some(crate::block::ZoneRange { start_y: 0, end_y: 5 }),
            exit_code: Some(0),
            start_time: None,
            end_time: None,
            collapsed: false,
            bookmarked: false,
        };
        let header = render_block_header(&block, true);
        // Selected header should have SELECTION background color escape
        assert!(header.contains("\x1b[48;2;40;44;66m")); // bgc(SELECTION)
    }
}
