//! Command palette — a searchable overlay of all available actions.
//!
//! Opened with `Ctrl+P`, the palette renders as a floating ANSI box with
//! fuzzy search filtering. Actions include slash commands, mode toggles,
//! navigation shortcuts, and session management.
//!
//! Uses simple substring/prefix matching for v1 (no external fuzzy crate needed).

/// Layout constants for the palette overlay.
const PALETTE_WIDTH: u16 = 60;
const PALETTE_MAX_ITEMS: usize = 10;
const PALETTE_TOP_OFFSET: u16 = 3;

/// Category for grouping palette actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    /// Slash commands: /help, /clear, /model, etc.
    Command,
    /// Block nav, scroll, jump.
    Navigation,
    /// Toggle input mode, plan mode.
    Mode,
    /// New session, export, history.
    Session,
    /// Run specific tools.
    Tool,
}

/// A single action in the command palette.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    /// Display name (used for matching).
    pub name: String,
    /// Short description shown to the right.
    pub description: String,
    /// Keyboard shortcut hint, if any.
    pub shortcut: Option<String>,
    /// Category for grouping.
    pub category: ActionCategory,
    /// The command string to execute (e.g. "/help", "toggle_mode").
    pub command: String,
}

/// State of the command palette overlay.
#[derive(Debug, Clone)]
pub struct PaletteState {
    /// Whether the palette is currently visible.
    pub open: bool,
    /// Current filter query.
    pub query: String,
    /// Filtered indices into the entries list, with match scores.
    pub filtered: Vec<(usize, u32)>,
    /// Currently highlighted index in the filtered list.
    pub selected: usize,
}

/// The command palette with all registered actions.
pub struct CommandPalette {
    /// All available actions.
    entries: Vec<PaletteEntry>,
    /// Current state.
    state: PaletteState,
}

impl CommandPalette {
    /// Create a new command palette with default actions.
    pub fn new() -> Self {
        let entries = default_entries();
        let filtered: Vec<(usize, u32)> = (0..entries.len()).map(|i| (i, 0)).collect();
        Self {
            entries,
            state: PaletteState {
                open: false,
                query: String::new(),
                filtered,
                selected: 0,
            },
        }
    }

    /// Open the palette.
    pub fn open(&mut self) {
        self.state.open = true;
        self.state.query.clear();
        self.state.selected = 0;
        self.refilter();
    }

    /// Close the palette.
    pub fn close(&mut self) {
        self.state.open = false;
        self.state.query.clear();
        self.state.selected = 0;
    }

    /// Toggle the palette open/closed.
    pub fn toggle(&mut self) {
        if self.state.open {
            self.close();
        } else {
            self.open();
        }
    }

    /// Whether the palette is open.
    pub fn is_open(&self) -> bool {
        self.state.open
    }

    /// Add a character to the filter query and re-filter.
    pub fn type_char(&mut self, c: char) {
        self.state.query.push(c);
        self.refilter();
        self.state.selected = 0;
    }

    /// Remove the last character from the filter query and re-filter.
    pub fn backspace(&mut self) {
        self.state.query.pop();
        self.refilter();
        self.state.selected = 0;
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if !self.state.filtered.is_empty() && self.state.selected > 0 {
            self.state.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        if !self.state.filtered.is_empty()
            && self.state.selected + 1 < self.state.filtered.len().min(PALETTE_MAX_ITEMS)
        {
            self.state.selected += 1;
        }
    }

    /// Get the command string of the currently selected entry.
    ///
    /// Returns `None` if no entry is selected or the palette is empty.
    pub fn selected_command(&self) -> Option<&str> {
        let (idx, _) = self.state.filtered.get(self.state.selected)?;
        self.entries.get(*idx).map(|e| e.command.as_str())
    }

    /// Get the currently selected entry.
    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        let (idx, _) = self.state.filtered.get(self.state.selected)?;
        self.entries.get(*idx)
    }

