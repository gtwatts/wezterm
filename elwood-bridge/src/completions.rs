//! Multi-source completion engine with ghost text support.
//!
//! Provides Fish-style autosuggestions from multiple sources:
//! - **History**: prefix match against command history, scored by recency
//! - **Filesystem**: path completion when input contains `/` or starts with `.`
//! - **Static**: common command completions
//!
//! The top suggestion is rendered as dim "ghost text" after the cursor.

use crate::semantic_bridge::SemanticBridge;
use std::path::Path;
use std::time::Instant;

/// A single completion suggestion.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    /// The full text of the completion (including the prefix the user already typed).
    pub text: String,
    /// Where this completion came from.
    pub source: CompletionSource,
    /// Score for ranking (higher = better).
    pub score: f32,
}

/// Source of a completion suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionSource {
    /// From command history.
    History,
    /// From filesystem path completion.
    Filesystem,
    /// From static/built-in completions.
    Static,
    /// From project symbol index (functions, structs, etc.).
    Symbol,
}

/// A history entry with metadata for frecency scoring.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The command text.
    pub text: String,
    /// When the command was last used.
    pub last_used: Instant,
    /// Number of times the command has been used.
    pub use_count: u32,
}

/// Multi-source completion engine.
pub struct CompletionEngine {
    /// Command history entries.
    history: Vec<HistoryEntry>,
}

impl CompletionEngine {
    /// Create a new completion engine with no history.
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
        }
    }

    /// Create a completion engine pre-loaded with history.
    pub fn with_history(history: Vec<HistoryEntry>) -> Self {
        Self { history }
    }

    /// Add a history entry from a submitted command.
    pub fn add_history(&mut self, text: String) {
        // Check if we already have this exact text
        if let Some(entry) = self.history.iter_mut().find(|e| e.text == text) {
            entry.use_count += 1;
            entry.last_used = Instant::now();
        } else {
            self.history.push(HistoryEntry {
                text,
                last_used: Instant::now(),
                use_count: 1,
            });
        }
    }

    /// Get completions for the given input.
    ///
    /// Returns completions ranked by source priority and score.
    /// The `cwd` parameter is used for filesystem completions.
    pub fn get_completions(&self, input: &str, cwd: &Path) -> Vec<Completion> {
        self.get_completions_with_symbols(input, cwd, None)
    }

    /// Get completions including symbol completions from a [`SemanticBridge`].
    ///
    /// When `semantic_bridge` is `Some`, the last whitespace-delimited token
    /// of `input` is used to fuzzy-match project symbols (functions, structs,
    /// etc.) from the symbol index.
    pub fn get_completions_with_symbols(
        &self,
        input: &str,
        cwd: &Path,
        semantic_bridge: Option<&SemanticBridge>,
    ) -> Vec<Completion> {
        if input.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::new();

        // 1. History completions (highest priority)
        results.extend(self.history_completions(input));

        // 2. Filesystem completions (if input looks like a path)
        let last_token = input.rsplit_once(' ').map(|(_, t)| t).unwrap_or(input);
        if looks_like_path(last_token) {
            results.extend(filesystem_completions(last_token, input, cwd));
        }

        // 3. Symbol completions (from tree-sitter index)
        if let Some(bridge) = semantic_bridge {
            results.extend(symbol_completions(last_token, input, bridge));
        }

        // 4. Static completions
        results.extend(static_completions(input));

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Deduplicate by text
        let mut seen = std::collections::HashSet::new();
        results.retain(|c| seen.insert(c.text.clone()));

        results
    }

    /// Get the top ghost text suggestion (the suffix to display after current input).
    ///
    /// Returns `None` if no suggestion matches or the suggestion equals the input.
    pub fn ghost_text(&self, input: &str, cwd: &Path) -> Option<String> {
        if input.is_empty() {
            return None;
        }
        let completions = self.get_completions(input, cwd);
        completions.first().and_then(|c| {
            c.text.strip_prefix(input).map(|suffix| suffix.to_string())
        }).filter(|s| !s.is_empty())
    }

    /// Get history entries matching a prefix.
    fn history_completions(&self, input: &str) -> Vec<Completion> {
        let input_lower = input.to_ascii_lowercase();
        let now = Instant::now();

        self.history
            .iter()
            .filter(|entry| {
                entry.text.to_ascii_lowercase().starts_with(&input_lower)
                    && entry.text != input
            })
            .map(|entry| {
                let score = frecency_score(entry, now);
                Completion {
                    text: entry.text.clone(),
                    source: CompletionSource::History,
                    score: score as f32 + 100.0, // Boost history above other sources
                }
            })
            .take(10)
            .collect()
    }
}

