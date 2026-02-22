//! Multi-line input editor with command history.
//!
//! `InputEditor` replaces the simple `String` input buffer in `ElwoodPane`.
//! It supports:
//! - Multi-line editing (Shift+Enter inserts newline)
//! - Cursor movement (arrow keys, Home/End, Ctrl+A/E)
//! - Word deletion (Ctrl+W, Alt+Backspace)
//! - Per-mode command history (Up/Down arrows)
//! - History navigation resets on any new input
//!
//! ## Coordinate System
//!
//! `cursor_row` is 0-based index into `lines`.
//! `cursor_col` is 0-based byte column within the line (always clamped to char boundaries).

use crate::runtime::InputMode;

/// Maximum number of lines in a single input (hard cap for rendering).
pub const MAX_INPUT_LINES: usize = 8;

/// Maximum history entries stored per mode.
const MAX_HISTORY: usize = 1000;

/// Multi-line text editor with per-mode command history.
#[derive(Debug, Clone)]
pub struct InputEditor {
    /// Line buffer — always has at least one entry.
    lines: Vec<String>,
    /// Current cursor row (0-based index into `lines`).
    cursor_row: usize,
    /// Current cursor column (0-based byte offset within `lines[cursor_row]`).
    cursor_col: usize,
    /// Current input mode (determines which history list is used).
    mode: InputMode,
    /// History for `InputMode::Agent`.
    agent_history: Vec<String>,
    /// History for `InputMode::Terminal`.
    terminal_history: Vec<String>,
    /// Index into the current mode's history (None = not browsing history).
    history_index: Option<usize>,
    /// Draft saved when Up is pressed for the first time (restored on Down past end).
    history_draft: Option<Vec<String>>,
}