    /// Re-filter entries based on the current query.
    fn refilter(&mut self) {
        let query = self.state.query.to_ascii_lowercase();
        if query.is_empty() {
            self.state.filtered = (0..self.entries.len()).map(|i| (i, 0)).collect();
            return;
        }

        self.state.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let name_lower = entry.name.to_ascii_lowercase();
                let desc_lower = entry.description.to_ascii_lowercase();

                // Score: exact prefix > substring in name > substring in description
                if name_lower.starts_with(&query) {
                    Some((i, 100))
                } else if name_lower.contains(&query) {
                    Some((i, 50))
                } else if desc_lower.contains(&query) {
                    Some((i, 25))
                } else {
                    // Fuzzy: check if all query chars appear in order
                    let mut qi = query.chars().peekable();
                    for c in name_lower.chars() {
                        if qi.peek() == Some(&c) {
                            qi.next();
                        }
                    }
                    if qi.peek().is_none() {
                        Some((i, 10))
                    } else {
                        None
                    }
                }
            })
            .collect();

        // Sort by score descending
        self.state.filtered.sort_by(|a, b| b.1.cmp(&a.1));
    }

    /// Render the palette as ANSI escape sequences.
    ///
    /// Uses absolute cursor positioning to overlay the palette on the screen.
    pub fn render(&self, screen_width: u16) -> String {
        if !self.state.open {
            return String::new();
        }

        let width = (PALETTE_WIDTH as usize).min(screen_width.saturating_sub(4) as usize);
        let left = ((screen_width as usize).saturating_sub(width)) / 2;
        let top = PALETTE_TOP_OFFSET;

        let r = "\x1b[0m";
        let border = "\x1b[38;2;122;162;247m"; // ACCENT
        let fg = "\x1b[38;2;192;202;245m"; // FG
        let muted = "\x1b[38;2;86;95;137m"; // MUTED
        let sel_bg = "\x1b[48;2;40;44;66m"; // SELECTION
        let bold = "\x1b[1m";

        let mut out = String::with_capacity(4096);
        out.push_str("\x1b[s"); // save cursor
        out.push_str("\x1b[?25l"); // hide cursor

        let inner = width.saturating_sub(2);

        // Helper to draw a line at an absolute position
        let mut row = top;

        // Top border
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let title = " Command Palette ";
        let fill_len = inner.saturating_sub(title.len() + 1);
        let fill: String = std::iter::repeat('\u{2500}').take(fill_len).collect();
        out.push_str(&format!(
            "{border}\u{256D}\u{2500}{r}{border}{bold}{title}{r}{border}{fill}\u{256E}{r}",
        ));
        row += 1;

        // Search input row
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let query_display: String = self.state.query.chars().take(inner.saturating_sub(4)).collect();
        let query_pad = inner.saturating_sub(query_display.len() + 3);
        out.push_str(&format!(
            "{border}\u{2502}{r} {fg}{bold}>{r} {fg}{query_display}{}{r} {border}\u{2502}{r}",
            " ".repeat(query_pad),
        ));
        row += 1;

        // Separator
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let sep: String = std::iter::repeat('\u{2500}').take(inner).collect();
        out.push_str(&format!("{border}\u{251C}{sep}\u{2524}{r}"));
        row += 1;

        // Entries
        let visible_count = self.state.filtered.len().min(PALETTE_MAX_ITEMS);
        if visible_count == 0 {
            out.push_str(&format!("\x1b[{row};{}H", left + 1));
            let no_results = "No matching commands";
            let pad = inner.saturating_sub(no_results.len() + 2);
            out.push_str(&format!(
                "{border}\u{2502}{r} {muted}{no_results}{}{r} {border}\u{2502}{r}",
                " ".repeat(pad),
            ));
            row += 1;
        } else {
            for (vi, &(entry_idx, _score)) in
                self.state.filtered.iter().take(PALETTE_MAX_ITEMS).enumerate()
            {
                let entry = &self.entries[entry_idx];
                let is_selected = vi == self.state.selected;
                let bg = if is_selected { sel_bg } else { "" };
                let bg_end = if is_selected { r } else { "" };

                out.push_str(&format!("\x1b[{row};{}H", left + 1));

                // Name (left) + description (right)
                let shortcut_str = entry
                    .shortcut
                    .as_deref()
                    .map(|s| format!(" {muted}[{s}]{r}{bg}"))
                    .unwrap_or_default();

                let name_len = entry.name.len();
                let shortcut_visible_len = entry.shortcut.as_ref().map(|s| s.len() + 3).unwrap_or(0);
                let desc_available = inner.saturating_sub(name_len + shortcut_visible_len + 4);
                let desc: String = entry.description.chars().take(desc_available).collect();
                let pad = inner.saturating_sub(name_len + shortcut_visible_len + desc.len() + 3);

                let marker = if is_selected { "\u{25B8}" } else { " " };

                out.push_str(&format!(
                    "{border}\u{2502}{r}{bg}{marker}{fg}{bold}{}{r}{bg}{shortcut_str}{}{muted}{desc}{bg_end} {border}\u{2502}{r}",
                    entry.name,
                    " ".repeat(pad),
                ));
                row += 1;
            }
        }

        // Bottom border
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let bot: String = std::iter::repeat('\u{2500}').take(inner).collect();
        out.push_str(&format!("{border}\u{2570}{bot}\u{256F}{r}"));

        // Footer hint
        row += 1;
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        out.push_str(&format!(
            "{muted}  \u{2191}\u{2193} navigate  \u{23CE} select  Esc close{r}",
        ));

        out.push_str("\x1b[?25h"); // show cursor
        out.push_str("\x1b[u"); // restore cursor
        out
    }

    /// Get the entries list (for testing).
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }

    /// Get the current state (for testing).
    pub fn state(&self) -> &PaletteState {
        &self.state
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the default set of palette entries.
fn default_entries() -> Vec<PaletteEntry> {
    vec![
        PaletteEntry {
            name: "/help".to_string(),
            description: "Show available commands".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/help".to_string(),
        },
        PaletteEntry {
            name: "/clear".to_string(),
            description: "Clear conversation".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/clear".to_string(),
        },
        PaletteEntry {
            name: "/model".to_string(),
            description: "Switch LLM model".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/model".to_string(),
        },
        PaletteEntry {
            name: "/cost".to_string(),
            description: "Show token usage & cost".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/cost".to_string(),
        },
        PaletteEntry {
            name: "/undo".to_string(),
            description: "Undo last file change".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/undo".to_string(),
        },
        PaletteEntry {
            name: "/redo".to_string(),
            description: "Redo undone change".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/redo".to_string(),
        },
        PaletteEntry {
            name: "/plan".to_string(),
            description: "Toggle plan mode".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/plan".to_string(),
        },
        PaletteEntry {
            name: "/permissions".to_string(),
            description: "Show permission rules".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/permissions".to_string(),
        },
        PaletteEntry {
            name: "/memory".to_string(),
            description: "Show auto-memory".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/memory".to_string(),
        },
        PaletteEntry {
            name: "/compact".to_string(),
            description: "Summarize conversation".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/compact".to_string(),
        },
        PaletteEntry {
            name: "/diff".to_string(),
            description: "Show git diff".to_string(),
            shortcut: None,
            category: ActionCategory::Command,
            command: "/diff".to_string(),
        },
        PaletteEntry {
            name: "/export".to_string(),
            description: "Export session to file".to_string(),
            shortcut: None,
            category: ActionCategory::Session,
            command: "/export".to_string(),
        },
        PaletteEntry {
            name: "Toggle Input Mode".to_string(),
            description: "Switch Agent/Terminal".to_string(),
            shortcut: Some("Ctrl+T".to_string()),
            category: ActionCategory::Mode,
            command: "toggle_mode".to_string(),
        },
        PaletteEntry {
            name: "Fuzzy Finder".to_string(),
            description: "Search files, commands, history".to_string(),
            shortcut: Some("Ctrl+F".to_string()),
            category: ActionCategory::Tool,
            command: "fuzzy_finder".to_string(),
        },
        PaletteEntry {
            name: "Quick Fix".to_string(),
            description: "Fix last error".to_string(),
            shortcut: None,
            category: ActionCategory::Tool,
            command: "quick_fix".to_string(),
        },
        PaletteEntry {
            name: "Previous Block".to_string(),
            description: "Navigate to previous block".to_string(),
            shortcut: Some("Ctrl+Up".to_string()),
            category: ActionCategory::Navigation,
            command: "nav_prev".to_string(),
        },
        PaletteEntry {
            name: "Next Block".to_string(),
            description: "Navigate to next block".to_string(),
            shortcut: Some("Ctrl+Down".to_string()),
            category: ActionCategory::Navigation,
            command: "nav_next".to_string(),
        },
        PaletteEntry {
            name: "Fuzzy History".to_string(),
            description: "Search command history".to_string(),
            shortcut: Some("Ctrl+R".to_string()),
            category: ActionCategory::Navigation,
            command: "history_search".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Creation ─────────────────────────────────────────────────────

    #[test]
    fn new_palette_has_entries() {
        let palette = CommandPalette::new();
        assert!(!palette.entries.is_empty());
        assert!(palette.entries.len() >= 15);
    }

    #[test]
    fn new_palette_starts_closed() {
        let palette = CommandPalette::new();
        assert!(!palette.is_open());
    }

    // ── Open / Close / Toggle ────────────────────────────────────────

    #[test]
    fn open_and_close() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(palette.is_open());
        palette.close();
        assert!(!palette.is_open());
    }

    #[test]
    fn toggle() {
        let mut palette = CommandPalette::new();
        palette.toggle();
        assert!(palette.is_open());
        palette.toggle();
        assert!(!palette.is_open());
    }

    // ── Filtering ────────────────────────────────────────────────────

    #[test]
    fn empty_query_shows_all() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.state.filtered.len(), palette.entries.len());
    }

    #[test]
    fn filter_by_prefix() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.type_char('/');
        palette.type_char('h');
        palette.type_char('e');
        // Should match "/help" at minimum
        assert!(palette
            .state
            .filtered
            .iter()
            .any(|&(idx, _)| palette.entries[idx].name == "/help"));
    }

    #[test]
    fn filter_by_description() {
        let mut palette = CommandPalette::new();
        palette.open();
        for c in "token".chars() {
            palette.type_char(c);
        }
        // "/cost" has description "Show token usage & cost"
        assert!(palette
            .state
            .filtered
            .iter()
            .any(|&(idx, _)| palette.entries[idx].name == "/cost"));
    }

    #[test]
    fn filter_no_match() {
        let mut palette = CommandPalette::new();
        palette.open();
        for c in "zzzzz".chars() {
            palette.type_char(c);
        }
        assert!(palette.state.filtered.is_empty());
    }

    #[test]
    fn backspace_widens_filter() {
        let mut palette = CommandPalette::new();
        palette.open();
        for c in "zzzzz".chars() {
            palette.type_char(c);
        }
        assert!(palette.state.filtered.is_empty());
        // Backspace all
        for _ in 0..5 {
            palette.backspace();
        }
        assert_eq!(palette.state.filtered.len(), palette.entries.len());
    }

    // ── Selection navigation ─────────────────────────────────────────

    #[test]
    fn select_down_wraps_at_limit() {
        let mut palette = CommandPalette::new();
        palette.open();
        let max = palette.state.filtered.len().min(PALETTE_MAX_ITEMS);
        for _ in 0..max + 5 {
            palette.select_next();
        }
        assert!(palette.state.selected < max);
    }

    #[test]
    fn select_up_stops_at_zero() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.select_prev();
        assert_eq!(palette.state.selected, 0);
    }

    #[test]
    fn select_down_then_up() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.select_next();
        palette.select_next();
        assert_eq!(palette.state.selected, 2);
        palette.select_prev();
        assert_eq!(palette.state.selected, 1);
    }

    // ── Selected command ─────────────────────────────────────────────

    #[test]
    fn selected_command_default() {
        let mut palette = CommandPalette::new();
        palette.open();
        let cmd = palette.selected_command();
        assert!(cmd.is_some());
        // First entry should be /help
        assert_eq!(cmd.unwrap(), "/help");
    }

    #[test]
    fn selected_command_after_nav() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.select_next();
        let cmd = palette.selected_command();
        assert!(cmd.is_some());
        assert_eq!(cmd.unwrap(), "/clear");
    }

    #[test]
    fn selected_command_empty_filter() {
        let mut palette = CommandPalette::new();
        palette.open();
        for c in "zzzzz".chars() {
            palette.type_char(c);
        }
        assert_eq!(palette.selected_command(), None);
    }

    // ── Render ───────────────────────────────────────────────────────

    #[test]
    fn render_when_closed_is_empty() {
        let palette = CommandPalette::new();
        assert!(palette.render(80).is_empty());
    }

    #[test]
    fn render_when_open_has_content() {
        let mut palette = CommandPalette::new();
        palette.open();
        let rendered = palette.render(80);
        assert!(!rendered.is_empty());
        assert!(rendered.contains("Command Palette"));
    }

    #[test]
    fn render_includes_entries() {
        let mut palette = CommandPalette::new();
        palette.open();
        let rendered = palette.render(80);
        assert!(rendered.contains("/help"));
    }

    #[test]
    fn render_narrow_screen() {
        let mut palette = CommandPalette::new();
        palette.open();
        // Should not panic even on very narrow screen
        let rendered = palette.render(30);
        assert!(!rendered.is_empty());
    }

    // ── Fuzzy matching ───────────────────────────────────────────────

    #[test]
    fn fuzzy_match_subsequence() {
        let mut palette = CommandPalette::new();
        palette.open();
        // "hep" should fuzzy-match "/help" (h-e-p chars in order: /h-e-l-p)
        for c in "hep".chars() {
            palette.type_char(c);
        }
        assert!(palette
            .state
            .filtered
            .iter()
            .any(|&(idx, _)| palette.entries[idx].name == "/help"));
    }

    // ── Default trait ────────────────────────────────────────────────

    #[test]
    fn default_palette() {
        let palette = CommandPalette::default();
        assert!(!palette.entries.is_empty());
    }

    // ── Entry categories ─────────────────────────────────────────────

    #[test]
    fn entries_have_categories() {
        let palette = CommandPalette::new();
        assert!(palette.entries.iter().any(|e| e.category == ActionCategory::Command));
        assert!(palette.entries.iter().any(|e| e.category == ActionCategory::Mode));
        assert!(palette.entries.iter().any(|e| e.category == ActionCategory::Navigation));
        assert!(palette.entries.iter().any(|e| e.category == ActionCategory::Tool));
    }

    // ── Open resets state ────────────────────────────────────────────

    #[test]
    fn open_resets_query_and_selection() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.type_char('h');
        palette.select_next();
        palette.close();
        palette.open();
        assert!(palette.state.query.is_empty());
        assert_eq!(palette.state.selected, 0);
        assert_eq!(palette.state.filtered.len(), palette.entries.len());
    }
}
