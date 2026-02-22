//! Terminal-native tool implementations.
//!
//! These tools leverage WezTerm's native pane system instead of subprocess hacks.
//! They are registered in elwood-core's tool registry when running inside WezTerm.
//!
//! ## Tools
//!
//! | Tool | Description |
//! |------|-------------|
//! | `ReadPaneTool` | Read visible content of any WezTerm pane by ID |
//! | `ListPanesTool` | List all panes with titles, IDs, process info |
//! | `WatchPaneTool` | Subscribe to pane output, get notified on changes |
//! | `SendToPaneTool` | Send keystrokes/text to another pane |
//! | `NativeBashTool` | Execute commands in a real terminal pane |
//! | `ProcessTreeTool` | Read process hierarchy from WezTerm's mux |
//! | `SplitPaneTool` | Create new terminal panes from agent |

use crate::observer::{PaneInfo, PaneObserver};
use mux::pane::{CachePolicy, PaneId};
use serde::{Deserialize, Serialize};

/// Input for ReadPaneTool.
#[derive(Debug, Deserialize)]
pub struct ReadPaneInput {
    /// The pane ID to read. Use ListPanesTool to discover pane IDs.
    pub pane_id: PaneId,
    /// If true, read from cache (faster but may be stale).
    /// If false, read the pane content directly.
    #[serde(default)]
    pub allow_stale: bool,
}

/// Output from ReadPaneTool.
#[derive(Debug, Serialize)]
pub struct ReadPaneOutput {
    pub pane_id: PaneId,
    pub title: String,
    pub lines: Vec<String>,
    pub viewport_rows: usize,
}

/// Read the visible content of a WezTerm pane.
pub fn read_pane(
    observer: &PaneObserver,
    input: &ReadPaneInput,
) -> anyhow::Result<ReadPaneOutput> {
    let snapshot = if input.allow_stale {
        observer
            .get_snapshot(input.pane_id)
            .or_else(|| PaneObserver::read_pane_now(input.pane_id))
    } else {
        PaneObserver::read_pane_now(input.pane_id)
    };

    match snapshot {
        Some(snap) => Ok(ReadPaneOutput {
            pane_id: input.pane_id,
            title: snap.title,
            lines: snap.lines,
            viewport_rows: snap.dimensions.1,
        }),
        None => anyhow::bail!("Pane {} not found or not accessible", input.pane_id),
    }
}

/// List all panes in the terminal.
pub fn list_panes() -> Vec<PaneInfo> {
    PaneObserver::list_panes()
}

/// Input for SendToPaneTool.
#[derive(Debug, Deserialize)]
pub struct SendToPaneInput {
    /// The pane ID to send text to.
    pub pane_id: PaneId,
    /// The text to send (as keystrokes).
    pub text: String,
    /// If true, append a newline (Enter key) after the text.
    #[serde(default = "default_true")]
    pub press_enter: bool,
}

fn default_true() -> bool {
    true
}

/// Send text to another pane (as if typed).
pub fn send_to_pane(input: &SendToPaneInput) -> anyhow::Result<()> {
    let mux = mux::Mux::try_get()
        .ok_or_else(|| anyhow::anyhow!("Mux not available"))?;
    let pane = mux
        .get_pane(input.pane_id)
        .ok_or_else(|| anyhow::anyhow!("Pane {} not found", input.pane_id))?;

    let mut text = input.text.clone();
    if input.press_enter {
        text.push('\r');
    }

    pane.send_paste(&text)?;
    Ok(())
}

/// Input for WatchPaneTool.
#[derive(Debug, Deserialize)]
pub struct WatchPaneInput {
    /// The pane ID to watch.
    pub pane_id: PaneId,
    /// Set to false to stop watching.
    #[serde(default = "default_true")]
    pub subscribe: bool,
}

/// Subscribe to or unsubscribe from pane output notifications.
pub fn watch_pane(observer: &PaneObserver, input: &WatchPaneInput) {
    if input.subscribe {
        observer.subscribe(input.pane_id);
    } else {
        observer.unsubscribe(input.pane_id);
    }
}

// ─── NativeBashTool ──────────────────────────────────────────────────

/// Input for NativeBashTool.
#[derive(Debug, Deserialize)]
pub struct NativeBashInput {
    /// The command to execute.
    pub command: String,
    /// If provided, send the command to this existing pane instead of spawning new.
    pub pane_id: Option<PaneId>,
    /// Working directory for the command (only used when spawning a new pane).
    pub cwd: Option<String>,
    /// Timeout in seconds. Default: 120.
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    120
}

/// Output from NativeBashTool.
#[derive(Debug, Serialize)]
pub struct NativeBashOutput {
    /// The pane ID where the command is running.
    pub pane_id: PaneId,
    /// Initial output captured from the pane (may be empty for long-running commands).
    pub initial_output: Vec<String>,
    /// Whether the command appears to have completed.
    pub completed: bool,
}

