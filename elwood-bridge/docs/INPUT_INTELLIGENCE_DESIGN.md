# Input Intelligence Design Document

**Status**: Draft
**Author**: Research Agent
**Date**: 2026-02-22
**Scope**: NL classifier, completion engine, command palette, fuzzy history for Elwood Terminal

---

## 1. Natural Language Classifier

### 1.1 Background: How Warp Does It

Warp's Universal Input uses a **local classifier** (no network calls) that runs entirely on-device. It operates in three modes:

- **Agent Mode** -- natural language detected, routes to LLM
- **Terminal Mode** -- shell command detected, routes to `$SHELL -c`
- **Auto-Detection Mode** -- classifier decides per-keystroke

Key features:
- A **denylist** allows users to mark specific inputs that are falsely classified (e.g., `terraform plan` misdetected as NL). Stored in `Settings > AI > Input > Natural Language Denylist`.
- Manual override prefixes: `*` forces Agent Mode, `!` forces Terminal Mode.
- Toggle keybinding: `Cmd+I` / `Ctrl+I`.

### 1.2 Our Design: Heuristic Decision Tree

We avoid ML entirely. A pure-Rust heuristic classifier using feature extraction and weighted scoring achieves <1ms classification time on any input.

#### Feature Extraction

```rust
pub struct NlFeatures {
    /// Starts with a known command binary (ls, git, cargo, docker, etc.)
    starts_with_command: bool,
    /// Contains shell operators: |, >, >>, <, &&, ||, ;, `
    has_shell_operators: bool,
    /// Contains path-like patterns: /, ./, ../, ~/
    has_path_patterns: bool,
    /// Contains flag-like patterns: -f, --verbose, -xvf
    has_flags: bool,
    /// Starts with a question word: what, how, why, where, when, can, could, should, would, is, are, do, does
    starts_with_question_word: bool,
    /// Contains English prose indicators: "please", "help me", "I want", "I need", "explain"
    has_prose_markers: bool,
    /// Contains only ASCII-safe command chars (no spaces except between args)
    looks_like_argv: bool,
    /// Ratio of words that are English dictionary words (approximated by length/capitalization heuristics)
    english_word_ratio: f32,
    /// Input length in chars
    length: usize,
    /// Number of words (whitespace-separated tokens)
    word_count: usize,
    /// First token matches a filesystem path that exists
    first_token_is_path: bool,
}
```

#### Command Prefix Table

A pre-built `HashSet<&str>` of ~200 common command prefixes, populated at construction time:

```
ls, cd, pwd, echo, cat, head, tail, grep, find, sed, awk, sort, uniq,
wc, cut, tr, tee, xargs, mkdir, rmdir, rm, cp, mv, ln, touch, chmod,
chown, chgrp, stat, file, diff, patch, tar, gzip, gunzip, zip, unzip,
curl, wget, ssh, scp, rsync, git, docker, kubectl, cargo, rustc, rustup,
npm, npx, node, bun, python, python3, pip, pip3, go, make, cmake, gcc,
clang, javac, java, mvn, gradle, ruby, gem, perl, php, composer,
brew, apt, yum, dnf, pacman, snap, systemctl, journalctl, sudo, su,
env, export, alias, unalias, source, eval, exec, nohup, screen, tmux,
htop, top, ps, kill, killall, pkill, nice, renice, df, du, mount,
umount, fdisk, lsblk, free, uname, whoami, id, groups, passwd, date,
cal, uptime, hostname, ifconfig, ip, ping, traceroute, nslookup, dig,
netstat, ss, iptables, man, info, which, whereis, type, history,
terraform, ansible, vagrant, helm, skaffold, pulumi, sam, cdk,
pytest, jest, vitest, mocha, rspec, elwood
```

#### Classification Algorithm

```rust
pub fn classify(input: &str, denylist: &HashSet<String>, allowlist: &HashSet<String>) -> Classification {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Classification { mode: InputMode::Agent, confidence: 0.0 };
    }

    // 1. Check denylist/allowlist overrides (exact match)
    if denylist.contains(trimmed) {
        return Classification { mode: InputMode::Terminal, confidence: 1.0 };
    }
    if allowlist.contains(trimmed) {
        return Classification { mode: InputMode::Agent, confidence: 1.0 };
    }

    // 2. Prefix overrides (matching Warp conventions)
    if trimmed.starts_with('!') {
        return Classification { mode: InputMode::Terminal, confidence: 1.0 };
    }

    // 3. Extract features
    let features = extract_features(trimmed);

    // 4. Weighted scoring (positive = terminal, negative = agent)
    let mut score: f32 = 0.0;

    if features.starts_with_command     { score += 3.0; }
    if features.has_shell_operators     { score += 4.0; }
    if features.has_path_patterns       { score += 1.5; }
    if features.has_flags               { score += 3.0; }
    if features.first_token_is_path     { score += 2.0; }
    if features.looks_like_argv         { score += 2.0; }

    if features.starts_with_question_word { score -= 4.0; }
    if features.has_prose_markers         { score -= 3.0; }
    if features.english_word_ratio > 0.7  { score -= 2.0; }
    if features.word_count > 6 && !features.has_shell_operators { score -= 1.5; }

    // 5. Convert score to confidence and mode
    let confidence = (score.abs() / 8.0).min(1.0);
    let mode = if score >= 0.0 { InputMode::Terminal } else { InputMode::Agent };

    Classification { mode, confidence }
}
```

#### Confidence Threshold

```rust
const AUTO_DETECT_THRESHOLD: f32 = 0.3;
```

When `confidence < AUTO_DETECT_THRESHOLD`, the classifier is uncertain. In that case:
- Default to the current input mode (no auto-switch).
- Show a subtle indicator in the status bar: `[?] auto-detect uncertain`.

#### Example Classifications

| Input | Mode | Confidence | Reason |
|-------|------|------------|--------|
| `ls -la` | Terminal | 0.88 | command prefix + flag |
| `git status` | Terminal | 0.75 | command prefix |
| `what files are in this directory?` | Agent | 0.88 | question word + prose + high english ratio |
| `list all files` | Agent | 0.50 | no command prefix, english words |
| `cargo build --release` | Terminal | 1.0 | command prefix + flag |
| `help me fix this rust error` | Agent | 0.88 | prose markers + question-like |
| `explain the error in main.rs` | Agent | 0.75 | prose marker + no flags |
| `find . -name "*.rs"` | Terminal | 1.0 | command prefix + flag + path |
| `terraform plan` | Terminal | 0.75 | command prefix |
| `can you run the tests?` | Agent | 0.88 | question word + prose |

#### Data Structures

```rust
pub struct Classification {
    pub mode: InputMode,
    pub confidence: f32,
}

