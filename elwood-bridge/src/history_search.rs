//! Interactive fuzzy history search (Ctrl+R).
//!
//! Provides an overlay search interface for command history with
//! substring/prefix matching, frecency scoring, and ANSI rendering.

use crate::runtime::InputMode;

/// A single history record with metadata.
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    /// The command or message text.
    pub text: String,
    /// Unix epoch timestamp (seconds) when entered.
    pub timestamp: u64,
    /// Which mode it was entered in.
    pub mode: InputMode,
    /// Working directory at time of entry.
    pub directory: Option<String>,
    /// Number of times this exact text has been used.
    pub use_count: u32,
}

/// A search result with scoring metadata.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Index into the history entries.
    pub index: usize,
    /// Combined score (match quality + frecency).
    pub score: f64,
    /// The matched text (for display).
    pub text: String,
}

/// Layout constants for the history search overlay.
const SEARCH_WIDTH: u16 = 60;
const SEARCH_MAX_RESULTS: usize = 10;
const SEARCH_TOP_OFFSET: u16 = 3;

/// State of the history search overlay.
#[derive(Debug, Clone)]
pub struct HistorySearchState {
    /// Whether the search overlay is open.
    pub open: bool,
    /// Current search query.
    pub query: String,
    /// Filtered results.
    pub results: Vec<SearchResult>,
    /// Currently highlighted index in results.
    pub selected: usize,
}

/// Interactive history search with overlay rendering.
pub struct HistorySearch {
    /// All history entries.
    entries: Vec<HistoryRecord>,
    /// Current state.
    state: HistorySearchState,
}

