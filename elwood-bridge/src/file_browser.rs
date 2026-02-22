//! File browser overlay for the Elwood pane (F2).
//!
//! Provides a `.gitignore`-aware, lazily-loaded directory tree that the user
//! can navigate, filter, and select files from. Selected files can be opened
//! in `$EDITOR` or attached as agent context via the `@` mechanism.
//!
//! ## Layout
//!
//! ```text
//! +-- Files -- ~/project -- 42 items --------+
//! | > filter...                                |
//! |                                            |
//! | v src/                                     |
//! |   > api/                                   |
//! |     main.rs              4.2 KB            |
//! |     lib.rs               1.1 KB            |
//! | > tests/                                   |
//! |   Cargo.toml             320 B             |
//! |   README.md              2.1 KB            |
//! |                                            |
//! | [Enter] Open  [Space] Preview  [@] Attach  |
//! | [/] Filter  [Esc] Close                    |
//! +--------------------------------------------+
//! ```

use ignore::WalkBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Maximum directory depth to walk during a scan.
const MAX_DEPTH: usize = 8;

/// Maximum number of entries returned from a single directory scan.
const MAX_DIR_ENTRIES: usize = 500;

/// Maximum number of preview lines to show for a file.
const PREVIEW_LINES: usize = 20;

/// Type of directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Directory,
    Symlink,
}

/// A single entry in the file tree.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Absolute path.
    pub path: PathBuf,
    /// Display name (just the file/dir name component).
    pub name: String,
    /// Entry type.
    pub entry_type: EntryType,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Nesting depth (0 = root level).
    pub depth: usize,
}

/// Interactive file tree browser state.
pub struct FileTree {
    /// Root directory being browsed.
    pub root: PathBuf,
    /// Flat list of currently visible entries (expanded tree).
    pub entries: Vec<FileEntry>,
    /// Currently selected index in `entries`.
    pub selected_index: usize,
    /// Set of directories that are expanded (shown open).
    pub expanded_dirs: HashSet<PathBuf>,
    /// Current filter query string.
    pub filter: String,
    /// Whether the filter input is active (typing mode).
    pub filter_active: bool,
    /// Scroll offset for the visible window.
    pub scroll_offset: usize,
    /// Cached preview text for the currently selected file.
    preview_cache: Option<(PathBuf, Vec<String>)>,
    /// Whether the preview panel is visible.
    pub show_preview: bool,
}