pub struct NlClassifier {
    /// Known command binaries (pre-populated ~200 entries).
    command_prefixes: HashSet<&'static str>,
    /// User-maintained denylist (inputs always classified as Terminal).
    denylist: HashSet<String>,
    /// User-maintained allowlist (inputs always classified as Agent).
    allowlist: HashSet<String>,
    /// Question word prefixes.
    question_words: &'static [&'static str],
    /// Prose marker phrases.
    prose_markers: &'static [&'static str],
}
```

#### Performance

The classifier does:
- 1 `HashSet::contains` check for first token (~O(1))
- Linear scan of ~10 feature checks over the input string
- No allocations beyond the `NlFeatures` struct (stack-allocated)
- No regex compilation or evaluation

**Target: <100 microseconds per classification** (validated with `criterion` benchmarks).

---

## 2. Completion Engine

### 2.1 Background: Fish Shell Autosuggestions

Fish shell's autosuggestion system is the gold standard for ghost-text completions:

- **Primary source**: Command history, searched by prefix match. Most recent matches prioritized.
- **Fallback**: Tab completion system (commands, file paths, context-specific completions).
- **Rendering**: Dimmed gray text after the cursor, controlled by `$fish_color_autosuggestion`.
- **Acceptance**: Right arrow accepts entire suggestion, Alt+Right accepts one word.
- **Performance**: Debounced computation, asynchronous history validation, result caching.
- **Multi-line**: Since Fish 4.2.0, supports multi-line history suggestions.

### 2.2 Multi-Source Architecture

```
                    ┌─────────────────────────────────────────┐
                    │           CompletionEngine               │
                    │                                         │
   input changed    │  ┌──────────┐  ┌────────────┐  ┌──────┐ │
  ─────────────────►│  │ History  │  │ Filesystem │  │  AI  │ │
                    │  │ Source   │  │ Source     │  │Source│ │
                    │  └────┬─────┘  └─────┬──────┘  └──┬───┘ │
                    │       │              │             │     │
                    │       ▼              ▼             ▼     │
                    │  ┌────────────────────────────────────┐  │
                    │  │         Merger + Ranker            │  │
                    │  │  (frecency scoring, dedup, limit)  │  │
                    │  └──────────────┬─────────────────────┘  │
                    │                 │                        │
                    │                 ▼                        │
                    │          Top suggestion(s)              │
                    └─────────────────────────────────────────┘
                                      │
                                      ▼
                             Ghost text rendering
