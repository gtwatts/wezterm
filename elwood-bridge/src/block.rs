//! Block model for the Elwood agent pane.
//!
//! A "block" is a Warp-style logical unit grouping a shell prompt, user input,
//! and the resulting output. Blocks are built on top of WezTerm's existing
//! OSC 133 semantic zone infrastructure (`SemanticType::Prompt/Input/Output`).
//!
//! ## Block structure
//!
//! ```text
//! ┌─ Block 0 ──────────────────────────────────────┐
//! │  $ ls -la            ← Prompt + Input zone      │
//! │  total 42            ← Output zone              │
//! │  drwxr-xr-x  5 ...                              │
//! └────────────────────────── exit 0 · 0.3s ────────┘
//! ```
//!
//! ## In agent mode (ElwoodPane)
//!
//! The virtual terminal does not get real shell OSC 133 markers.  Instead,
//! `BlockManager::push_agent_block()` and `BlockManager::push_output_block()`
//! are called explicitly as agent events arrive, giving us synthetic blocks
//! without OSC 133 parsing.

use std::time::Instant;
use wezterm_term::StableRowIndex;


/// Opaque identifier for a block.
pub type BlockId = u64;

/// A contiguous row range within the terminal scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoneRange {
    pub start_y: StableRowIndex,
    pub end_y: StableRowIndex,
}

impl ZoneRange {
    /// Returns `true` if `row` falls within this zone (inclusive both ends).
    pub fn contains(&self, row: StableRowIndex) -> bool {
        row >= self.start_y && row <= self.end_y
    }
}

/// A single logical block: prompt + input + output.
///
/// Any of the three zone fields may be absent (e.g. an output-only block
/// produced by the agent before the shell emits a prompt marker).
#[derive(Debug, Clone)]
pub struct Block {
    pub id: BlockId,

    /// Rows that contain the shell prompt chrome (OSC 133;A / 133;B).
    pub prompt_zone: Option<ZoneRange>,

    /// Rows that contain user input (OSC 133;B–133;C or 133;I).
    pub input_zone: Option<ZoneRange>,

    /// Rows that contain command output (OSC 133;C–133;D).
    pub output_zone: Option<ZoneRange>,

    /// Exit code from OSC 133;D (if received).
    pub exit_code: Option<i32>,

    /// Wall-clock time when the block started (roughly when input was submitted).
    pub start_time: Option<Instant>,

    /// Wall-clock time when the block ended (OSC 133;D or TurnComplete).
    pub end_time: Option<Instant>,

    /// Whether the output zone is collapsed (hidden in the view).
    pub collapsed: bool,

    /// Whether the user has bookmarked this block.
    pub bookmarked: bool,
}

impl Block {
    /// The first row of this block (whichever zone is earliest).
    pub fn first_row(&self) -> Option<StableRowIndex> {
        let candidates = [
            self.prompt_zone.map(|z| z.start_y),
            self.input_zone.map(|z| z.start_y),
            self.output_zone.map(|z| z.start_y),
        ];
        candidates.iter().filter_map(|r| *r).min()
    }

    /// The last row of this block (whichever zone is latest).
    pub fn last_row(&self) -> Option<StableRowIndex> {
        let candidates = [
            self.prompt_zone.map(|z| z.end_y),
            self.input_zone.map(|z| z.end_y),
            self.output_zone.map(|z| z.end_y),
        ];
        candidates.iter().filter_map(|r| *r).max()
    }

    /// Returns `true` if `row` falls within any zone of this block.
    pub fn contains_row(&self, row: StableRowIndex) -> bool {
        self.prompt_zone.map_or(false, |z| z.contains(row))
            || self.input_zone.map_or(false, |z| z.contains(row))
            || self.output_zone.map_or(false, |z| z.contains(row))
    }

    /// Duration of this block in seconds, or `None` if timing is unavailable.
    pub fn duration_secs(&self) -> Option<f64> {
        match (self.start_time, self.end_time) {
            (Some(start), Some(end)) => Some(end.duration_since(start).as_secs_f64()),
            (Some(start), None) => Some(start.elapsed().as_secs_f64()),
            _ => None,
        }
    }
}