impl FileTree {
    /// Create a new file tree rooted at `root`.
    ///
    /// Scans the root directory (top-level only) on creation.
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            entries: Vec::new(),
            selected_index: 0,
            expanded_dirs: HashSet::new(),
            filter: String::new(),
            filter_active: false,
            scroll_offset: 0,
            preview_cache: None,
            show_preview: false,
        };
        tree.rebuild_entries();
        tree
    }

    /// Scan a directory for its immediate children, respecting `.gitignore`.
    ///
    /// Returns entries sorted: directories first (alphabetical), then files
    /// (alphabetical).
    fn scan_directory(path: &Path) -> Vec<FileEntry> {
        let depth_offset = path.components().count();
        let mut dirs = Vec::new();
        let mut files = Vec::new();

        let walker = WalkBuilder::new(path)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .max_depth(Some(1)) // immediate children only
            .sort_by_file_name(|a, b| a.cmp(b))
            .build();

        for entry in walker.flatten() {
            let entry_path = entry.path();
            // Skip the root itself
            if entry_path == path {
                continue;
            }

            let name = entry_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let ft = entry.file_type();
            let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
            let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);

            let size = if is_dir {
                0
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            };

            let entry_type = if is_dir {
                EntryType::Directory
            } else if is_symlink {
                EntryType::Symlink
            } else {
                EntryType::File
            };

            let depth = entry_path
                .components()
                .count()
                .saturating_sub(depth_offset);

            let fe = FileEntry {
                path: entry_path.to_path_buf(),
                name,
                entry_type,
                size,
                depth,
            };

            if is_dir {
                dirs.push(fe);
            } else {
                files.push(fe);
            }
        }

        dirs.extend(files);
        dirs.truncate(MAX_DIR_ENTRIES);
        dirs
    }

    /// Rebuild the flat entry list from the root directory and expanded dirs.
    fn rebuild_entries(&mut self) {
        let mut entries = Vec::new();
        let root = self.root.clone();
        self.collect_entries(&root, 0, &mut entries);

        // Apply filter if non-empty
        if !self.filter.is_empty() {
            let query = self.filter.to_lowercase();
            entries.retain(|e| {
                e.entry_type == EntryType::Directory
                    || e.name.to_lowercase().contains(&query)
                    || e.path
                        .strip_prefix(&self.root)
                        .unwrap_or(&e.path)
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
            });
            // Remove directories that have no matching file descendants
            // (simple: keep all dirs for now, they serve as structure)
        }

        self.entries = entries;
        // Clamp selection
        if self.selected_index >= self.entries.len() {
            self.selected_index = self.entries.len().saturating_sub(1);
        }
    }

    /// Recursively collect entries from `dir` into `out`.
    fn collect_entries(&self, dir: &Path, depth: usize, out: &mut Vec<FileEntry>) {
        if depth > MAX_DEPTH {
            return;
        }

        let children = Self::scan_directory(dir);
        for mut child in children {
            child.depth = depth;
            let is_expanded = child.entry_type == EntryType::Directory
                && self.expanded_dirs.contains(&child.path);
            let child_path = child.path.clone();
            out.push(child);

            if is_expanded {
                self.collect_entries(&child_path, depth + 1, out);
            }
        }
    }

    /// Toggle expand/collapse for the currently selected entry (if it's a directory).
    pub fn toggle_expand(&mut self) {
        if let Some(entry) = self.entries.get(self.selected_index) {
            if entry.entry_type == EntryType::Directory {
                let path = entry.path.clone();
                if self.expanded_dirs.contains(&path) {
                    self.expanded_dirs.remove(&path);
                } else {
                    self.expanded_dirs.insert(path);
                }
                self.rebuild_entries();
            }
        }
    }

    /// Move selection up by one.
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            self.invalidate_preview();
        }
        self.ensure_visible();
    }

    /// Move selection down by one.
    pub fn move_down(&mut self) {
        if self.selected_index + 1 < self.entries.len() {
            self.selected_index += 1;
            self.invalidate_preview();
        }
        self.ensure_visible();
    }

    /// Ensure the selected item is within the visible scroll window.
    fn ensure_visible(&mut self) {
        // We don't know the viewport height here; it's adjusted during render.
        // Store a flag and let render() fix scroll_offset.
    }

    /// Get the currently selected entry, if any.
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected_index)
    }

    /// Get the path of the currently selected entry.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected_entry().map(|e| e.path.as_path())
    }

    /// Set the filter query and rebuild the entry list.
    pub fn apply_filter(&mut self, query: &str) {
        self.filter = query.to_string();
        self.rebuild_entries();
    }

    /// Insert a character into the filter.
    pub fn filter_insert_char(&mut self, c: char) {
        self.filter.push(c);
        self.rebuild_entries();
    }

    /// Delete the last character from the filter.
    pub fn filter_backspace(&mut self) {
        self.filter.pop();
        self.rebuild_entries();
    }

    /// Clear the filter.
    pub fn filter_clear(&mut self) {
        self.filter.clear();
        self.rebuild_entries();
    }

    /// Invalidate the preview cache (e.g. after selection change).
    fn invalidate_preview(&mut self) {
        self.preview_cache = None;
    }

    /// Get preview lines for the selected file.
    ///
    /// Returns cached lines or reads the first N lines from disk.
    pub fn preview_lines(&mut self) -> Option<&[String]> {
        let entry = self.entries.get(self.selected_index)?;
        if entry.entry_type != EntryType::File {
            return None;
        }

        let path = entry.path.clone();

        // Check cache — if already cached for this path, return it
        let needs_load = match self.preview_cache {
            Some((ref cached_path, _)) if *cached_path == path => false,
            _ => true,
        };

        if needs_load {
            // Read first N lines
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => return None,
            };
            let lines: Vec<String> = content
                .lines()
                .take(PREVIEW_LINES)
                .map(|l| {
                    if l.len() > 120 {
                        let mut end = 120;
                        while !l.is_char_boundary(end) && end > 0 {
                            end -= 1;
                        }
                        format!("{}...", &l[..end])
                    } else {
                        l.to_string()
                    }
                })
                .collect();

            self.preview_cache = Some((path, lines));
        }

        self.preview_cache.as_ref().map(|(_, lines)| lines.as_slice())
    }

    /// Total number of entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Render the file browser as an ANSI overlay string.
    ///
    /// The overlay is drawn at absolute positions within the terminal,
    /// centered vertically and horizontally.
    pub fn render(&mut self, width: u16, height: u16) -> String {
        let r = "\x1b[0m";
        let bold = "\x1b[1m";
        let dim = "\x1b[2m";

        // Colors (Tokyo Night)
        let accent = "\x1b[38;2;122;162;247m";
        let fg = "\x1b[38;2;192;202;245m";
        let muted = "\x1b[38;2;86;95;137m";
        let success = "\x1b[38;2;158;206;106m";
        let warning = "\x1b[38;2;224;175;104m";
        let info = "\x1b[38;2;125;207;255m";
        let border = "\x1b[38;2;59;66;97m";
        let sel_bg = "\x1b[48;2;40;44;66m";

        // Overlay dimensions
        let overlay_w = (width as usize).min(72).max(40);
        let overlay_h = (height as usize).saturating_sub(4).min(30).max(10);
        let start_col = ((width as usize).saturating_sub(overlay_w)) / 2 + 1;
        let start_row = ((height as usize).saturating_sub(overlay_h)) / 2 + 1;

        // Calculate content area (minus borders, header, filter, footer)
        let content_h = overlay_h.saturating_sub(5); // top border + title + filter + footer + bottom border

        // Handle preview split
        let (tree_w, _preview_w) = if self.show_preview {
            let tw = overlay_w / 2;
            (tw, overlay_w.saturating_sub(tw))
        } else {
            (overlay_w, 0)
        };

        // Adjust scroll offset
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        } else if self.selected_index >= self.scroll_offset + content_h {
            self.scroll_offset = self.selected_index.saturating_sub(content_h - 1);
        }

        let goto = |row: usize, col: usize| -> String {
            format!("\x1b[{};{}H", row, col)
        };

        let hline = |n: usize| -> String {
            std::iter::repeat('\u{2500}').take(n).collect()
        };

        let mut out = String::with_capacity(4096);
        out.push_str("\x1b[s"); // save cursor
        out.push_str("\x1b[?25l"); // hide cursor

        let mut row = start_row;

        // ── Top border with title ────────────────────────────────────
        let root_display = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "~".to_string());
        let count = self.entries.len();
        let title = format!(" Files \u{2500}\u{2500} {root_display} \u{2500}\u{2500} {count} items ");
        let title_len = title.chars().count();
        let fill = overlay_w.saturating_sub(title_len + 2);

        out.push_str(&goto(row, start_col));
        out.push_str(&format!(
            "{accent}\u{256D}\u{2500}{r}{accent}{bold}{title}{r}{accent}{}{}\u{256E}{r}",
            hline(fill),
            "",
        ));
        row += 1;

        // ── Filter row ───────────────────────────────────────────────
        let inner_w = overlay_w.saturating_sub(4); // "| " + content + " |"
        out.push_str(&goto(row, start_col));

        if self.filter_active {
            let filter_display: String = self.filter.chars().take(inner_w.saturating_sub(4)).collect();
            let filter_pad = inner_w.saturating_sub(filter_display.chars().count() + 4);
            out.push_str(&format!(
                "{accent}\u{2502}{r} {info}\u{1F50D} {fg}{filter_display}\u{2588}{r}{}{accent}\u{2502}{r}",
                " ".repeat(filter_pad),
            ));
        } else if !self.filter.is_empty() {
            let filter_display: String = self.filter.chars().take(inner_w.saturating_sub(4)).collect();
            let filter_pad = inner_w.saturating_sub(filter_display.chars().count() + 4);
            out.push_str(&format!(
                "{accent}\u{2502}{r} {muted}\u{1F50D} {fg}{filter_display}{r}{}{accent}\u{2502}{r}",
                " ".repeat(filter_pad),
            ));
        } else {
            let placeholder = "/ to filter...";
            let pad = inner_w.saturating_sub(placeholder.len());
            out.push_str(&format!(
                "{accent}\u{2502}{r} {muted}{dim}{placeholder}{r}{}{accent}\u{2502}{r}",
                " ".repeat(pad),
            ));
        }
        row += 1;

        // ── Separator ────────────────────────────────────────────────
        out.push_str(&goto(row, start_col));
        out.push_str(&format!(
            "{border}\u{2502}{}{border}\u{2502}{r}",
            " ".repeat(overlay_w.saturating_sub(2)),
        ));
        row += 1;

        // ── File entries ─────────────────────────────────────────────
        let visible_end = (self.scroll_offset + content_h).min(self.entries.len());
        let visible_range = self.scroll_offset..visible_end;

        for (display_row, idx) in visible_range.enumerate() {
            let entry = &self.entries[idx];
            let is_selected = idx == self.selected_index;

            let indent = "  ".repeat(entry.depth);
            let indent_len = entry.depth * 2;

            let (icon, icon_color) = match entry.entry_type {
                EntryType::Directory => {
                    if self.expanded_dirs.contains(&entry.path) {
                        ("\u{25BE} ", warning) // down triangle (expanded)
                    } else {
                        ("\u{25B8} ", warning) // right triangle (collapsed)
                    }
                }
                EntryType::File => ("  ", fg),
                EntryType::Symlink => ("  ", info),
            };

            let name_color = match entry.entry_type {
                EntryType::Directory => success,
                EntryType::File => fg,
                EntryType::Symlink => info,
            };

            // File size (right-aligned)
            let size_str = if entry.entry_type == EntryType::File {
                format_file_size(entry.size)
            } else {
                String::new()
            };
            let size_len = size_str.len();

            // Available width for name
            let name_avail = tree_w
                .saturating_sub(4) // borders + padding
                .saturating_sub(indent_len)
                .saturating_sub(2) // icon
                .saturating_sub(size_len + 1); // size + gap

            let name_display: String = entry.name.chars().take(name_avail).collect();
            let name_len = name_display.chars().count();
            let name_pad = name_avail.saturating_sub(name_len);

            let sel_start = if is_selected { sel_bg } else { "" };
            let sel_end = if is_selected { r } else { "" };

            out.push_str(&goto(row + display_row, start_col));
            out.push_str(&format!(
                "{accent}\u{2502}{r}{sel_start} {indent}{icon_color}{icon}{r}{sel_start}{name_color}{name_display}{r}{sel_start}{}{muted}{size_str}{r}{sel_start} {sel_end}{accent}\u{2502}{r}",
                " ".repeat(name_pad),
            ));
        }

        // Fill remaining content rows if needed
        let rendered_rows = visible_end.saturating_sub(self.scroll_offset);
        for i in rendered_rows..content_h {
            out.push_str(&goto(row + i, start_col));
            out.push_str(&format!(
                "{accent}\u{2502}{r}{}{accent}\u{2502}{r}",
                " ".repeat(overlay_w.saturating_sub(2)),
            ));
        }
        row += content_h;

        // ── Footer with keybindings ──────────────────────────────────
        let key_bg = "\x1b[48;2;40;44;66m";
        let key_fg = "\x1b[38;2;192;202;245m";

        out.push_str(&goto(row, start_col));
        let footer = format!(
            " {key_bg}{key_fg} Enter {r} {muted}Open{r}  {key_bg}{key_fg} Space {r} {muted}Preview{r}  {key_bg}{key_fg} @ {r} {muted}Attach{r}  {key_bg}{key_fg} Esc {r} {muted}Close{r} ",
        );
        // Pad footer to fill width
        let footer_visible_len = 4 + 5 + 1 + 5 + 2 + 7 + 2 + 1 + 6 + 2 + 3 + 2 + 5; // approx visible chars
        let footer_pad = overlay_w.saturating_sub(footer_visible_len + 2);
        out.push_str(&format!(
            "{accent}\u{2502}{r}{footer}{}{accent}\u{2502}{r}",
            " ".repeat(footer_pad),
        ));
        row += 1;

        // ── Bottom border ────────────────────────────────────────────
        out.push_str(&goto(row, start_col));
        out.push_str(&format!(
            "{accent}\u{2570}{}{}\u{256F}{r}",
            hline(overlay_w.saturating_sub(2)),
            "",
        ));

        // ── Preview panel (right side, if enabled) ───────────────────
        if self.show_preview {
            // Extract preview data before calling render helper to avoid borrow conflict
            let preview_data: Option<Vec<String>> = self.preview_lines().map(|s| s.to_vec());
            let file_name = self
                .selected_entry()
                .map(|e| e.name.clone())
                .unwrap_or_default();
            let preview = render_preview_panel(
                preview_data.as_deref(),
                &file_name,
                start_row,
                start_col + tree_w,
                overlay_w.saturating_sub(tree_w),
                content_h + 3,
            );
            out.push_str(&preview);
        }

        out.push_str("\x1b[?25h"); // show cursor
        out.push_str("\x1b[u"); // restore cursor

        out
    }

}

