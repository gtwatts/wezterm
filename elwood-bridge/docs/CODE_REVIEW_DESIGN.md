# Code Review & Diff Viewer System Design

**Component**: elwood-bridge (WezTerm integration layer)
**Status**: Design Document (pre-implementation)
**Author**: Research Agent
**Date**: 2026-02-22

---

## 1. Goals & Prior Art

### 1.1 Design Goals

Build a code review and diff viewer system for Elwood Terminal that:

1. Renders inline diffs directly in the agent chat scroll area
2. Supports line-level and word-level diff granularity
3. Enables inline commenting on specific lines within a diff
4. Integrates with the agent feedback loop (propose -> review -> revise)
5. Provides `/diff` and `/pr-review` commands for git integration
6. Uses the existing ANSI virtual terminal rendering (no GUI widgets)

### 1.2 Warp Reference

Warp's code review system (Agents 3.0, September 2025) provides:

- **Code Review Panel**: A side-pane showing uncommitted changes with file sidebar, hunk navigation, and three comparison modes (uncommitted, vs main, vs custom branch).
- **Inline Comments**: Users select changed lines and add annotations. Comments are anchored to specific file:line pairs so the agent understands exactly what to fix.
- **Batch Feedback**: Multiple comments are collected and submitted at once. The agent applies all feedback in a single iteration and returns an updated diff.
- **Agent Diff Flow**: When the agent modifies files, Warp gathers edits into a visual diff view with hunks. Users navigate hunks with arrow keys, and can approve, reject, or comment on each.
- **In-place Editing**: Users can edit diffs directly in the review panel. Changes sync bidirectionally with the working directory.
- **Hunk Revert**: Individual hunks can be reverted from the gutter.

**Key Warp insight**: 96%+ acceptance rate of agent-suggested diffs. The review is lightweight by design -- most changes are accepted; the UI exists for the cases where human review catches issues.

### 1.3 Delta Reference

Delta (dandavison/delta) is a Rust-based syntax-highlighting pager for git output, relevant as an architecture reference:

- **State Machine Parser**: Input is parsed via states corresponding to semantic sections (HunkMinus, HunkPlus, etc.).
- **Syntect Integration**: Delta calls the syntect library to compute syntax highlighting for minus/plus lines. The language is determined by filename in the diff header.
- **Painter Architecture**: A `Painter` struct holds the syntax highlighter, output stream, and two line buffers (minus lines and plus lines). Nearby lines are buffered before painting so word-level edits can be detected.
- **Within-Line Changes**: Alignment inference between homologous minus/plus line pairs, then Levenshtein edit inference for word-level emphasis.
- **ANSI Color**: Supports 8 ANSI colors, 256 color mode, and 24-bit truecolor. Detects `COLORTERM=truecolor` for full color output.

---

## 2. Diff Engine

### 2.1 Library Selection: `similar` (recommended)

| Feature | `similar` | `diffy` | `imara-diff` |
|---------|-----------|---------|--------------|
| Line-level diff | Yes | Yes | Yes |
| Word-level diff | Yes (`from_words`) | No | No |
| Inline emphasis | Yes (`iter_inline_changes`) | No | No |
| Algorithms | Patience, Myers, LCS | Myers | Myers, Histogram |
| Unicode-aware | Yes (grapheme-level with `unicode` feature) | No | No |
| Zero-dependency | Yes (core) | No | Yes |
| Unified diff output | Yes (`unified_diff()`) | Yes | No |
| Active maintenance | Yes (mitsuhiko) | Low | Yes |

**Decision**: Use `similar` with `inline` and `unicode` features.

- `TextDiff::from_lines()` for line-level diffing
- `iter_inline_changes()` for word-level emphasis within changed lines
- `grouped_ops()` for hunk extraction with context lines
- `ratio()` for quick similarity scoring (useful for detecting renames)

### 2.2 Diff Computation Pipeline

