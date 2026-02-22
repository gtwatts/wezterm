//! fzf-style fuzzy finder for files, commands, history, symbols, and bookmarks.
//!
//! Triggered by Ctrl+F. Provides a unified search overlay with match highlighting
//! and configurable source providers.

use std::path::PathBuf;

// ── Fuzzy match result ─────────────────────────────────────────────────────

/// Result of fuzzy-matching a pattern against a candidate string.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Overall quality score (higher is better).
    pub score: i32,
    /// Byte positions in the candidate that matched the pattern characters.
    pub matched_positions: Vec<usize>,
}

// ── Fuzzy scoring algorithm ────────────────────────────────────────────────

/// Bonus points for consecutive matching characters.
const BONUS_CONSECUTIVE: i32 = 3;
/// Bonus for matching at a word boundary (after `_`, `-`, `/`, `.`, or start).
const BONUS_WORD_BOUNDARY: i32 = 5;
/// Bonus for matching at a camelCase transition (lowercase -> uppercase).
const BONUS_CAMEL_CASE: i32 = 4;
/// Bonus for matching the very first character of the candidate.
const BONUS_EXACT_PREFIX: i32 = 10;
/// Bonus when the case of query and candidate characters match exactly.
const BONUS_CASE_MATCH: i32 = 1;
/// Penalty per gap character between consecutive matches.
const PENALTY_GAP: i32 = -1;
/// Base score for each matched character.
const SCORE_MATCH: i32 = 4;

/// Score a `pattern` against a `candidate` using Smith-Waterman-inspired fuzzy matching.
///
/// Returns `None` if not all pattern characters can be found in order.
/// Matching is case-insensitive, with a small bonus for exact case matches.
///
/// # Examples
///
/// ```
/// use elwood_bridge::fuzzy_finder::score;
///
/// let m = score("fb", "FooBar.rs").unwrap();
/// assert!(m.score > 0);
/// assert_eq!(m.matched_positions, vec![0, 3]);
/// ```
pub fn score(pattern: &str, candidate: &str) -> Option<FuzzyMatch> {
    if pattern.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            matched_positions: Vec::new(),
        });
    }

    let pattern_lower: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let pattern_orig: Vec<char> = pattern.chars().collect();
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let candidate_lower: Vec<char> = candidate
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .collect();

    // Quick check: can all pattern chars be found in order?
    {
        let mut pi = 0;
        for &cc in &candidate_lower {
            if pi < pattern_lower.len() && cc == pattern_lower[pi] {
                pi += 1;
            }
        }
        if pi < pattern_lower.len() {
            return None;
        }
    }

    // Greedy forward match collecting positions
    let mut positions = Vec::with_capacity(pattern_lower.len());
    let mut pi = 0;
    for (ci, &cc) in candidate_lower.iter().enumerate() {
        if pi < pattern_lower.len() && cc == pattern_lower[pi] {
            positions.push(ci);
            pi += 1;
        }
    }

    // Calculate score
    let mut total_score: i32 = 0;
    let mut prev_pos: Option<usize> = None;

    for (qi, &pos) in positions.iter().enumerate() {
        total_score += SCORE_MATCH;

        // Exact case match bonus
        if candidate_chars[pos] == pattern_orig[qi] {
            total_score += BONUS_CASE_MATCH;
        }

        // Prefix bonus
        if pos == 0 {
            total_score += BONUS_EXACT_PREFIX;
        }

        // Word boundary bonus
        if pos > 0 {
            let prev_char = candidate_chars[pos - 1];
            if prev_char == '_' || prev_char == '-' || prev_char == '/' || prev_char == '.' || prev_char == ' ' {
                total_score += BONUS_WORD_BOUNDARY;
            }
            // camelCase transition: lowercase followed by uppercase
            if prev_char.is_ascii_lowercase() && candidate_chars[pos].is_ascii_uppercase() {
                total_score += BONUS_CAMEL_CASE;
            }
        }

        // Consecutive / gap scoring
        if let Some(pp) = prev_pos {
            let gap = pos - pp - 1;
            if gap == 0 {
                total_score += BONUS_CONSECUTIVE;
            } else {
                total_score += PENALTY_GAP * gap as i32;
            }
        }

        prev_pos = Some(pos);
    }

    Some(FuzzyMatch {
        score: total_score,
        matched_positions: positions,
    })
}

