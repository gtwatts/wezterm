//! Vim modal editing for the Elwood input editor.
//!
//! Implements a vim-like editing experience with Normal, Insert, Visual, and
//! Command modes. Designed to integrate with [`InputEditor`] — when enabled,
//! key events are routed through [`VimMode`] which returns [`VimAction`] values
//! that the editor applies to its buffer.
//!
//! ## Modes
//!
//! - **Normal**: Motions (`h/l/w/b/e/0/$`), operators (`d/c/y/x/r/p`),
//!   count prefixes (`3w`), dot repeat (`.`), `f/F` char find.
//! - **Insert**: Regular typing; `Esc` or `Ctrl+[` returns to Normal.
//! - **Visual**: Character-wise selection; motions extend selection;
//!   `d/y/c` operate on the selection.
//! - **Command**: `:` line — `:w` submits, `:q` clears, `:set paste`/`:set nopaste`.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use elwood_bridge::vim_mode::{VimMode, VimAction};
//!
//! let mut vim = VimMode::new();
//! // In the editor's key handler, delegate to vim:
//! // let action = vim.handle_key(key, modifiers, &buffer_lines, cursor_row, cursor_col);
//! // Then apply the returned VimAction to the editor buffer.
//! ```

use std::fmt;

// ─── Vim State ─────────────────────────────────────────────────────────────

/// The current editing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimState {
    /// Normal mode — motions and operators.
    Normal,
    /// Insert mode — regular character typing.
    Insert,
    /// Visual (character-wise) selection mode.
    Visual,
    /// Command-line mode (`:` prefix).
    Command,
}

impl fmt::Display for VimState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VimState::Normal => write!(f, "-- NORMAL --"),
            VimState::Insert => write!(f, "-- INSERT --"),
            VimState::Visual => write!(f, "-- VISUAL --"),
            VimState::Command => write!(f, ":"),
        }
    }
}

// ─── Pending Operator ──────────────────────────────────────────────────────

/// An operator waiting for a motion (e.g., `d` waits for `w` to become `dw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}

// ─── Vim Action ────────────────────────────────────────────────────────────

/// Action returned by `VimMode::handle_key` — the editor applies these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimAction {
    /// No-op — key was consumed but no buffer change needed.
    NoOp,
    /// Insert a character at the current cursor position.
    InsertChar(char),
    /// Insert a newline (split line) at cursor.
    InsertNewline,
    /// Delete a range of text: (start_row, start_col) .. (end_row, end_col).
    DeleteRange {
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    },
    /// Delete the entire line at the given row.
    DeleteLine(usize),
    /// Move the cursor to (row, col).
    MoveCursor { row: usize, col: usize },
    /// Change mode (also moves cursor if transitioning to normal).
    ChangeMode(VimState),
    /// Replace a single character at (row, col) with the given char.
    ReplaceChar { row: usize, col: usize, ch: char },
    /// Paste text after cursor.
    PasteAfter(String),
    /// Paste text before cursor.
    PasteBefore(String),
    /// Submit the input (equivalent to pressing Enter).
    Submit,
    /// Clear the input buffer.
    ClearInput,
    /// Undo last change.
    Undo,
    /// Redo last undone change.
    Redo,
    /// Backspace in insert mode.
    Backspace,
    /// A command-line command was entered (e.g., `:set paste`).
    CommandOutput(String),
    /// Multiple actions to execute in sequence.
    Batch(Vec<VimAction>),
}

// ─── Recorded Command (for dot repeat) ─────────────────────────────────────

/// A recorded command for `.` (dot) repeat.
#[derive(Debug, Clone)]
struct RecordedCommand {
    /// The operator (if any).
    operator: Option<Operator>,
    /// Count prefix.
    count: usize,
    /// The motion key(s) that completed the command.
    keys: Vec<char>,
    /// For insert commands, the text that was typed before returning to normal.
    inserted_text: Option<String>,
}

// ─── VimMode ───────────────────────────────────────────────────────────────

/// Vim modal editing state machine.
///
/// Integrates with `InputEditor` by processing key events and returning
/// `VimAction` values. The editor applies the actions to its buffer.
#[derive(Debug, Clone)]
pub struct VimMode {
    /// Current vim state/mode.
    state: VimState,
    /// Yank register (clipboard).
    register: String,
    /// Whether the register contains a full line (for `p`/`P` line-wise paste).
    register_linewise: bool,
    /// Accumulated count prefix (e.g., the `3` in `3w`).
    count_prefix: Option<usize>,
    /// Pending operator awaiting a motion.
    pending_operator: Option<Operator>,
    /// Last completed command (for `.` repeat).
    last_command: Option<RecordedCommand>,
    /// Keys accumulated for the current command.
    current_keys: Vec<char>,
    /// Text inserted during the current insert session (for dot repeat).
    insert_buffer: String,
    /// Visual mode anchor position (row, col).
    visual_anchor: Option<(usize, usize)>,
    /// Command-line buffer (for `:` mode).
    command_buffer: String,
    /// Waiting for a character argument (after `f`, `F`, `r`).
    awaiting_char: Option<char>,
}

impl VimMode {
    /// Create a new VimMode in Normal state.
    pub fn new() -> Self {
        Self {
            state: VimState::Normal,
            register: String::new(),
            register_linewise: false,
            count_prefix: None,
            pending_operator: None,
            last_command: None,
            current_keys: Vec::new(),
            insert_buffer: String::new(),
            visual_anchor: None,
            command_buffer: String::new(),
            awaiting_char: None,
        }
    }

    /// Current vim state.
    pub fn state(&self) -> VimState {
        self.state
    }

    /// The command-line buffer content (for rendering in command mode).
    pub fn command_buffer(&self) -> &str {
        &self.command_buffer
    }

    /// The yank register content.
    pub fn register(&self) -> &str {
        &self.register
    }

    /// Visual mode anchor position, if in visual mode.
    pub fn visual_anchor(&self) -> Option<(usize, usize)> {
        self.visual_anchor
    }

    /// Process a key event and return the action the editor should take.
    ///
    /// `lines`: current buffer lines, `cursor_row`/`cursor_col`: current position.
    /// `key`: the character (or control key mapped to char), `ctrl`: Ctrl held,
    /// `shift`: Shift held.
    pub fn handle_key(
        &mut self,
        key: char,
        ctrl: bool,
        lines: &[String],
        cursor_row: usize,
        cursor_col: usize,
    ) -> VimAction {
        match self.state {
            VimState::Normal => self.handle_normal(key, ctrl, lines, cursor_row, cursor_col),
            VimState::Insert => self.handle_insert(key, ctrl),
            VimState::Visual => self.handle_visual(key, ctrl, lines, cursor_row, cursor_col),
            VimState::Command => self.handle_command(key, ctrl),
        }
    }

    // ─── Normal Mode ───────────────────────────────────────────────────