```

### 2.3 Completion Sources

#### History Source (synchronous, <1ms)

```rust
pub struct HistorySource {
    /// All history entries with metadata.
    entries: Vec<HistoryEntry>,
    /// Index for prefix lookup (sorted by frecency score descending).
    prefix_index: Vec<(String, usize)>, // (lowercase prefix, entry index)
}

pub struct HistoryEntry {
    pub text: String,
    pub last_used: Instant,
    pub use_count: u32,
    pub directory: Option<String>,
}
```

**Frecency scoring** (frequency + recency):

```rust
fn frecency_score(entry: &HistoryEntry, now: Instant) -> f64 {
    let age_hours = now.duration_since(entry.last_used).as_secs_f64() / 3600.0;
    let recency_weight = match age_hours {
        h if h < 1.0   => 8.0,   // Last hour
        h if h < 24.0  => 4.0,   // Last day
        h if h < 168.0 => 2.0,   // Last week
        _              => 1.0,   // Older
    };
    (entry.use_count as f64).ln_1p() * recency_weight
}
```

**Lookup**: Binary search on `prefix_index` for prefix match, then sort top-N by frecency.

#### Filesystem Source (synchronous, <5ms)

Activated when the current token looks like a path (starts with `/`, `./`, `../`, `~/`, or contains `/`).

```rust
pub struct FilesystemSource;

impl FilesystemSource {
    pub fn complete(&self, partial_path: &str, limit: usize) -> Vec<String> {
        // 1. Expand ~ to home dir
        // 2. Split into (parent_dir, prefix)
        // 3. readdir(parent_dir), filter by prefix
        // 4. Sort: directories first, then alphabetical
        // 5. Append / for directories
        // 6. Return top `limit` results
    }
}
```

#### AI Source (asynchronous, 200ms-2s)

Queries the LLM for contextual completions. Only activated when:
- Input is >= 3 characters
- No history match found
- Current mode is Agent (not Terminal)
- A configurable delay (300ms) has passed since last keystroke (debounce)

```rust
pub struct AiSource {
    /// Channel to request completions from the agent runtime.
    request_tx: flume::Sender<AiCompletionRequest>,
    /// Channel to receive completion results.
    result_rx: flume::Receiver<AiCompletionResult>,
    /// The most recent pending result (updated asynchronously).
    latest: Mutex<Option<AiCompletionResult>>,
}

pub struct AiCompletionRequest {
    pub input: String,
    pub context: String, // e.g., current directory, recent commands
}

pub struct AiCompletionResult {
    pub suggestions: Vec<String>,
    pub input_at_request: String, // discard if input has changed
}
```

### 2.4 Merger and Ranking

```rust
pub struct CompletionEngine {
    history: HistorySource,
    filesystem: FilesystemSource,
    ai: AiSource,
    /// Debounce timer for AI requests.
    last_input_change: Instant,
}

impl CompletionEngine {
    pub fn get_suggestion(&self, input: &str, mode: InputMode) -> Option<String> {
        if input.is_empty() {
            return None;
        }

        // Source priority (first match wins for ghost text):
        // 1. History (instant, highest confidence)
        // 2. Filesystem (if path-like token, instant)
        // 3. AI (async, shown after delay)

        // History
        if let Some(hist_match) = self.history.prefix_match(input) {
            return Some(hist_match);
        }

        // Filesystem (only for the current token, not full input)
        let last_token = input.rsplit_once(' ').map(|(_, t)| t).unwrap_or(input);
        if looks_like_path(last_token) {
            if let Some(path_match) = self.filesystem.complete(last_token, 1).first() {
                // Return the full input with the token replaced
                let prefix = input.strip_suffix(last_token).unwrap_or("");
                return Some(format!("{prefix}{path_match}"));
            }
        }

        // AI (async -- return cached result if available and still relevant)
        if mode == InputMode::Agent {
            if let Some(ai_result) = self.ai.latest.lock().as_ref() {
                if input.starts_with(&ai_result.input_at_request) {
                    if let Some(suggestion) = ai_result.suggestions.first() {
                        return Some(suggestion.clone());
                    }
                }
            }
        }

        None
    }
}
```

### 2.5 Ghost Text Rendering (ANSI)

Ghost text is rendered as **dim italic text** after the cursor in the input box.

```rust
// ANSI escape codes for ghost text
const GHOST_START: &str = "\x1b[2;3m";  // dim + italic
const GHOST_END: &str = "\x1b[22;23m";  // reset dim + italic