impl HistorySearch {
    /// Create a new empty history search.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            state: HistorySearchState {
                open: false,
                query: String::new(),
                results: Vec::new(),
                selected: 0,
            },
        }
    }

    /// Create a history search with pre-loaded entries.
    pub fn with_entries(entries: Vec<HistoryRecord>) -> Self {
        Self {
            entries,
            state: HistorySearchState {
                open: false,
                query: String::new(),
                results: Vec::new(),
                selected: 0,
            },
        }
    }

    /// Add a new entry to history.
    pub fn add_entry(&mut self, record: HistoryRecord) {
        // Deduplicate: update use_count and timestamp if text matches
        if let Some(existing) = self.entries.iter_mut().find(|e| e.text == record.text) {
            existing.use_count += 1;
            existing.timestamp = record.timestamp;
            return;
        }
        self.entries.push(record);

        // Cap at 50,000 entries
        if self.entries.len() > 50_000 {
            self.entries.remove(0);
        }
    }

    /// Open the search overlay.
    pub fn open(&mut self) {
        self.state.open = true;
        self.state.query.clear();
        self.state.selected = 0;
        self.search();
    }

    /// Close the search overlay.
    pub fn close(&mut self) {
        self.state.open = false;
        self.state.query.clear();
        self.state.results.clear();
        self.state.selected = 0;
    }

    /// Whether the overlay is open.
    pub fn is_open(&self) -> bool {
        self.state.open
    }

    /// Add a character to the search query and re-search.
    pub fn type_char(&mut self, c: char) {
        self.state.query.push(c);
        self.state.selected = 0;
        self.search();
    }

    /// Remove the last character from the query and re-search.
    pub fn backspace(&mut self) {
        self.state.query.pop();
        self.state.selected = 0;
        self.search();
    }

    /// Move selection up.
    pub fn select_prev(&mut self) {
        if self.state.selected > 0 {
            self.state.selected -= 1;
        }
    }

    /// Move selection down.
    pub fn select_next(&mut self) {
        let max = self.state.results.len().min(SEARCH_MAX_RESULTS);
        if max > 0 && self.state.selected + 1 < max {
            self.state.selected += 1;
        }
    }

    /// Get the text of the currently selected result.
    pub fn selected_text(&self) -> Option<&str> {
        self.state
            .results
            .get(self.state.selected)
            .map(|r| r.text.as_str())
    }

    /// Run the search against all history entries.
    fn search(&mut self) {
        let query = self.state.query.to_ascii_lowercase();

        if query.is_empty() {
            // Show most recent entries
            let mut results: Vec<SearchResult> = self
                .entries
                .iter()
                .enumerate()
                .rev() // Most recent first
                .take(SEARCH_MAX_RESULTS)
                .map(|(i, entry)| SearchResult {
                    index: i,
                    score: entry.timestamp as f64,
                    text: entry.text.clone(),
                })
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            self.state.results = results;
            return;
        }

        let mut results: Vec<SearchResult> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let entry_lower = entry.text.to_ascii_lowercase();

                // Score: prefix match > substring > fuzzy subsequence
                let match_score = if entry_lower.starts_with(&query) {
                    100.0
                } else if entry_lower.contains(&query) {
                    50.0
                } else if fuzzy_match(&query, &entry_lower) {
                    10.0
                } else {
                    return None;
                };

                // Frecency boost
                let recency = recency_score(entry.timestamp);
                let frequency = (entry.use_count as f64).ln_1p();
                let score = match_score + recency * 10.0 + frequency * 5.0;

                Some(SearchResult {
                    index: i,
                    score,
                    text: entry.text.clone(),
                })
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(SEARCH_MAX_RESULTS);
        self.state.results = results;
    }

    /// Render the history search overlay as ANSI escape sequences.
    pub fn render(&self, screen_width: u16) -> String {
        if !self.state.open {
            return String::new();
        }

        let width = (SEARCH_WIDTH as usize).min(screen_width.saturating_sub(4) as usize);
        let left = ((screen_width as usize).saturating_sub(width)) / 2;
        let top = SEARCH_TOP_OFFSET;

        let r = "\x1b[0m";
        let border = "\x1b[38;2;122;162;247m"; // ACCENT
        let fg = "\x1b[38;2;192;202;245m"; // FG
        let muted = "\x1b[38;2;86;95;137m"; // MUTED
        let sel_bg = "\x1b[48;2;40;44;66m"; // SELECTION
        let bold = "\x1b[1m";
        let accent = "\x1b[38;2;122;162;247m";

        let mut out = String::with_capacity(4096);
        out.push_str("\x1b[s"); // save cursor
        out.push_str("\x1b[?25l"); // hide cursor

        let inner = width.saturating_sub(2);
        let mut row = top;

        // Top border with title
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let title = " History Search (Ctrl+R) ";
        let fill_len = inner.saturating_sub(title.len() + 1);
        let fill: String = std::iter::repeat('\u{2500}').take(fill_len).collect();
        out.push_str(&format!(
            "{border}\u{256D}\u{2500}{r}{border}{bold}{title}{r}{border}{fill}\u{256E}{r}",
        ));
        row += 1;

        // Search input
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let query_display: String = self.state.query.chars().take(inner.saturating_sub(4)).collect();
        let query_pad = inner.saturating_sub(query_display.len() + 3);
        out.push_str(&format!(
            "{border}\u{2502}{r} {accent}{bold}>{r} {fg}{query_display}{}{r} {border}\u{2502}{r}",
            " ".repeat(query_pad),
        ));
        row += 1;

        // Separator
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let sep: String = std::iter::repeat('\u{2500}').take(inner).collect();
        out.push_str(&format!("{border}\u{251C}{sep}\u{2524}{r}"));
        row += 1;

        // Results
        let visible_count = self.state.results.len().min(SEARCH_MAX_RESULTS);
        if visible_count == 0 {
            out.push_str(&format!("\x1b[{row};{}H", left + 1));
            let msg = if self.state.query.is_empty() {
                "No history"
            } else {
                "No matches"
            };
            let pad = inner.saturating_sub(msg.len() + 2);
            out.push_str(&format!(
                "{border}\u{2502}{r} {muted}{msg}{}{r} {border}\u{2502}{r}",
                " ".repeat(pad),
            ));
            row += 1;
        } else {
            for (vi, result) in self.state.results.iter().take(SEARCH_MAX_RESULTS).enumerate() {
                let is_selected = vi == self.state.selected;
                let bg = if is_selected { sel_bg } else { "" };
                let bg_end = if is_selected { r } else { "" };

                out.push_str(&format!("\x1b[{row};{}H", left + 1));

                let marker = if is_selected { "\u{25B8}" } else { " " };

                // Truncate text to fit
                let max_text = inner.saturating_sub(4);
                let display: String = result.text.chars().take(max_text).collect();
                let pad = inner.saturating_sub(display.len() + 2);

                out.push_str(&format!(
                    "{border}\u{2502}{r}{bg}{marker}{fg}{display}{}{bg_end} {border}\u{2502}{r}",
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
            "{muted}  \u{2191}\u{2193} navigate  \u{23CE} insert  Esc cancel{r}",
        ));

        out.push_str("\x1b[?25h"); // show cursor
        out.push_str("\x1b[u"); // restore cursor
        out
    }

    /// Get the entries list (for testing).
    pub fn entries(&self) -> &[HistoryRecord] {
        &self.entries
    }

    /// Get the current state (for testing).
    pub fn state(&self) -> &HistorySearchState {
        &self.state
    }
}

impl Default for HistorySearch {
    fn default() -> Self {
        Self::new()
    }
}

/// Simple fuzzy subsequence match: all chars of needle appear in order in haystack.
fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let mut needle_chars = needle.chars().peekable();
    for c in haystack.chars() {
        if needle_chars.peek() == Some(&c) {
            needle_chars.next();
        }
    }
    needle_chars.peek().is_none()
}