impl Default for CompletionEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Calculate frecency score (frequency + recency).
///
/// More recent commands and frequently used commands score higher.
pub fn frecency_score(entry: &HistoryEntry, now: Instant) -> f64 {
    let age_secs = now.duration_since(entry.last_used).as_secs_f64();
    let age_hours = age_secs / 3600.0;
    let recency_weight = if age_hours < 1.0 {
        8.0
    } else if age_hours < 24.0 {
        4.0
    } else if age_hours < 168.0 {
        2.0
    } else {
        1.0
    };
    (entry.use_count as f64).ln_1p() * recency_weight
}

/// Check if a token looks like a filesystem path.
fn looks_like_path(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || (token.contains('/') && !token.contains(' '))
}

/// Generate filesystem completions for a partial path.
fn filesystem_completions(partial_path: &str, full_input: &str, cwd: &Path) -> Vec<Completion> {
    let expanded = if partial_path.starts_with("~/") {
        if let Some(home) = dirs_next::home_dir() {
            home.join(&partial_path[2..])
        } else {
            return Vec::new();
        }
    } else if partial_path.starts_with('/') {
        std::path::PathBuf::from(partial_path)
    } else {
        cwd.join(partial_path)
    };

    let (parent, prefix) = if expanded.is_dir() && partial_path.ends_with('/') {
        (expanded.clone(), String::new())
    } else {
        let parent = expanded.parent().unwrap_or(cwd).to_path_buf();
        let prefix = expanded
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        (parent, prefix)
    };

    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };

    let prefix_before_token = full_input.strip_suffix(partial_path).unwrap_or("");

    let mut completions: Vec<Completion> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Skip hidden files unless prefix starts with '.'
            if name.starts_with('.') && !prefix.starts_with('.') {
                return false;
            }
            name.starts_with(&prefix)
        })
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let suffix = if is_dir { "/" } else { "" };

            // Build the full path token
            let completed_token = if partial_path.ends_with('/') {
                format!("{partial_path}{name}{suffix}")
            } else {
                let parent_part = partial_path
                    .rsplit_once('/')
                    .map(|(p, _)| format!("{p}/"))
                    .unwrap_or_default();
                format!("{parent_part}{name}{suffix}")
            };

            let full_text = format!("{prefix_before_token}{completed_token}");
            let score = if is_dir { 1.5 } else { 1.0 };

            Completion {
                text: full_text,
                source: CompletionSource::Filesystem,
                score,
            }
        })
        .take(20)
        .collect();

    // Sort directories first, then alphabetically
    completions.sort_by(|a, b| {
        let a_dir = a.text.ends_with('/');
        let b_dir = b.text.ends_with('/');
        b_dir.cmp(&a_dir).then_with(|| a.text.cmp(&b.text))
    });

    completions.truncate(10);
    completions
}