// Ghost text color (TokyoNight muted: rgb(86, 95, 137))
const GHOST_COLOR: &str = "\x1b[38;2;86;95;137m";
```

In `render_input_box()`, after rendering the current input text:

```rust
if let Some(suggestion) = completion_engine.get_suggestion(input, mode) {
    // The suggestion includes the full text; extract the suffix after current input
    if let Some(ghost_suffix) = suggestion.strip_prefix(input) {
        out.push_str(GHOST_COLOR);
        out.push_str(GHOST_START);
        out.push_str(ghost_suffix);
        out.push_str(GHOST_END);
        out.push_str(RESET);
    }
}
```

### 2.6 Keybindings for Completion

| Key | Action |
|-----|--------|
| `Tab` | Accept entire suggestion |
| `Right Arrow` (at end of line) | Accept entire suggestion |
| `Alt+Right` | Accept one word from suggestion |
| `Esc` | Dismiss suggestion |
| Any other key | Continue typing (suggestion updates) |

Integration with `key_down()` in `pane.rs`:

```rust
KeyCode::Tab if mods.is_empty() => {
    if let Some(suggestion) = self.current_ghost_suggestion() {
        // Replace editor content with suggestion
        self.accept_suggestion(&suggestion);
    }
}
KeyCode::RightArrow if mods.is_empty() && at_end_of_line => {
    if let Some(suggestion) = self.current_ghost_suggestion() {
        self.accept_suggestion(&suggestion);
    } else {
        // Normal cursor movement
    }
}
KeyCode::RightArrow if mods == KeyModifiers::ALT => {
    if let Some(suggestion) = self.current_ghost_suggestion() {
        self.accept_next_word(&suggestion);
    }
}
```

---

## 3. Command Palette (Ctrl+P)

### 3.1 Background

A VS Code / Sublime-style command palette that overlays the chat area. Provides quick access to all Elwood actions via fuzzy search.

### 3.2 Overlay Rendering

The palette renders as a floating ANSI box centered on the screen, drawn over the scroll region using absolute cursor positioning:

```
┌──────────────────────────────────────────────┐
│  ╭─ Command Palette ──────────────────────╮  │
│  │ > search query_                        │  │
│  ├────────────────────────────────────────┤  │
│  │  /help          Show available commands│  │
│  │  /model         Switch LLM model      │  │
│  │  /clear         Clear conversation     │  │
│  │  /cost          Show token usage       │  │
│  │  /undo          Undo last change       │  │
│  ╰────────────────────────────────────────╯  │
└──────────────────────────────────────────────┘
```

Layout constants:

```rust
const PALETTE_WIDTH: u16 = 60;       // Fixed width
const PALETTE_MAX_ITEMS: usize = 10; // Max visible results
const PALETTE_TOP_OFFSET: u16 = 3;   // Rows from top of screen
```

Rendering uses absolute cursor positioning (`goto(row, col)`) with cursor save/restore to avoid disturbing the scroll region:

```rust
fn render_palette(state: &PaletteState, screen: &ScreenState) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("\x1b[s");           // save cursor
    out.push_str("\x1b[?25l");        // hide cursor

    let left = (screen.width.saturating_sub(PALETTE_WIDTH)) / 2;
    let top = PALETTE_TOP_OFFSET;

    // Draw box chrome + search input + filtered results
    // Each row: goto(top + row_offset, left) + content + clear_eol

    out.push_str("\x1b[?25h");        // show cursor
    out.push_str("\x1b[u");           // restore cursor
    out
}
```

### 3.3 Action Registry

```rust
pub struct PaletteAction {
    /// Display name (used for fuzzy matching).
    pub name: String,
    /// Short description shown to the right.
    pub description: String,
    /// Keybinding hint (if any).
    pub keybinding: Option<String>,
    /// Category for grouping.
    pub category: ActionCategory,
    /// Handler closure.
    pub handler: Box<dyn Fn(&ElwoodPane) + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    Command,     // Slash commands: /help, /clear, /model, etc.
    Navigation,  // Block nav, scroll, jump
    Mode,        // Toggle input mode, plan mode
    Session,     // New session, export, history
    Tool,        // Run specific tools
    Settings,    // Toggle settings
}
```

Built-in actions (initial set):

| Name | Description | Keybinding | Category |
|------|-------------|------------|----------|
| `/help` | Show available commands | - | Command |
| `/clear` | Clear conversation | - | Command |
| `/model` | Switch LLM model | - | Command |
| `/cost` | Show token usage & cost | - | Command |
| `/undo` | Undo last file change | - | Command |
| `/redo` | Redo undone change | - | Command |
| `/plan` | Toggle plan mode | - | Command |
| `/permissions` | Show permission rules | - | Command |
| `/memory` | Show auto-memory | - | Command |
| Toggle Input Mode | Switch Agent/Terminal | `Ctrl+T` | Mode |
| Quick Fix | Fix last error | `Ctrl+F` | Tool |
| Previous Block | Navigate to prev block | `Ctrl+Up` | Navigation |
| Next Block | Navigate to next block | `Ctrl+Down` | Navigation |
| Fuzzy History | Search command history | `Ctrl+R` | Navigation |
| New Session | Start fresh session | - | Session |
| Export Session | Export chat to file | - | Session |

### 3.4 Fuzzy Search with nucleo

We use the `nucleo-matcher` crate (the lower-level matching engine) for filtering palette actions. For our action list (< 100 items), direct matcher usage is appropriate without the higher-level `nucleo` crate.

```rust
use nucleo_matcher::{Matcher, Config};
use nucleo_matcher::pattern::{Pattern, CaseMatching, Normalization};