```
Input (old_text, new_text, filename)
  |
  v
TextDiff::from_lines(&old_text, &new_text)
  |
  v
grouped_ops(context_lines=3)  -->  Vec<Vec<DiffOp>>  (hunks)
  |
  v
For each Replace op:  iter_inline_changes()  -->  word-level emphasis
  |
  v
DiffHunk { header, lines: Vec<DiffLine> }
  |
  v
Syntax highlight each line via syntect (keyed by filename extension)
  |
  v
Render to ANSI escape sequences
```

### 2.3 Core Types

```rust
/// A single diff for one file.
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: String,
    pub hunks: Vec<DiffHunk>,
    pub stats: DiffStats,
    /// Whether this is a new file, deleted file, or modification.
    pub kind: DiffKind,
}

pub enum DiffKind {
    Added,
    Deleted,
    Modified,
    Renamed { old_path: String },
}

pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
}

/// A contiguous group of changes with surrounding context.
pub struct DiffHunk {
    pub header: String,           // e.g. "@@ -10,7 +10,8 @@ fn main()"
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

/// A single line within a hunk.
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    /// Segments with styling (for word-level emphasis).
    pub segments: Vec<DiffSegment>,
    /// Comment attached to this line, if any.
    pub comment: Option<InlineComment>,
}

pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

/// A styled segment within a line (word-level diff or syntax highlighting).
pub struct DiffSegment {
    pub text: String,
    pub style: SegmentStyle,
}

pub struct SegmentStyle {
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub dim: bool,
}
```

---

## 3. Diff Viewer Component

### 3.1 Rendering in the Chat Scroll Area

Diffs render as a special block type within the chat area (inside the ANSI scroll region). This integrates naturally with the existing `BlockManager` and `ScreenState` architecture.

```
Chat scroll area:
  ...
  Elwood: I've updated the function to handle errors.

  ╭─ Diff: src/main.rs (+5 -3) ───────────────────────────╮
  │      fn process_data(input: &str) -> Result<Data> {    │  context
  │  -       let raw = parse(input);                       │  deletion (red bg)
  │  +       let raw = parse(input)?;                      │  addition (green bg)
  │  +       if raw.is_empty() {                           │  addition
  │  +           return Err(Error::EmptyInput);            │  addition
  │  +       }                                             │  addition
  │      let processed = transform(raw);                   │  context
  │  -       Ok(processed)                                 │  deletion
  │  +       Ok(processed.validate()?)                     │  addition
  │      }                                                 │  context
  ├────────────────────────────────────────────────────────┤
  │  [y] approve  [n] reject  [c] comment  [j/k] nav      │  action bar
  ╰────────────────────────────────────────────────────────╯
  ...
```

### 3.2 Color Scheme (TokyoNight)

Consistent with the existing palette in `screen.rs`:

| Element | Foreground | Background |
|---------|-----------|-----------|
| Context line | FG (192,202,245) | BG (26,27,38) |
| Addition | SUCCESS (158,206,106) | Dark green (30,50,30) |
| Deletion | ERROR (247,118,142) | Dark red (50,30,30) |
| Word-level add emphasis | Bold SUCCESS | Brighter green (40,70,40) |
| Word-level del emphasis | Bold ERROR | Brighter red (70,40,40) |
| Line numbers | MUTED (86,95,137) | BG |
| Hunk header | ACCENT (122,162,247) | BG |
| File header | BOLD ACCENT | BG |
| Diff border | BORDER (59,66,97) | BG |

```rust
// Diff-specific palette additions for screen.rs
const DIFF_ADD_BG: (u8, u8, u8) = (30, 50, 30);
const DIFF_DEL_BG: (u8, u8, u8) = (50, 30, 30);
const DIFF_ADD_EMPHASIS_BG: (u8, u8, u8) = (40, 70, 40);
const DIFF_DEL_EMPHASIS_BG: (u8, u8, u8) = (70, 40, 40);
```

### 3.3 Layout: Line Number Gutter

Each diff line shows dual line numbers (old and new):

```
  10   10 │     fn process_data(input: &str) -> Result<Data> {
  11      │ -       let raw = parse(input);
       11 │ +       let raw = parse(input)?;
       12 │ +       if raw.is_empty() {
       13 │ +           return Err(Error::EmptyInput);
       14 │ +       }
  12   15 │     let processed = transform(raw);
```