/// Score based on how recent a timestamp is (higher = more recent).
fn recency_score(timestamp: u64) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age_hours = (now.saturating_sub(timestamp)) as f64 / 3600.0;
    if age_hours < 1.0 {
        8.0
    } else if age_hours < 24.0 {
        4.0
    } else if age_hours < 168.0 {
        2.0
    } else {
        1.0
    }
}

/// Format a relative timestamp for display (e.g. "2m ago", "1h ago").
pub fn format_relative_time(timestamp: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age_secs = now.saturating_sub(timestamp);

    if age_secs < 60 {
        format!("{}s ago", age_secs)
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{}h ago", age_secs / 3600)
    } else {
        format!("{}d ago", age_secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<HistoryRecord> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        vec![
            HistoryRecord {
                text: "cargo test --workspace".to_string(),
                timestamp: now,
                mode: InputMode::Terminal,
                directory: Some("/home/user/project".to_string()),
                use_count: 5,
            },
            HistoryRecord {
                text: "cargo build --release".to_string(),
                timestamp: now - 3600,
                mode: InputMode::Terminal,
                directory: Some("/home/user/project".to_string()),
                use_count: 3,
            },
            HistoryRecord {
                text: "git push origin main".to_string(),
                timestamp: now - 7200,
                mode: InputMode::Terminal,
                directory: None,
                use_count: 10,
            },
            HistoryRecord {
                text: "explain the error in main.rs".to_string(),
                timestamp: now - 1800,
                mode: InputMode::Agent,
                directory: None,
                use_count: 1,
            },
            HistoryRecord {
                text: "ls -la src/".to_string(),
                timestamp: now - 10800,
                mode: InputMode::Terminal,
                directory: None,
                use_count: 2,
            },
        ]
    }

    // ── Creation ─────────────────────────────────────────────────────

    #[test]
    fn new_search_starts_closed() {
        let search = HistorySearch::new();
        assert!(!search.is_open());
    }

    #[test]
    fn with_entries_has_entries() {
        let search = HistorySearch::with_entries(sample_entries());
        assert_eq!(search.entries().len(), 5);
    }

    // ── Open / Close ─────────────────────────────────────────────────

    #[test]
    fn open_and_close() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        assert!(search.is_open());
        assert!(!search.state().results.is_empty());
        search.close();
        assert!(!search.is_open());
        assert!(search.state().results.is_empty());
    }

    // ── Search ───────────────────────────────────────────────────────

    #[test]
    fn search_prefix_match() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        for c in "cargo".chars() {
            search.type_char(c);
        }
        // Should find "cargo test" and "cargo build"
        assert!(search.state().results.len() >= 2);
        assert!(search
            .state()
            .results
            .iter()
            .any(|r| r.text.starts_with("cargo")));
    }

    #[test]
    fn search_substring_match() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        for c in "release".chars() {
            search.type_char(c);
        }
        assert!(search
            .state()
            .results
            .iter()
            .any(|r| r.text.contains("release")));
    }

    #[test]
    fn search_no_match() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        for c in "zzzzz".chars() {
            search.type_char(c);
        }
        assert!(search.state().results.is_empty());
    }

    #[test]
    fn search_case_insensitive() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        for c in "CARGO".chars() {
            search.type_char(c);
        }
        assert!(!search.state().results.is_empty());
    }

    #[test]
    fn empty_query_shows_recent() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        // Empty query should show entries sorted by recency
        assert!(!search.state().results.is_empty());
        assert!(search.state().results.len() <= SEARCH_MAX_RESULTS);
    }

    // ── Selection ────────────────────────────────────────────────────

    #[test]
    fn select_navigation() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        assert_eq!(search.state().selected, 0);

        search.select_next();
        assert_eq!(search.state().selected, 1);

        search.select_prev();
        assert_eq!(search.state().selected, 0);
    }

    #[test]
    fn select_up_at_zero() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        search.select_prev();
        assert_eq!(search.state().selected, 0);
    }

    #[test]
    fn selected_text() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        let text = search.selected_text();
        assert!(text.is_some());
    }

    #[test]
    fn selected_text_empty_history() {
        let mut search = HistorySearch::new();
        search.open();
        assert_eq!(search.selected_text(), None);
    }

    // ── Add entry ────────────────────────────────────────────────────

    #[test]
    fn add_entry_new() {
        let mut search = HistorySearch::new();
        search.add_entry(HistoryRecord {
            text: "new command".to_string(),
            timestamp: 1000,
            mode: InputMode::Terminal,
            directory: None,
            use_count: 1,
        });
        assert_eq!(search.entries().len(), 1);
    }

    #[test]
    fn add_entry_deduplicates() {
        let mut search = HistorySearch::new();
        search.add_entry(HistoryRecord {
            text: "duplicate".to_string(),
            timestamp: 1000,
            mode: InputMode::Terminal,
            directory: None,
            use_count: 1,
        });
        search.add_entry(HistoryRecord {
            text: "duplicate".to_string(),
            timestamp: 2000,
            mode: InputMode::Terminal,
            directory: None,
            use_count: 1,
        });
        assert_eq!(search.entries().len(), 1);
        assert_eq!(search.entries()[0].use_count, 2);
        assert_eq!(search.entries()[0].timestamp, 2000);
    }

    // ── Backspace ────────────────────────────────────────────────────

    #[test]
    fn backspace_widens_search() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        for c in "zzzzz".chars() {
            search.type_char(c);
        }
        assert!(search.state().results.is_empty());

        for _ in 0..5 {
            search.backspace();
        }
        assert!(!search.state().results.is_empty());
    }

    // ── Render ───────────────────────────────────────────────────────

    #[test]
    fn render_when_closed() {
        let search = HistorySearch::new();
        assert!(search.render(80).is_empty());
    }

    #[test]
    fn render_when_open() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        let rendered = search.render(80);
        assert!(!rendered.is_empty());
        assert!(rendered.contains("History Search"));
    }

    #[test]
    fn render_narrow_screen() {
        let mut search = HistorySearch::with_entries(sample_entries());
        search.open();
        let rendered = search.render(30);
        assert!(!rendered.is_empty());
    }

    // ── Fuzzy match ──────────────────────────────────────────────────

    #[test]
    fn fuzzy_match_basic() {
        assert!(fuzzy_match("ct", "cargo test"));
        assert!(fuzzy_match("gpm", "git push origin main"));
        assert!(!fuzzy_match("zz", "cargo test"));
    }

    #[test]
    fn fuzzy_match_empty_needle() {
        assert!(fuzzy_match("", "anything"));
    }

    #[test]
    fn fuzzy_match_same_string() {
        assert!(fuzzy_match("hello", "hello"));
    }

    // ── Format relative time ─────────────────────────────────────────

    #[test]
    fn format_relative_time_recent() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_relative_time(now - 30);
        assert!(result.contains("s ago"));
    }

    #[test]
    fn format_relative_time_minutes() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_relative_time(now - 300);
        assert!(result.contains("m ago"));
    }

    #[test]
    fn format_relative_time_hours() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_relative_time(now - 7200);
        assert!(result.contains("h ago"));
    }

    #[test]
    fn format_relative_time_days() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = format_relative_time(now - 172800);
        assert!(result.contains("d ago"));
    }

    // ── Default trait ────────────────────────────────────────────────

    #[test]
    fn default_search() {
        let search = HistorySearch::default();
        assert!(!search.is_open());
        assert!(search.entries().is_empty());
    }
}
