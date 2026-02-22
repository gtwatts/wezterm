//! Diff viewer: interactive state and ANSI rendering for code review.
//!
//! The diff viewer renders diffs as colored ANSI lines in the chat scroll area,
//! with navigation, inline comments, and approve/reject actions.

use crate::diff::{DiffHunk, DiffLine, DiffLineKind, FileDiff};

// ─── Color Palette (TokyoNight, matching screen.rs) ─────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

const FG: (u8, u8, u8) = (192, 202, 245);
const SUCCESS: (u8, u8, u8) = (158, 206, 106);
const ERROR: (u8, u8, u8) = (247, 118, 142);
const ACCENT: (u8, u8, u8) = (122, 162, 247);
const MUTED: (u8, u8, u8) = (86, 95, 137);
const BORDER: (u8, u8, u8) = (59, 66, 97);
const SELECTION: (u8, u8, u8) = (40, 44, 66);
const INFO: (u8, u8, u8) = (125, 207, 255);

// Diff-specific colors from design doc
const DIFF_ADD_BG: (u8, u8, u8) = (30, 50, 30);
const DIFF_DEL_BG: (u8, u8, u8) = (50, 30, 30);
const DIFF_ADD_EMPHASIS_BG: (u8, u8, u8) = (40, 70, 40);
const DIFF_DEL_EMPHASIS_BG: (u8, u8, u8) = (70, 40, 40);