/// Execute a command in a real terminal pane.
///
/// Unlike subprocess-based bash execution, this runs the command in a real PTY
/// with full terminal emulation. Interactive programs, REPLs, and curses UIs
/// all work naturally. The agent can observe the output via the pane observer.
pub fn native_bash(
    observer: &PaneObserver,
    input: &NativeBashInput,
) -> anyhow::Result<NativeBashOutput> {
    let pane_id = if let Some(id) = input.pane_id {
        // Send command to existing pane
        send_to_pane(&SendToPaneInput {
            pane_id: id,
            text: input.command.clone(),
            press_enter: true,
        })?;
        id
    } else {
        // No pane specified — send to first non-elwood pane, or error
        let panes = list_panes();
        let shell_pane = panes
            .iter()
            .find(|p| !p.is_dead)
            .or(panes.first());

        match shell_pane {
            Some(p) => {
                send_to_pane(&SendToPaneInput {
                    pane_id: p.pane_id,
                    text: input.command.clone(),
                    press_enter: true,
                })?;
                p.pane_id
            }
            None => {
                anyhow::bail!("No pane available to run command. Use ListPanesTool to find a target pane.");
            }
        }
    };

    // Subscribe to the pane to watch output
    observer.subscribe(pane_id);

    // Brief pause to let output start, then read
    std::thread::sleep(std::time::Duration::from_millis(100));

    let snapshot = PaneObserver::read_pane_now(pane_id);
    let (lines, completed) = match snapshot {
        Some(snap) => {
            // Heuristic: if the last non-empty line looks like a shell prompt, command is done
            let last_line = snap.lines.iter().rev().find(|l| !l.trim().is_empty());
            let looks_complete = last_line
                .map(|l| l.ends_with("$ ") || l.ends_with("% ") || l.ends_with("# "))
                .unwrap_or(false);
            (snap.lines, looks_complete)
        }
        None => (vec![], false),
    };

    Ok(NativeBashOutput {
        pane_id,
        initial_output: lines,
        completed,
    })
}

// ─── ProcessTreeTool ─────────────────────────────────────────────────

/// Process information for a single pane.
#[derive(Debug, Serialize)]
pub struct PaneProcessInfo {
    pub pane_id: PaneId,
    pub title: String,
    /// Foreground process name (e.g., "vim", "cargo", "bash").
    pub foreground_process: Option<String>,
    /// TTY device name.
    pub tty: Option<String>,
    /// Working directory of the foreground process.
    pub cwd: Option<String>,
}

/// Get process information for all panes.
///
/// Reads the foreground process name, working directory, and TTY from
/// WezTerm's mux — no need for `ps` or process scraping.
pub fn process_tree() -> Vec<PaneProcessInfo> {
    let mux = match mux::Mux::try_get() {
        Some(m) => m,
        None => return vec![],
    };

    let mut result = vec![];

    for pane in mux.iter_panes() {
        let fg_name = pane.get_foreground_process_name(CachePolicy::AllowStale);
        let tty = pane.tty_name();
        let cwd = pane
            .get_current_working_dir(CachePolicy::AllowStale)
            .and_then(|u| u.to_file_path().ok())
            .map(|p| p.display().to_string());

        result.push(PaneProcessInfo {
            pane_id: pane.pane_id(),
            title: pane.get_title(),
            foreground_process: fg_name,
            tty,
            cwd,
        });
    }

    result
}

/// Get detailed process info for a specific pane.
pub fn process_info(pane_id: PaneId) -> anyhow::Result<PaneProcessInfo> {
    let mux = mux::Mux::try_get()
        .ok_or_else(|| anyhow::anyhow!("Mux not available"))?;
    let pane = mux
        .get_pane(pane_id)
        .ok_or_else(|| anyhow::anyhow!("Pane {} not found", pane_id))?;

    let fg_name = pane.get_foreground_process_name(CachePolicy::FetchImmediate);
    let tty = pane.tty_name();
    let cwd = pane
        .get_current_working_dir(CachePolicy::FetchImmediate)
        .and_then(|u| u.to_file_path().ok())
        .map(|p| p.display().to_string());

    Ok(PaneProcessInfo {
        pane_id: pane.pane_id(),
        title: pane.get_title(),
        foreground_process: fg_name,
        tty,
        cwd,
    })
}

// ─── SplitPaneTool ───────────────────────────────────────────────────

/// Input for SplitPaneTool.
#[derive(Debug, Deserialize)]
pub struct SplitPaneInput {
    /// Direction to split: "right", "left", "top", "bottom".
    pub direction: String,
    /// Optional command to run in the new pane.
    pub command: Option<String>,
    /// Size as percentage (0-100). Default: 50.
    #[serde(default = "default_split_size")]
    pub size_percent: u8,
}

fn default_split_size() -> u8 {
    50
}

/// Output from SplitPaneTool.
#[derive(Debug, Serialize)]
pub struct SplitPaneOutput {
    /// The ID of the newly created pane.
    pub new_pane_id: PaneId,
}

/// Create a new terminal pane by splitting an existing one.
///
/// The agent can use this to create dedicated panes for running builds,
/// tests, or other tasks while keeping the agent pane visible.
pub fn split_pane(
    _source_pane_id: PaneId,
    _input: &SplitPaneInput,
) -> anyhow::Result<SplitPaneOutput> {
    // NOTE: Splitting panes requires async access to the Mux's tab system.
    // The actual split operation goes through:
    //   tab.split_and_insert(pane_index, direction, pane)
    // This is complex because it involves the tab layout tree.
    //
    // For now, we document the interface. The actual implementation requires
    // hooking into the GUI event loop to perform the split operation.
    //
    // TODO(Phase 5): Wire to mux::tab::Tab::split_and_insert()
    anyhow::bail!(
        "SplitPaneTool is not yet fully implemented. \
         Use the WezTerm keybinding (Ctrl+Shift+E) to split panes, \
         or SendToPaneTool to interact with existing panes."
    )
}