/// Manages the list of blocks for a pane.
///
/// Two distinct workflows are supported:
///
/// 1. **Shell panes** — call `sync_from_zones()` with the zones returned by
///    `Pane::get_semantic_zones()`.  The manager groups consecutive
///    Prompt→Input→Output zones into blocks automatically.
///
/// 2. **Agent pane (ElwoodPane)** — call `push_agent_block()` / `finish_block()`
///    explicitly as agent events arrive, since there is no shell emitting OSC 133.
#[derive(Debug, Default)]
pub struct BlockManager {
    blocks: Vec<Block>,
    next_id: BlockId,
    /// Selected block index for navigation.
    selected: Option<usize>,
}

impl BlockManager {
    /// Create an empty `BlockManager`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current list of blocks (oldest first).
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Number of blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns `true` if there are no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    // ── Shell workflow ──────────────────────────────────────────────────────

    /// Rebuild the block list from the semantic zones returned by
    /// `Pane::get_semantic_zones()`.
    ///
    /// Zones are expected to arrive in terminal order (top-to-bottom).
    /// The algorithm groups consecutive runs of Prompt/Input/Output zones
    /// into logical blocks.  An Output zone that follows an Input zone is
    /// part of the same block.  A new Prompt zone always starts a new block.
    ///
    /// Existing collapsed/bookmarked state is preserved by matching on the
    /// first row of each block.
    pub fn sync_from_zones(&mut self, zones: &[wezterm_term::SemanticZone]) {
        use wezterm_term::SemanticType;

        // Build a map of preserved state keyed by block's first row
        let preserved: std::collections::HashMap<StableRowIndex, (bool, bool)> = self
            .blocks
            .iter()
            .filter_map(|b| b.first_row().map(|r| (r, (b.collapsed, b.bookmarked))))
            .collect();

        let mut new_blocks: Vec<Block> = Vec::new();

        for zone in zones {
            let range = ZoneRange {
                start_y: zone.start_y,
                end_y: zone.end_y,
            };

            match zone.semantic_type {
                SemanticType::Prompt => {
                    // Always starts a new block
                    let id = self.next_id();
                    new_blocks.push(Block {
                        id,
                        prompt_zone: Some(range),
                        input_zone: None,
                        output_zone: None,
                        exit_code: None,
                        start_time: None,
                        end_time: None,
                        collapsed: false,
                        bookmarked: false,
                    });
                }
                SemanticType::Input => {
                    if let Some(last) = new_blocks.last_mut() {
                        if last.input_zone.is_none() && last.output_zone.is_none() {
                            // Attach to current block
                            last.input_zone = Some(range);
                            continue;
                        }
                    }
                    // No suitable block — create a new one
                    let id = self.next_id();
                    new_blocks.push(Block {
                        id,
                        prompt_zone: None,
                        input_zone: Some(range),
                        output_zone: None,
                        exit_code: None,
                        start_time: None,
                        end_time: None,
                        collapsed: false,
                        bookmarked: false,
                    });
                }
                SemanticType::Output => {
                    if let Some(last) = new_blocks.last_mut() {
                        if last.output_zone.is_none() {
                            last.output_zone = Some(range);
                            continue;
                        }
                        // Extend existing output zone if contiguous
                        if let Some(ref mut oz) = last.output_zone {
                            if zone.start_y == oz.end_y + 1 {
                                oz.end_y = zone.end_y;
                                continue;
                            }
                        }
                    }
                    // Orphan output zone — wrap in its own block
                    let id = self.next_id();
                    new_blocks.push(Block {
                        id,
                        prompt_zone: None,
                        input_zone: None,
                        output_zone: Some(range),
                        exit_code: None,
                        start_time: None,
                        end_time: None,
                        collapsed: false,
                        bookmarked: false,
                    });
                }
            }
        }

        // Restore preserved state
        for block in &mut new_blocks {
            if let Some(first_row) = block.first_row() {
                if let Some(&(collapsed, bookmarked)) = preserved.get(&first_row) {
                    block.collapsed = collapsed;
                    block.bookmarked = bookmarked;
                }
            }
        }

        self.blocks = new_blocks;
    }