    fn handle_normal(
        &mut self,
        key: char,
        ctrl: bool,
        lines: &[String],
        cursor_row: usize,
        cursor_col: usize,
    ) -> VimAction {
        // Handle Ctrl+R for redo in normal mode
        if ctrl && (key == 'r' || key == 'R' || key == '\x12') {
            return VimAction::Redo;
        }

        // Handle awaiting char (for f/F/r)
        if let Some(cmd) = self.awaiting_char.take() {
            return self.handle_char_argument(cmd, key, lines, cursor_row, cursor_col);
        }

        // Count prefix accumulation (digits 1-9 start, 0 only continues)
        if key.is_ascii_digit() {
            let digit = key as usize - '0' as usize;
            if key != '0' || self.count_prefix.is_some() {
                let current = self.count_prefix.unwrap_or(0);
                self.count_prefix = Some(current * 10 + digit);
                return VimAction::NoOp;
            }
        }

        let count = self.count_prefix.take().unwrap_or(1);
        self.current_keys.push(key);

        // If we have a pending operator, the next key is a motion
        if let Some(op) = self.pending_operator {
            // Handle doubled operator (dd, cc, yy)
            let doubled = match op {
                Operator::Delete => key == 'd',
                Operator::Change => key == 'c',
                Operator::Yank => key == 'y',
            };
            if doubled {
                return self.execute_line_operator(op, count, lines, cursor_row);
            }

            // Special: d$/c$/y$ — to end of line
            if key == '$' {
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                return self.execute_operator_range(
                    op, cursor_row, cursor_col, cursor_row, line_len, lines,
                );
            }

            // Special: d0/c0/y0 — to start of line
            if key == '0' {
                return self.execute_operator_range(
                    op, cursor_row, 0, cursor_row, cursor_col, lines,
                );
            }

            // Motion-based operator
            if let Some((target_row, target_col)) =
                self.resolve_motion(key, count, lines, cursor_row, cursor_col)
            {
                return self.execute_operator_motion(
                    op, cursor_row, cursor_col, target_row, target_col, lines,
                );
            }

            // f/F with pending operator
            if key == 'f' || key == 'F' {
                self.awaiting_char = Some(key);
                return VimAction::NoOp;
            }

            // Unrecognized motion — cancel operator
            self.pending_operator = None;
            self.current_keys.clear();
            return VimAction::NoOp;
        }

        match key {
            // ── Mode transitions ──────────────────────────────────
            'i' => {
                self.enter_insert();
                VimAction::ChangeMode(VimState::Insert)
            }
            'a' => {
                self.enter_insert();
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                let new_col = (cursor_col + 1).min(line_len);
                VimAction::Batch(vec![
                    VimAction::MoveCursor { row: cursor_row, col: new_col },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            'A' => {
                self.enter_insert();
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                VimAction::Batch(vec![
                    VimAction::MoveCursor { row: cursor_row, col: line_len },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            'I' => {
                self.enter_insert();
                let first_non_ws = first_non_whitespace(
                    lines.get(cursor_row).map(|s| s.as_str()).unwrap_or(""),
                );
                VimAction::Batch(vec![
                    VimAction::MoveCursor { row: cursor_row, col: first_non_ws },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            'o' => {
                self.enter_insert();
                // Open line below: we signal InsertNewline at end of current line
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                VimAction::Batch(vec![
                    VimAction::MoveCursor { row: cursor_row, col: line_len },
                    VimAction::InsertNewline,
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            'O' => {
                self.enter_insert();
                // Open line above: move to start of current line, insert newline, move up
                VimAction::Batch(vec![
                    VimAction::MoveCursor { row: cursor_row, col: 0 },
                    VimAction::InsertNewline,
                    VimAction::MoveCursor { row: cursor_row, col: 0 },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            'v' => {
                self.state = VimState::Visual;
                self.visual_anchor = Some((cursor_row, cursor_col));
                self.current_keys.clear();
                VimAction::ChangeMode(VimState::Visual)
            }
            ':' => {
                self.state = VimState::Command;
                self.command_buffer.clear();
                self.current_keys.clear();
                VimAction::ChangeMode(VimState::Command)
            }

            // ── Motions ───────────────────────────────────────────
            'h' | 'l' | 'w' | 'b' | 'e' | '0' | '$' | '^' | 'j' | 'k' => {
                if let Some((row, col)) =
                    self.resolve_motion(key, count, lines, cursor_row, cursor_col)
                {
                    self.current_keys.clear();
                    VimAction::MoveCursor { row, col }
                } else {
                    self.current_keys.clear();
                    VimAction::NoOp
                }
            }
            'g' => {
                // Check for 'gg' sequence (current_keys already has this 'g' pushed)
                if self.current_keys.len() >= 2
                    && self.current_keys[self.current_keys.len() - 2] == 'g'
                {
                    self.current_keys.clear();
                    VimAction::MoveCursor { row: 0, col: 0 }
                } else {
                    // Wait for second key
                    VimAction::NoOp
                }
            }
            'G' => {
                self.current_keys.clear();
                let last_row = lines.len().saturating_sub(1);
                let col = lines.get(last_row).map(|l| l.len()).unwrap_or(0);
                VimAction::MoveCursor { row: last_row, col: col.saturating_sub(1).max(0) }
            }

            // Handle 'gg' if current_keys has ['g', 'g']
            // (Note: first 'g' was already pushed; but we handle the second here)
            // Actually, the first 'g' case above returns NoOp and pushes to current_keys.
            // So on the second key press, we need to check if previous was 'g'.
            // Let me restructure: check current_keys length.

            // ── f/F char find ─────────────────────────────────────
            'f' | 'F' => {
                self.awaiting_char = Some(key);
                VimAction::NoOp
            }

            // ── Operators ─────────────────────────────────────────
            'd' => {
                self.pending_operator = Some(Operator::Delete);
                VimAction::NoOp
            }
            'c' => {
                self.pending_operator = Some(Operator::Change);
                VimAction::NoOp
            }
            'y' => {
                self.pending_operator = Some(Operator::Yank);
                VimAction::NoOp
            }

            // ── Single-key operators ──────────────────────────────
            'x' => {
                self.record_command(None, count, vec!['x'], None);
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                if cursor_col < line_len {
                    let end_col = advance_n_chars(
                        lines.get(cursor_row).map(|s| s.as_str()).unwrap_or(""),
                        cursor_col,
                        count,
                    );
                    // Yank deleted text
                    if let Some(line) = lines.get(cursor_row) {
                        let end = end_col.min(line.len());
                        self.register = line[cursor_col..end].to_string();
                        self.register_linewise = false;
                    }
                    self.current_keys.clear();
                    VimAction::DeleteRange {
                        start_row: cursor_row,
                        start_col: cursor_col,
                        end_row: cursor_row,
                        end_col: end_col.min(line_len),
                    }
                } else {
                    self.current_keys.clear();
                    VimAction::NoOp
                }
            }
            'r' => {
                self.awaiting_char = Some('r');
                VimAction::NoOp
            }

            // ── Paste ─────────────────────────────────────────────
            'p' => {
                self.current_keys.clear();
                if self.register.is_empty() {
                    VimAction::NoOp
                } else if self.register_linewise {
                    // Line-wise paste: insert on next line
                    let text = self.register.clone();
                    VimAction::PasteAfter(text)
                } else {
                    let text = self.register.clone();
                    VimAction::PasteAfter(text)
                }
            }
            'P' => {
                self.current_keys.clear();
                if self.register.is_empty() {
                    VimAction::NoOp
                } else {
                    let text = self.register.clone();
                    VimAction::PasteBefore(text)
                }
            }

            // ── Undo/Redo ─────────────────────────────────────────
            'u' => {
                self.current_keys.clear();
                VimAction::Undo
            }

            // ── Dot repeat ────────────────────────────────────────
            '.' => {
                self.current_keys.clear();
                self.execute_dot_repeat(lines, cursor_row, cursor_col)
            }

            _ => {
                self.current_keys.clear();
                VimAction::NoOp
            }
        }
    }

    // ─── Insert Mode ───────────────────────────────────────────────────

    fn handle_insert(&mut self, key: char, ctrl: bool) -> VimAction {
        // Escape or Ctrl+[ returns to normal
        if key == '\x1b' || (ctrl && key == '[') {
            return self.exit_insert();
        }

        // Ctrl+C also exits insert mode (like vim)
        if ctrl && key == 'c' {
            return self.exit_insert();
        }

        // Backspace
        if key == '\x08' || key == '\x7f' {
            self.insert_buffer.push('\x08'); // record backspace
            return VimAction::Backspace;
        }

        // Enter in insert mode
        if key == '\r' || key == '\n' {
            self.insert_buffer.push('\n');
            return VimAction::InsertNewline;
        }

        // Regular character
        if !ctrl && !key.is_control() {
            self.insert_buffer.push(key);
            return VimAction::InsertChar(key);
        }

        VimAction::NoOp
    }

    fn enter_insert(&mut self) {
        self.state = VimState::Insert;
        self.insert_buffer.clear();
        self.current_keys.clear();
    }

    fn exit_insert(&mut self) -> VimAction {
        let inserted = if self.insert_buffer.is_empty() {
            None
        } else {
            Some(self.insert_buffer.clone())
        };

        // Record the insert command for dot repeat
        if !self.current_keys.is_empty() || inserted.is_some() {
            // The entry keys are already in current_keys from the command that started insert
            self.last_command = Some(RecordedCommand {
                operator: None,
                count: 1,
                keys: vec!['i'], // simplified — the insert mode entry key
                inserted_text: inserted,
            });
        }

        self.state = VimState::Normal;
        self.insert_buffer.clear();
        self.current_keys.clear();
        self.pending_operator = None;
        VimAction::ChangeMode(VimState::Normal)
    }

    // ─── Visual Mode ───────────────────────────────────────────────────

    fn handle_visual(
        &mut self,
        key: char,
        ctrl: bool,
        lines: &[String],
        cursor_row: usize,
        cursor_col: usize,
    ) -> VimAction {
        // Escape exits visual mode
        if key == '\x1b' || (ctrl && key == '[') || key == 'v' {
            self.state = VimState::Normal;
            self.visual_anchor = None;
            self.current_keys.clear();
            return VimAction::ChangeMode(VimState::Normal);
        }

        let count = self.count_prefix.take().unwrap_or(1);

        // Motions extend the selection (move cursor, anchor stays)
        match key {
            'h' | 'l' | 'w' | 'b' | 'e' | '0' | '$' | '^' | 'j' | 'k' => {
                if let Some((row, col)) =
                    self.resolve_motion(key, count, lines, cursor_row, cursor_col)
                {
                    return VimAction::MoveCursor { row, col };
                }
                VimAction::NoOp
            }
            // Operators on selection
            'd' | 'x' => {
                if let Some((anchor_row, anchor_col)) = self.visual_anchor.take() {
                    let (sr, sc, er, ec) =
                        normalize_range(anchor_row, anchor_col, cursor_row, cursor_col);
                    // Yank before delete
                    self.yank_range(lines, sr, sc, er, ec);
                    self.register_linewise = false;
                    self.state = VimState::Normal;
                    VimAction::Batch(vec![
                        VimAction::DeleteRange {
                            start_row: sr,
                            start_col: sc,
                            end_row: er,
                            end_col: ec,
                        },
                        VimAction::ChangeMode(VimState::Normal),
                    ])
                } else {
                    self.state = VimState::Normal;
                    VimAction::ChangeMode(VimState::Normal)
                }
            }
            'y' => {
                if let Some((anchor_row, anchor_col)) = self.visual_anchor.take() {
                    let (sr, sc, er, ec) =
                        normalize_range(anchor_row, anchor_col, cursor_row, cursor_col);
                    self.yank_range(lines, sr, sc, er, ec);
                    self.register_linewise = false;
                    self.state = VimState::Normal;
                    // Move cursor to start of selection
                    VimAction::Batch(vec![
                        VimAction::MoveCursor { row: sr, col: sc },
                        VimAction::ChangeMode(VimState::Normal),
                    ])
                } else {
                    self.state = VimState::Normal;
                    VimAction::ChangeMode(VimState::Normal)
                }
            }
            'c' => {
                if let Some((anchor_row, anchor_col)) = self.visual_anchor.take() {
                    let (sr, sc, er, ec) =
                        normalize_range(anchor_row, anchor_col, cursor_row, cursor_col);
                    self.yank_range(lines, sr, sc, er, ec);
                    self.register_linewise = false;
                    self.state = VimState::Insert;
                    self.enter_insert();
                    VimAction::Batch(vec![
                        VimAction::DeleteRange {
                            start_row: sr,
                            start_col: sc,
                            end_row: er,
                            end_col: ec,
                        },
                        VimAction::ChangeMode(VimState::Insert),
                    ])
                } else {
                    self.state = VimState::Normal;
                    VimAction::ChangeMode(VimState::Normal)
                }
            }
            _ => VimAction::NoOp,
        }
    }

    // ─── Command Mode ──────────────────────────────────────────────────

    fn handle_command(&mut self, key: char, ctrl: bool) -> VimAction {
        // Escape cancels
        if key == '\x1b' || (ctrl && key == '[') {
            self.state = VimState::Normal;
            self.command_buffer.clear();
            self.current_keys.clear();
            return VimAction::ChangeMode(VimState::Normal);
        }

        // Enter executes command
        if key == '\r' || key == '\n' {
            let cmd = self.command_buffer.clone();
            self.state = VimState::Normal;
            self.command_buffer.clear();
            self.current_keys.clear();
            return self.execute_ex_command(&cmd);
        }

        // Backspace
        if key == '\x08' || key == '\x7f' {
            if self.command_buffer.is_empty() {
                self.state = VimState::Normal;
                self.current_keys.clear();
                return VimAction::ChangeMode(VimState::Normal);
            }
            self.command_buffer.pop();
            return VimAction::NoOp;
        }

        // Regular character
        if !key.is_control() {
            self.command_buffer.push(key);
        }
        VimAction::NoOp
    }

    fn execute_ex_command(&self, cmd: &str) -> VimAction {
        let cmd = cmd.trim();
        match cmd {
            "w" => VimAction::Submit,
            "q" => VimAction::ClearInput,
            "wq" => VimAction::Submit,
            "set paste" => VimAction::CommandOutput("Paste mode ON".to_string()),
            "set nopaste" => VimAction::CommandOutput("Paste mode OFF".to_string()),
            _ => VimAction::CommandOutput(format!("Unknown command: {cmd}")),
        }
    }

    // ─── Motion Resolution ─────────────────────────────────────────────

    /// Resolve a motion key to a target (row, col) position.
    fn resolve_motion(
        &self,
        key: char,
        count: usize,
        lines: &[String],
        row: usize,
        col: usize,
    ) -> Option<(usize, usize)> {
        let line = lines.get(row).map(|s| s.as_str()).unwrap_or("");
        let line_len = line.len();

        match key {
            'h' => {
                let new_col = retreat_n_chars(line, col, count);
                Some((row, new_col))
            }
            'l' => {
                // In normal mode, cursor can't go past last char (len-1 for non-empty)
                let max_col = if line_len > 0 { line_len.saturating_sub(1) } else { 0 };
                let new_col = advance_n_chars(line, col, count).min(max_col);
                Some((row, new_col))
            }
            'w' => {
                let (r, c) = word_forward(lines, row, col, count);
                Some((r, c))
            }
            'b' => {
                let (r, c) = word_backward(lines, row, col, count);
                Some((r, c))
            }
            'e' => {
                let (r, c) = word_end(lines, row, col, count);
                Some((r, c))
            }
            '0' => Some((row, 0)),
            '$' => {
                let end = if line_len > 0 { line_len.saturating_sub(1) } else { 0 };
                Some((row, end))
            }
            '^' => {
                let pos = first_non_whitespace(line);
                Some((row, pos))
            }
            'j' => {
                let target_row = (row + count).min(lines.len().saturating_sub(1));
                let target_line = lines.get(target_row).map(|s| s.as_str()).unwrap_or("");
                let new_col = col.min(target_line.len().saturating_sub(1).max(0));
                // Clamp to char boundary
                let new_col = clamp_to_char_boundary(target_line, new_col);
                Some((target_row, new_col))
            }
            'k' => {
                let target_row = row.saturating_sub(count);
                let target_line = lines.get(target_row).map(|s| s.as_str()).unwrap_or("");
                let new_col = col.min(target_line.len().saturating_sub(1).max(0));
                let new_col = clamp_to_char_boundary(target_line, new_col);
                Some((target_row, new_col))
            }
            _ => None,
        }
    }

    // ─── Character Argument Handling ───────────────────────────────────

    fn handle_char_argument(
        &mut self,
        cmd: char,
        target: char,
        lines: &[String],
        cursor_row: usize,
        cursor_col: usize,
    ) -> VimAction {
        let count = self.count_prefix.take().unwrap_or(1);
        let line = lines.get(cursor_row).map(|s| s.as_str()).unwrap_or("");

        match cmd {
            'f' => {
                if let Some(pos) = find_char_forward(line, cursor_col, target, count) {
                    // If there's a pending operator, execute it
                    if let Some(op) = self.pending_operator.take() {
                        let end = advance_n_chars(line, pos, 1); // inclusive
                        return self.execute_operator_range(
                            op, cursor_row, cursor_col, cursor_row, end, lines,
                        );
                    }
                    self.current_keys.clear();
                    VimAction::MoveCursor { row: cursor_row, col: pos }
                } else {
                    self.pending_operator = None;
                    self.current_keys.clear();
                    VimAction::NoOp
                }
            }
            'F' => {
                if let Some(pos) = find_char_backward(line, cursor_col, target, count) {
                    if let Some(op) = self.pending_operator.take() {
                        return self.execute_operator_range(
                            op, cursor_row, pos, cursor_row, cursor_col, lines,
                        );
                    }
                    self.current_keys.clear();
                    VimAction::MoveCursor { row: cursor_row, col: pos }
                } else {
                    self.pending_operator = None;
                    self.current_keys.clear();
                    VimAction::NoOp
                }
            }
            'r' => {
                self.record_command(None, 1, vec!['r', target], None);
                self.current_keys.clear();
                VimAction::ReplaceChar {
                    row: cursor_row,
                    col: cursor_col,
                    ch: target,
                }
            }
            _ => {
                self.current_keys.clear();
                VimAction::NoOp
            }
        }
    }

    // ─── Operator Execution ────────────────────────────────────────────

    fn execute_line_operator(
        &mut self,
        op: Operator,
        count: usize,
        lines: &[String],
        cursor_row: usize,
    ) -> VimAction {
        self.pending_operator = None;

        // Yank the line(s)
        let end_row = (cursor_row + count).min(lines.len());
        let yanked: Vec<&str> = lines[cursor_row..end_row].iter().map(|s| s.as_str()).collect();
        self.register = yanked.join("\n");
        self.register_linewise = true;

        self.record_command(Some(op), count, self.current_keys.clone(), None);
        self.current_keys.clear();

        match op {
            Operator::Delete => {
                // dd: delete line
                VimAction::DeleteLine(cursor_row)
            }
            Operator::Change => {
                // cc: delete line content and enter insert
                self.enter_insert();
                let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                VimAction::Batch(vec![
                    VimAction::DeleteRange {
                        start_row: cursor_row,
                        start_col: 0,
                        end_row: cursor_row,
                        end_col: line_len,
                    },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            Operator::Yank => {
                // yy: yank line (already done above)
                VimAction::NoOp
            }
        }
    }

    fn execute_operator_motion(
        &mut self,
        op: Operator,
        from_row: usize,
        from_col: usize,
        to_row: usize,
        to_col: usize,
        lines: &[String],
    ) -> VimAction {
        self.pending_operator = None;

        let (sr, sc, er, ec) = normalize_range(from_row, from_col, to_row, to_col);

        // Yank the range
        self.yank_range(lines, sr, sc, er, ec);
        self.register_linewise = false;

        self.record_command(Some(op), 1, self.current_keys.clone(), None);
        self.current_keys.clear();

        match op {
            Operator::Delete => VimAction::DeleteRange {
                start_row: sr,
                start_col: sc,
                end_row: er,
                end_col: ec,
            },
            Operator::Change => {
                self.enter_insert();
                VimAction::Batch(vec![
                    VimAction::DeleteRange {
                        start_row: sr,
                        start_col: sc,
                        end_row: er,
                        end_col: ec,
                    },
                    VimAction::ChangeMode(VimState::Insert),
                ])
            }
            Operator::Yank => {
                // Move cursor to start of yanked region
                VimAction::MoveCursor { row: sr, col: sc }
            }
        }
    }

    fn execute_operator_range(
        &mut self,
        op: Operator,
        from_row: usize,
        from_col: usize,
        to_row: usize,
        to_col: usize,
        lines: &[String],
    ) -> VimAction {
        self.execute_operator_motion(op, from_row, from_col, to_row, to_col, lines)
    }

    // ─── Yank Helper ───────────────────────────────────────────────────

    fn yank_range(
        &mut self,
        lines: &[String],
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) {
        if start_row == end_row {
            if let Some(line) = lines.get(start_row) {
                let s = start_col.min(line.len());
                let e = end_col.min(line.len());
                if s <= e {
                    self.register = line[s..e].to_string();
                }
            }
        } else {
            let mut result = String::new();
            for row in start_row..=end_row {
                if let Some(line) = lines.get(row) {
                    if row == start_row {
                        result.push_str(&line[start_col.min(line.len())..]);
                    } else if row == end_row {
                        result.push('\n');
                        result.push_str(&line[..end_col.min(line.len())]);
                    } else {
                        result.push('\n');
                        result.push_str(line);
                    }
                }
            }
            self.register = result;
        }
    }

    // ─── Dot Repeat ────────────────────────────────────────────────────

    fn record_command(
        &mut self,
        operator: Option<Operator>,
        count: usize,
        keys: Vec<char>,
        inserted_text: Option<String>,
    ) {
        self.last_command = Some(RecordedCommand {
            operator,
            count,
            keys,
            inserted_text,
        });
    }

    fn execute_dot_repeat(
        &mut self,
        lines: &[String],
        cursor_row: usize,
        cursor_col: usize,
    ) -> VimAction {
        let cmd = match &self.last_command {
            Some(c) => c.clone(),
            None => return VimAction::NoOp,
        };

        // Replay the command
        let mut actions = Vec::new();

        if let Some(op) = cmd.operator {
            // Operator-motion command
            if cmd.keys.len() >= 2 {
                let motion_key = *cmd.keys.last().unwrap_or(&' ');
                // Check for line-wise operator (dd, cc, yy)
                let is_line_op = match op {
                    Operator::Delete => motion_key == 'd',
                    Operator::Change => motion_key == 'c',
                    Operator::Yank => motion_key == 'y',
                };
                if is_line_op {
                    return self.execute_line_operator(op, cmd.count, lines, cursor_row);
                }
                // Motion-based
                if let Some((tr, tc)) =
                    self.resolve_motion(motion_key, cmd.count, lines, cursor_row, cursor_col)
                {
                    // Re-record the command (so subsequent dots work)
                    self.last_command = Some(cmd);
                    return self.execute_operator_motion(
                        op, cursor_row, cursor_col, tr, tc, lines,
                    );
                }
            }
        } else if let Some(ref inserted) = cmd.inserted_text {
            // Insert command — replay the typed text
            for ch in inserted.chars() {
                if ch == '\x08' {
                    actions.push(VimAction::Backspace);
                } else if ch == '\n' {
                    actions.push(VimAction::InsertNewline);
                } else {
                    actions.push(VimAction::InsertChar(ch));
                }
            }
            // Re-record for subsequent dots
            self.last_command = Some(cmd);
            if actions.len() == 1 {
                return actions.into_iter().next().unwrap_or(VimAction::NoOp);
            }
            return VimAction::Batch(actions);
        } else {
            // Simple command like 'x'
            for &k in &cmd.keys {
                if k == 'x' {
                    let line_len = lines.get(cursor_row).map(|l| l.len()).unwrap_or(0);
                    if cursor_col < line_len {
                        let end_col = advance_n_chars(
                            lines.get(cursor_row).map(|s| s.as_str()).unwrap_or(""),
                            cursor_col,
                            cmd.count,
                        );
                        actions.push(VimAction::DeleteRange {
                            start_row: cursor_row,
                            start_col: cursor_col,
                            end_row: cursor_row,
                            end_col: end_col.min(line_len),
                        });
                    }
                }
            }
            // Re-record for subsequent dots
            self.last_command = Some(cmd);
            if actions.len() == 1 {
                return actions.into_iter().next().unwrap_or(VimAction::NoOp);
            }
            if !actions.is_empty() {
                return VimAction::Batch(actions);
            }
        }

        VimAction::NoOp
    }
}

impl Default for VimMode {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Motion Helpers ────────────────────────────────────────────────────────

/// Advance `n` characters forward from byte position `col` in `line`.
fn advance_n_chars(line: &str, col: usize, n: usize) -> usize {
    let mut pos = col;
    for _ in 0..n {
        if pos >= line.len() {
            break;
        }
        // Find next char boundary
        let mut next = pos + 1;
        while next < line.len() && !line.is_char_boundary(next) {
            next += 1;
        }
        pos = next;
    }
    pos.min(line.len())
}

/// Retreat `n` characters backward from byte position `col` in `line`.
fn retreat_n_chars(line: &str, col: usize, n: usize) -> usize {
    let mut pos = col;
    for _ in 0..n {
        if pos == 0 {
            break;
        }
        let mut prev = pos - 1;
        while prev > 0 && !line.is_char_boundary(prev) {
            prev -= 1;
        }
        pos = prev;
    }
    pos
}

/// Find the first non-whitespace byte position in `line`.
fn first_non_whitespace(line: &str) -> usize {
    line.bytes()
        .position(|b| b != b' ' && b != b'\t')
        .unwrap_or(0)
}

/// Clamp a byte position to the nearest valid char boundary.
fn clamp_to_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Move forward by `count` words (vim `w` motion).
fn word_forward(lines: &[String], row: usize, col: usize, count: usize) -> (usize, usize) {
    let mut r = row;
    let mut c = col;

    for _ in 0..count {
        let line = lines.get(r).map(|s| s.as_str()).unwrap_or("");

        if c >= line.len() {
            // At end of line — move to next line
            if r + 1 < lines.len() {
                r += 1;
                // Skip leading whitespace on the new line
                let next_line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
                c = first_non_whitespace(next_line);
            }
            continue;
        }

        let bytes = line.as_bytes();
        let mut pos = c;

        // Determine current char class
        let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let current_is_word = is_word_char(bytes[pos]);
        let current_is_space = bytes[pos] == b' ' || bytes[pos] == b'\t';

        if current_is_space {
            // Skip whitespace
            while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
                pos += 1;
            }
        } else if current_is_word {
            // Skip word chars
            while pos < bytes.len() && is_word_char(bytes[pos]) {
                pos += 1;
            }
            // Skip whitespace after word
            while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
                pos += 1;
            }
        } else {
            // Skip punctuation
            while pos < bytes.len()
                && !is_word_char(bytes[pos])
                && bytes[pos] != b' '
                && bytes[pos] != b'\t'
            {
                pos += 1;
            }
            // Skip whitespace after punctuation
            while pos < bytes.len() && (bytes[pos] == b' ' || bytes[pos] == b'\t') {
                pos += 1;
            }
        }

        if pos >= line.len() && r + 1 < lines.len() {
            r += 1;
            let next_line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
            c = first_non_whitespace(next_line);
        } else {
            c = pos.min(line.len());
        }
    }

    (r, c)
}

/// Move backward by `count` words (vim `b` motion).
fn word_backward(lines: &[String], row: usize, col: usize, count: usize) -> (usize, usize) {
    let mut r = row;
    let mut c = col;

    for _ in 0..count {
        if c == 0 {
            // At start of line — move to end of previous line
            if r > 0 {
                r -= 1;
                let prev_line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
                c = prev_line.len();
            } else {
                break;
            }
        }

        let line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
        let bytes = line.as_bytes();
        let mut pos = c;

        // Skip trailing whitespace
        while pos > 0 && (bytes[pos - 1] == b' ' || bytes[pos - 1] == b'\t') {
            pos -= 1;
        }

        if pos == 0 {
            c = 0;
            continue;
        }

        // Determine class of char before pos
        let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if is_word_char(bytes[pos - 1]) {
            while pos > 0 && is_word_char(bytes[pos - 1]) {
                pos -= 1;
            }
        } else {
            while pos > 0
                && !is_word_char(bytes[pos - 1])
                && bytes[pos - 1] != b' '
                && bytes[pos - 1] != b'\t'
            {
                pos -= 1;
            }
        }

        c = pos;
    }

    (r, c)
}

/// Move to end of `count`-th word (vim `e` motion).
fn word_end(lines: &[String], row: usize, col: usize, count: usize) -> (usize, usize) {
    let mut r = row;
    let mut c = col;

    for _ in 0..count {
        let line = lines.get(r).map(|s| s.as_str()).unwrap_or("");

        // First, move at least one position forward
        let mut pos = c;
        if pos < line.len() {
            pos = advance_n_chars(line, pos, 1);
        }

        // Skip whitespace
        loop {
            let current_line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
            let current_bytes = current_line.as_bytes();

            while pos < current_bytes.len()
                && (current_bytes[pos] == b' ' || current_bytes[pos] == b'\t')
            {
                pos += 1;
            }

            if pos < current_line.len() {
                break;
            }

            // Move to next line
            if r + 1 < lines.len() {
                r += 1;
                pos = 0;
            } else {
                break;
            }
        }

        let line = lines.get(r).map(|s| s.as_str()).unwrap_or("");
        let bytes = line.as_bytes();

        if pos >= bytes.len() {
            c = line.len().saturating_sub(1).max(0);
            continue;
        }

        // Move to end of current word
        let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if is_word_char(bytes[pos]) {
            while pos + 1 < bytes.len() && is_word_char(bytes[pos + 1]) {
                pos += 1;
            }
        } else {
            while pos + 1 < bytes.len()
                && !is_word_char(bytes[pos + 1])
                && bytes[pos + 1] != b' '
                && bytes[pos + 1] != b'\t'
            {
                pos += 1;
            }
        }

        c = pos;
    }

    (r, c)
}

/// Find character `ch` forward in `line` starting after `col`, `count` times.
fn find_char_forward(line: &str, col: usize, ch: char, count: usize) -> Option<usize> {
    let mut found = 0;
    for (i, c) in line.char_indices() {
        if i <= col {
            continue;
        }
        if c == ch {
            found += 1;
            if found == count {
                return Some(i);
            }
        }
    }
    None
}

/// Find character `ch` backward in `line` before `col`, `count` times.
fn find_char_backward(line: &str, col: usize, ch: char, count: usize) -> Option<usize> {
    let mut found = 0;
    let indices: Vec<(usize, char)> = line.char_indices().collect();
    for &(i, c) in indices.iter().rev() {
        if i >= col {
            continue;
        }
        if c == ch {
            found += 1;
            if found == count {
                return Some(i);
            }
        }
    }
    None
}

/// Normalize a range so (start_row, start_col) <= (end_row, end_col).
fn normalize_range(
    r1: usize,
    c1: usize,
    r2: usize,
    c2: usize,
) -> (usize, usize, usize, usize) {
    if r1 < r2 || (r1 == r2 && c1 <= c2) {
        (r1, c1, r2, c2)
    } else {
        (r2, c2, r1, c1)
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.split('\n').map(String::from).collect()
    }

    fn vim() -> VimMode {
        VimMode::new()
    }

    // ─── Mode Transitions ──────────────────────────────────────────────

    #[test]
    fn test_initial_state_is_normal() {
        let v = vim();
        assert_eq!(v.state(), VimState::Normal);
    }

    #[test]
    fn test_i_enters_insert_mode() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('i', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Insert);
        assert_eq!(action, VimAction::ChangeMode(VimState::Insert));
    }

    #[test]
    fn test_escape_returns_to_normal() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('i', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Insert);

        let action = v.handle_key('\x1b', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Normal);
        assert_eq!(action, VimAction::ChangeMode(VimState::Normal));
    }

    #[test]
    fn test_ctrl_bracket_returns_to_normal() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('i', false, &buf, 0, 0);
        let action = v.handle_key('[', true, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Normal);
        assert_eq!(action, VimAction::ChangeMode(VimState::Normal));
    }

    #[test]
    fn test_a_enters_insert_after_cursor() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('a', false, &buf, 0, 2);
        assert_eq!(v.state(), VimState::Insert);
        match action {
            VimAction::Batch(actions) => {
                assert_eq!(actions[0], VimAction::MoveCursor { row: 0, col: 3 });
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_A_enters_insert_at_eol() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('A', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Insert);
        match action {
            VimAction::Batch(actions) => {
                assert_eq!(actions[0], VimAction::MoveCursor { row: 0, col: 5 });
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_I_enters_insert_at_first_non_ws() {
        let mut v = vim();
        let buf = lines("  hello");
        let action = v.handle_key('I', false, &buf, 0, 4);
        assert_eq!(v.state(), VimState::Insert);
        match action {
            VimAction::Batch(actions) => {
                assert_eq!(actions[0], VimAction::MoveCursor { row: 0, col: 2 });
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_v_enters_visual_mode() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('v', false, &buf, 0, 2);
        assert_eq!(v.state(), VimState::Visual);
        assert_eq!(v.visual_anchor(), Some((0, 2)));
        assert_eq!(action, VimAction::ChangeMode(VimState::Visual));
    }

    #[test]
    fn test_colon_enters_command_mode() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key(':', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Command);
        assert_eq!(action, VimAction::ChangeMode(VimState::Command));
    }

    // ─── Normal Mode Motions ───────────────────────────────────────────

    #[test]
    fn test_h_moves_left() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('h', false, &buf, 0, 3);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 2 });
    }

    #[test]
    fn test_h_at_start_stays() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('h', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 0 });
    }

    #[test]
    fn test_l_moves_right() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('l', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 1 });
    }

    #[test]
    fn test_l_at_end_stays() {
        let mut v = vim();
        let buf = lines("hello");
        // In normal mode, cursor max is len-1 = 4
        let action = v.handle_key('l', false, &buf, 0, 4);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 4 });
    }

    #[test]
    fn test_0_moves_to_line_start() {
        let mut v = vim();
        let buf = lines("hello world");
        let action = v.handle_key('0', false, &buf, 0, 7);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 0 });
    }

    #[test]
    fn test_dollar_moves_to_line_end() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('$', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 4 });
    }

    #[test]
    fn test_caret_moves_to_first_non_ws() {
        let mut v = vim();
        let buf = lines("   hello");
        let action = v.handle_key('^', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 3 });
    }

    #[test]
    fn test_w_moves_to_next_word() {
        let mut v = vim();
        let buf = lines("hello world");
        let action = v.handle_key('w', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 6 });
    }

    #[test]
    fn test_b_moves_to_prev_word() {
        let mut v = vim();
        let buf = lines("hello world");
        let action = v.handle_key('b', false, &buf, 0, 6);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 0 });
    }

    #[test]
    fn test_e_moves_to_end_of_word() {
        let mut v = vim();
        let buf = lines("hello world");
        let action = v.handle_key('e', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 4 });
    }

    #[test]
    fn test_j_moves_down() {
        let mut v = vim();
        let buf = lines("hello\nworld");
        let action = v.handle_key('j', false, &buf, 0, 2);
        assert_eq!(action, VimAction::MoveCursor { row: 1, col: 2 });
    }

    #[test]
    fn test_k_moves_up() {
        let mut v = vim();
        let buf = lines("hello\nworld");
        let action = v.handle_key('k', false, &buf, 1, 2);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 2 });
    }

    #[test]
    fn test_j_clamps_col() {
        let mut v = vim();
        let buf = lines("hello world\nhi");
        let action = v.handle_key('j', false, &buf, 0, 8);
        // "hi" has len 2, max col in normal mode = 1
        assert_eq!(action, VimAction::MoveCursor { row: 1, col: 1 });
    }

    #[test]
    fn test_G_moves_to_last_line() {
        let mut v = vim();
        let buf = lines("one\ntwo\nthree");
        let action = v.handle_key('G', false, &buf, 0, 0);
        // Last line "three" has len 5, cursor at len-1 = 4
        assert_eq!(action, VimAction::MoveCursor { row: 2, col: 4 });
    }

    // ─── Count Prefix ──────────────────────────────────────────────────

    #[test]
    fn test_count_prefix_3w() {
        let mut v = vim();
        let buf = lines("one two three four five");
        v.handle_key('3', false, &buf, 0, 0); // count = 3
        let action = v.handle_key('w', false, &buf, 0, 0);
        // 3 words forward: "one " -> "two " -> "three " -> col 14
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 14 });
    }

    #[test]
    fn test_count_prefix_2h() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('2', false, &buf, 0, 4);
        let action = v.handle_key('h', false, &buf, 0, 4);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 2 });
    }

    #[test]
    fn test_count_prefix_5l() {
        let mut v = vim();
        let buf = lines("hello world!");
        v.handle_key('5', false, &buf, 0, 0);
        let action = v.handle_key('l', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 5 });
    }

    // ─── f/F Find Character ────────────────────────────────────────────

    #[test]
    fn test_f_finds_char_forward() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('f', false, &buf, 0, 0); // await char
        let action = v.handle_key('w', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 6 });
    }

    #[test]
    fn test_F_finds_char_backward() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('F', false, &buf, 0, 8); // await char
        let action = v.handle_key('l', false, &buf, 0, 8);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 3 });
    }

    #[test]
    fn test_f_not_found_is_noop() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('f', false, &buf, 0, 0);
        let action = v.handle_key('z', false, &buf, 0, 0);
        assert_eq!(action, VimAction::NoOp);
    }

    // ─── Operators ─────────────────────────────────────────────────────

    #[test]
    fn test_x_deletes_char() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('x', false, &buf, 0, 2);
        match action {
            VimAction::DeleteRange {
                start_row, start_col, end_row, end_col,
            } => {
                assert_eq!(start_row, 0);
                assert_eq!(start_col, 2);
                assert_eq!(end_row, 0);
                assert_eq!(end_col, 3);
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
        assert_eq!(v.register(), "l");
    }

    #[test]
    fn test_x_at_end_is_noop() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('x', false, &buf, 0, 5);
        assert_eq!(action, VimAction::NoOp);
    }

    #[test]
    fn test_dd_deletes_line() {
        let mut v = vim();
        let buf = lines("hello\nworld");
        v.handle_key('d', false, &buf, 0, 0);
        let action = v.handle_key('d', false, &buf, 0, 0);
        assert_eq!(action, VimAction::DeleteLine(0));
        assert_eq!(v.register(), "hello");
    }

    #[test]
    fn test_dw_deletes_word() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('d', false, &buf, 0, 0);
        let action = v.handle_key('w', false, &buf, 0, 0);
        match action {
            VimAction::DeleteRange {
                start_row, start_col, end_row, end_col,
            } => {
                assert_eq!(start_row, 0);
                assert_eq!(start_col, 0);
                assert_eq!(end_row, 0);
                assert_eq!(end_col, 6);
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
        assert_eq!(v.register(), "hello ");
    }

    #[test]
    fn test_d_dollar_deletes_to_eol() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('d', false, &buf, 0, 5);
        let action = v.handle_key('$', false, &buf, 0, 5);
        match action {
            VimAction::DeleteRange {
                start_row, start_col, end_row, end_col,
            } => {
                assert_eq!(start_col, 5);
                assert_eq!(end_col, 11);
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
    }

    #[test]
    fn test_cw_changes_word() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('c', false, &buf, 0, 0);
        let action = v.handle_key('w', false, &buf, 0, 0);
        match action {
            VimAction::Batch(actions) => {
                assert!(matches!(actions[0], VimAction::DeleteRange { .. }));
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
        assert_eq!(v.state(), VimState::Insert);
    }

    #[test]
    fn test_cc_changes_line() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('c', false, &buf, 0, 3);
        let action = v.handle_key('c', false, &buf, 0, 3);
        match action {
            VimAction::Batch(actions) => {
                assert!(matches!(actions[0], VimAction::DeleteRange { .. }));
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_yy_yanks_line() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('y', false, &buf, 0, 0);
        let action = v.handle_key('y', false, &buf, 0, 0);
        assert_eq!(action, VimAction::NoOp);
        assert_eq!(v.register(), "hello world");
        assert!(v.register_linewise);
    }

    #[test]
    fn test_yw_yanks_word() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('y', false, &buf, 0, 0);
        let action = v.handle_key('w', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 0 });
        assert_eq!(v.register(), "hello ");
    }

    // ─── Replace ───────────────────────────────────────────────────────

    #[test]
    fn test_r_replaces_char() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('r', false, &buf, 0, 0);
        let action = v.handle_key('X', false, &buf, 0, 0);
        assert_eq!(
            action,
            VimAction::ReplaceChar { row: 0, col: 0, ch: 'X' }
        );
    }

    // ─── Paste ─────────────────────────────────────────────────────────

    #[test]
    fn test_p_pastes_after() {
        let mut v = vim();
        let buf = lines("hello world");
        // Yank something first
        v.handle_key('y', false, &buf, 0, 0);
        v.handle_key('w', false, &buf, 0, 0);
        // yw on "hello world" at col 0 yanks "hello " (word + space)
        assert_eq!(v.register(), "hello ");
        // Now paste
        let action = v.handle_key('p', false, &buf, 0, 3);
        match action {
            VimAction::PasteAfter(text) => assert_eq!(text, "hello "),
            _ => panic!("expected PasteAfter, got {action:?}"),
        }
    }

    #[test]
    fn test_P_pastes_before() {
        let mut v = vim();
        let buf = lines("hello");
        v.register = "world".to_string();
        let action = v.handle_key('P', false, &buf, 0, 0);
        match action {
            VimAction::PasteBefore(text) => assert_eq!(text, "world"),
            _ => panic!("expected PasteBefore, got {action:?}"),
        }
    }

    #[test]
    fn test_p_empty_register_noop() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('p', false, &buf, 0, 0);
        assert_eq!(action, VimAction::NoOp);
    }

    // ─── Undo/Redo ─────────────────────────────────────────────────────

    #[test]
    fn test_u_undoes() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('u', false, &buf, 0, 0);
        assert_eq!(action, VimAction::Undo);
    }

    #[test]
    fn test_ctrl_r_redoes() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('\x12', true, &buf, 0, 0);
        assert_eq!(action, VimAction::Redo);
    }

    // ─── Insert Mode ───────────────────────────────────────────────────

    #[test]
    fn test_insert_char() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('i', false, &buf, 0, 0);
        let action = v.handle_key('x', false, &buf, 0, 0);
        assert_eq!(action, VimAction::InsertChar('x'));
    }

    #[test]
    fn test_insert_backspace() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('i', false, &buf, 0, 0);
        let action = v.handle_key('\x7f', false, &buf, 0, 0);
        assert_eq!(action, VimAction::Backspace);
    }

    #[test]
    fn test_insert_enter() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('i', false, &buf, 0, 0);
        let action = v.handle_key('\r', false, &buf, 0, 0);
        assert_eq!(action, VimAction::InsertNewline);
    }

    // ─── Command Mode ──────────────────────────────────────────────────

    #[test]
    fn test_command_w_submits() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        v.handle_key('w', false, &buf, 0, 0);
        let action = v.handle_key('\r', false, &buf, 0, 0);
        assert_eq!(action, VimAction::Submit);
        assert_eq!(v.state(), VimState::Normal);
    }

    #[test]
    fn test_command_q_clears() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        v.handle_key('q', false, &buf, 0, 0);
        let action = v.handle_key('\r', false, &buf, 0, 0);
        assert_eq!(action, VimAction::ClearInput);
    }

    #[test]
    fn test_command_escape_cancels() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        v.handle_key('w', false, &buf, 0, 0);
        let action = v.handle_key('\x1b', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Normal);
        assert_eq!(action, VimAction::ChangeMode(VimState::Normal));
    }

    #[test]
    fn test_command_set_paste() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        for ch in "set paste".chars() {
            v.handle_key(ch, false, &buf, 0, 0);
        }
        let action = v.handle_key('\r', false, &buf, 0, 0);
        assert_eq!(action, VimAction::CommandOutput("Paste mode ON".to_string()));
    }

    #[test]
    fn test_command_unknown() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        for ch in "foobar".chars() {
            v.handle_key(ch, false, &buf, 0, 0);
        }
        let action = v.handle_key('\r', false, &buf, 0, 0);
        match action {
            VimAction::CommandOutput(msg) => assert!(msg.contains("Unknown command")),
            _ => panic!("expected CommandOutput, got {action:?}"),
        }
    }

    #[test]
    fn test_command_backspace() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        v.handle_key('w', false, &buf, 0, 0);
        v.handle_key('q', false, &buf, 0, 0);
        v.handle_key('\x7f', false, &buf, 0, 0); // backspace 'q'
        let action = v.handle_key('\r', false, &buf, 0, 0);
        assert_eq!(action, VimAction::Submit); // just "w" left
    }

    #[test]
    fn test_command_backspace_empty_exits() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key(':', false, &buf, 0, 0);
        let action = v.handle_key('\x7f', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Normal);
        assert_eq!(action, VimAction::ChangeMode(VimState::Normal));
    }

    // ─── Visual Mode ───────────────────────────────────────────────────

    #[test]
    fn test_visual_motion_extends_selection() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('v', false, &buf, 0, 0);
        let action = v.handle_key('l', false, &buf, 0, 0);
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 1 });
        // Anchor should still be at (0, 0)
        assert_eq!(v.visual_anchor(), Some((0, 0)));
    }

    #[test]
    fn test_visual_d_deletes_selection() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('v', false, &buf, 0, 0);
        // Simulate cursor moved to col 5 via l motions
        let action = v.handle_key('d', false, &buf, 0, 5);
        match action {
            VimAction::Batch(actions) => {
                match &actions[0] {
                    VimAction::DeleteRange {
                        start_row, start_col, end_row, end_col,
                    } => {
                        assert_eq!(*start_row, 0);
                        assert_eq!(*start_col, 0);
                        assert_eq!(*end_row, 0);
                        assert_eq!(*end_col, 5);
                    }
                    _ => panic!("expected DeleteRange"),
                }
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Normal));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_visual_y_yanks_selection() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('v', false, &buf, 0, 0);
        let action = v.handle_key('y', false, &buf, 0, 5);
        assert_eq!(v.register(), "hello");
        assert_eq!(v.state(), VimState::Normal);
    }

    #[test]
    fn test_visual_escape_exits() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('v', false, &buf, 0, 0);
        let action = v.handle_key('\x1b', false, &buf, 0, 0);
        assert_eq!(v.state(), VimState::Normal);
        assert!(v.visual_anchor().is_none());
    }

    #[test]
    fn test_visual_c_changes_selection() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('v', false, &buf, 0, 0);
        let action = v.handle_key('c', false, &buf, 0, 5);
        match action {
            VimAction::Batch(actions) => {
                assert!(matches!(actions[0], VimAction::DeleteRange { .. }));
                assert_eq!(actions[1], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
        assert_eq!(v.state(), VimState::Insert);
    }

    // ─── Dot Repeat ────────────────────────────────────────────────────

    #[test]
    fn test_dot_repeats_x() {
        let mut v = vim();
        let buf = lines("hello");
        v.handle_key('x', false, &buf, 0, 0);
        // x deletes char at 0 => register = "h"
        // Now dot repeat
        let action = v.handle_key('.', false, &buf, 0, 0);
        match action {
            VimAction::DeleteRange { start_col, end_col, .. } => {
                assert_eq!(start_col, 0);
                assert_eq!(end_col, 1);
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
    }

    #[test]
    fn test_dot_with_no_previous_is_noop() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('.', false, &buf, 0, 0);
        assert_eq!(action, VimAction::NoOp);
    }

    // ─── o/O (Open Line) ──────────────────────────────────────────────

    #[test]
    fn test_o_opens_line_below() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('o', false, &buf, 0, 2);
        match action {
            VimAction::Batch(actions) => {
                assert_eq!(actions[0], VimAction::MoveCursor { row: 0, col: 5 });
                assert_eq!(actions[1], VimAction::InsertNewline);
                assert_eq!(actions[2], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    #[test]
    fn test_O_opens_line_above() {
        let mut v = vim();
        let buf = lines("hello");
        let action = v.handle_key('O', false, &buf, 0, 2);
        match action {
            VimAction::Batch(actions) => {
                assert_eq!(actions[0], VimAction::MoveCursor { row: 0, col: 0 });
                assert_eq!(actions[1], VimAction::InsertNewline);
                assert_eq!(actions[2], VimAction::MoveCursor { row: 0, col: 0 });
                assert_eq!(actions[3], VimAction::ChangeMode(VimState::Insert));
            }
            _ => panic!("expected Batch, got {action:?}"),
        }
    }

    // ─── State Display ─────────────────────────────────────────────────

    #[test]
    fn test_vim_state_display() {
        assert_eq!(VimState::Normal.to_string(), "-- NORMAL --");
        assert_eq!(VimState::Insert.to_string(), "-- INSERT --");
        assert_eq!(VimState::Visual.to_string(), "-- VISUAL --");
        assert_eq!(VimState::Command.to_string(), ":");
    }

    // ─── Helper Functions ──────────────────────────────────────────────

    #[test]
    fn test_advance_n_chars() {
        assert_eq!(advance_n_chars("hello", 0, 1), 1);
        assert_eq!(advance_n_chars("hello", 0, 3), 3);
        assert_eq!(advance_n_chars("hello", 3, 5), 5); // past end
    }

    #[test]
    fn test_advance_n_chars_unicode() {
        let s = "世界hello";
        assert_eq!(advance_n_chars(s, 0, 1), 3); // 世 is 3 bytes
        assert_eq!(advance_n_chars(s, 0, 2), 6); // 世界
    }

    #[test]
    fn test_retreat_n_chars() {
        assert_eq!(retreat_n_chars("hello", 3, 1), 2);
        assert_eq!(retreat_n_chars("hello", 3, 5), 0); // stops at 0
        assert_eq!(retreat_n_chars("hello", 0, 1), 0); // already at 0
    }

    #[test]
    fn test_first_non_whitespace() {
        assert_eq!(first_non_whitespace("  hello"), 2);
        assert_eq!(first_non_whitespace("hello"), 0);
        assert_eq!(first_non_whitespace("   "), 0); // all ws, returns 0
    }

    #[test]
    fn test_find_char_forward() {
        assert_eq!(find_char_forward("hello world", 0, 'o', 1), Some(4));
        assert_eq!(find_char_forward("hello world", 0, 'o', 2), Some(7));
        assert_eq!(find_char_forward("hello", 0, 'z', 1), None);
    }

    #[test]
    fn test_find_char_backward() {
        assert_eq!(find_char_backward("hello world", 8, 'l', 1), Some(3));
        assert_eq!(find_char_backward("hello world", 8, 'l', 2), Some(2));
        assert_eq!(find_char_backward("hello", 4, 'z', 1), None);
    }

    #[test]
    fn test_normalize_range() {
        assert_eq!(normalize_range(0, 5, 0, 2), (0, 2, 0, 5));
        assert_eq!(normalize_range(0, 2, 0, 5), (0, 2, 0, 5));
        assert_eq!(normalize_range(1, 0, 0, 3), (0, 3, 1, 0));
    }

    #[test]
    fn test_word_forward_basic() {
        let buf = lines("hello world test");
        assert_eq!(word_forward(&buf, 0, 0, 1), (0, 6));
        assert_eq!(word_forward(&buf, 0, 0, 2), (0, 12));
    }

    #[test]
    fn test_word_backward_basic() {
        let buf = lines("hello world test");
        assert_eq!(word_backward(&buf, 0, 12, 1), (0, 6));
        assert_eq!(word_backward(&buf, 0, 12, 2), (0, 0));
    }

    #[test]
    fn test_word_end_basic() {
        let buf = lines("hello world");
        assert_eq!(word_end(&buf, 0, 0, 1), (0, 4));
    }

    #[test]
    fn test_word_forward_cross_line() {
        let buf = lines("hello\nworld");
        // 1 word forward from (0,0) crosses to start of "world" line
        let (r, c) = word_forward(&buf, 0, 0, 1);
        assert_eq!(r, 1);
        assert_eq!(c, 0);

        // 2 words forward goes past "world" (end of line, only word on line 1)
        let (r2, c2) = word_forward(&buf, 0, 0, 2);
        assert_eq!(r2, 1);
        assert!(c2 >= 5); // at or past end of "world"
    }

    #[test]
    fn test_word_backward_cross_line() {
        let buf = lines("hello\nworld");
        let (r, c) = word_backward(&buf, 1, 0, 1);
        assert_eq!(r, 0);
    }

    // ─── gg Sequence ───────────────────────────────────────────────────

    #[test]
    fn test_gg_moves_to_top() {
        let mut v = vim();
        let buf = lines("one\ntwo\nthree");
        v.handle_key('g', false, &buf, 2, 3);
        let action = v.handle_key('g', false, &buf, 2, 3);
        // The 'g' case in the match pushes to current_keys and returns NoOp.
        // On the second 'g', the default case checks for gg sequence.
        assert_eq!(action, VimAction::MoveCursor { row: 0, col: 0 });
    }

    // ─── d0 ────────────────────────────────────────────────────────────

    #[test]
    fn test_d0_deletes_to_line_start() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('d', false, &buf, 0, 6);
        let action = v.handle_key('0', false, &buf, 0, 6);
        match action {
            VimAction::DeleteRange {
                start_col, end_col, ..
            } => {
                assert_eq!(start_col, 0);
                assert_eq!(end_col, 6);
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
    }

    // ─── df (delete to find) ───────────────────────────────────────────

    #[test]
    fn test_df_deletes_to_char() {
        let mut v = vim();
        let buf = lines("hello world");
        v.handle_key('d', false, &buf, 0, 0);
        v.handle_key('f', false, &buf, 0, 0);
        let action = v.handle_key('o', false, &buf, 0, 0);
        match action {
            VimAction::DeleteRange {
                start_col, end_col, ..
            } => {
                assert_eq!(start_col, 0);
                assert_eq!(end_col, 5); // inclusive of 'o' at 4, so end = 5
            }
            _ => panic!("expected DeleteRange, got {action:?}"),
        }
    }

    // ─── Default impl ──────────────────────────────────────────────────

    #[test]
    fn test_default_is_new() {
        let v = VimMode::default();
        assert_eq!(v.state(), VimState::Normal);
        assert!(v.register().is_empty());
    }
}