impl InputEditor {
    /// Create a new editor in the given mode.
    pub fn new(mode: InputMode) -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            mode,
            agent_history: Vec::new(),
            terminal_history: Vec::new(),
            history_index: None,
            history_draft: None,
        }
    }

    // ─── Accessors ──────────────────────────────────────────────────────

    /// Return all lines as a slice (read-only).
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Number of lines currently in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Current cursor row (0-based).
    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    /// Current cursor column as a byte offset (0-based).
    pub fn cursor_col(&self) -> usize {
        self.cursor_col
    }

    /// Current input mode.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Return the full multi-line content as a single string (lines joined with `\n`).
    pub fn content(&self) -> String {
        self.lines.join("\n")
    }

    /// True when the buffer is empty (single empty line).
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    // ─── Mode ───────────────────────────────────────────────────────────

    /// Switch to a new mode.  Clears the buffer and resets history browsing.
    pub fn set_mode(&mut self, mode: InputMode) {
        self.mode = mode;
        self.reset_history_state();
    }

    // ─── Edit Operations ────────────────────────────────────────────────

    /// Insert a single character at the current cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.reset_history_state();
        let col = self.cursor_col;
        self.lines[self.cursor_row].insert(col, c);
        self.cursor_col += c.len_utf8();
    }

    /// Insert a newline at the current cursor position (splits the line).
    /// Does nothing if already at MAX_INPUT_LINES.
    pub fn insert_newline(&mut self) {
        if self.lines.len() >= MAX_INPUT_LINES {
            return;
        }
        self.reset_history_state();
        let col = self.cursor_col;
        let tail = self.lines[self.cursor_row].split_off(col);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// Delete the character immediately before the cursor (Backspace).
    pub fn backspace(&mut self) {
        self.reset_history_state();
        if self.cursor_col > 0 {
            // Remove char before cursor on same line
            let col = prev_char_boundary(&self.lines[self.cursor_row], self.cursor_col);
            self.lines[self.cursor_row].remove(col);
            self.cursor_col = col;
        } else if self.cursor_row > 0 {
            // Merge with previous line
            let current_line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev_len = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current_line);
            self.cursor_col = prev_len;
        }
    }

    /// Delete the word before the cursor (Ctrl+W / Alt+Backspace).
    pub fn delete_word_backward(&mut self) {
        self.reset_history_state();
        if self.cursor_col == 0 && self.cursor_row == 0 {
            return;
        }
        if self.cursor_col == 0 {
            // Merge current line into previous line (same as backspace at col 0)
            let current_line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev_len = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current_line);
            self.cursor_col = prev_len;
            return;
        }

        let line = &self.lines[self.cursor_row];
        let col = word_start_before(line, self.cursor_col);
        self.lines[self.cursor_row].replace_range(col..self.cursor_col, "");
        self.cursor_col = col;
    }

    /// Delete from cursor to start of line (Ctrl+U).
    pub fn delete_to_line_start(&mut self) {
        self.reset_history_state();
        let col = self.cursor_col;
        if col > 0 {
            self.lines[self.cursor_row].replace_range(..col, "");
            self.cursor_col = 0;
        }
    }

    // ─── Cursor Movement ────────────────────────────────────────────────

    /// Move cursor to start of current line.
    pub fn move_to_line_start(&mut self) {
        self.cursor_col = 0;
    }

    /// Move cursor to end of current line.
    pub fn move_to_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// Move cursor left one character.
    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col = prev_char_boundary(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    /// Move cursor right one character.
    pub fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.cursor_col = next_char_boundary(&self.lines[self.cursor_row], self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// Move cursor up one row, or navigate history if on the first line.
    ///
    /// Returns `true` if history navigation was triggered.
    pub fn move_up(&mut self) -> bool {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.clamp_col();
            false
        } else {
            self.history_prev()
        }
    }

    /// Move cursor down one row, or navigate history if on the last line.
    ///
    /// Returns `true` if history navigation was triggered.
    pub fn move_down(&mut self) -> bool {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.clamp_col();
            false
        } else {
            self.history_next()
        }
    }

    // ─── Submit ─────────────────────────────────────────────────────────

    /// Return the current content and clear the buffer.
    ///
    /// Saves the content to history (unless empty or duplicate of last entry).
    /// Returns `None` if the buffer is empty.
    pub fn submit(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let content = self.content();
        self.push_history(content.clone());
        self.clear();
        Some(content)
    }

    /// Clear the buffer, resetting to a single empty line.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.reset_history_state();
    }

    // ─── History ────────────────────────────────────────────────────────

    /// Navigate to the previous history entry (older). Returns `true` if the
    /// buffer was updated.
    fn history_prev(&mut self) -> bool {
        // Save current draft before first history navigation
        if self.history_index.is_none() {
            self.history_draft = Some(self.lines.clone());
        }

        let history = self.current_history();
        if history.is_empty() {
            return false;
        }

        let new_index = match self.history_index {
            None => history.len() - 1,
            Some(0) => return false, // already at oldest
            Some(i) => i - 1,
        };

        let entry = history[new_index].clone();
        self.history_index = Some(new_index);
        self.load_from_string(&entry);
        true
    }

    /// Navigate to the next history entry (newer). Returns `true` if the
    /// buffer was updated (including restoring the draft).
    fn history_next(&mut self) -> bool {
        let history_len = self.current_history().len();

        match self.history_index {
            None => false,
            Some(i) if i + 1 >= history_len => {
                // Restore draft
                self.history_index = None;
                let draft = self.history_draft.take().unwrap_or_else(|| vec![String::new()]);
                self.lines = draft;
                self.cursor_row = self.lines.len().saturating_sub(1);
                self.cursor_col = self.lines[self.cursor_row].len();
                true
            }
            Some(i) => {
                let new_index = i + 1;
                let entry = self.current_history()[new_index].clone();
                self.history_index = Some(new_index);
                self.load_from_string(&entry);
                true
            }
        }
    }

    /// Add an entry to the current mode's history, deduplicating consecutive repeats.
    fn push_history(&mut self, entry: String) {
        let history = self.current_history_mut();
        if history.last().map(|s| s.as_str()) == Some(entry.as_str()) {
            return; // Skip consecutive duplicates
        }
        history.push(entry);
        if history.len() > MAX_HISTORY {
            history.remove(0);
        }
    }

    /// Reference to the current mode's history (immutable).
    fn current_history(&self) -> &Vec<String> {
        match self.mode {
            InputMode::Agent => &self.agent_history,
            InputMode::Terminal => &self.terminal_history,
        }
    }

    /// Reference to the current mode's history (mutable).
    fn current_history_mut(&mut self) -> &mut Vec<String> {
        match self.mode {
            InputMode::Agent => &mut self.agent_history,
            InputMode::Terminal => &mut self.terminal_history,
        }
    }

    /// Load content from a history entry string (may contain `\n`).
    fn load_from_string(&mut self, s: &str) {
        self.lines = s.split('\n').map(String::from).collect();
        if self.lines.is_empty() {
            self.lines = vec![String::new()];
        }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].len();
    }

    /// Reset history browsing state (called on any edit operation).
    fn reset_history_state(&mut self) {
        self.history_index = None;
        self.history_draft = None;
    }

    // ─── Internals ──────────────────────────────────────────────────────

    /// Clamp `cursor_col` to the length of the current line (after row change).
    fn clamp_col(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col > line_len {
            self.cursor_col = line_len;
        }
        // Ensure we're on a char boundary
        while self.cursor_col > 0 && !self.lines[self.cursor_row].is_char_boundary(self.cursor_col)
        {
            self.cursor_col -= 1;
        }
    }
}