fn fgc(c: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}
fn bgc(c: (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

// Box drawing
const BOX_TL: char = '\u{256D}'; // ╭
const BOX_TR: char = '\u{256E}'; // ╮
const BOX_BL: char = '\u{2570}'; // ╰
const BOX_BR: char = '\u{256F}'; // ╯
const BOX_H: char = '\u{2500}'; // ─
const BOX_V: char = '\u{2502}'; // │
const BOX_SEP: char = '\u{251C}'; // ├

const CLEAR_EOL: &str = "\x1b[K";

// ─── Inline Comment ─────────────────────────────────────────────────────

/// A comment attached to a specific line in a diff.
#[derive(Debug, Clone)]
pub struct InlineComment {
    /// Index of the file in the diffs list.
    pub file_idx: usize,
    /// Line number (in the new file for additions/context, old for deletions).
    pub line_no: usize,
    /// The comment text.
    pub text: String,
}

/// Action taken by the user after reviewing a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// Approve all changes.
    Approve,
    /// Reject all changes (discard).
    Reject,
    /// Request changes with comments.
    RequestChanges,
}

// ─── DiffViewer ─────────────────────────────────────────────────────────

/// Interactive state for a diff review session.
pub struct DiffViewer {
    /// The diffs being reviewed.
    pub diffs: Vec<FileDiff>,
    /// Current file index.
    pub current_file: usize,
    /// Cursor line within the flattened diff lines.
    pub cursor_line: usize,
    /// Scroll offset for the rendered view.
    pub scroll_offset: usize,
    /// Collected inline comments (not yet submitted).
    pub comments: Vec<InlineComment>,
    /// Whether the viewer is in comment-input mode.
    pub commenting: bool,
    /// In-progress comment text (while `commenting` is true).
    pub comment_input: String,
    /// Whether this diff has been resolved (approved/rejected).
    pub resolved: bool,
    /// Description from the agent (shown in the header).
    pub description: String,
}

impl DiffViewer {
    /// Create a new DiffViewer for the given diffs.
    pub fn new(diffs: Vec<FileDiff>, description: String) -> Self {
        Self {
            diffs,
            current_file: 0,
            cursor_line: 0,
            scroll_offset: 0,
            comments: Vec::new(),
            commenting: false,
            comment_input: String::new(),
            resolved: false,
            description,
        }
    }

    /// Total number of renderable lines across all hunks in the current file.
    fn total_lines(&self) -> usize {
        self.current_diff()
            .map(|d| d.hunks.iter().map(|h| h.lines.len()).sum())
            .unwrap_or(0)
    }

    /// Get the current file diff being viewed.
    pub fn current_diff(&self) -> Option<&FileDiff> {
        self.diffs.get(self.current_file)
    }

    // ── Navigation ──────────────────────────────────────────────────────

    /// Move cursor down one line.
    pub fn move_down(&mut self) {
        let total = self.total_lines();
        if total > 0 && self.cursor_line < total - 1 {
            self.cursor_line += 1;
        }
    }

    /// Move cursor up one line.
    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
        }
    }

    /// Jump to the next hunk.
    pub fn next_hunk(&mut self) {
        if let Some(diff) = self.current_diff() {
            let mut offset = 0;
            for hunk in &diff.hunks {
                let hunk_end = offset + hunk.lines.len();
                if offset > self.cursor_line {
                    self.cursor_line = offset;
                    return;
                }
                offset = hunk_end;
            }
        }
    }

    /// Jump to the previous hunk.
    pub fn prev_hunk(&mut self) {
        if let Some(diff) = self.current_diff() {
            let mut offsets: Vec<usize> = Vec::new();
            let mut offset = 0;
            for hunk in &diff.hunks {
                offsets.push(offset);
                offset += hunk.lines.len();
            }
            // Find the last offset that is strictly before cursor_line
            for &o in offsets.iter().rev() {
                if o < self.cursor_line {
                    self.cursor_line = o;
                    return;
                }
            }
        }
    }

    /// Jump to the next file.
    pub fn next_file(&mut self) {
        if self.current_file + 1 < self.diffs.len() {
            self.current_file += 1;
            self.cursor_line = 0;
            self.scroll_offset = 0;
        }
    }

    /// Jump to the previous file.
    pub fn prev_file(&mut self) {
        if self.current_file > 0 {
            self.current_file -= 1;
            self.cursor_line = 0;
            self.scroll_offset = 0;
        }
    }

    /// Toggle collapse on the hunk containing the current cursor.
    pub fn toggle_hunk_collapse(&mut self) {
        if let Some(diff) = self.diffs.get_mut(self.current_file) {
            let mut offset = 0;
            for hunk in &mut diff.hunks {
                let hunk_end = offset + hunk.lines.len();
                if self.cursor_line >= offset && self.cursor_line < hunk_end {
                    hunk.collapsed = !hunk.collapsed;
                    return;
                }
                offset = hunk_end;
            }
        }
    }

    // ── Comment Mode ────────────────────────────────────────────────────

    /// Enter comment mode at the current cursor position.
    pub fn start_comment(&mut self) {
        self.commenting = true;
        self.comment_input.clear();
    }

    /// Cancel comment input.
    pub fn cancel_comment(&mut self) {
        self.commenting = false;
        self.comment_input.clear();
    }

    /// Submit the current comment.
    pub fn submit_comment(&mut self) {
        if self.comment_input.trim().is_empty() {
            self.cancel_comment();
            return;
        }

        // Determine the line number for this comment
        let line_no = self.cursor_line_number().unwrap_or(0);

        self.comments.push(InlineComment {
            file_idx: self.current_file,
            line_no,
            text: self.comment_input.clone(),
        });

        self.commenting = false;
        self.comment_input.clear();
    }

    /// Insert a character into the comment input.
    pub fn comment_insert_char(&mut self, ch: char) {
        self.comment_input.push(ch);
    }

    /// Delete the last character from the comment input.
    pub fn comment_backspace(&mut self) {
        self.comment_input.pop();
    }

    /// Get the line number (new-side preferred) for the current cursor position.
    fn cursor_line_number(&self) -> Option<usize> {
        let diff = self.current_diff()?;
        let mut offset = 0;
        for hunk in &diff.hunks {
            for line in &hunk.lines {
                if offset == self.cursor_line {
                    return line.new_lineno.or(line.old_lineno);
                }
                offset += 1;
            }
        }
        None
    }

    /// Check if the line at the cursor has a comment.
    fn has_comment_at(&self, file_idx: usize, line_no: usize) -> Option<&InlineComment> {
        self.comments
            .iter()
            .find(|c| c.file_idx == file_idx && c.line_no == line_no)
    }

    // ── Rendering ───────────────────────────────────────────────────────

    /// Render the diff viewer as ANSI lines for the chat scroll area.
    ///
    /// Returns a single string with `\r\n` line endings suitable for
    /// writing to the virtual terminal.
    pub fn render(&self, width: usize) -> String {
        let mut out = String::with_capacity(4096);
        let w = width.max(40);

        if self.diffs.is_empty() {
            out.push_str(&format!(
                "\r\n{}{}No changes to review.{RESET}\r\n",
                fgc(MUTED), DIM,
            ));
            return out;
        }

        // File header
        if let Some(diff) = self.current_diff() {
            out.push_str(&self.render_file_header(diff, w));

            // Hunks
            let mut flat_idx = 0;
            for hunk in &diff.hunks {
                out.push_str(&self.render_hunk_header(hunk, w));

                if hunk.collapsed {
                    let line_count = hunk.lines.len();
                    out.push_str(&format!(
                        "{}  {} [{line_count} lines, Space to expand]{RESET}\r\n",
                        fgc(MUTED), DIM,
                    ));
                    flat_idx += hunk.lines.len();
                    continue;
                }

                for line in &hunk.lines {
                    let is_cursor = flat_idx == self.cursor_line && !self.resolved;
                    let line_no = line.new_lineno.or(line.old_lineno).unwrap_or(0);
                    let comment = self.has_comment_at(self.current_file, line_no);
                    out.push_str(&render_diff_line(line, w, is_cursor, comment.is_some()));

                    // Render inline comment below the line
                    if let Some(c) = comment {
                        out.push_str(&render_inline_comment(&c.text, w));
                    }

                    // If commenting at this line, show the input
                    if self.commenting && is_cursor {
                        out.push_str(&self.render_comment_input(w));
                    }

                    flat_idx += 1;
                }
            }

            // File navigation indicator for multi-file diffs
            if self.diffs.len() > 1 {
                out.push_str(&format!(
                    "{}  File {}/{} — [ ] prev  ] next{RESET}\r\n",
                    fgc(MUTED),
                    self.current_file + 1,
                    self.diffs.len(),
                ));
            }
        }

        // Action bar (only if not resolved)
        if !self.resolved {
            out.push_str(&self.render_action_bar(w));
        }

        out
    }

    /// Render the file header block.
    fn render_file_header(&self, diff: &FileDiff, w: usize) -> String {
        let border = fgc(BORDER);
        let accent = fgc(ACCENT);

        let path = &diff.new_path;
        let stats = format!(
            "+{} -{}",
            diff.stats.additions, diff.stats.deletions
        );
        let kind_label = match &diff.kind {
            crate::diff::DiffKind::Added => " (new file)",
            crate::diff::DiffKind::Deleted => " (deleted)",
            crate::diff::DiffKind::Modified => "",
            crate::diff::DiffKind::Renamed { old_path } => {
                // Can't return a reference to local, just indicate rename
                return self.render_rename_header(old_path, path, &stats, w);
            }
        };

        let title = format!(" Diff: {path} ({stats}){kind_label} ");
        let fill_len = w.saturating_sub(title.len() + 2);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        format!(
            "\r\n{border}{BOX_TL}{BOX_H}{RESET}{accent}{BOLD}{title}{RESET}{border}{fill}{BOX_TR}{RESET}\r\n",
        )
    }

    /// Render a rename header.
    fn render_rename_header(
        &self,
        old_path: &str,
        new_path: &str,
        stats: &str,
        w: usize,
    ) -> String {
        let border = fgc(BORDER);
        let accent = fgc(ACCENT);

        let title = format!(" Diff: {old_path} -> {new_path} ({stats}) (renamed) ");
        let fill_len = w.saturating_sub(title.len() + 2);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        format!(
            "\r\n{border}{BOX_TL}{BOX_H}{RESET}{accent}{BOLD}{title}{RESET}{border}{fill}{BOX_TR}{RESET}\r\n",
        )
    }

    /// Render a hunk header line.
    fn render_hunk_header(&self, hunk: &DiffHunk, w: usize) -> String {
        let border = fgc(BORDER);
        let info = fgc(INFO);

        let header = &hunk.header;
        let fill_len = w.saturating_sub(header.len() + 6);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        format!(
            "{border}{BOX_V}{RESET} {info}{DIM}{header}{RESET} {border}{fill}{RESET}\r\n",
        )
    }

    /// Render the comment input line.
    fn render_comment_input(&self, w: usize) -> String {
        let border = fgc(BORDER);
        let accent = fgc(ACCENT);
        let fg_main = fgc(FG);

        let prefix = "  Comment: ";
        let input_w = w.saturating_sub(prefix.len() + 4);
        let display: String = self.comment_input.chars().take(input_w).collect();

        format!(
            "{border}{BOX_V}{RESET} {accent}{prefix}{RESET}{fg_main}{display}{RESET}{CLEAR_EOL}\r\n",
        )
    }

    /// Render the action bar at the bottom.
    fn render_action_bar(&self, w: usize) -> String {
        let border = fgc(BORDER);
        let key_bg = bgc(SELECTION);
        let key_fg = fgc(ACCENT);
        let muted = fgc(MUTED);
        let accent = fgc(ACCENT);

        let sep: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        let mut out = format!("{border}{BOX_SEP}{sep}{RESET}\r\n");

        let comment_count = if self.comments.is_empty() {
            String::new()
        } else {
            format!(
                "  {accent}({} comment{}){RESET}",
                self.comments.len(),
                if self.comments.len() == 1 { "" } else { "s" }
            )
        };

        out.push_str(&format!(
            "{border}{BOX_V}{RESET} \
             {key_bg}{key_fg}{BOLD} y {RESET} {muted}approve{RESET}  \
             {key_bg}{key_fg}{BOLD} n {RESET} {muted}reject{RESET}  \
             {key_bg}{key_fg}{BOLD} c {RESET} {muted}comment{RESET}  \
             {key_bg}{key_fg}{BOLD} j/k {RESET} {muted}nav{RESET}  \
             {key_bg}{key_fg}{BOLD} q {RESET} {muted}close{RESET}\
             {comment_count}\r\n",
        ));

        let bot: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        out.push_str(&format!("{border}{BOX_BL}{bot}{BOX_BR}{RESET}\r\n"));

        out
    }

    /// Format all comments for submission to the agent.
    pub fn format_comments_for_agent(&self) -> String {
        if self.comments.is_empty() {
            return String::new();
        }

        let mut out = String::from("Review comments:\n\n");
        for comment in &self.comments {
            let file = self
                .diffs
                .get(comment.file_idx)
                .map(|d| d.new_path.as_str())
                .unwrap_or("(unknown)");
            out.push_str(&format!(
                "- {}:{}: {}\n",
                file, comment.line_no, comment.text
            ));
        }
        out
    }
}