/// Render the preview panel for the selected file (standalone to avoid borrow conflict).
fn render_preview_panel(
    lines: Option<&[String]>,
    file_name: &str,
    start_row: usize,
    start_col: usize,
    width: usize,
    height: usize,
) -> String {
    let r = "\x1b[0m";
    let dim = "\x1b[2m";
    let fg = "\x1b[38;2;192;202;245m";
    let muted = "\x1b[38;2;86;95;137m";
    let accent = "\x1b[38;2;122;162;247m";

    let goto = |row: usize, col: usize| -> String {
        format!("\x1b[{};{}H", row, col)
    };

    let mut out = String::new();
    let inner_w = width.saturating_sub(2);

    // Title row
    out.push_str(&goto(start_row + 1, start_col));
    let title: String = file_name.chars().take(inner_w.saturating_sub(2)).collect();
    let title_pad = inner_w.saturating_sub(title.chars().count() + 1);
    out.push_str(&format!(
        "{accent}\u{2502}{r} {muted}{dim}{title}{r}{}",
        " ".repeat(title_pad),
    ));

    // Content rows
    match lines {
        Some(lines) => {
            for (i, line) in lines.iter().take(height.saturating_sub(2)).enumerate() {
                out.push_str(&goto(start_row + 2 + i, start_col));
                let display: String = line.chars().take(inner_w.saturating_sub(1)).collect();
                let pad = inner_w.saturating_sub(display.chars().count() + 1);
                out.push_str(&format!(
                    "{accent}\u{2502}{r} {fg}{display}{r}{}",
                    " ".repeat(pad),
                ));
            }
            // Fill remaining rows
            let used = lines.len().min(height.saturating_sub(2));
            for i in used..height.saturating_sub(2) {
                out.push_str(&goto(start_row + 2 + i, start_col));
                out.push_str(&format!(
                    "{accent}\u{2502}{r}{}",
                    " ".repeat(inner_w),
                ));
            }
        }
        None => {
            let msg = "(no preview)";
            for i in 0..height.saturating_sub(2) {
                out.push_str(&goto(start_row + 2 + i, start_col));
                if i == height / 2 {
                    let pad = inner_w.saturating_sub(msg.len() + 1);
                    let left = pad / 2;
                    let right = pad.saturating_sub(left);
                    out.push_str(&format!(
                        "{accent}\u{2502}{r}{}{muted}{dim}{msg}{r}{}",
                        " ".repeat(left),
                        " ".repeat(right),
                    ));
                } else {
                    out.push_str(&format!(
                        "{accent}\u{2502}{r}{}",
                        " ".repeat(inner_w),
                    ));
                }
            }
        }
    }

    out
}