// ── Source trait and items ──────────────────────────────────────────────────

/// Action to perform when a fuzzy-finder item is selected.
#[derive(Debug, Clone)]
pub enum FuzzyAction {
    /// Insert text into the input editor.
    InsertText(String),
    /// Open a file (send @file reference to agent).
    OpenFile(PathBuf),
    /// Attach a file as @-context.
    AttachContext(String),
    /// Scroll the chat to a specific block index.
    ScrollToBlock(usize),
    /// Execute a slash command.
    ExecuteCommand(String),
}

/// A single item that can appear in the fuzzy finder results.
#[derive(Debug, Clone)]
pub struct FuzzyItem {
    /// Primary display text (used for matching).
    pub text: String,
    /// Optional subtitle / detail text.
    pub detail: Option<String>,
    /// Name of the source that provided this item.
    pub source_name: String,
    /// Action to perform when selected.
    pub action: FuzzyAction,
}

/// Trait for providing items to the fuzzy finder.
pub trait FuzzySource: Send {
    /// Human-readable name (e.g. "Files", "Commands", "History").
    fn name(&self) -> &str;
    /// Return all items from this source.
    fn items(&self) -> Vec<FuzzyItem>;
}

// ── Built-in sources ───────────────────────────────────────────────────────

/// Source that walks the project directory for files (.gitignore-aware).
pub struct FileSource {
    #[allow(dead_code)]
    root: PathBuf,
    cached: Vec<FuzzyItem>,
}

impl FileSource {
    /// Create a new file source rooted at the given directory.
    ///
    /// Walks the directory tree immediately to cache the file list.
    pub fn new(root: PathBuf) -> Self {
        let mut items = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(true) // respect .gitignore + hidden
            .max_depth(Some(12))
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            let display = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            items.push(FuzzyItem {
                text: display.clone(),
                detail: None,
                source_name: "Files".to_string(),
                action: FuzzyAction::OpenFile(path.to_path_buf()),
            });
            // Cap at 10,000 files to stay responsive
            if items.len() >= 10_000 {
                break;
            }
        }

        Self {
            root,
            cached: items,
        }
    }
}

impl FuzzySource for FileSource {
    fn name(&self) -> &str {
        "Files"
    }

    fn items(&self) -> Vec<FuzzyItem> {
        self.cached.clone()
    }
}

/// Source for slash commands.
pub struct SlashCommandSource {
    items: Vec<FuzzyItem>,
}

impl SlashCommandSource {
    /// Create a new slash command source from the registered commands.
    pub fn new() -> Self {
        let commands = crate::commands::get_commands();
        let items = commands
            .iter()
            .map(|cmd| FuzzyItem {
                text: format!("/{}", cmd.name),
                detail: Some(cmd.description.to_string()),
                source_name: "Commands".to_string(),
                action: FuzzyAction::ExecuteCommand(format!("/{}", cmd.name)),
            })
            .collect();
        Self { items }
    }
}

impl Default for SlashCommandSource {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzySource for SlashCommandSource {
    fn name(&self) -> &str {
        "Commands"
    }

    fn items(&self) -> Vec<FuzzyItem> {
        self.items.clone()
    }
}

/// Source backed by history records.
pub struct HistorySource {
    items: Vec<FuzzyItem>,
}

impl HistorySource {
    /// Create a history source from a list of text entries (most recent first).
    pub fn from_texts(texts: Vec<String>) -> Self {
        let items = texts
            .into_iter()
            .map(|text| FuzzyItem {
                text: text.clone(),
                detail: None,
                source_name: "History".to_string(),
                action: FuzzyAction::InsertText(text),
            })
            .collect();
        Self { items }
    }
}

impl FuzzySource for HistorySource {
    fn name(&self) -> &str {
        "History"
    }