pub struct PaletteFuzzy {
    matcher: Matcher,
}

impl PaletteFuzzy {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn filter(&mut self, query: &str, actions: &[PaletteAction]) -> Vec<(usize, u32)> {
        if query.is_empty() {
            // Return all actions with score 0
            return (0..actions.len()).map(|i| (i, 0)).collect();
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut results: Vec<(usize, u32)> = Vec::new();

        for (i, action) in actions.iter().enumerate() {
            let haystack = nucleo_matcher::Utf32String::from(action.name.as_str());
            if let Some(score) = pattern.score(haystack.slice(..), &mut self.matcher) {
                results.push((i, score));
            }
        }

        results.sort_by(|a, b| b.1.cmp(&a.1)); // Highest score first
        results
    }
}
```

### 3.5 State Machine

```rust
pub struct PaletteState {
    /// Whether the palette is currently open.
    pub open: bool,
    /// Current search query.
    pub query: String,
    /// Filtered results (indices into action registry + scores).
    pub filtered: Vec<(usize, u32)>,
    /// Currently highlighted index in the filtered list.
    pub selected: usize,
}
```

### 3.6 Keyboard Handling

When palette is open, all key events are intercepted:

| Key | Action |
|-----|--------|
| `Esc` | Close palette |
| `Enter` | Execute selected action |
| `Up` | Move selection up |
| `Down` | Move selection down |
| `Backspace` | Delete char from query |
| `Char(c)` | Append to query, re-filter |
| `Ctrl+P` | Close palette (toggle) |

---

## 4. Fuzzy History Search (Ctrl+R)

### 4.1 Background: Atuin

Atuin is the state-of-the-art shell history tool:
- SQLite-backed storage with full context (directory, duration, exit code)
- Configurable search modes: PREFIX, FULLTEXT, FUZZY
- Filtering by host, directory, session
- Written in Rust for performance

### 4.2 History Store

```rust
pub struct HistoryStore {
    /// All history entries (both Agent and Terminal modes combined).
    entries: Vec<HistoryRecord>,
    /// File path for persistent storage.
    path: PathBuf,
    /// Whether the store has unsaved changes.
    dirty: bool,
}

pub struct HistoryRecord {
    /// The command or message text.
    pub text: String,
    /// When it was executed/sent.
    pub timestamp: u64,  // Unix epoch seconds
    /// Which mode it was entered in.
    pub mode: InputMode,
    /// Working directory at time of entry.
    pub directory: Option<String>,
    /// Exit code (for Terminal commands only).
    pub exit_code: Option<i32>,
    /// Number of times this exact text has been used.
    pub use_count: u32,
}
```

**Persistence**: JSONL file at `~/.elwood/history.jsonl`. Loaded on startup (lazy), appended on each submit. Capped at 50,000 entries (oldest evicted).

### 4.3 Fuzzy Matching

Using `nucleo-matcher` for consistent fuzzy matching across the entire system:

```rust
pub struct HistorySearch {
    matcher: Matcher,
    /// Pre-converted history strings for nucleo (avoids re-allocation).
    haystacks: Vec<Utf32String>,
}

impl HistorySearch {
    pub fn search(&mut self, query: &str, entries: &[HistoryRecord], limit: usize) -> Vec<SearchResult> {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut results: Vec<SearchResult> = Vec::new();

        for (i, haystack) in self.haystacks.iter().enumerate() {
            if let Some(score) = pattern.score(haystack.slice(..), &mut self.matcher) {
                // Boost score by frecency
                let frecency = frecency_score(&entries[i]);
                let final_score = score as f64 + frecency * 100.0;
                results.push(SearchResult {
                    index: i,
                    score: final_score,
                    matched_ranges: vec![], // Could extract with pattern.indices()
                });
            }
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }
}

pub struct SearchResult {
    pub index: usize,
    pub score: f64,
    pub matched_ranges: Vec<(usize, usize)>,
}
```

### 4.4 Interactive UI

The history search renders as an overlay (similar to the command palette) but with a different layout:

```
┌──────────────────────────────────────────────────┐
│  ╭─ History Search (Ctrl+R) ─────────────────╮   │
│  │ > query_                                  │   │
│  ├───────────────────────────────────────────┤   │
│  │ > cargo test --workspace         (2m ago) │   │
│  │   cargo build --release         (15m ago) │   │
│  │   git push origin main           (1h ago) │   │
│  │   cargo clippy -- -D warnings    (2h ago) │   │
│  │   ls -la src/                     (3h ago) │   │
│  ╰───────────────────────────────────────────╯   │
└──────────────────────────────────────────────────┘
```

Features:
- **Highlighted match characters**: Matched characters shown in accent color.
- **Relative timestamps**: "2m ago", "1h ago", "3d ago".
- **Mode indicator**: Agent entries shown with a different icon than Terminal entries.
- **Preview**: Selected entry shows full text (for multi-line entries).

### 4.5 State Machine

```rust
pub struct HistorySearchState {
    pub open: bool,
    pub query: String,
    pub results: Vec<SearchResult>,
    pub selected: usize,
    /// Full text of the selected entry (for preview).
    pub preview: Option<String>,
}
```

### 4.6 Keybindings

| Key | Action |
|-----|--------|
| `Ctrl+R` | Open/close history search |
| `Esc` | Close without selecting |
| `Enter` | Insert selected entry into editor |
| `Up` | Move selection up |
| `Down` | Move selection down |
| `Char(c)` | Append to query, re-search |
| `Backspace` | Delete char from query |

When an entry is selected (Enter), the text is loaded into the `InputEditor` and the overlay closes. The user can then edit before submitting.

---

## 5. Integration with InputEditor

### 5.1 Input Mode State Machine

The `InputEditor` (and by extension, `ElwoodPane`) operates in one of these states:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorState {
    /// Normal typing. Ghost text suggestion may be visible.
    Normal,
    /// Command palette is open. All keys routed to palette.
    Palette,
    /// Fuzzy history search is open. All keys routed to search.
    HistorySearch,
    /// Awaiting permission approval. Only y/n/Esc accepted.
    AwaitingPermission,
}
```

### 5.2 Key Event Routing

```rust
fn key_down(&self, key: KeyCode, mods: KeyModifiers) -> anyhow::Result<()> {
    let state = *self.editor_state.lock();

    match state {
        EditorState::AwaitingPermission => {
            // Only y/n/Esc (existing code)
            self.handle_permission_key(key, mods)
        }
        EditorState::Palette => {
            self.handle_palette_key(key, mods)
        }
        EditorState::HistorySearch => {
            self.handle_history_search_key(key, mods)
        }
        EditorState::Normal => {
            // Check for overlay triggers first
            if key == KeyCode::Char('p') && mods == KeyModifiers::CTRL {
                self.open_palette();
                return Ok(());
            }
            if key == KeyCode::Char('r') && mods == KeyModifiers::CTRL {
                self.open_history_search();
                return Ok(());
            }

            // Normal input handling (existing code)
            // After any character input, trigger completion update
            self.handle_normal_key(key, mods)?;

            // Update ghost text suggestion
            self.update_completion();

            Ok(())
        }
    }
}
```

### 5.3 Completion Trigger Flow

```
User types character
    │
    ▼
InputEditor.insert_char(c)
    │
    ▼
sync_editor_to_screen()
    │
    ▼
CompletionEngine.get_suggestion(input, mode)
    │
    ├── History prefix match? ──► ghost text
    ├── Path-like? ──► filesystem complete ──► ghost text
    └── Agent mode + debounce elapsed? ──► async AI request
    │
    ▼
refresh_input_box() (includes ghost text)
```

### 5.4 NL Classification Trigger

Classification runs on every character input, but only affects the mode indicator when in auto-detect mode:

```rust
fn update_classification(&self) {
    if !self.auto_detect_enabled {
        return;
    }
    let input = self.input_editor.lock().content();
    let classification = self.nl_classifier.classify(&input);

    // Update status bar indicator (subtle, non-disruptive)
    let mut ss = self.screen.lock();
    ss.detected_mode = Some(classification.mode);
    ss.detection_confidence = classification.confidence;
}
```

The actual mode switch only happens on `Enter` (submit):

```rust
fn submit_with_auto_detect(&self) {
    let content = self.input_editor.lock().content();
    let classification = self.nl_classifier.classify(&content);

    if classification.confidence >= AUTO_DETECT_THRESHOLD {
        match classification.mode {
            InputMode::Agent => self.submit_input(),
            InputMode::Terminal => self.submit_command(),
        }
    } else {
        // Use current explicit mode
        match self.input_editor.lock().mode() {
            InputMode::Agent => self.submit_input(),
            InputMode::Terminal => self.submit_command(),
        }
    }
}
```

---

## 6. Implementation Plan

### 6.1 Files to Create

| File | Purpose |
|------|---------|
| `src/classifier.rs` | `NlClassifier`, `NlFeatures`, `Classification` |
| `src/completion.rs` | `CompletionEngine`, `HistorySource`, `FilesystemSource`, `AiSource` |
| `src/palette.rs` | `CommandPalette`, `PaletteAction`, `PaletteState`, rendering |
| `src/history.rs` | `HistoryStore`, `HistoryRecord`, `HistorySearch`, JSONL persistence |
| `src/overlay.rs` | Shared overlay rendering utilities (box drawing, positioning, fuzzy highlight) |

### 6.2 Files to Modify

| File | Changes |
|------|---------|
| `src/editor.rs` | Add `EditorState` enum, ghost text field, completion acceptance methods |
| `src/pane.rs` | Integrate `key_down` routing for palette/history/completion states |
| `src/screen.rs` | Add ghost text rendering in `render_input_box()`, overlay rendering |
| `src/lib.rs` | Add `mod classifier; mod completion; mod palette; mod history; mod overlay;` |
| `Cargo.toml` | Add `nucleo-matcher = "0.3"` dependency |

### 6.3 Dependencies to Add

| Crate | Version | Purpose |
|-------|---------|---------|
| `nucleo-matcher` | `0.3` | Fuzzy matching (palette, history search) |

**Not** adding `skim` -- nucleo is 6x faster, better Unicode handling, and used by Helix (our reference editor). The lower-level `nucleo-matcher` is sufficient since we never search >1000 items interactively.

**Not** adding `nucleo` (the high-level crate) -- that includes parallelism and streaming infrastructure we don't need for <1000-item lists.

### 6.4 Data Structures Summary

```rust
// classifier.rs
pub struct NlClassifier { ... }
pub struct Classification { mode: InputMode, confidence: f32 }

// completion.rs
pub struct CompletionEngine { history: HistorySource, filesystem: FilesystemSource, ai: AiSource }
pub struct HistorySource { entries: Vec<HistoryEntry>, prefix_index: Vec<(String, usize)> }

// palette.rs
pub struct CommandPalette { actions: Vec<PaletteAction>, state: PaletteState, fuzzy: PaletteFuzzy }
pub struct PaletteAction { name: String, description: String, handler: ... }
pub struct PaletteState { open: bool, query: String, filtered: Vec<...>, selected: usize }

// history.rs
pub struct HistoryStore { entries: Vec<HistoryRecord>, path: PathBuf }
pub struct HistoryRecord { text: String, timestamp: u64, mode: InputMode, ... }
pub struct HistorySearchState { open: bool, query: String, results: Vec<...>, selected: usize }
```

### 6.5 Test Strategy

#### Unit Tests

| Module | Tests |
|--------|-------|
| `classifier.rs` | 15+ tests: known commands, NL phrases, edge cases (empty, single char, Unicode), denylist/allowlist, confidence thresholds |
| `completion.rs` | 10+ tests: history prefix match, filesystem completion, frecency ordering, ghost text suffix extraction |
| `palette.rs` | 10+ tests: action registration, fuzzy filter correctness, selection navigation, empty query returns all |
| `history.rs` | 10+ tests: JSONL round-trip, frecency scoring, search result ordering, deduplication, cap enforcement |

#### Example Test Inputs

```rust
#[test]
fn classify_known_commands() {
    let c = NlClassifier::new();
    assert_eq!(c.classify("ls -la").mode, InputMode::Terminal);
    assert_eq!(c.classify("git status").mode, InputMode::Terminal);
    assert_eq!(c.classify("cargo build --release").mode, InputMode::Terminal);
    assert!(c.classify("cargo build --release").confidence > 0.7);
}

#[test]
fn classify_natural_language() {
    let c = NlClassifier::new();
    assert_eq!(c.classify("what files are here?").mode, InputMode::Agent);
    assert_eq!(c.classify("help me fix this error").mode, InputMode::Agent);
    assert_eq!(c.classify("explain the diff").mode, InputMode::Agent);
    assert!(c.classify("explain the diff").confidence > 0.5);
}

#[test]
fn classify_ambiguous_defaults_to_current() {
    let c = NlClassifier::new();
    let result = c.classify("run tests");
    // Could be either -- confidence should be low
    assert!(result.confidence < 0.3);
}

#[test]
fn history_frecency_prefers_recent() {
    let recent = HistoryEntry { use_count: 1, last_used: Instant::now(), .. };
    let frequent = HistoryEntry { use_count: 100, last_used: Instant::now() - Duration::from_secs(86400 * 30), .. };
    assert!(frecency_score(&recent) > frecency_score(&frequent));
}
```

#### Integration Tests

- End-to-end: type input, verify ghost text appears with correct suggestion
- Palette: open, type query, verify filtered results, select and execute
- History: submit entries, open Ctrl+R, search, verify ordering

### 6.6 Implementation Order

1. **`classifier.rs`** -- standalone, no dependencies on other new modules
2. **`history.rs`** -- standalone persistence + search, needed by completion engine
3. **`completion.rs`** -- depends on history.rs, provides ghost text
4. **`overlay.rs`** -- shared rendering utilities
5. **`palette.rs`** -- depends on overlay.rs + nucleo-matcher
6. **Wire into `editor.rs`** -- `EditorState`, ghost text field
7. **Wire into `pane.rs`** -- key routing, overlay rendering
8. **Wire into `screen.rs`** -- ghost text in input box

---

## 7. Research References

- [Warp Universal Input documentation](https://docs.warp.dev/terminal/universal-input)
- [nucleo: fast fuzzy matcher for Rust](https://github.com/helix-editor/nucleo) -- 6x faster than skim, used by Helix editor
- [nucleo-matcher API documentation](https://docs.rs/nucleo-matcher)
- [skim: Rust FZF implementation](https://github.com/skim-rs/skim) -- not chosen due to performance gap vs nucleo
- [Atuin: magical shell history](https://github.com/atuinsh/atuin) -- SQLite-backed, Rust, multi-mode search
- [Fish shell autosuggestions](https://fishshell.com/docs/current/interactive.html) -- ghost text, debounced history, async validation
- [Helix editor nucleo PR #7814](https://github.com/helix-editor/helix/pull/7814) -- transition from skim to nucleo
- [fzf discussion: Thoughts about Nucleo](https://github.com/junegunn/fzf/discussions/3491) -- performance comparison
