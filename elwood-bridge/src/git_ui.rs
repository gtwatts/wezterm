//! Git integration UI: staging view, commit view, and git operations.
//!
//! Provides interactive overlays for common git workflows (stage, commit, push, log)
//! rendered as ANSI text in the ElwoodPane's chat scroll area.

use std::path::Path;
use std::process::Command;

// ─── Color Palette (TokyoNight, matching screen.rs / diff_viewer.rs) ─────

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
const WARNING: (u8, u8, u8) = (224, 175, 104);
const INFO: (u8, u8, u8) = (125, 207, 255);

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
const BOX_H: char = '\u{2500}';  // ─
const BOX_V: char = '\u{2502}';  // │

const CLEAR_EOL: &str = "\x1b[K";

// ─── FileStatus ──────────────────────────────────────────────────────────

/// Classification of a file's git status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Copied,
}

impl FileStatus {
    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Untracked => "untracked",
            Self::Copied => "copied",
        }
    }

    /// Color tuple for this status.
    fn color(&self) -> (u8, u8, u8) {
        match self {
            Self::Modified => WARNING,
            Self::Added => SUCCESS,
            Self::Deleted => ERROR,
            Self::Renamed => INFO,
            Self::Untracked => MUTED,
            Self::Copied => INFO,
        }
    }
}

/// A file with its git status and staging state.
#[derive(Debug, Clone)]
pub struct GitFileStatus {
    /// Relative path of the file.
    pub path: String,
    /// What kind of change this is.
    pub status: FileStatus,
    /// Whether this file is currently staged (index).
    pub staged: bool,
}

// ─── Git Operations (sync, via CLI) ──────────────────────────────────────

/// Run a git command and return stdout, or an error string.
fn run_git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            stderr
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse `git status --porcelain=v1` output into structured file statuses.
pub fn get_file_statuses(cwd: &Path) -> Result<Vec<GitFileStatus>, String> {
    let output = run_git(cwd, &["status", "--porcelain=v1"])?;
    Ok(parse_porcelain_status(&output))
}

/// Parse porcelain v1 status lines.
///
/// Format: `XY path` where X = index status, Y = worktree status.
fn parse_porcelain_status(output: &str) -> Vec<GitFileStatus> {
    let mut files = Vec::new();

    for line in output.lines() {
        if line.len() < 3 {
            continue;
        }
        let index_code = line.as_bytes()[0];
        let worktree_code = line.as_bytes()[1];
        let path = line[3..].to_string();

        // Determine file status and staging state.
        // Index column (X): staged changes
        // Worktree column (Y): unstaged changes
        let (status, staged) = match (index_code, worktree_code) {
            (b'?', b'?') => (FileStatus::Untracked, false),
            (b'A', _) => (FileStatus::Added, true),
            (b'M', _) => (FileStatus::Modified, true),
            (b'D', _) => (FileStatus::Deleted, true),
            (b'R', _) => (FileStatus::Renamed, true),
            (b'C', _) => (FileStatus::Copied, true),
            (_, b'M') => (FileStatus::Modified, false),
            (_, b'D') => (FileStatus::Deleted, false),
            (_, b'A') => (FileStatus::Added, false),
            _ => continue,
        };

        files.push(GitFileStatus {
            path,
            status,
            staged,
        });
    }

    files
}

/// Stage specific files.
pub fn git_stage_files(cwd: &Path, paths: &[&str]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    args.extend(paths);
    run_git(cwd, &args)?;
    Ok(())
}

/// Unstage specific files.
pub fn git_unstage_files(cwd: &Path, paths: &[&str]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["reset", "HEAD", "--"];
    args.extend(paths);
    run_git(cwd, &args)?;
    Ok(())
}

/// Commit with the given message.
pub fn git_commit(cwd: &Path, message: &str) -> Result<String, String> {
    run_git(cwd, &["commit", "-m", message])
}

/// Push to remote (current branch).
pub fn git_push(cwd: &Path) -> Result<String, String> {
    run_git(cwd, &["push"])
}

/// Get recent log entries formatted nicely.
pub fn git_log(cwd: &Path, count: usize) -> Result<Vec<LogEntry>, String> {
    let count_arg = format!("-{count}");
    let output = run_git(
        cwd,
        &[
            "log",
            &count_arg,
            "--pretty=format:%h%x00%an%x00%ar%x00%s",
        ],
    )?;
    Ok(parse_log_entries(&output))
}