Gutter width: 5 chars for old + 5 chars for new + 3 chars for marker (" - ", " + ", "   ") = 13 chars fixed gutter.

### 3.4 Navigation

| Key | Action |
|-----|--------|
| `j` / `Down` | Move to next line within diff |
| `k` / `Up` | Move to previous line within diff |
| `n` | Jump to next hunk |
| `N` | Jump to previous hunk |
| `]` | Jump to next file (multi-file diff) |
| `[` | Jump to previous file |
| `Space` | Toggle hunk collapse/expand |
| `q` / `Esc` | Exit diff viewer (back to chat) |

Navigation state lives in a `DiffViewerState` struct that tracks the cursor position within the diff block.

### 3.5 Hunk Collapse/Expand

Large hunks (>20 lines) are collapsed by default, showing only the hunk header:

```
  ╭─ @@ -45,30 +45,35 @@ fn transform() ─── [25 lines, Space to expand] ─╮
```

When expanded, all lines are shown. The `DiffHunk` struct tracks `collapsed: bool`.

### 3.6 DiffViewerState

```rust
/// Tracks navigation state within an active diff view.
pub struct DiffViewerState {
    /// The diff being viewed.
    pub diff: FileDiff,
    /// Current cursor line index (within the flattened list of all diff lines).
    pub cursor_line: usize,
    /// Current hunk index.
    pub current_hunk: usize,
    /// Whether the viewer is in comment-input mode.
    pub commenting: bool,
    /// Lines selected for commenting (range start..end).
    pub selection: Option<(usize, usize)>,
    /// Collected comments (not yet submitted).
    pub pending_comments: Vec<InlineComment>,
}
```

---

## 4. Inline Comment System

### 4.1 Comment Model

```rust
/// A comment attached to a specific location in a diff.
pub struct InlineComment {
    /// Which file this comment is on.
    pub file_path: String,
    /// Line number in the new file (for additions/context) or old file (for deletions).
    pub line_number: usize,
    /// Whether this refers to old or new side.
    pub side: CommentSide,
    /// The comment text.
    pub body: String,
    /// Resolution status.
    pub resolved: bool,
}

pub enum CommentSide {
    Old,
    New,
}
```

### 4.2 Comment Input Flow

1. **Select a line**: User navigates to a diff line and presses `c`
2. **Inline editor opens**: A single-line input area appears immediately below the selected line, using the same `InputEditor` component (reused from the main input box)
3. **Type comment**: User types their feedback
4. **Submit or cancel**: Enter submits (adding to `pending_comments`), Esc cancels
5. **Visual indicator**: Commented lines get a small marker in the gutter: `[*]`

```
  11      │ -       let raw = parse(input);
       11 │ +       let raw = parse(input)?;
  [*]     │    Comment: "This should also validate the input format before parsing"
       12 │ +       if raw.is_empty() {
```

### 4.3 Batch Submission

When the user presses `Enter` (from the action bar, not within a comment), all pending comments are batched and sent to the agent:

```rust
AgentRequest::ReviewFeedback {
    file_path: String,
    comments: Vec<InlineComment>,
    action: ReviewAction,
}

pub enum ReviewAction {
    /// Approve all changes in this diff.
    Approve,
    /// Reject all changes (revert).
    Reject,
    /// Request revisions based on the attached comments.
    RequestChanges,
}
```

The agent receives the batch, applies the feedback, and returns an updated diff for re-review.

### 4.4 Agent Resolution Flow

```
1. Agent proposes edit  -->  DiffBlock rendered in chat
2. User reviews diff:
   a. Press [y] -> Approve -> changes applied to disk
   b. Press [n] -> Reject -> changes discarded
   c. Press [c] -> Enter comment mode
      - Add comments to multiple lines
      - Press [Enter] to submit all comments
3. Agent receives comments -> makes revisions -> new DiffBlock
4. User reviews again (goto step 2)
```

---

## 5. Agent Code Review Flow

### 5.1 Protocol Extensions

New `AgentResponse` variants:

```rust
pub enum AgentResponse {
    // ... existing variants ...

    /// Agent proposes a file edit, shown as a diff.
    FileEdit {
        file_path: String,
        old_content: String,
        new_content: String,
        description: String,
    },

    /// Agent proposes multiple file edits as a batch.
    BatchFileEdit {
        edits: Vec<FileEditProposal>,
        description: String,
    },
}

pub struct FileEditProposal {
    pub file_path: String,
    pub old_content: String,
    pub new_content: String,
}
```

New `AgentRequest` variants:

```rust
pub enum AgentRequest {
    // ... existing variants ...

    /// User reviewed a proposed edit.
    ReviewFeedback {
        file_path: String,
        comments: Vec<InlineComment>,
        action: ReviewAction,
    },

    /// User approved a batch of edits.
    BatchApproval {
        approved_files: Vec<String>,
        rejected_files: Vec<String>,
        comments: Vec<InlineComment>,
    },
}
```

### 5.2 Integration with Existing Block Model

The diff viewer creates a new block type in `BlockManager`:

```rust
pub enum BlockKind {
    /// Standard agent response or command output block.
    Standard,
    /// Diff review block (interactive, has action bar).
    DiffReview {
        state: DiffViewerState,
    },
}
```

The `Block` struct gets an optional `kind` field. When rendering, the block manager checks the kind and delegates to the diff renderer instead of the standard output renderer.

### 5.3 Write Path: Agent Edit -> Diff Block

When the agent produces a `FileEdit` response:

1. `pane.rs` receives `AgentResponse::FileEdit { old_content, new_content, ... }`
2. Computes `FileDiff` using `similar::TextDiff::from_lines()`
3. Creates a `DiffViewerState` and a new `Block` with `BlockKind::DiffReview`
4. Renders the diff block via `screen::render_diff_block()`
5. The diff block includes the action bar (`[y] approve [n] reject [c] comment`)
6. Keyboard focus shifts to the diff block (handled in `key_down()`)

### 5.4 Read Path: User Review -> Agent Feedback

When the user interacts with a diff block:

1. `pane.rs::key_down()` detects that the current block is a `DiffReview`
2. Delegates key handling to `DiffViewerState::handle_key()`
3. On approve: writes `new_content` to disk, sends `ReviewFeedback { action: Approve }`
4. On reject: sends `ReviewFeedback { action: Reject }`
5. On comment submit: sends `ReviewFeedback { action: RequestChanges, comments }`
6. The diff block transitions to a resolved state (grayed out, non-interactive)

---

## 6. Syntax Highlighting

### 6.1 Library: `syntect`

Syntect is the standard Rust library for syntax highlighting using Sublime Text syntax definitions. It powers both `bat` and `delta`.

**Integration approach**: Lazy-loaded, single instance shared across all diff renders.

```rust
use syntect::parsing::SyntaxSet;
use syntect::highlighting::{ThemeSet, Theme};
use std::sync::LazyLock;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(|| {
    SyntaxSet::load_defaults_newlines()
});

static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let ts = ThemeSet::load_defaults();
    // Use a dark theme compatible with TokyoNight
    ts.themes["base16-ocean.dark"].clone()
});
```

### 6.2 Highlighting Pipeline

```
DiffLine.text
  |
  v
SyntaxSet::find_syntax_for_file(filename)
  |
  v
HighlightLines::highlight_line(text, syntax_set)
  |
  v
Vec<(Style, &str)>  -- syntect output
  |
  v
Convert to ANSI: style.foreground -> \x1b[38;2;R;G;Bm
  |
  v
Merge with diff emphasis (word-level bg colors)
  |
  v
Final ANSI string for the line
```

### 6.3 Merge Strategy: Diff Colors + Syntax Colors

The diff background color (green/red) is always applied. Syntax highlighting provides foreground color only. Word-level emphasis overrides both foreground and background for the specific tokens that changed.

Layer priority (highest wins):
1. Word-level diff emphasis (fg + bg)
2. Diff line background (green/red/none)
3. Syntax highlighting foreground
4. Default foreground (FG from palette)

### 6.4 Feature Flag

Syntax highlighting adds binary size (~2MB for embedded grammars). Gate behind a Cargo feature:

```toml
[features]
default = ["syntax-highlight"]
syntax-highlight = ["syntect"]

[dependencies]
syntect = { version = "5", optional = true, default-features = false, features = ["default-syntaxes", "default-themes", "regex-onig"] }
```

Without the feature, diffs render with diff colors but no syntax highlighting.

---

## 7. Git Integration

### 7.1 Library: Shell Out to `git`

**Decision**: Shell out to `git` rather than using `git2` or `gix`.

Rationale:
- WezTerm already has no `git2` dependency; adding it pulls in libgit2 C library
- Our existing tool architecture (BashTool) already shells out for git
- `git diff` output is well-defined and easy to parse
- Performance is adequate (diffs are small; user-initiated, not in hot path)
- Avoids dependency on `git2`'s C bindings or `gix`'s large crate graph

### 7.2 Commands

#### `/diff` -- Show Unstaged Changes

```
/diff                   Show all unstaged changes
/diff --staged          Show staged changes
/diff <file>            Show changes for a specific file
/diff HEAD~3..HEAD      Show changes in a commit range
```

Implementation:
1. Run `git diff` (or `git diff --staged`) via `std::process::Command`
2. Parse the unified diff output into `Vec<FileDiff>`
3. Render each `FileDiff` as a diff block in the chat area

#### `/pr-review` -- Review Pull Request

```
/pr-review              Review current branch's PR
/pr-review <number>     Review PR #number
```

Implementation:
1. Run `gh pr diff <number>` (requires `gh` CLI) or `git diff main...HEAD`
2. Optionally fetch PR comments via `gh api repos/{owner}/{repo}/pulls/{number}/comments`
3. Render the diff with existing comments shown inline

### 7.3 Unified Diff Parser

Parse standard unified diff format output from `git diff`:

```rust
pub fn parse_unified_diff(input: &str) -> Vec<FileDiff> {
    // Split on "diff --git a/... b/..."
    // Parse each file section:
    //   - "--- a/file" / "+++ b/file" for paths
    //   - "@@ -old_start,count +new_start,count @@" for hunk headers
    //   - " " context, "-" deletion, "+" addition lines
    // Return structured FileDiff
}
```

This parser handles the `git diff` output format directly, so we don't need to read file contents and compute diffs ourselves for git integration. The `similar`-based engine is used only for agent-proposed edits where we have old/new content.

---

## 8. Rendering Functions

### 8.1 New Functions in `screen.rs`

```rust
/// Render a file diff block in the chat scroll area.
pub fn render_diff_block(diff: &FileDiff, width: u16, state: &DiffViewerState) -> String {
    // 1. File header: "Diff: src/main.rs (+5 -3)"
    // 2. For each hunk:
    //    a. Hunk header: "@@ -10,7 +10,8 @@ fn main()"
    //    b. If collapsed: "[N lines, Space to expand]"
    //    c. If expanded: render each DiffLine with gutter
    // 3. Action bar: "[y] approve [n] reject [c] comment [j/k] nav"
    // 4. Box border with BORDER color
}

/// Render a single diff line with line numbers and color.
fn render_diff_line(line: &DiffLine, width: u16, is_cursor: bool) -> String {
    // 1. Old line number (5 chars, right-aligned, MUTED)
    // 2. New line number (5 chars, right-aligned, MUTED)
    // 3. Marker: " + " / " - " / "   "
    // 4. Content with syntax highlighting + diff emphasis
    // 5. If cursor line: SELECTION background
    // 6. If has comment: "[*]" marker
}

/// Render the action bar at the bottom of a diff block.
fn render_diff_action_bar(state: &DiffViewerState, width: u16) -> String {
    // Key hints: [y] approve  [n] reject  [c] comment  [q] close
    // Comment count if any pending: "(3 comments)"
}

/// Render an inline comment below a diff line.
fn render_inline_comment(comment: &InlineComment, width: u16) -> String {
    // Indented comment with ACCENT border
    // "[*] Comment: {body}"
}
```

### 8.2 ANSI Generation Pattern

Follow the existing pattern in `screen.rs`:

```rust
fn render_diff_line(line: &DiffLine, width: u16, is_cursor: bool) -> String {
    let mut out = String::with_capacity(256);

    // Background based on line kind
    let line_bg = match line.kind {
        DiffLineKind::Addition => bgc(DIFF_ADD_BG),
        DiffLineKind::Deletion => bgc(DIFF_DEL_BG),
        DiffLineKind::Context => String::new(),
    };
    let cursor_bg = if is_cursor { bgc(SELECTION) } else { String::new() };

    // Line numbers
    let old_num = line.old_lineno
        .map(|n| format!("{:>4} ", n))
        .unwrap_or_else(|| "     ".to_string());
    let new_num = line.new_lineno
        .map(|n| format!("{:>4} ", n))
        .unwrap_or_else(|| "     ".to_string());

    // Marker
    let marker = match line.kind {
        DiffLineKind::Addition => format!("{}+{}", fgc(SUCCESS), RESET),
        DiffLineKind::Deletion => format!("{}-{}", fgc(ERROR), RESET),
        DiffLineKind::Context => " ".to_string(),
    };

    out.push_str(&format!(
        "{cursor_bg}{line_bg}{muted}{old_num}{new_num}{RESET}{marker} ",
        muted = fgc(MUTED),
    ));

    // Content with segments (syntax + diff emphasis)
    for seg in &line.segments {
        if let Some(bg) = seg.style.bg {
            out.push_str(&bgc(bg));
        }
        if let Some(fg) = seg.style.fg {
            out.push_str(&fgc(fg));
        }
        if seg.style.bold {
            out.push_str(BOLD);
        }
        out.push_str(&seg.text);
        out.push_str(RESET);
        out.push_str(&line_bg); // restore line bg
    }

    out.push_str(&format!("{RESET}{CLEAR_EOL}"));
    out
}
```

---

## 9. Implementation Plan

### 9.1 Files to Create

| File | Purpose |
|------|---------|
| `src/diff.rs` | `FileDiff`, `DiffHunk`, `DiffLine` types, `similar`-based diff engine, unified diff parser |
| `src/diff_viewer.rs` | `DiffViewerState`, keyboard handling, comment collection, rendering coordination |
| `src/highlight.rs` | Syntect integration, ANSI color conversion, lazy-loaded syntax set (feature-gated) |

### 9.2 Files to Modify

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `similar`, `syntect` (optional) dependencies |
| `src/lib.rs` | Add `mod diff; mod diff_viewer; mod highlight;` |
| `src/runtime.rs` | Add `FileEdit`, `BatchFileEdit` to `AgentResponse`; add `ReviewFeedback`, `BatchApproval` to `AgentRequest` |
| `src/block.rs` | Add `BlockKind` enum, optional `kind` field to `Block` |
| `src/screen.rs` | Add `render_diff_block()`, `render_diff_line()`, `render_diff_action_bar()`, `render_inline_comment()`, diff palette constants |
| `src/pane.rs` | Handle `FileEdit` response, delegate key events to diff viewer when active, implement `/diff` command routing |
| `src/commands.rs` | Add `/diff` and `/pr-review` slash commands |

### 9.3 New Protocol Messages

```rust
// AgentResponse additions
FileEdit { file_path, old_content, new_content, description }
BatchFileEdit { edits: Vec<FileEditProposal>, description }

// AgentRequest additions
ReviewFeedback { file_path, comments: Vec<InlineComment>, action: ReviewAction }
BatchApproval { approved_files, rejected_files, comments }
```

### 9.4 New Dependencies

```toml
# Cargo.toml additions
similar = { version = "2", features = ["inline", "unicode"] }
syntect = { version = "5", optional = true, default-features = false, features = ["default-syntaxes", "default-themes", "regex-onig"] }

[features]
default = ["syntax-highlight"]
syntax-highlight = ["syntect"]
```

### 9.5 Integration with Block Model

The `Block` struct in `block.rs` gains:

```rust
pub struct Block {
    // ... existing fields ...

    /// Block kind (standard output vs interactive diff review).
    pub kind: BlockKind,
}

pub enum BlockKind {
    Standard,
    DiffReview {
        /// Viewer state for interactive diff navigation and commenting.
        state: DiffViewerState,
    },
}
```