/// Generate symbol completions from the project's tree-sitter index.
fn symbol_completions(
    last_token: &str,
    full_input: &str,
    bridge: &SemanticBridge,
) -> Vec<Completion> {
    if last_token.len() < 2 {
        return Vec::new();
    }

    let prefix_before_token = full_input.strip_suffix(last_token).unwrap_or("");

    bridge
        .complete_symbol(last_token, 10)
        .into_iter()
        .map(|sym| {
            let full_text = format!("{prefix_before_token}{}", sym.name);
            Completion {
                text: full_text,
                source: CompletionSource::Symbol,
                score: sym.score * 0.8, // Slightly below history boost
            }
        })
        .collect()
}

/// Generate static completions for common commands.
fn static_completions(input: &str) -> Vec<Completion> {
    const COMMON_COMMANDS: &[&str] = &[
        "git status",
        "git add .",
        "git commit -m \"",
        "git push",
        "git pull",
        "git log --oneline",
        "git diff",
        "git checkout",
        "git branch",
        "cargo build",
        "cargo build --release",
        "cargo test",
        "cargo test --workspace",
        "cargo clippy",
        "cargo clippy -- -D warnings",
        "cargo check",
        "cargo run",
        "cargo fmt",
        "npm install",
        "npm run build",
        "npm run dev",
        "npm test",
        "docker compose up",
        "docker compose down",
        "docker ps",
        "ls -la",
        "ls -la src/",
    ];

    let input_lower = input.to_ascii_lowercase();

    COMMON_COMMANDS
        .iter()
        .filter(|cmd| {
            cmd.to_ascii_lowercase().starts_with(&input_lower) && **cmd != input
        })
        .map(|cmd| Completion {
            text: cmd.to_string(),
            source: CompletionSource::Static,
            score: 0.5,
        })
        .take(5)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn engine_with_history() -> CompletionEngine {
        let now = Instant::now();
        CompletionEngine::with_history(vec![
            HistoryEntry {
                text: "cargo test --workspace".to_string(),
                last_used: now,
                use_count: 5,
            },
            HistoryEntry {
                text: "cargo build --release".to_string(),
                last_used: now - Duration::from_secs(3600),
                use_count: 3,
            },
            HistoryEntry {
                text: "git push origin main".to_string(),
                last_used: now - Duration::from_secs(7200),
                use_count: 10,
            },
            HistoryEntry {
                text: "cargo clippy -- -D warnings".to_string(),
                last_used: now - Duration::from_secs(1800),
                use_count: 2,
            },
        ])
    }

    // ── History completions ──────────────────────────────────────────

    #[test]
    fn history_prefix_match() {
        let engine = engine_with_history();
        let completions = engine.get_completions("cargo t", Path::new("/tmp"));
        assert!(!completions.is_empty());
        assert_eq!(completions[0].text, "cargo test --workspace");
        assert_eq!(completions[0].source, CompletionSource::History);
    }

    #[test]
    fn history_no_match() {
        let engine = engine_with_history();
        let completions = engine.get_completions("python", Path::new("/tmp"));
        // No history matches, might get static completions
        assert!(completions.iter().all(|c| c.source != CompletionSource::History));
    }

    #[test]
    fn history_multiple_matches() {
        let engine = engine_with_history();
        let completions = engine.get_completions("cargo", Path::new("/tmp"));
        let history_count = completions
            .iter()
            .filter(|c| c.source == CompletionSource::History)
            .count();
        assert!(history_count >= 3); // cargo test, cargo build, cargo clippy
    }

    #[test]
    fn history_exact_match_excluded() {
        let engine = engine_with_history();
        let completions = engine.get_completions("cargo test --workspace", Path::new("/tmp"));
        // Exact match should be excluded (no ghost text for identical input)
        assert!(completions
            .iter()
            .all(|c| c.text != "cargo test --workspace"));
    }

    // ── Ghost text ───────────────────────────────────────────────────

    #[test]
    fn ghost_text_suffix() {
        let engine = engine_with_history();
        let ghost = engine.ghost_text("cargo t", Path::new("/tmp"));
        assert_eq!(ghost, Some("est --workspace".to_string()));
    }

    #[test]
    fn ghost_text_empty_input() {
        let engine = engine_with_history();
        let ghost = engine.ghost_text("", Path::new("/tmp"));
        assert_eq!(ghost, None);
    }

    #[test]
    fn ghost_text_no_match() {
        let engine = engine_with_history();
        let ghost = engine.ghost_text("zzzzz", Path::new("/tmp"));
        assert_eq!(ghost, None);
    }

    // ── Static completions ───────────────────────────────────────────

    #[test]
    fn static_git_completions() {
        let engine = CompletionEngine::new();
        let completions = engine.get_completions("git s", Path::new("/tmp"));
        assert!(completions.iter().any(|c| c.text == "git status"));
    }

    #[test]
    fn static_cargo_completions() {
        let engine = CompletionEngine::new();
        let completions = engine.get_completions("cargo b", Path::new("/tmp"));
        assert!(completions.iter().any(|c| c.text == "cargo build"));
    }

    // ── Frecency scoring ─────────────────────────────────────────────

    #[test]
    fn frecency_recent_scores_higher() {
        let now = Instant::now();
        let recent = HistoryEntry {
            text: "recent".to_string(),
            last_used: now,
            use_count: 1,
        };
        let old = HistoryEntry {
            text: "old".to_string(),
            last_used: now - Duration::from_secs(86400 * 30),
            use_count: 1,
        };
        assert!(frecency_score(&recent, now) > frecency_score(&old, now));
    }

    #[test]
    fn frecency_frequent_scores_higher() {
        let now = Instant::now();
        let frequent = HistoryEntry {
            text: "frequent".to_string(),
            last_used: now,
            use_count: 100,
        };
        let rare = HistoryEntry {
            text: "rare".to_string(),
            last_used: now,
            use_count: 1,
        };
        assert!(frecency_score(&frequent, now) > frecency_score(&rare, now));
    }

    // ── Path detection ───────────────────────────────────────────────

    #[test]
    fn path_detection() {
        assert!(looks_like_path("/usr/bin"));
        assert!(looks_like_path("./src"));
        assert!(looks_like_path("../parent"));
        assert!(looks_like_path("~/Documents"));
        assert!(looks_like_path("src/main.rs"));
        assert!(!looks_like_path("hello world"));
        assert!(!looks_like_path("cargo"));
    }

    // ── Add history ──────────────────────────────────────────────────

    #[test]
    fn add_history_new_entry() {
        let mut engine = CompletionEngine::new();
        engine.add_history("ls -la".to_string());
        let ghost = engine.ghost_text("ls", Path::new("/tmp"));
        assert_eq!(ghost, Some(" -la".to_string()));
    }

    #[test]
    fn add_history_increment_existing() {
        let mut engine = CompletionEngine::new();
        engine.add_history("cargo test".to_string());
        engine.add_history("cargo test".to_string());
        assert_eq!(engine.history.len(), 1);
        assert_eq!(engine.history[0].use_count, 2);
    }

    // ── Filesystem completions ───────────────────────────────────────

    #[test]
    fn filesystem_completion_root() {
        let engine = CompletionEngine::new();
        // /tmp should exist on all unix systems
        let completions = engine.get_completions("/tmp/", Path::new("/"));
        // We just verify it doesn't panic — actual contents vary
        let _ = completions;
    }

    // ── Case insensitive ─────────────────────────────────────────────

    #[test]
    fn case_insensitive_history_match() {
        let mut engine = CompletionEngine::new();
        engine.add_history("Git Status".to_string());
        let completions = engine.get_completions("git", Path::new("/tmp"));
        assert!(completions.iter().any(|c| c.text == "Git Status"));
    }

    // ── Default trait ────────────────────────────────────────────────

    #[test]
    fn default_engine() {
        let engine = CompletionEngine::default();
        let completions = engine.get_completions("git s", Path::new("/tmp"));
        assert!(completions.iter().any(|c| c.text == "git status"));
    }
}