/// A single git log entry.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub hash: String,
    pub author: String,
    pub time_ago: String,
    pub subject: String,
}

/// Parse the null-separated log format.
fn parse_log_entries(output: &str) -> Vec<LogEntry> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '\0').collect();
            if parts.len() == 4 {
                Some(LogEntry {
                    hash: parts[0].to_string(),
                    author: parts[1].to_string(),
                    time_ago: parts[2].to_string(),
                    subject: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Get the staged diff for AI commit message generation.
pub fn git_staged_diff(cwd: &Path) -> Result<String, String> {
    run_git(cwd, &["diff", "--cached", "--no-color"])
}

/// Format a detailed `git status` for display.
pub fn format_git_status(cwd: &Path) -> Result<String, String> {
    let files = get_file_statuses(cwd)?;
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "unknown".to_string());

    let mut out = String::new();
    out.push_str(&format!(
        "{}{}On branch {}{}\r\n\r\n",
        fgc(ACCENT),
        BOLD,
        branch,
        RESET
    ));

    if files.is_empty() {
        out.push_str(&format!(
            "{}Nothing to commit, working tree clean.{}\r\n",
            fgc(SUCCESS), RESET
        ));
        return Ok(out);
    }

    // Staged files
    let staged: Vec<_> = files.iter().filter(|f| f.staged).collect();
    if !staged.is_empty() {
        out.push_str(&format!(
            "{}{}Changes to be committed:{}\r\n",
            fgc(SUCCESS), BOLD, RESET
        ));
        for f in &staged {
            let color = fgc(f.status.color());
            out.push_str(&format!(
                "  {}  {:<12}{} {}{}\r\n",
                color,
                f.status.label(),
                RESET,
                fgc(FG),
                f.path
            ));
        }
        out.push_str("\r\n");
    }

    // Unstaged modified/deleted
    let unstaged: Vec<_> = files
        .iter()
        .filter(|f| !f.staged && f.status != FileStatus::Untracked)
        .collect();
    if !unstaged.is_empty() {
        out.push_str(&format!(
            "{}{}Changes not staged:{}\r\n",
            fgc(WARNING), BOLD, RESET
        ));
        for f in &unstaged {
            let color = fgc(f.status.color());
            out.push_str(&format!(
                "  {}  {:<12}{} {}{}\r\n",
                color,
                f.status.label(),
                RESET,
                fgc(FG),
                f.path
            ));
        }
        out.push_str("\r\n");
    }

    // Untracked
    let untracked: Vec<_> = files
        .iter()
        .filter(|f| f.status == FileStatus::Untracked)
        .collect();
    if !untracked.is_empty() {
        out.push_str(&format!(
            "{}{}Untracked files:{}\r\n",
            fgc(MUTED), BOLD, RESET
        ));
        for f in &untracked {
            out.push_str(&format!("  {}  {}{}\r\n", fgc(MUTED), f.path, RESET));
        }
        out.push_str("\r\n");
    }

    Ok(out)
}

/// Format a pretty `git log` for display.
pub fn format_git_log(cwd: &Path, count: usize) -> Result<String, String> {
    let entries = git_log(cwd, count)?;
    if entries.is_empty() {
        return Ok(format!(
            "{}No commits found.{}\r\n",
            fgc(MUTED), RESET
        ));
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{}{}Recent commits:{}\r\n\r\n",
        fgc(ACCENT), BOLD, RESET
    ));

    for entry in &entries {
        out.push_str(&format!(
            "  {}{}{}  {}{}{}  {}{}{}  {}{}{}\r\n",
            fgc(WARNING),
            entry.hash,
            RESET,
            fgc(ACCENT),
            entry.author,
            RESET,
            fgc(MUTED),
            entry.time_ago,
            RESET,
            fgc(FG),
            entry.subject,
            RESET,
        ));
    }

    Ok(out)
}

/// Format push output for display.
pub fn format_git_push_result(result: Result<String, String>) -> String {
    match result {
        Ok(output) => {
            let mut out = String::new();
            out.push_str(&format!(
                "\r\n{}{}Push successful{}\r\n",
                fgc(SUCCESS), BOLD, RESET
            ));
            if !output.is_empty() {
                out.push_str(&format!("{}{}{}\r\n", fgc(FG), output, RESET));
            }
            out
        }
        Err(e) => format!(
            "\r\n{}{}Push failed:{} {}{}{}\r\n",
            fgc(ERROR),
            BOLD,
            RESET,
            fgc(FG),
            e,
            RESET
        ),
    }
}

// ─── StagingView ─────────────────────────────────────────────────────────

/// Interactive file staging view.
pub struct StagingView {
    /// All files from `git status`.
    pub files: Vec<GitFileStatus>,
    /// Currently highlighted file index.
    pub cursor: usize,
    /// Working directory for git operations.
    pub cwd: std::path::PathBuf,
}

impl StagingView {
    /// Create a new StagingView by reading the current git status.
    pub fn new(cwd: &Path) -> Result<Self, String> {
        let files = get_file_statuses(cwd)?;
        if files.is_empty() {
            return Err("No changes to stage.".to_string());
        }
        Ok(Self {
            files,
            cursor: 0,
            cwd: cwd.to_path_buf(),
        })
    }

    /// Move cursor down.
    pub fn move_down(&mut self) {
        if !self.files.is_empty() && self.cursor < self.files.len() - 1 {
            self.cursor += 1;
        }
    }

    /// Move cursor up.
    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Toggle the staged state of the file at the cursor.
    pub fn toggle_current(&mut self) {
        if let Some(file) = self.files.get_mut(self.cursor) {
            if file.staged {
                // Unstage
                let _ = git_unstage_files(&self.cwd, &[file.path.as_str()]);
                file.staged = false;
            } else {
                // Stage
                let _ = git_stage_files(&self.cwd, &[file.path.as_str()]);
                file.staged = true;
            }
        }
    }

    /// Stage/unstage all files.
    pub fn toggle_all(&mut self) {
        let all_staged = self.files.iter().all(|f| f.staged);
        if all_staged {
            // Unstage all
            let paths: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
            let _ = git_unstage_files(&self.cwd, &paths);
            for f in &mut self.files {
                f.staged = false;
            }
        } else {
            // Stage all
            let paths: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
            let _ = git_stage_files(&self.cwd, &paths);
            for f in &mut self.files {
                f.staged = true;
            }
        }
    }

    /// Return the list of currently staged file paths.
    pub fn staged_paths(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| f.staged)
            .map(|f| f.path.as_str())
            .collect()
    }

    /// Refresh file statuses from git.
    pub fn refresh(&mut self) {
        if let Ok(files) = get_file_statuses(&self.cwd) {
            self.files = files;
            if self.cursor >= self.files.len() {
                self.cursor = self.files.len().saturating_sub(1);
            }
        }
    }

    /// Render the staging view as ANSI for the chat scroll area.
    pub fn render(&self, width: usize) -> String {
        let w = width.max(40);
        let border = fgc(BORDER);
        let accent = fgc(ACCENT);
        let fg_main = fgc(FG);
        let muted = fgc(MUTED);

        let mut out = String::with_capacity(2048);

        // Title bar
        let title = " Stage Files ";
        let staged_count = self.files.iter().filter(|f| f.staged).count();
        let count_label = format!(" {staged_count}/{} staged ", self.files.len());
        let fill_len = w.saturating_sub(title.len() + count_label.len() + 2);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        out.push_str(&format!(
            "\r\n{border}{BOX_TL}{BOX_H}{RESET}{accent}{BOLD}{title}{RESET}{border}{fill}{RESET}{muted}{count_label}{RESET}{border}{BOX_TR}{RESET}\r\n",
        ));

        // File list
        for (i, file) in self.files.iter().enumerate() {
            let is_cursor = i == self.cursor;
            let cursor_bg = if is_cursor {
                bgc(SELECTION)
            } else {
                String::new()
            };
            let pointer = if is_cursor { "\u{25B8}" } else { " " };
            let checkbox = if file.staged { "[x]" } else { "[ ]" };
            let status_color = fgc(file.status.color());
            let status_label = format!("({})", file.status.label());

            let path_w = w.saturating_sub(26);
            let path_display: String = file.path.chars().take(path_w).collect();

            out.push_str(&format!(
                "{border}{BOX_V}{RESET}{cursor_bg} {accent}{pointer}{RESET}{cursor_bg} {fg_main}{checkbox}{RESET}{cursor_bg} {fg_main}{path_display}{RESET}{cursor_bg}  {status_color}{status_label}{RESET}{CLEAR_EOL}\r\n",
            ));
        }

        // Action bar
        let sep: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        out.push_str(&format!("{border}{sep}{RESET}\r\n"));

        let key_bg = bgc(SELECTION);
        let key_fg = fgc(ACCENT);
        out.push_str(&format!(
            "{border}{BOX_V}{RESET} \
             {key_bg}{key_fg}{BOLD} Space {RESET} {muted}toggle{RESET}  \
             {key_bg}{key_fg}{BOLD} a {RESET} {muted}all{RESET}  \
             {key_bg}{key_fg}{BOLD} Enter {RESET} {muted}confirm{RESET}  \
             {key_bg}{key_fg}{BOLD} Esc {RESET} {muted}cancel{RESET}\r\n",
        ));

        let bot: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        out.push_str(&format!("{border}{BOX_BL}{bot}{BOX_BR}{RESET}\r\n"));

        out
    }
}