    fn items(&self) -> Vec<FuzzyItem> {
        self.items.clone()
    }
}

/// Placeholder source for symbols (integration with `SemanticBridge` later).
pub struct SymbolSource;

impl FuzzySource for SymbolSource {
    fn name(&self) -> &str {
        "Symbols"
    }

    fn items(&self) -> Vec<FuzzyItem> {
        Vec::new()
    }
}

// ── FuzzyFinder overlay ────────────────────────────────────────────────────

/// Maximum number of results to display.
const MAX_RESULTS: usize = 15;
/// Layout constants.
const FINDER_TOP_OFFSET: u16 = 2;

/// Source filter tab names, in display order.
const SOURCE_TABS: &[&str] = &["All", "Files", "Commands", "History"];

/// The fuzzy finder overlay state.
pub struct FuzzyFinder {
    /// Current search query.
    query: String,
    /// Scored and sorted results.
    results: Vec<(FuzzyItem, FuzzyMatch)>,
    /// Currently highlighted result index.
    selected_index: usize,
    /// Registered sources (kept for potential re-scan).
    #[allow(dead_code)]
    sources: Vec<Box<dyn FuzzySource>>,
    /// All items collected from sources (cached on creation).
    all_items: Vec<FuzzyItem>,
    /// Active source filter (`None` = show all).
    active_filter: Option<String>,
    /// Index into SOURCE_TABS for the active tab.
    active_tab: usize,
}

impl FuzzyFinder {
    /// Create a new fuzzy finder with the given sources.
    pub fn new(sources: Vec<Box<dyn FuzzySource>>) -> Self {
        let all_items: Vec<FuzzyItem> = sources.iter().flat_map(|s| s.items()).collect();
        // Start with all items shown (empty query), sorted by source order
        let results: Vec<(FuzzyItem, FuzzyMatch)> = all_items
            .iter()
            .take(MAX_RESULTS)
            .map(|item| {
                (
                    item.clone(),
                    FuzzyMatch {
                        score: 0,
                        matched_positions: Vec::new(),
                    },
                )
            })
            .collect();

        Self {
            query: String::new(),
            results,
            selected_index: 0,
            sources,
            all_items,
            active_filter: None,
            active_tab: 0,
        }
    }

    /// Update the search query and re-score all items.
    pub fn update_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.rescore();
        self.selected_index = 0;
    }