// ─── Line-level Rendering ───────────────────────────────────────────────

/// Render a single diff line with line numbers, marker, and colored content.
fn render_diff_line(
    line: &DiffLine,
    _width: usize,
    is_cursor: bool,
    has_comment: bool,
) -> String {
    let mut out = String::with_capacity(256);
    let muted = fgc(MUTED);
    let border = fgc(BORDER);

    // Line background based on kind
    let line_bg = match line.kind {
        DiffLineKind::Addition => bgc(DIFF_ADD_BG),
        DiffLineKind::Deletion => bgc(DIFF_DEL_BG),
        DiffLineKind::Context => String::new(),
    };

    let cursor_bg = if is_cursor {
        bgc(SELECTION)
    } else {
        String::new()
    };

    // Line numbers (5 chars each, right-aligned)
    let old_num = line
        .old_lineno
        .map(|n| format!("{:>4} ", n))
        .unwrap_or_else(|| "     ".to_string());
    let new_num = line
        .new_lineno
        .map(|n| format!("{:>4} ", n))
        .unwrap_or_else(|| "     ".to_string());

    // Marker
    let (marker_ch, marker_color) = match line.kind {
        DiffLineKind::Addition => ('+', fgc(SUCCESS)),
        DiffLineKind::Deletion => ('-', fgc(ERROR)),
        DiffLineKind::Context => (' ', String::new()),
    };

    // Comment indicator
    let comment_mark = if has_comment { "[*]" } else { "   " };

    // Build the line
    out.push_str(&format!(
        "{border}{BOX_V}{RESET}{cursor_bg}{line_bg}\
         {muted}{old_num}{new_num}{RESET}\
         {cursor_bg}{line_bg}\
         {marker_color}{marker_ch}{RESET} \
         {cursor_bg}{line_bg}",
    ));

    // Content with word-level emphasis
    for seg in &line.segments {
        if seg.emphasized {
            let emph_bg = match line.kind {
                DiffLineKind::Addition => bgc(DIFF_ADD_EMPHASIS_BG),
                DiffLineKind::Deletion => bgc(DIFF_DEL_EMPHASIS_BG),
                DiffLineKind::Context => String::new(),
            };
            let emph_fg = match line.kind {
                DiffLineKind::Addition => fgc(SUCCESS),
                DiffLineKind::Deletion => fgc(ERROR),
                DiffLineKind::Context => fgc(FG),
            };
            out.push_str(&format!(
                "{emph_bg}{emph_fg}{BOLD}{text}{RESET}{cursor_bg}{line_bg}",
                text = seg.text.trim_end_matches('\n'),
            ));
        } else {
            let fg = fgc(FG);
            out.push_str(&format!(
                "{fg}{text}{RESET}{cursor_bg}{line_bg}",
                text = seg.text.trim_end_matches('\n'),
            ));
        }
    }

    // Comment marker
    if has_comment {
        out.push_str(&format!(" {}{comment_mark}{RESET}", fgc(ACCENT)));
    }

    out.push_str(&format!("{RESET}{CLEAR_EOL}\r\n"));
    out
}