// ─── Unicode Helpers ────────────────────────────────────────────────────────

/// Return the byte offset of the character boundary before `pos` in `s`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.saturating_sub(1);
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Return the byte offset of the next character boundary after `pos` in `s`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p.min(s.len())
}

/// Return the byte offset of the start of the word before `pos` in `s`.
///
/// Skips trailing whitespace, then skips non-whitespace characters.
fn word_start_before(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = pos;

    // Skip trailing whitespace
    while i > 0 && bytes[i - 1] == b' ' {
        i -= 1;
    }
    // Skip word characters
    while i > 0 && bytes[i - 1] != b' ' {
        i -= 1;
    }
    i
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> InputEditor {
        InputEditor::new(InputMode::Agent)
    }

    // ─── Basic Editing ──────────────────────────────────────────────────

    #[test]
    fn test_insert_chars() {
        let mut e = editor();
        e.insert_char('h');
        e.insert_char('i');
        assert_eq!(e.content(), "hi");
        assert_eq!(e.cursor_col(), 2);
    }

    #[test]
    fn test_backspace() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_char('b');
        e.backspace();
        assert_eq!(e.content(), "a");
        assert_eq!(e.cursor_col(), 1);
    }

    #[test]
    fn test_backspace_at_start_does_nothing() {
        let mut e = editor();
        e.backspace();
        assert!(e.is_empty());
    }

    #[test]
    fn test_insert_newline() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_newline();
        e.insert_char('b');
        assert_eq!(e.line_count(), 2);
        assert_eq!(e.content(), "a\nb");
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 1);
    }

    #[test]
    fn test_insert_newline_splits_line() {
        let mut e = editor();
        for c in "hello".chars() { e.insert_char(c); }
        // move cursor to position 2 (after 'he')
        e.cursor_col = 2;
        e.insert_newline();
        assert_eq!(e.lines()[0], "he");
        assert_eq!(e.lines()[1], "llo");
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 0);
    }

    #[test]
    fn test_max_lines_respected() {
        let mut e = editor();
        for _ in 0..MAX_INPUT_LINES + 5 {
            e.insert_newline();
        }
        assert!(e.line_count() <= MAX_INPUT_LINES);
    }

    #[test]
    fn test_backspace_merges_lines() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_newline();
        e.insert_char('b');
        // cursor is at row 1, col 1 — backspace should remove 'b'
        e.backspace();
        assert_eq!(e.line_count(), 2);
        assert_eq!(e.content(), "a\n");
        // backspace again merges lines
        e.backspace();
        assert_eq!(e.line_count(), 1);
        assert_eq!(e.content(), "a");
    }

    // ─── Word Deletion ──────────────────────────────────────────────────

    #[test]
    fn test_delete_word_backward() {
        let mut e = editor();
        for c in "foo bar".chars() { e.insert_char(c); }
        e.delete_word_backward();
        assert_eq!(e.content(), "foo ");
    }

    #[test]
    fn test_delete_word_skips_trailing_space() {
        let mut e = editor();
        for c in "foo   ".chars() { e.insert_char(c); }
        e.delete_word_backward();
        assert_eq!(e.content(), "");
    }

    #[test]
    fn test_delete_to_line_start() {
        let mut e = editor();
        for c in "hello world".chars() { e.insert_char(c); }
        e.delete_to_line_start();
        assert!(e.is_empty());
    }

    // ─── Cursor Movement ────────────────────────────────────────────────

    #[test]
    fn test_move_to_line_start_end() {
        let mut e = editor();
        for c in "abc".chars() { e.insert_char(c); }
        e.move_to_line_start();
        assert_eq!(e.cursor_col(), 0);
        e.move_to_line_end();
        assert_eq!(e.cursor_col(), 3);
    }

    #[test]
    fn test_move_left_right() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_char('b');
        e.move_left();
        assert_eq!(e.cursor_col(), 1);
        e.move_right();
        assert_eq!(e.cursor_col(), 2);
    }

    #[test]
    fn test_move_left_wraps_to_prev_line() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_newline();
        // cursor at row 1, col 0
        e.move_left();
        assert_eq!(e.cursor_row(), 0);
        assert_eq!(e.cursor_col(), 1);
    }

    #[test]
    fn test_move_right_wraps_to_next_line() {
        let mut e = editor();
        e.insert_char('a');
        e.insert_newline();
        e.insert_char('b');
        // move to start of last line
        e.move_to_line_start();
        // go to end of first line
        e.cursor_row = 0;
        e.cursor_col = 1;
        e.move_right();
        assert_eq!(e.cursor_row(), 1);
        assert_eq!(e.cursor_col(), 0);
    }

    #[test]
    fn test_move_up_down_multiline() {
        let mut e = editor();
        for c in "hello".chars() { e.insert_char(c); }
        e.insert_newline();
        for c in "world".chars() { e.insert_char(c); }
        // cursor at row 1, col 5
        let was_history = e.move_up();
        assert!(!was_history);
        assert_eq!(e.cursor_row(), 0);

        let was_history = e.move_down();
        assert!(!was_history);
        assert_eq!(e.cursor_row(), 1);
    }

    // ─── Submit / Clear ──────────────────────────────────────────────────

    #[test]
    fn test_submit_returns_content_and_clears() {
        let mut e = editor();
        for c in "hello".chars() { e.insert_char(c); }
        let content = e.submit();
        assert_eq!(content, Some("hello".to_string()));
        assert!(e.is_empty());
    }

    #[test]
    fn test_submit_empty_returns_none() {
        let mut e = editor();
        assert_eq!(e.submit(), None);
    }

    #[test]
    fn test_submit_multiline() {
        let mut e = editor();
        for c in "line1".chars() { e.insert_char(c); }
        e.insert_newline();
        for c in "line2".chars() { e.insert_char(c); }
        let content = e.submit();
        assert_eq!(content, Some("line1\nline2".to_string()));
    }

    // ─── History ────────────────────────────────────────────────────────

    #[test]
    fn test_history_nav() {
        let mut e = editor();
        // Submit a few entries
        for c in "cmd1".chars() { e.insert_char(c); }
        e.submit();
        for c in "cmd2".chars() { e.insert_char(c); }
        e.submit();
        for c in "cmd3".chars() { e.insert_char(c); }
        e.submit();

        // Navigate up through history
        let changed = e.move_up();
        assert!(changed);
        assert_eq!(e.content(), "cmd3");

        let changed = e.move_up();
        assert!(changed);
        assert_eq!(e.content(), "cmd2");

        let changed = e.move_up();
        assert!(changed);
        assert_eq!(e.content(), "cmd1");

        // At oldest — moving up further does nothing
        let changed = e.move_up();
        assert!(!changed);
        assert_eq!(e.content(), "cmd1");

        // Navigate back down
        let changed = e.move_down();
        assert!(changed);
        assert_eq!(e.content(), "cmd2");

        // Navigate to end (restores empty draft)
        e.move_down(); // cmd3
        let changed = e.move_down(); // back to draft
        assert!(changed);
        assert!(e.is_empty());
    }

    #[test]
    fn test_history_draft_preserved() {
        let mut e = editor();
        for c in "cmd1".chars() { e.insert_char(c); }
        e.submit();

        // Start typing a new command
        for c in "draft".chars() { e.insert_char(c); }

        // Navigate up and back down — draft should be restored
        e.move_up();
        assert_eq!(e.content(), "cmd1");
        e.move_down();
        assert_eq!(e.content(), "draft");
    }

    #[test]
    fn test_history_deduplicates_consecutive() {
        let mut e = editor();
        for c in "same".chars() { e.insert_char(c); }
        e.submit();
        for c in "same".chars() { e.insert_char(c); }
        e.submit();

        // Only one "same" in history
        e.move_up();
        assert_eq!(e.content(), "same");
        let changed = e.move_up();
        assert!(!changed); // at oldest
    }

    #[test]
    fn test_history_separate_per_mode() {
        let mut e = editor();
        // Add to Agent history
        for c in "agent_cmd".chars() { e.insert_char(c); }
        e.submit();

        // Switch to Terminal mode and add
        e.set_mode(InputMode::Terminal);
        for c in "term_cmd".chars() { e.insert_char(c); }
        e.submit();

        // Terminal history should show term_cmd
        e.move_up();
        assert_eq!(e.content(), "term_cmd");
        e.move_down(); // restore draft

        // Switch back to Agent — history should show agent_cmd
        e.set_mode(InputMode::Agent);
        e.move_up();
        assert_eq!(e.content(), "agent_cmd");
    }

    #[test]
    fn test_edit_resets_history_browsing() {
        let mut e = editor();
        for c in "cmd1".chars() { e.insert_char(c); }
        e.submit();

        e.move_up();
        assert_eq!(e.content(), "cmd1");

        // Any insertion should exit history browsing
        e.insert_char('x');
        assert!(e.history_index.is_none());
    }

    // ─── Unicode ────────────────────────────────────────────────────────

    #[test]
    fn test_insert_unicode_chars() {
        let mut e = editor();
        e.insert_char('世');
        e.insert_char('界');
        assert_eq!(e.content(), "世界");
        assert_eq!(e.cursor_col(), 6); // 3 bytes each
    }

    #[test]
    fn test_backspace_unicode() {
        let mut e = editor();
        e.insert_char('世');
        e.insert_char('界');
        e.backspace();
        assert_eq!(e.content(), "世");
        assert_eq!(e.cursor_col(), 3);
    }

    // ─── Helper functions ────────────────────────────────────────────────

    #[test]
    fn test_prev_char_boundary_ascii() {
        let s = "hello";
        assert_eq!(prev_char_boundary(s, 3), 2);
        assert_eq!(prev_char_boundary(s, 1), 0);
        assert_eq!(prev_char_boundary(s, 0), 0);
    }

    #[test]
    fn test_prev_char_boundary_unicode() {
        let s = "世界"; // 3 bytes each
        assert_eq!(prev_char_boundary(s, 6), 3);
        assert_eq!(prev_char_boundary(s, 3), 0);
    }

    #[test]
    fn test_word_start_before() {
        assert_eq!(word_start_before("foo bar", 7), 4);
        assert_eq!(word_start_before("foo bar   ", 10), 4);
        assert_eq!(word_start_before("hello", 5), 0);
        assert_eq!(word_start_before("", 0), 0);
    }
}