    /// Type a character into the query.
    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.rescore();
        self.selected_index = 0;
    }

    /// Delete the last character from the query.
    pub fn backspace(&mut self) {
        self.query.pop();
        self.rescore();
        self.selected_index = 0;
    }

    /// Move selection to the next item.
    pub fn select_next(&mut self) {
        let max = self.results.len().min(MAX_RESULTS);
        if max > 0 && self.selected_index + 1 < max {
            self.selected_index += 1;
        }
    }

    /// Move selection to the previous item.
    pub fn select_prev(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Cycle to the next source tab.
    pub fn cycle_tab(&mut self) {
        self.active_tab = (self.active_tab + 1) % SOURCE_TABS.len();
        self.active_filter = if self.active_tab == 0 {
            None
        } else {
            Some(SOURCE_TABS[self.active_tab].to_string())
        };
        self.rescore();
        self.selected_index = 0;
    }

    /// Get the action for the currently selected item.
    pub fn selected_action(&self) -> Option<&FuzzyAction> {
        self.results
            .get(self.selected_index)
            .map(|(item, _)| &item.action)
    }

    /// Get the currently selected item.
    pub fn selected_item(&self) -> Option<&FuzzyItem> {
        self.results.get(self.selected_index).map(|(item, _)| item)
    }

    /// Current query text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Current results (for testing).
    pub fn results(&self) -> &[(FuzzyItem, FuzzyMatch)] {
        &self.results
    }

    /// Current selected index (for testing).
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Re-score and sort all items against the current query and filter.
    fn rescore(&mut self) {
        let items = self.filtered_items();

        if self.query.is_empty() {
            self.results = items
                .into_iter()
                .take(MAX_RESULTS)
                .map(|item| {
                    (
                        item,
                        FuzzyMatch {
                            score: 0,
                            matched_positions: Vec::new(),
                        },
                    )
                })
                .collect();
            return;
        }

        let mut scored: Vec<(FuzzyItem, FuzzyMatch)> = items
            .into_iter()
            .filter_map(|item| {
                score(&self.query, &item.text).map(|m| (item, m))
            })
            .collect();

        scored.sort_by(|a, b| b.1.score.cmp(&a.1.score));
        scored.truncate(MAX_RESULTS);
        self.results = scored;
    }

    /// Get items filtered by the active source tab.
    fn filtered_items(&self) -> Vec<FuzzyItem> {
        match &self.active_filter {
            None => self.all_items.clone(),
            Some(filter) => self
                .all_items
                .iter()
                .filter(|item| item.source_name == *filter)
                .cloned()
                .collect(),
        }
    }

    /// Render the fuzzy finder overlay as ANSI escape sequences.
    pub fn render(&self, cols: usize, rows: usize) -> String {
        let width = cols.min(80).max(40);
        let left = cols.saturating_sub(width) / 2;
        let top = FINDER_TOP_OFFSET;

        let r = "\x1b[0m";
        let border = "\x1b[38;2;122;162;247m";
        let fg = "\x1b[38;2;192;202;245m";
        let muted = "\x1b[38;2;86;95;137m";
        let sel_bg = "\x1b[48;2;40;44;66m";
        let bold = "\x1b[1m";
        let accent = "\x1b[38;2;122;162;247m";
        let match_hl = "\x1b[38;2;249;226;175m\x1b[1m"; // bold yellow for match chars

        let mut out = String::with_capacity(8192);
        out.push_str("\x1b[s");     // save cursor
        out.push_str("\x1b[?25l"); // hide cursor

        let inner = width.saturating_sub(2);
        let mut row = top;

        // Top border with title
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let title = " Fuzzy Finder ";
        let fill_len = inner.saturating_sub(title.len() + 1);
        let fill: String = "\u{2500}".repeat(fill_len);
        out.push_str(&format!(
            "{border}\u{256D}\u{2500}{r}{border}{bold}{title}{r}{border}{fill}\u{256E}{r}",
        ));
        row += 1;

        // Search input row
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let query_display: String = self.query.chars().take(inner.saturating_sub(4)).collect();
        let query_pad = inner.saturating_sub(query_display.len() + 3);
        out.push_str(&format!(
            "{border}\u{2502}{r} {accent}{bold}>{r} {fg}{query_display}{}{r} {border}\u{2502}{r}",
            " ".repeat(query_pad),
        ));
        row += 1;

        // Source filter tabs
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let mut tabs_str = String::new();
        let mut tabs_visible_len = 0;
        for (i, tab) in SOURCE_TABS.iter().enumerate() {
            if i > 0 {
                tabs_str.push_str(&format!("{muted} "));
                tabs_visible_len += 1;
            }
            if i == self.active_tab {
                tabs_str.push_str(&format!("{accent}{bold}[{tab}]{r}"));
            } else {
                tabs_str.push_str(&format!("{muted}[{tab}]{r}"));
            }
            tabs_visible_len += tab.len() + 2;
        }
        let tab_pad = inner.saturating_sub(tabs_visible_len + 1);
        out.push_str(&format!(
            "{border}\u{2502}{r} {tabs_str}{}{border}\u{2502}{r}",
            " ".repeat(tab_pad),
        ));
        row += 1;

        // Separator
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let sep = "\u{2500}".repeat(inner);
        out.push_str(&format!("{border}\u{251C}{sep}\u{2524}{r}"));
        row += 1;

        // Results
        let visible_count = self.results.len().min(MAX_RESULTS);
        if visible_count == 0 {
            out.push_str(&format!("\x1b[{row};{}H", left + 1));
            let msg = if self.query.is_empty() {
                "Type to search..."
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
            // Calculate max rows we can show (leave room for border + footer)
            let max_visible = ((rows as u16).saturating_sub(row + 3)) as usize;
            let show_count = visible_count.min(max_visible).min(MAX_RESULTS);

            for (vi, (item, fmatch)) in self.results.iter().take(show_count).enumerate() {
                let is_selected = vi == self.selected_index;
                let bg = if is_selected { sel_bg } else { "" };
                let bg_end = if is_selected { r } else { "" };

                out.push_str(&format!("\x1b[{row};{}H", left + 1));

                let marker = if is_selected { "\u{25B8}" } else { " " };

                // Build the display text with match highlighting
                let max_text = inner.saturating_sub(4);
                let display_chars: Vec<char> = item.text.chars().take(max_text).collect();
                let mut highlighted = String::new();
                for (ci, ch) in display_chars.iter().enumerate() {
                    if fmatch.matched_positions.contains(&ci) {
                        highlighted.push_str(&format!("{match_hl}{ch}{r}{bg}{fg}"));
                    } else {
                        highlighted.push(*ch);
                    }
                }

                let text_visible_len = display_chars.len();

                // Source tag
                let tag = &item.source_name;
                let tag_len = tag.len() + 3; // " [tag]"
                let pad = inner.saturating_sub(text_visible_len + tag_len + 2);

                out.push_str(&format!(
                    "{border}\u{2502}{r}{bg}{marker}{fg}{highlighted}{}{muted} [{tag}]{bg_end} {border}\u{2502}{r}",
                    " ".repeat(pad),
                ));
                row += 1;
            }
        }

        // Bottom border
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        let bot = "\u{2500}".repeat(inner);
        out.push_str(&format!("{border}\u{2570}{bot}\u{256F}{r}"));

        // Footer hints
        row += 1;
        out.push_str(&format!("\x1b[{row};{}H", left + 1));
        out.push_str(&format!(
            "{muted}  \u{23CE} select  Tab switch source  \u{2191}\u{2193} navigate  Esc close{r}",
        ));

        out.push_str("\x1b[?25h"); // show cursor
        out.push_str("\x1b[u");    // restore cursor
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Scoring tests ──────────────────────────────────────────────────

    #[test]
    fn test_fuzzy_score_exact_match() {
        let m = score("main.rs", "main.rs").unwrap();
        // Exact match should score very high
        assert!(m.score > 30);
        assert_eq!(m.matched_positions.len(), 7);
    }

    #[test]
    fn test_fuzzy_score_prefix_match() {
        let m = score("main", "main.rs").unwrap();
        assert!(m.score > 20);
        // All 4 chars should be consecutive from position 0
        assert_eq!(m.matched_positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_fuzzy_score_consecutive_bonus() {
        // "abc" in "abcdef" (consecutive) vs "abc" in "axbxcx" (spread)
        let consecutive = score("abc", "abcdef").unwrap();
        let spread = score("abc", "axbxcx").unwrap();
        assert!(
            consecutive.score > spread.score,
            "consecutive {} should beat spread {}",
            consecutive.score,
            spread.score
        );
    }

    #[test]
    fn test_fuzzy_score_word_boundary_bonus() {
        // "fb" matching "foo_bar" at word boundaries vs "fooXbar" without
        let boundary = score("fb", "foo_bar").unwrap();
        let no_boundary = score("fb", "fxxxxxb").unwrap();
        assert!(
            boundary.score > no_boundary.score,
            "boundary {} should beat no_boundary {}",
            boundary.score,
            no_boundary.score
        );
    }

    #[test]
    fn test_fuzzy_score_no_match() {
        assert!(score("xyz", "abc").is_none());
        assert!(score("zz", "hello world").is_none());
    }

    #[test]
    fn test_fuzzy_score_case_insensitive() {
        let m = score("main", "MAIN.RS").unwrap();
        assert!(m.score > 0);
        assert_eq!(m.matched_positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_fuzzy_score_empty_pattern() {
        let m = score("", "anything").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.matched_positions.is_empty());
    }

    #[test]
    fn test_fuzzy_score_camel_case() {
        let m = score("fb", "FooBar").unwrap();
        assert!(m.score > 0);
        assert_eq!(m.matched_positions, vec![0, 3]);
    }

    // ── FuzzyFinder tests ──────────────────────────────────────────────

    fn make_test_source(name: &str, items: Vec<(&str, &str)>) -> Box<dyn FuzzySource> {
        struct TestSource {
            name: String,
            items: Vec<FuzzyItem>,
        }
        impl FuzzySource for TestSource {
            fn name(&self) -> &str {
                &self.name
            }
            fn items(&self) -> Vec<FuzzyItem> {
                self.items.clone()
            }
        }

        let name_str = name.to_string();
        let fuzzy_items = items
            .into_iter()
            .map(|(text, detail)| FuzzyItem {
                text: text.to_string(),
                detail: if detail.is_empty() {
                    None
                } else {
                    Some(detail.to_string())
                },
                source_name: name_str.clone(),
                action: FuzzyAction::InsertText(text.to_string()),
            })
            .collect();
        Box::new(TestSource {
            name: name_str,
            items: fuzzy_items,
        })
    }

    #[test]
    fn test_fuzzy_finder_update_query() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![
                ("src/main.rs", ""),
                ("src/lib.rs", ""),
                ("README.md", ""),
                ("Cargo.toml", ""),
            ],
        )];
        let mut finder = FuzzyFinder::new(sources);

        finder.update_query("main");
        assert!(!finder.results().is_empty());
        // "src/main.rs" should be the top result
        assert_eq!(finder.results()[0].0.text, "src/main.rs");
    }

    #[test]
    fn test_fuzzy_finder_navigation() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![
                ("file_a.rs", ""),
                ("file_b.rs", ""),
                ("file_c.rs", ""),
            ],
        )];
        let mut finder = FuzzyFinder::new(sources);

        assert_eq!(finder.selected_index(), 0);

        finder.select_next();
        assert_eq!(finder.selected_index(), 1);

        finder.select_next();
        assert_eq!(finder.selected_index(), 2);

        // Should not go past end
        finder.select_next();
        assert_eq!(finder.selected_index(), 2);

        finder.select_prev();
        assert_eq!(finder.selected_index(), 1);

        finder.select_prev();
        assert_eq!(finder.selected_index(), 0);

        // Should not go below 0
        finder.select_prev();
        assert_eq!(finder.selected_index(), 0);
    }

    #[test]
    fn test_fuzzy_source_file_source() {
        // FileSource with a temp dir
        let dir = std::env::temp_dir().join("elwood_fuzzy_test_files");
        let _ = std::fs::create_dir_all(&dir);
        let test_file = dir.join("test_file.txt");
        let _ = std::fs::write(&test_file, "hello");

        let source = FileSource::new(dir.clone());
        let items = source.items();
        assert!(
            items.iter().any(|i| i.text.contains("test_file.txt")),
            "FileSource should find test_file.txt, got: {:?}",
            items.iter().map(|i| &i.text).collect::<Vec<_>>()
        );

        // Check that the action is OpenFile
        let item = items.iter().find(|i| i.text.contains("test_file.txt")).unwrap();
        matches!(&item.action, FuzzyAction::OpenFile(_));

        // Cleanup
        let _ = std::fs::remove_file(&test_file);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_fuzzy_match_highlighting() {
        let m = score("mr", "main.rs").unwrap();
        // 'm' at position 0, 'r' at position 5
        assert_eq!(m.matched_positions[0], 0);
        // The 'r' should match at position 5 (in "main.rs")
        assert_eq!(m.matched_positions[1], 5);
    }

    #[test]
    fn test_fuzzy_finder_tab_cycling() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![
            make_test_source("Files", vec![("src/main.rs", "")]),
            make_test_source("Commands", vec![("/help", "Show help")]),
            make_test_source("History", vec![("cargo test", "")]),
        ];
        let mut finder = FuzzyFinder::new(sources);

        // Initially on "All" tab
        assert_eq!(finder.active_tab, 0);
        assert_eq!(finder.results().len(), 3);

        // Tab to "Files"
        finder.cycle_tab();
        assert_eq!(finder.active_tab, 1);
        assert_eq!(finder.results().len(), 1);
        assert_eq!(finder.results()[0].0.source_name, "Files");

        // Tab to "Commands"
        finder.cycle_tab();
        assert_eq!(finder.active_tab, 2);
        assert_eq!(finder.results().len(), 1);
        assert_eq!(finder.results()[0].0.source_name, "Commands");

        // Tab to "History"
        finder.cycle_tab();
        assert_eq!(finder.active_tab, 3);

        // Tab wraps back to "All"
        finder.cycle_tab();
        assert_eq!(finder.active_tab, 0);
        assert_eq!(finder.results().len(), 3);
    }

    #[test]
    fn test_fuzzy_finder_render_not_empty() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![("src/main.rs", "")],
        )];
        let finder = FuzzyFinder::new(sources);
        let rendered = finder.render(80, 24);
        assert!(!rendered.is_empty());
        assert!(rendered.contains("Fuzzy Finder"));
    }

    #[test]
    fn test_fuzzy_finder_render_narrow_screen() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![("src/main.rs", "")],
        )];
        let finder = FuzzyFinder::new(sources);
        let rendered = finder.render(40, 24);
        assert!(!rendered.is_empty());
    }

    #[test]
    fn test_slash_command_source() {
        let source = SlashCommandSource::new();
        let items = source.items();
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.text == "/help"));
        assert!(items.iter().any(|i| i.text == "/clear"));
    }

    #[test]
    fn test_history_source() {
        let source = HistorySource::from_texts(vec![
            "cargo test".to_string(),
            "git push".to_string(),
        ]);
        let items = source.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "cargo test");
        assert_eq!(items[0].source_name, "History");
    }

    #[test]
    fn test_symbol_source_empty() {
        let source = SymbolSource;
        assert!(source.items().is_empty());
        assert_eq!(source.name(), "Symbols");
    }

    #[test]
    fn test_fuzzy_finder_type_and_backspace() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![
                ("src/main.rs", ""),
                ("src/lib.rs", ""),
                ("README.md", ""),
            ],
        )];
        let mut finder = FuzzyFinder::new(sources);

        finder.type_char('m');
        finder.type_char('a');
        assert_eq!(finder.query(), "ma");
        assert!(finder.results().iter().any(|r| r.0.text.contains("main")));

        finder.backspace();
        assert_eq!(finder.query(), "m");

        finder.backspace();
        assert_eq!(finder.query(), "");
        // Empty query shows all items
        assert_eq!(finder.results().len(), 3);
    }

    #[test]
    fn test_fuzzy_finder_selected_action() {
        let sources: Vec<Box<dyn FuzzySource>> = vec![make_test_source(
            "Files",
            vec![("src/main.rs", "")],
        )];
        let finder = FuzzyFinder::new(sources);
        let action = finder.selected_action();
        assert!(action.is_some());
        matches!(action.unwrap(), FuzzyAction::InsertText(_));
    }
}