    // ── Agent workflow ──────────────────────────────────────────────────────

    /// Begin a new agent block starting at `start_row`.
    ///
    /// Used when the ElwoodPane writes a user prompt + response pair.
    /// Returns the new block's id.
    pub fn push_agent_block(&mut self, start_row: StableRowIndex) -> BlockId {
        let id = self.next_id();
        self.blocks.push(Block {
            id,
            prompt_zone: Some(ZoneRange {
                start_y: start_row,
                end_y: start_row,
            }),
            input_zone: None,
            output_zone: None,
            exit_code: None,
            start_time: Some(Instant::now()),
            end_time: None,
            collapsed: false,
            bookmarked: false,
        });
        id
    }

    /// Extend the output zone of the most recent block to `end_row`.
    ///
    /// If there is no current block, this is a no-op.
    pub fn extend_output(&mut self, end_row: StableRowIndex) {
        if let Some(last) = self.blocks.last_mut() {
            match last.output_zone {
                None => {
                    last.output_zone = Some(ZoneRange {
                        start_y: end_row,
                        end_y: end_row,
                    });
                }
                Some(ref mut oz) => {
                    oz.end_y = end_row;
                }
            }
        }
    }

    /// Mark the most recent block as finished with the given exit code.
    pub fn finish_block(&mut self, exit_code: Option<i32>) {
        if let Some(last) = self.blocks.last_mut() {
            last.exit_code = exit_code;
            last.end_time = Some(Instant::now());
        }
    }

    // ── Query ───────────────────────────────────────────────────────────────

    /// Returns the block that contains `row`, if any.
    pub fn get_block_at_row(&self, row: StableRowIndex) -> Option<&Block> {
        self.blocks.iter().find(|b| b.contains_row(row))
    }