/// Format a file size in human-readable form.
fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create temp dir");
        let base = dir.path();

        // Create directory structure
        fs::create_dir_all(base.join("src/api")).unwrap();
        fs::create_dir_all(base.join("tests")).unwrap();

        // Create files
        fs::write(base.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        fs::write(base.join("README.md"), "# Test\n").unwrap();
        fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(base.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
        fs::write(base.join("src/api/mod.rs"), "pub mod handler;\n").unwrap();
        fs::write(base.join("tests/test_main.rs"), "#[test] fn it_works() {}\n").unwrap();

        dir
    }

    #[test]
    fn test_new_scans_root() {
        let dir = setup_test_dir();
        let tree = FileTree::new(dir.path().to_path_buf());
        assert!(!tree.entries.is_empty());
    }

    #[test]
    fn test_scan_directory_sorts_dirs_first() {
        let dir = setup_test_dir();
        let entries = FileTree::scan_directory(dir.path());

        // Find first file index and last dir index
        let last_dir_idx = entries.iter().rposition(|e| e.entry_type == EntryType::Directory);
        let first_file_idx = entries.iter().position(|e| e.entry_type == EntryType::File);

        if let (Some(ld), Some(ff)) = (last_dir_idx, first_file_idx) {
            assert!(ld < ff, "directories should come before files");
        }
    }

    #[test]
    fn test_toggle_expand() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        let initial_count = tree.entries.len();

        // Find the src directory
        let src_idx = tree
            .entries
            .iter()
            .position(|e| e.name == "src" && e.entry_type == EntryType::Directory);

        if let Some(idx) = src_idx {
            tree.selected_index = idx;
            tree.toggle_expand();
            // After expanding, should have more entries
            assert!(tree.entries.len() > initial_count);

            // Toggle again to collapse
            // Find src again (index may have shifted)
            let src_idx2 = tree
                .entries
                .iter()
                .position(|e| e.name == "src" && e.entry_type == EntryType::Directory)
                .unwrap();
            tree.selected_index = src_idx2;
            tree.toggle_expand();
            assert_eq!(tree.entries.len(), initial_count);
        }
    }

    #[test]
    fn test_move_up_down() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        assert_eq!(tree.selected_index, 0);

        tree.move_down();
        assert_eq!(tree.selected_index, 1);

        tree.move_down();
        assert_eq!(tree.selected_index, 2);

        tree.move_up();
        assert_eq!(tree.selected_index, 1);

        tree.move_up();
        assert_eq!(tree.selected_index, 0);

        // Should not go below 0
        tree.move_up();
        assert_eq!(tree.selected_index, 0);
    }

    #[test]
    fn test_move_down_clamps() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        let max = tree.entries.len().saturating_sub(1);
        for _ in 0..100 {
            tree.move_down();
        }
        assert_eq!(tree.selected_index, max);
    }

    #[test]
    fn test_filter() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        // Expand src/ and tests/ so inner files are visible
        for i in 0..tree.entries.len() {
            if tree.entries[i].entry_type == EntryType::Directory {
                tree.selected_index = i;
                tree.toggle_expand();
            }
        }
        let unfiltered_count = tree.entries.len();

        tree.apply_filter("main");
        // Should have fewer entries (only matching files + parent dirs)
        assert!(tree.entries.len() <= unfiltered_count);
        // Should contain main.rs (from expanded src/ directory)
        assert!(tree.entries.iter().any(|e| e.name.contains("main")));

        tree.filter_clear();
        assert_eq!(tree.entries.len(), unfiltered_count);
    }

    #[test]
    fn test_filter_insert_backspace() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        tree.filter_insert_char('m');
        assert_eq!(tree.filter, "m");

        tree.filter_insert_char('a');
        assert_eq!(tree.filter, "ma");

        tree.filter_backspace();
        assert_eq!(tree.filter, "m");

        tree.filter_backspace();
        assert_eq!(tree.filter, "");
    }

    #[test]
    fn test_selected_path() {
        let dir = setup_test_dir();
        let tree = FileTree::new(dir.path().to_path_buf());

        let path = tree.selected_path();
        assert!(path.is_some());
        assert!(path.unwrap().exists());
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1_048_576), "1.0 MB");
        assert_eq!(format_file_size(2_621_440), "2.5 MB");
    }

    #[test]
    fn test_render_produces_output() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());
        let output = tree.render(80, 24);
        assert!(!output.is_empty());
        assert!(output.contains("Files"));
        assert!(output.contains("items"));
    }

    #[test]
    fn test_entry_count() {
        let dir = setup_test_dir();
        let tree = FileTree::new(dir.path().to_path_buf());
        assert_eq!(tree.entry_count(), tree.entries.len());
        assert!(tree.entry_count() > 0);
    }

    #[test]
    fn test_preview_lines_for_file() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        // Find a file entry
        let file_idx = tree
            .entries
            .iter()
            .position(|e| e.entry_type == EntryType::File);

        if let Some(idx) = file_idx {
            tree.selected_index = idx;
            let lines = tree.preview_lines();
            assert!(lines.is_some());
            assert!(!lines.unwrap().is_empty());
        }
    }

    #[test]
    fn test_preview_lines_for_directory_is_none() {
        let dir = setup_test_dir();
        let mut tree = FileTree::new(dir.path().to_path_buf());

        let dir_idx = tree
            .entries
            .iter()
            .position(|e| e.entry_type == EntryType::Directory);

        if let Some(idx) = dir_idx {
            tree.selected_index = idx;
            let lines = tree.preview_lines();
            assert!(lines.is_none());
        }
    }

    #[test]
    fn test_empty_directory() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let tree = FileTree::new(dir.path().to_path_buf());
        assert!(tree.entries.is_empty());
        assert_eq!(tree.selected_index, 0);
    }

    #[test]
    fn test_render_empty_tree() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut tree = FileTree::new(dir.path().to_path_buf());
        let output = tree.render(80, 24);
        assert!(!output.is_empty());
        assert!(output.contains("0 items"));
    }
}