// ─── CommitView ──────────────────────────────────────────────────────────

/// Interactive commit message editor.
pub struct CommitView {
    /// The commit message (may be AI-generated or manually edited).
    pub message: String,
    /// Number of staged files.
    pub staged_count: usize,
    /// Whether the user is editing the message.
    pub editing: bool,
    /// Cursor position within the message when editing.
    pub cursor_pos: usize,
    /// Working directory for the commit.
    pub cwd: std::path::PathBuf,
}

impl CommitView {
    /// Create a new CommitView with the given message and staged file count.
    pub fn new(cwd: &Path, message: String, staged_count: usize) -> Self {
        Self {
            message,
            staged_count,
            editing: false,
            cursor_pos: 0,
            cwd: cwd.to_path_buf(),
        }
    }

    /// Enter edit mode.
    pub fn start_edit(&mut self) {
        self.editing = true;
        self.cursor_pos = self.message.len();
    }

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, ch: char) {
        if self.cursor_pos <= self.message.len() {
            self.message.insert(self.cursor_pos, ch);
            self.cursor_pos += ch.len_utf8();
        }
    }

    /// Insert a newline.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            // Find the previous char boundary
            let prev = self.message[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.message.drain(prev..self.cursor_pos);
            self.cursor_pos = prev;
        }
    }

    /// Execute the commit.
    pub fn commit(&self) -> Result<String, String> {
        if self.message.trim().is_empty() {
            return Err("Commit message cannot be empty.".to_string());
        }
        git_commit(&self.cwd, &self.message)
    }

    /// Render the commit view as ANSI for the chat scroll area.
    pub fn render(&self, width: usize) -> String {
        let w = width.max(40);
        let border = fgc(BORDER);
        let accent = fgc(ACCENT);
        let fg_main = fgc(FG);
        let muted = fgc(MUTED);
        let success = fgc(SUCCESS);

        let mut out = String::with_capacity(2048);

        // Title bar
        let title = " Commit ";
        let count_label = format!(" {}{} file{} staged ",
            self.staged_count,
            "",
            if self.staged_count == 1 { "" } else { "s" }
        );
        let fill_len = w.saturating_sub(title.len() + count_label.len() + 2);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        out.push_str(&format!(
            "\r\n{border}{BOX_TL}{BOX_H}{RESET}{accent}{BOLD}{title}{RESET}{border}{fill}{RESET}{success}{count_label}{RESET}{border}{BOX_TR}{RESET}\r\n",
        ));

        // Label
        let label = if self.editing {
            "Editing message:"
        } else {
            "AI-generated message:"
        };
        out.push_str(&format!(
            "{border}{BOX_V}{RESET} {muted}{label}{RESET}{CLEAR_EOL}\r\n",
        ));

        // Message content box
        let inner_w = w.saturating_sub(6);
        let inner_fill: String = std::iter::repeat(BOX_H).take(inner_w).collect();

        out.push_str(&format!(
            "{border}{BOX_V}{RESET} {border}{BOX_TL}{inner_fill}{BOX_TR}{RESET}\r\n",
        ));

        for line in self.message.lines() {
            let display: String = line.chars().take(inner_w).collect();
            let pad = inner_w.saturating_sub(display.chars().count());
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} {border}{BOX_V}{RESET} {fg_main}{display}{}{RESET} {border}{BOX_V}{RESET}\r\n",
                " ".repeat(pad),
            ));
        }

        // Handle empty message
        if self.message.is_empty() {
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} {border}{BOX_V}{RESET} {muted}{DIM}(empty){RESET}{} {border}{BOX_V}{RESET}\r\n",
                " ".repeat(inner_w.saturating_sub(7)),
            ));
        }

        out.push_str(&format!(
            "{border}{BOX_V}{RESET} {border}{BOX_BL}{inner_fill}{BOX_BR}{RESET}\r\n",
        ));

        // Action bar
        let sep: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        out.push_str(&format!("{border}{sep}{RESET}\r\n"));

        let key_bg = bgc(SELECTION);
        let key_fg = fgc(ACCENT);
        if self.editing {
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} \
                 {key_bg}{key_fg}{BOLD} Enter {RESET} {muted}commit{RESET}  \
                 {key_bg}{key_fg}{BOLD} Esc {RESET} {muted}stop editing{RESET}\r\n",
            ));
        } else {
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} \
                 {key_bg}{key_fg}{BOLD} Enter {RESET} {muted}commit{RESET}  \
                 {key_bg}{key_fg}{BOLD} e {RESET} {muted}edit{RESET}  \
                 {key_bg}{key_fg}{BOLD} Esc {RESET} {muted}cancel{RESET}\r\n",
            ));
        }

        let bot: String = std::iter::repeat(BOX_H).take(w.saturating_sub(2)).collect();
        out.push_str(&format!("{border}{BOX_BL}{bot}{BOX_BR}{RESET}\r\n"));

        out
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_modified_staged() {
        let output = "M  src/main.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert!(files[0].staged);
    }

    #[test]
    fn test_parse_porcelain_modified_unstaged() {
        let output = " M src/lib.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].status, FileStatus::Modified);
        assert!(!files[0].staged);
    }

    #[test]
    fn test_parse_porcelain_untracked() {
        let output = "?? new_file.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "new_file.rs");
        assert_eq!(files[0].status, FileStatus::Untracked);
        assert!(!files[0].staged);
    }

    #[test]
    fn test_parse_porcelain_added() {
        let output = "A  tests/new_test.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Added);
        assert!(files[0].staged);
    }

    #[test]
    fn test_parse_porcelain_deleted() {
        let output = "D  old_file.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Deleted);
        assert!(files[0].staged);
    }

    #[test]
    fn test_parse_porcelain_mixed() {
        let output = "M  src/main.rs\n M src/lib.rs\n?? new.rs\nA  added.rs\nD  removed.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 5);

        assert_eq!(files[0].status, FileStatus::Modified);
        assert!(files[0].staged);

        assert_eq!(files[1].status, FileStatus::Modified);
        assert!(!files[1].staged);

        assert_eq!(files[2].status, FileStatus::Untracked);
        assert!(!files[2].staged);

        assert_eq!(files[3].status, FileStatus::Added);
        assert!(files[3].staged);

        assert_eq!(files[4].status, FileStatus::Deleted);
        assert!(files[4].staged);
    }

    #[test]
    fn test_parse_porcelain_empty() {
        let files = parse_porcelain_status("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_porcelain_renamed() {
        let output = "R  old_name.rs -> new_name.rs\n";
        let files = parse_porcelain_status(output);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Renamed);
        assert!(files[0].staged);
    }

    #[test]
    fn test_parse_log_entries() {
        let output = "abc1234\0John Doe\02 hours ago\0feat: add login\ndef5678\0Jane\01 day ago\0fix: typo";
        let entries = parse_log_entries(output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].hash, "abc1234");
        assert_eq!(entries[0].author, "John Doe");
        assert_eq!(entries[0].time_ago, "2 hours ago");
        assert_eq!(entries[0].subject, "feat: add login");
        assert_eq!(entries[1].hash, "def5678");
    }

    #[test]
    fn test_parse_log_entries_empty() {
        let entries = parse_log_entries("");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_file_status_label() {
        assert_eq!(FileStatus::Modified.label(), "modified");
        assert_eq!(FileStatus::Added.label(), "added");
        assert_eq!(FileStatus::Deleted.label(), "deleted");
        assert_eq!(FileStatus::Renamed.label(), "renamed");
        assert_eq!(FileStatus::Untracked.label(), "untracked");
        assert_eq!(FileStatus::Copied.label(), "copied");
    }

    #[test]
    fn test_staging_view_render() {
        let files = vec![
            GitFileStatus {
                path: "src/main.rs".to_string(),
                status: FileStatus::Modified,
                staged: true,
            },
            GitFileStatus {
                path: "src/lib.rs".to_string(),
                status: FileStatus::Modified,
                staged: false,
            },
            GitFileStatus {
                path: "new_file.rs".to_string(),
                status: FileStatus::Untracked,
                staged: false,
            },
        ];
        let view = StagingView {
            files,
            cursor: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };
        let output = view.render(80);
        assert!(output.contains("Stage Files"));
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("src/lib.rs"));
        assert!(output.contains("new_file.rs"));
        assert!(output.contains("[x]"));
        assert!(output.contains("[ ]"));
        assert!(output.contains("toggle"));
        assert!(output.contains("confirm"));
    }

    #[test]
    fn test_staging_view_navigation() {
        let files = vec![
            GitFileStatus {
                path: "a.rs".to_string(),
                status: FileStatus::Modified,
                staged: false,
            },
            GitFileStatus {
                path: "b.rs".to_string(),
                status: FileStatus::Modified,
                staged: false,
            },
            GitFileStatus {
                path: "c.rs".to_string(),
                status: FileStatus::Added,
                staged: true,
            },
        ];
        let mut view = StagingView {
            files,
            cursor: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };

        assert_eq!(view.cursor, 0);
        view.move_down();
        assert_eq!(view.cursor, 1);
        view.move_down();
        assert_eq!(view.cursor, 2);
        view.move_down(); // at end
        assert_eq!(view.cursor, 2);
        view.move_up();
        assert_eq!(view.cursor, 1);
        view.move_up();
        assert_eq!(view.cursor, 0);
        view.move_up(); // at start
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn test_staging_view_staged_paths() {
        let files = vec![
            GitFileStatus {
                path: "a.rs".to_string(),
                status: FileStatus::Modified,
                staged: true,
            },
            GitFileStatus {
                path: "b.rs".to_string(),
                status: FileStatus::Modified,
                staged: false,
            },
            GitFileStatus {
                path: "c.rs".to_string(),
                status: FileStatus::Added,
                staged: true,
            },
        ];
        let view = StagingView {
            files,
            cursor: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };
        let paths = view.staged_paths();
        assert_eq!(paths, vec!["a.rs", "c.rs"]);
    }

    #[test]
    fn test_commit_view_render() {
        let view = CommitView {
            message: "feat: add REST API authentication\n\n- Add JWT token validation\n- Implement login/register endpoints".to_string(),
            staged_count: 3,
            editing: false,
            cursor_pos: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };
        let output = view.render(80);
        assert!(output.contains("Commit"));
        assert!(output.contains("3 files staged"));
        assert!(output.contains("feat: add REST API"));
        assert!(output.contains("edit"));
        assert!(output.contains("commit"));
    }

    #[test]
    fn test_commit_view_edit() {
        let mut view = CommitView {
            message: "initial".to_string(),
            staged_count: 1,
            editing: false,
            cursor_pos: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };

        view.start_edit();
        assert!(view.editing);
        assert_eq!(view.cursor_pos, 7); // "initial".len()

        view.backspace();
        assert_eq!(view.message, "initia");

        view.insert_char('l');
        assert_eq!(view.message, "initial");

        // Render in edit mode
        let output = view.render(80);
        assert!(output.contains("Editing message"));
        assert!(output.contains("stop editing"));
    }

    #[test]
    fn test_commit_view_empty_message() {
        let view = CommitView {
            message: String::new(),
            staged_count: 1,
            editing: false,
            cursor_pos: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };
        let output = view.render(80);
        assert!(output.contains("(empty)"));
    }

    #[test]
    fn test_commit_view_commit_empty_rejected() {
        let view = CommitView {
            message: "   ".to_string(),
            staged_count: 1,
            editing: false,
            cursor_pos: 0,
            cwd: std::path::PathBuf::from("/tmp"),
        };
        let result = view.commit();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }
}