    /// Returns the block with the given `id`, if it exists.
    pub fn get_block_by_id(&self, id: BlockId) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// Returns the block with the given `id` (mutable), if it exists.
    pub fn get_block_by_id_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|b| b.id == id)
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    /// Jump to the start of the previous block relative to `current_row`.
    ///
    /// If `current_row` is inside (or at the start of) block N, this returns
    /// the first row of block N-1.  This matches Warp-style "go to previous
    /// block" semantics: pressing the keybinding again keeps going backward.
    ///
    /// Returns `None` if there is no earlier block.
    pub fn navigate_prev(&self, current_row: StableRowIndex) -> Option<StableRowIndex> {
        // Find the block that contains current_row (or whose first_row equals it)
        let current_block_first = self
            .blocks
            .iter()
            .filter(|b| b.contains_row(current_row) || b.first_row() == Some(current_row))
            .filter_map(|b| b.first_row())
            .min();

        // The threshold: look for blocks that start strictly before the current block's start
        let threshold = current_block_first.unwrap_or(current_row);

        // Return the latest block that starts strictly before the threshold
        self.blocks
            .iter()
            .rev()
            .filter_map(|b| b.first_row())
            .find(|&r| r < threshold)
    }

    /// Jump to the start of the next block relative to `current_row`.
    ///
    /// Returns the first row of the next block that starts strictly after
    /// `current_row`, or `None` if there is none.
    pub fn navigate_next(&self, current_row: StableRowIndex) -> Option<StableRowIndex> {
        // Find the last row of the block containing current_row, so we skip past
        // the current block even if cursor is at the very first row of a block.
        let current_block_last = self
            .blocks
            .iter()
            .filter(|b| b.contains_row(current_row) || b.first_row() == Some(current_row))
            .filter_map(|b| b.last_row())
            .max();

        let threshold = current_block_last.unwrap_or(current_row);

        // Return the earliest block that starts strictly after the threshold
        self.blocks
            .iter()
            .filter_map(|b| b.first_row())
            .find(|&r| r > threshold)
    }

    // ── Mutation ────────────────────────────────────────────────────────────

    /// Toggle the collapsed state of the block with the given id.
    ///
    /// Returns the new collapsed state, or `None` if the id was not found.
    pub fn toggle_collapse(&mut self, block_id: BlockId) -> Option<bool> {
        let block = self.get_block_by_id_mut(block_id)?;
        block.collapsed = !block.collapsed;
        Some(block.collapsed)
    }

    /// Toggle the bookmarked state of the block with the given id.
    ///
    /// Returns the new bookmarked state, or `None` if the id was not found.
    pub fn toggle_bookmark(&mut self, block_id: BlockId) -> Option<bool> {
        let block = self.get_block_by_id_mut(block_id)?;
        block.bookmarked = !block.bookmarked;
        Some(block.bookmarked)
    }

    /// Return the ids of all bookmarked blocks (in order).
    pub fn bookmarked_blocks(&self) -> Vec<BlockId> {
        self.blocks
            .iter()
            .filter(|b| b.bookmarked)
            .map(|b| b.id)
            .collect()
    }

    /// Return `(index, &Block)` pairs for all bookmarked blocks (in order).
    pub fn bookmarked_blocks_with_index(&self) -> Vec<(usize, &Block)> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.bookmarked)
            .collect()
    }

    // ── Selected block ──────────────────────────────────────────────────────

    /// The index of the currently selected block (for keyboard navigation).
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Get a reference to the currently selected block.
    pub fn selected_block(&self) -> Option<&Block> {
        self.selected.and_then(|i| self.blocks.get(i))
    }

    /// Select the block at `index`.  Clamps to valid range.
    pub fn select(&mut self, index: usize) {
        if !self.blocks.is_empty() {
            self.selected = Some(index.min(self.blocks.len() - 1));
        }
    }

    /// Deselect the current block.
    pub fn deselect(&mut self) {
        self.selected = None;
    }

    /// Move selection to the next block.  If nothing is selected, selects
    /// the first block.  Wraps at the end.
    pub fn navigate_next_selected(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        match self.selected {
            None => self.selected = Some(0),
            Some(i) if i + 1 < self.blocks.len() => self.selected = Some(i + 1),
            Some(_) => self.selected = Some(self.blocks.len() - 1),
        }
    }

    /// Move selection to the previous block.  If nothing is selected, selects
    /// the last block.  Wraps at the beginning.
    pub fn navigate_prev_selected(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        match self.selected {
            None => self.selected = Some(self.blocks.len() - 1),
            Some(0) => self.selected = Some(0),
            Some(i) => self.selected = Some(i - 1),
        }
    }

    /// Toggle collapsed state of the block at `index`.
    ///
    /// Returns the new collapsed state, or `None` if index is out of range.
    pub fn toggle_collapse_at(&mut self, index: usize) -> Option<bool> {
        let block = self.blocks.get_mut(index)?;
        block.collapsed = !block.collapsed;
        Some(block.collapsed)
    }

    /// Toggle bookmarked state of the block at `index`.
    ///
    /// Returns the new bookmarked state, or `None` if index is out of range.
    pub fn toggle_bookmark_at(&mut self, index: usize) -> Option<bool> {
        let block = self.blocks.get_mut(index)?;
        block.bookmarked = !block.bookmarked;
        Some(block.bookmarked)
    }

    /// Export a single block as a markdown string.
    ///
    /// Includes block metadata (exit code, duration) and placeholder text
    /// for command/output (since the actual terminal text is not stored here).
    pub fn export_block_markdown(&self, index: usize) -> Option<String> {
        let block = self.blocks.get(index)?;
        let mut md = String::new();
        md.push_str(&format!("## Block {}\n\n", block.id));

        if let Some(exit) = block.exit_code {
            md.push_str(&format!("**Exit code:** {exit}\n"));
        }
        if let Some(dur) = block.duration_secs() {
            md.push_str(&format!("**Duration:** {dur:.1}s\n"));
        }
        if block.bookmarked {
            md.push_str("**Bookmarked:** yes\n");
        }
        md.push('\n');

        // Row ranges (for cross-referencing with terminal content)
        if let Some(z) = block.prompt_zone {
            md.push_str(&format!("Prompt rows: {}..{}\n", z.start_y, z.end_y));
        }
        if let Some(z) = block.input_zone {
            md.push_str(&format!("Input rows: {}..{}\n", z.start_y, z.end_y));
        }
        if let Some(z) = block.output_zone {
            md.push_str(&format!("Output rows: {}..{}\n", z.start_y, z.end_y));
        }

        Some(md)
    }

    // ── Internal ────────────────────────────────────────────────────────────

    fn next_id(&mut self) -> BlockId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