When the pane detects a `FileEdit` response, it:
1. Computes the diff
2. Creates a `DiffViewerState`
3. Pushes a new `Block` with `BlockKind::DiffReview`
4. Renders the block via `screen::render_diff_block()`
5. Sets a pane-level `active_diff_block: Option<BlockId>` to route key events

### 9.6 Test Strategy

**Unit tests** (`diff.rs`):
- Diff computation: empty files, identical files, single-line change, multi-hunk changes
- Word-level emphasis: inserted word, deleted word, changed word
- Unified diff parser: standard git diff output, binary files, renames, new files
- Line number computation: context lines, additions, deletions

**Unit tests** (`diff_viewer.rs`):
- Navigation: cursor movement, hunk jumping, file jumping
- Comment lifecycle: add, edit, remove, batch submit
- State transitions: idle -> reviewing -> commenting -> submitted
- Collapse/expand hunks

**Unit tests** (`highlight.rs`):
- Syntax detection by filename
- ANSI color generation
- Merge of syntax + diff colors

**Integration tests** (`pane.rs`):
- `FileEdit` response renders a diff block
- Key events during diff review route correctly
- Approve writes file to disk
- Reject discards changes

**Rendering tests** (`screen.rs`):
- Diff block contains expected ANSI sequences
- Line numbers are correctly aligned
- Colors match the palette spec
- Action bar renders with correct key hints

### 9.7 Phased Delivery

**Phase 1: Core diff rendering** (Priority: HIGH)
- `diff.rs` with `similar` integration
- `screen.rs` diff rendering functions
- Wire into `pane.rs` as a new response type
- Basic approve/reject flow

**Phase 2: Inline comments** (Priority: MEDIUM)
- Comment input UI
- Batch comment submission
- Agent resolution flow
- Comment display in diff

**Phase 3: Git integration** (Priority: MEDIUM)
- `/diff` and `/diff --staged` commands
- Unified diff parser for `git diff` output
- `/pr-review` with `gh` CLI integration

**Phase 4: Syntax highlighting** (Priority: LOW)
- Syntect integration behind feature flag
- Color merge with diff emphasis
- Language detection by filename

---

## 10. Open Questions

1. **Side-by-side vs unified**: Should we support both rendering modes? Start with unified (simpler, works in narrower terminals), add side-by-side later via `/diff --side-by-side`.

2. **Multi-file review**: When the agent edits multiple files, should we show all diffs in sequence or provide a file picker? Start with sequential, add file list navigation in Phase 2.

3. **Edit-in-diff**: Should users be able to edit diff lines directly (like Warp)? Deferred to a future phase -- the comment-based workflow is sufficient for v1.

4. **Hunk-level approve/reject**: Should users approve/reject individual hunks within a file? Useful for large diffs. Could implement as Phase 2 extension with `h` key to toggle hunk selection.

5. **Clipboard integration**: Copying selected lines from a diff requires WezTerm clipboard API access. The existing `copy_current_block_output()` pattern in `pane.rs` can be extended.

---

## Appendix A: Research Sources

- [Warp Interactive Code Review](https://docs.warp.dev/code/code-review)
- [Warp Agents 3.0 — Full Terminal Use, Plan, Code Review Integration](https://www.warp.dev/blog/agents-3-full-terminal-use-plan-code-review-integration)
- [Warp 2025 In Review](https://www.warp.dev/blog/2025-in-review)
- [TechCrunch: Warp diff-tracking tools](https://techcrunch.com/2025/09/03/warp-brings-new-diff-tracking-tools-to-the-ai-coding-arms-race/)
- [similar crate (GitHub)](https://github.com/mitsuhiko/similar)
- [similar crate (docs.rs)](https://docs.rs/similar)
- [delta (GitHub)](https://github.com/dandavison/delta)
- [delta ARCHITECTURE.md](https://github.com/dandavison/delta/blob/main/ARCHITECTURE.md)
- [syntect (GitHub)](https://github.com/trishume/syntect)
- [git2 crate](https://crates.io/crates/git2)
- [gitoxide](https://github.com/GitoxideLabs/gitoxide)