/// Render an inline comment below a diff line.
fn render_inline_comment(text: &str, _width: usize) -> String {
    let border = fgc(BORDER);
    let accent = fgc(ACCENT);
    let fg_main = fgc(FG);

    format!(
        "{border}{BOX_V}{RESET}           {accent}[*] Comment:{RESET} {fg_main}{text}{RESET}{CLEAR_EOL}\r\n",
    )
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{self, DiffSegment};

    fn make_test_diff() -> FileDiff {
        let old = "fn main() {\n    println!(\"hello\");\n}\n";
        let new = "fn main() {\n    println!(\"hello world\");\n    println!(\"goodbye\");\n}\n";
        diff::compute_file_diff(Some("src/main.rs"), "src/main.rs", old, new, 3)
    }

    #[test]
    fn test_viewer_creation() {
        let diff = make_test_diff();
        let viewer = DiffViewer::new(vec![diff], "Test edit".into());
        assert_eq!(viewer.current_file, 0);
        assert_eq!(viewer.cursor_line, 0);
        assert!(!viewer.commenting);
        assert!(!viewer.resolved);
        assert!(viewer.comments.is_empty());
    }

    #[test]
    fn test_viewer_navigation() {
        let diff = make_test_diff();
        let viewer_lines = diff.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        assert_eq!(viewer.cursor_line, 0);
        viewer.move_down();
        assert_eq!(viewer.cursor_line, 1);
        viewer.move_up();
        assert_eq!(viewer.cursor_line, 0);

        // Can't go below 0
        viewer.move_up();
        assert_eq!(viewer.cursor_line, 0);

        // Move to end
        for _ in 0..viewer_lines + 5 {
            viewer.move_down();
        }
        assert_eq!(viewer.cursor_line, viewer_lines - 1);
    }

    #[test]
    fn test_viewer_file_navigation() {
        let diff1 = make_test_diff();
        let diff2 = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff1, diff2], "Test".into());

        assert_eq!(viewer.current_file, 0);
        viewer.next_file();
        assert_eq!(viewer.current_file, 1);
        assert_eq!(viewer.cursor_line, 0); // Reset on file change

        viewer.next_file(); // Can't go past last
        assert_eq!(viewer.current_file, 1);

        viewer.prev_file();
        assert_eq!(viewer.current_file, 0);

        viewer.prev_file(); // Can't go below 0
        assert_eq!(viewer.current_file, 0);
    }

    #[test]
    fn test_viewer_comments() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        // Start comment
        viewer.move_down(); // Move to a line with content
        viewer.start_comment();
        assert!(viewer.commenting);

        // Type a comment
        for ch in "This needs to be fixed".chars() {
            viewer.comment_insert_char(ch);
        }
        assert_eq!(viewer.comment_input, "This needs to be fixed");

        // Submit
        viewer.submit_comment();
        assert!(!viewer.commenting);
        assert_eq!(viewer.comments.len(), 1);
        assert_eq!(viewer.comments[0].text, "This needs to be fixed");
    }

    #[test]
    fn test_viewer_cancel_comment() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        viewer.start_comment();
        viewer.comment_insert_char('x');
        viewer.cancel_comment();
        assert!(!viewer.commenting);
        assert!(viewer.comments.is_empty());
        assert!(viewer.comment_input.is_empty());
    }

    #[test]
    fn test_viewer_empty_comment_discarded() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        viewer.start_comment();
        viewer.submit_comment(); // Empty comment
        assert!(!viewer.commenting);
        assert!(viewer.comments.is_empty());
    }

    #[test]
    fn test_viewer_comment_backspace() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        viewer.start_comment();
        viewer.comment_insert_char('a');
        viewer.comment_insert_char('b');
        viewer.comment_backspace();
        assert_eq!(viewer.comment_input, "a");
    }

    #[test]
    fn test_render_output() {
        let diff = make_test_diff();
        let viewer = DiffViewer::new(vec![diff], "Test edit".into());
        let output = viewer.render(80);

        // Should contain diff colors (ANSI sequences)
        assert!(output.contains("src/main.rs"));
        // Should contain action bar keys
        assert!(output.contains("approve"));
        assert!(output.contains("reject"));
        assert!(output.contains("comment"));
    }

    #[test]
    fn test_render_empty_diffs() {
        let viewer = DiffViewer::new(Vec::new(), "Empty".into());
        let output = viewer.render(80);
        assert!(output.contains("No changes"));
    }

    #[test]
    fn test_render_diff_line_addition() {
        let line = DiffLine {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(5),
            segments: vec![DiffSegment {
                text: "    new line".to_string(),
                emphasized: false,
            }],
        };
        let output = render_diff_line(&line, 80, false, false);
        // Should contain green-ish ANSI bg (30,50,30)
        assert!(output.contains("\x1b[48;2;30;50;30m"));
        // Should contain the + marker
        assert!(output.contains('+'));
        // Should contain the line number
        assert!(output.contains("5"));
    }

    #[test]
    fn test_render_diff_line_deletion() {
        let line = DiffLine {
            kind: DiffLineKind::Deletion,
            old_lineno: Some(3),
            new_lineno: None,
            segments: vec![DiffSegment {
                text: "    old line".to_string(),
                emphasized: false,
            }],
        };
        let output = render_diff_line(&line, 80, false, false);
        // Should contain red-ish ANSI bg (50,30,30)
        assert!(output.contains("\x1b[48;2;50;30;30m"));
        // Should contain the - marker
        assert!(output.contains('-'));
    }

    #[test]
    fn test_render_diff_line_with_emphasis() {
        let line = DiffLine {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(5),
            segments: vec![
                DiffSegment {
                    text: "let x = ".to_string(),
                    emphasized: false,
                },
                DiffSegment {
                    text: "new_value".to_string(),
                    emphasized: true,
                },
                DiffSegment {
                    text: ";".to_string(),
                    emphasized: false,
                },
            ],
        };
        let output = render_diff_line(&line, 80, false, false);
        // Should contain emphasis bg (40,70,40)
        assert!(output.contains("\x1b[48;2;40;70;40m"));
    }

    #[test]
    fn test_render_diff_line_cursor() {
        let line = DiffLine {
            kind: DiffLineKind::Context,
            old_lineno: Some(1),
            new_lineno: Some(1),
            segments: vec![DiffSegment {
                text: "context line".to_string(),
                emphasized: false,
            }],
        };
        let output = render_diff_line(&line, 80, true, false);
        // Should contain selection bg (40,44,66)
        assert!(output.contains("\x1b[48;2;40;44;66m"));
    }

    #[test]
    fn test_render_diff_line_with_comment_marker() {
        let line = DiffLine {
            kind: DiffLineKind::Addition,
            old_lineno: None,
            new_lineno: Some(5),
            segments: vec![DiffSegment {
                text: "text".to_string(),
                emphasized: false,
            }],
        };
        let output = render_diff_line(&line, 80, false, true);
        assert!(output.contains("[*]"));
    }

    #[test]
    fn test_format_comments_for_agent() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());
        viewer.comments.push(InlineComment {
            file_idx: 0,
            line_no: 2,
            text: "Fix this".to_string(),
        });
        viewer.comments.push(InlineComment {
            file_idx: 0,
            line_no: 5,
            text: "Also check this".to_string(),
        });

        let formatted = viewer.format_comments_for_agent();
        assert!(formatted.contains("src/main.rs:2: Fix this"));
        assert!(formatted.contains("src/main.rs:5: Also check this"));
    }

    #[test]
    fn test_render_resolved_no_action_bar() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());
        viewer.resolved = true;
        let output = viewer.render(80);
        // Resolved viewer should NOT show action bar
        assert!(!output.contains("approve"));
    }

    #[test]
    fn test_hunk_collapse() {
        let diff = make_test_diff();
        let mut viewer = DiffViewer::new(vec![diff], "Test".into());

        // Initially not collapsed
        let output1 = viewer.render(80);
        assert!(!output1.contains("Space to expand"));

        // Toggle collapse
        viewer.toggle_hunk_collapse();
        let output2 = viewer.render(80);
        assert!(output2.contains("Space to expand"));

        // Toggle back
        viewer.toggle_hunk_collapse();
        let output3 = viewer.render(80);
        assert!(!output3.contains("Space to expand"));
    }

    #[test]
    fn test_inline_comment_rendering() {
        let output = render_inline_comment("Great fix!", 80);
        assert!(output.contains("[*] Comment:"));
        assert!(output.contains("Great fix!"));
    }
}