// ── Block chrome rendering helpers ─────────────────────────────────────────

// TokyoNight palette subset for block chrome
const RESET: &str = "\x1b[0m";
const BORDER: (u8, u8, u8) = (59, 66, 97);    // muted blue-grey
const SUCCESS: (u8, u8, u8) = (158, 206, 106); // green
const ERROR: (u8, u8, u8) = (247, 118, 142);   // red
const MUTED: (u8, u8, u8) = (86, 95, 137);     // dim grey
const BOOKMARK: (u8, u8, u8) = (224, 175, 104); // amber

fn fg_rgb(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// ANSI escape string for the top border of a block.
///
/// Shows the block number and bookmarked indicator.
pub fn render_block_top_border(block: &Block, width: u16) -> String {
    let border_esc = fg_rgb(BORDER.0, BORDER.1, BORDER.2);
    let bookmark = if block.bookmarked { "★ " } else { "" };
    let id_label = format!("─ {}{}", bookmark, block.id);
    let fill_len = (width as usize).saturating_sub(id_label.len() + 4);
    let fill: String = std::iter::repeat('─').take(fill_len).collect();

    let id_part = if block.bookmarked {
        format!(
            "{bm}{id}{border}",
            bm = fg_rgb(BOOKMARK.0, BOOKMARK.1, BOOKMARK.2),
            id = id_label,
            border = border_esc,
        )
    } else {
        id_label.clone()
    };

    format!(
        "{border}╭{id}{fill}╮{reset}\r\n",
        border = border_esc,
        id = id_part,
        fill = fill,
        reset = RESET,
    )
}

/// ANSI escape string for the bottom border of a block.
///
/// Shows exit code and duration.
pub fn render_block_bottom_border(block: &Block, width: u16) -> String {
    let border_esc = fg_rgb(BORDER.0, BORDER.1, BORDER.2);

    let exit_str = match block.exit_code {
        Some(0) => format!(
            "{}exit 0{}",
            fg_rgb(SUCCESS.0, SUCCESS.1, SUCCESS.2),
            RESET,
        ),
        Some(n) => format!(
            "{}exit {}{}",
            fg_rgb(ERROR.0, ERROR.1, ERROR.2),
            n,
            RESET,
        ),
        None => String::new(),
    };

    let dur_str = block
        .duration_secs()
        .map(|d| format!(" {}· {:.1}s{}", fg_rgb(MUTED.0, MUTED.1, MUTED.2), d, RESET))
        .unwrap_or_default();

    let footer = format!(" {}{} ", exit_str, dur_str);
    let footer_visible = strip_ansi_visible_len(&footer);
    let fill_len = (width as usize).saturating_sub(footer_visible + 2);
    let fill: String = std::iter::repeat('─').take(fill_len).collect();

    format!(
        "{border}╰{fill}{footer}{border}╯{reset}\r\n",
        border = border_esc,
        fill = fill,
        footer = footer,
        reset = RESET,
    )
}

/// Approximate visible character length (ignoring ANSI escape sequences).
fn strip_ansi_visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_term::{SemanticType, SemanticZone};

    fn make_zone(start_y: isize, end_y: isize, semantic_type: SemanticType) -> SemanticZone {
        SemanticZone {
            start_y,
            start_x: 0,
            end_y,
            end_x: 0,
            semantic_type,
        }
    }

    #[test]
    fn test_sync_empty() {
        let mut mgr = BlockManager::new();
        mgr.sync_from_zones(&[]);
        assert!(mgr.is_empty());
    }

    #[test]
    fn test_sync_single_prompt_input_output() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 1, SemanticType::Input),
            make_zone(2, 5, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.len(), 1);
        let b = &mgr.blocks()[0];
        assert_eq!(b.prompt_zone, Some(ZoneRange { start_y: 0, end_y: 0 }));
        assert_eq!(b.input_zone, Some(ZoneRange { start_y: 1, end_y: 1 }));
        assert_eq!(b.output_zone, Some(ZoneRange { start_y: 2, end_y: 5 }));
    }

    #[test]
    fn test_sync_multiple_blocks() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            // Block 0
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 1, SemanticType::Input),
            make_zone(2, 3, SemanticType::Output),
            // Block 1
            make_zone(4, 4, SemanticType::Prompt),
            make_zone(5, 5, SemanticType::Input),
            make_zone(6, 10, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.len(), 2);
        assert_eq!(mgr.blocks()[0].first_row(), Some(0));
        assert_eq!(mgr.blocks()[1].first_row(), Some(4));
    }

    #[test]
    fn test_sync_orphan_output() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 5, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.len(), 1);
        let b = &mgr.blocks()[0];
        assert!(b.prompt_zone.is_none());
        assert!(b.input_zone.is_none());
        assert_eq!(b.output_zone, Some(ZoneRange { start_y: 0, end_y: 5 }));
    }

    #[test]
    fn test_get_block_at_row() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 1, SemanticType::Input),
            make_zone(2, 5, SemanticType::Output),
            make_zone(6, 6, SemanticType::Prompt),
            make_zone(7, 7, SemanticType::Input),
            make_zone(8, 12, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.get_block_at_row(3).map(|b| b.id), Some(0));
        assert_eq!(mgr.get_block_at_row(9).map(|b| b.id), Some(1));
        assert!(mgr.get_block_at_row(13).is_none());
    }

    #[test]
    fn test_navigate_prev_next() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 3, SemanticType::Output),
            make_zone(5, 5, SemanticType::Prompt),
            make_zone(6, 8, SemanticType::Output),
            make_zone(10, 10, SemanticType::Prompt),
            make_zone(11, 15, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        // Navigate from row 6 (inside block 1)
        assert_eq!(mgr.navigate_prev(6), Some(0)); // block 0 starts at row 0
        assert_eq!(mgr.navigate_next(6), Some(10)); // block 2 starts at row 10

        // Navigate from first block
        assert_eq!(mgr.navigate_prev(0), None);

        // Navigate from last block
        assert_eq!(mgr.navigate_next(10), None);
    }

    #[test]
    fn test_toggle_collapse_bookmark() {
        let mut mgr = BlockManager::new();
        let zones = vec![make_zone(0, 3, SemanticType::Output)];
        mgr.sync_from_zones(&zones);

        let id = mgr.blocks()[0].id;

        // Initially not collapsed or bookmarked
        assert!(!mgr.blocks()[0].collapsed);
        assert!(!mgr.blocks()[0].bookmarked);

        assert_eq!(mgr.toggle_collapse(id), Some(true));
        assert!(mgr.blocks()[0].collapsed);

        assert_eq!(mgr.toggle_collapse(id), Some(false));
        assert!(!mgr.blocks()[0].collapsed);

        assert_eq!(mgr.toggle_bookmark(id), Some(true));
        assert_eq!(mgr.bookmarked_blocks(), vec![id]);

        assert_eq!(mgr.toggle_bookmark(id), Some(false));
        assert!(mgr.bookmarked_blocks().is_empty());
    }

    #[test]
    fn test_preserved_state_across_sync() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 3, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        let id = mgr.blocks()[0].id;
        mgr.toggle_collapse(id);
        mgr.toggle_bookmark(id);

        // Re-sync with same zones (block starts at same row)
        mgr.sync_from_zones(&zones);

        // State should be preserved since first_row matches
        assert!(mgr.blocks()[0].collapsed);
        assert!(mgr.blocks()[0].bookmarked);
    }

    #[test]
    fn test_agent_workflow() {
        let mut mgr = BlockManager::new();

        let id = mgr.push_agent_block(10);
        assert_eq!(mgr.len(), 1);
        assert!(mgr.blocks()[0].start_time.is_some());

        mgr.extend_output(15);
        assert_eq!(mgr.blocks()[0].output_zone.unwrap().end_y, 15);

        mgr.finish_block(Some(0));
        assert_eq!(mgr.blocks()[0].exit_code, Some(0));
        assert!(mgr.blocks()[0].end_time.is_some());

        assert_eq!(mgr.get_block_by_id(id).unwrap().id, id);
    }

    #[test]
    fn test_zone_range_contains() {
        let z = ZoneRange { start_y: 5, end_y: 10 };
        assert!(z.contains(5));
        assert!(z.contains(7));
        assert!(z.contains(10));
        assert!(!z.contains(4));
        assert!(!z.contains(11));
    }

    #[test]
    fn test_block_first_last_row() {
        let block = Block {
            id: 0,
            prompt_zone: Some(ZoneRange { start_y: 0, end_y: 0 }),
            input_zone: Some(ZoneRange { start_y: 1, end_y: 1 }),
            output_zone: Some(ZoneRange { start_y: 2, end_y: 9 }),
            exit_code: None,
            start_time: None,
            end_time: None,
            collapsed: false,
            bookmarked: false,
        };
        assert_eq!(block.first_row(), Some(0));
        assert_eq!(block.last_row(), Some(9));
    }

    // ── Tests for selected block navigation ─────────────────────────────

    #[test]
    fn test_selected_block_initially_none() {
        let mgr = BlockManager::new();
        assert_eq!(mgr.selected_index(), None);
        assert!(mgr.selected_block().is_none());
    }

    #[test]
    fn test_navigate_next_selected_empty() {
        let mut mgr = BlockManager::new();
        mgr.navigate_next_selected();
        // No panic, stays None
        assert_eq!(mgr.selected_index(), None);
    }

    #[test]
    fn test_navigate_prev_selected_empty() {
        let mut mgr = BlockManager::new();
        mgr.navigate_prev_selected();
        // No panic, stays None
        assert_eq!(mgr.selected_index(), None);
    }

    #[test]
    fn test_navigate_next_selected_from_none() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 3, SemanticType::Output),
            make_zone(5, 8, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        // First call selects block 0
        mgr.navigate_next_selected();
        assert_eq!(mgr.selected_index(), Some(0));
        assert_eq!(mgr.selected_block().unwrap().id, mgr.blocks()[0].id);

        // Second call moves to block 1
        mgr.navigate_next_selected();
        assert_eq!(mgr.selected_index(), Some(1));

        // Third call stays at block 1 (end of list)
        mgr.navigate_next_selected();
        assert_eq!(mgr.selected_index(), Some(1));
    }

    #[test]
    fn test_navigate_prev_selected_from_none() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 3, SemanticType::Output),
            make_zone(5, 8, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        // First call selects last block
        mgr.navigate_prev_selected();
        assert_eq!(mgr.selected_index(), Some(1));

        // Second call moves to block 0
        mgr.navigate_prev_selected();
        assert_eq!(mgr.selected_index(), Some(0));

        // Third call stays at block 0 (beginning of list)
        mgr.navigate_prev_selected();
        assert_eq!(mgr.selected_index(), Some(0));
    }

    #[test]
    fn test_navigate_selected_single_block() {
        let mut mgr = BlockManager::new();
        let zones = vec![make_zone(0, 5, SemanticType::Output)];
        mgr.sync_from_zones(&zones);

        mgr.navigate_next_selected();
        assert_eq!(mgr.selected_index(), Some(0));

        mgr.navigate_next_selected();
        assert_eq!(mgr.selected_index(), Some(0)); // Can't go past the only block

        mgr.navigate_prev_selected();
        assert_eq!(mgr.selected_index(), Some(0)); // Can't go before the only block
    }

    // ── Tests for toggle by index ───────────────────────────────────────

    #[test]
    fn test_toggle_collapse_at_valid() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 3, SemanticType::Output),
            make_zone(5, 8, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.toggle_collapse_at(0), Some(true));
        assert!(mgr.blocks()[0].collapsed);
        assert!(!mgr.blocks()[1].collapsed);

        assert_eq!(mgr.toggle_collapse_at(0), Some(false));
        assert!(!mgr.blocks()[0].collapsed);
    }

    #[test]
    fn test_toggle_collapse_at_out_of_range() {
        let mut mgr = BlockManager::new();
        let zones = vec![make_zone(0, 3, SemanticType::Output)];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.toggle_collapse_at(5), None);
    }

    #[test]
    fn test_toggle_bookmark_at_valid() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 3, SemanticType::Output),
            make_zone(5, 8, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        assert_eq!(mgr.toggle_bookmark_at(1), Some(true));
        assert!(!mgr.blocks()[0].bookmarked);
        assert!(mgr.blocks()[1].bookmarked);
    }

    #[test]
    fn test_toggle_bookmark_at_out_of_range() {
        let mut mgr = BlockManager::new();
        assert_eq!(mgr.toggle_bookmark_at(0), None);
    }

    // ── Tests for bookmarked_blocks_with_index ──────────────────────────

    #[test]
    fn test_bookmarked_blocks_with_index() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 3, SemanticType::Output),
            make_zone(5, 8, SemanticType::Output),
            make_zone(10, 15, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        mgr.toggle_bookmark_at(0);
        mgr.toggle_bookmark_at(2);

        let bookmarked = mgr.bookmarked_blocks_with_index();
        assert_eq!(bookmarked.len(), 2);
        assert_eq!(bookmarked[0].0, 0);
        assert_eq!(bookmarked[1].0, 2);
    }

    #[test]
    fn test_bookmarked_blocks_with_index_empty() {
        let mgr = BlockManager::new();
        assert!(mgr.bookmarked_blocks_with_index().is_empty());
    }

    // ── Tests for export_block_markdown ──────────────────────────────────

    #[test]
    fn test_export_block_markdown_basic() {
        let mut mgr = BlockManager::new();
        let zones = vec![
            make_zone(0, 0, SemanticType::Prompt),
            make_zone(1, 1, SemanticType::Input),
            make_zone(2, 5, SemanticType::Output),
        ];
        mgr.sync_from_zones(&zones);

        let md = mgr.export_block_markdown(0);
        assert!(md.is_some());
        let md = md.unwrap();
        assert!(md.contains("## Block"));
        assert!(md.contains("Prompt rows"));
        assert!(md.contains("Input rows"));
        assert!(md.contains("Output rows"));
    }

    #[test]
    fn test_export_block_markdown_with_metadata() {
        let mut mgr = BlockManager::new();
        mgr.push_agent_block(10);
        mgr.extend_output(20);
        mgr.finish_block(Some(42));
        mgr.toggle_bookmark_at(0);

        let md = mgr.export_block_markdown(0).unwrap();
        assert!(md.contains("Exit code:** 42"));
        assert!(md.contains("Duration:**"));
        assert!(md.contains("Bookmarked:** yes"));
    }

    #[test]
    fn test_export_block_markdown_out_of_range() {
        let mgr = BlockManager::new();
        assert!(mgr.export_block_markdown(0).is_none());
    }

    // ── Test select and deselect ────────────────────────────────────────

    #[test]
    fn test_select_clamps_to_range() {
        let mut mgr = BlockManager::new();
        let zones = vec![make_zone(0, 3, SemanticType::Output)];
        mgr.sync_from_zones(&zones);

        mgr.select(100);
        assert_eq!(mgr.selected_index(), Some(0)); // clamped to len-1 = 0
    }

    #[test]
    fn test_deselect() {
        let mut mgr = BlockManager::new();
        let zones = vec![make_zone(0, 3, SemanticType::Output)];
        mgr.sync_from_zones(&zones);

        mgr.select(0);
        assert_eq!(mgr.selected_index(), Some(0));
        mgr.deselect();
        assert_eq!(mgr.selected_index(), None);
    }
}
