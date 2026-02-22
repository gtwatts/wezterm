//! ElwoodPane — implements WezTerm's `Pane` trait for agent output.
//!
//! The pane wraps a `wezterm_term::Terminal` (virtual terminal) and writes
//! agent output as ANSI escape sequences. WezTerm's renderer calls
//! `get_lines()` which delegates to the virtual terminal, giving us full
//! rich text rendering through the existing GPU pipeline.
//!
//! ## Rendering Flow
//!
//! ```text
//! AgentResponse::ContentDelta("hello")
//!   → RuntimeBridge response channel
//!   → ElwoodPane::poll_responses()
//!   → Write ANSI to virtual terminal
//!   → Increment seqno
//!   → WezTerm renderer detects seqno change
//!   → Calls get_lines() → virtual terminal returns styled lines
//!   → GPU renders with WezTerm's font/color pipeline
//! ```

use crate::formatter;
use crate::runtime::{AgentRequest, AgentResponse, RuntimeBridge};

use async_trait::async_trait;
use mux::domain::DomainId;
use mux::pane::{
    CachePolicy, CloseReason, ForEachPaneLogicalLine, LogicalLine, PaneId,
    PerformAssignmentResult, WithPaneLines,
};
use mux::renderable::{
    terminal_get_cursor_position, terminal_get_dirty_lines, terminal_get_dimensions,
    terminal_get_lines, terminal_with_lines_mut,
    terminal_for_each_logical_line_in_stable_range_mut, RenderableDimensions,
    StableCursorPosition,
};
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use rangeset::RangeSet;
use std::collections::HashMap;
use std::io::Write;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use termwiz::surface::{Line, SequenceNo};
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{
    KeyCode, KeyModifiers, MouseEvent, StableRowIndex, Terminal, TerminalConfiguration,
    TerminalSize,
};

/// Minimal terminal configuration for the virtual terminal.
#[derive(Debug)]
struct ElwoodTermConfig;

impl TerminalConfiguration for ElwoodTermConfig {
    fn scrollback_size(&self) -> usize {
        10_000
    }

    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

/// Tracks the ElwoodPane's current operational state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneState {
    /// Idle, waiting for user input (showing prompt).
    Idle,
    /// Agent is thinking/generating output.
    Running,
    /// Waiting for user to approve/deny a permission request.
    AwaitingPermission,
}

/// WezTerm Pane implementation for Elwood agent output.
///
/// Wraps a virtual terminal (`wezterm_term::Terminal`) and renders agent
/// output by writing ANSI escape sequences into it. The WezTerm renderer
/// reads from the virtual terminal via `get_lines()`.
pub struct ElwoodPane {
    pane_id: PaneId,
    domain_id: DomainId,
    terminal: Mutex<Terminal>,
    writer: Mutex<Box<dyn Write + Send>>,
    bridge: Arc<RuntimeBridge>,
    seqno: AtomicUsize,
    dead: Mutex<bool>,
    title: Mutex<String>,
    /// Accumulates user keyboard input for the prompt.
    input_buffer: Mutex<String>,
    /// Current pane operational state.
    state: Mutex<PaneState>,
    /// The pending permission request ID (when in AwaitingPermission state).
    pending_permission: Mutex<Option<PendingPermission>>,
}

/// A pending permission request waiting for user approval.
#[derive(Debug, Clone)]
struct PendingPermission {
    request_id: String,
    tool_name: String,
}

impl ElwoodPane {
    /// Create a new ElwoodPane with a virtual terminal.
    pub fn new(
        pane_id: PaneId,
        domain_id: DomainId,
        size: TerminalSize,
        bridge: Arc<RuntimeBridge>,
    ) -> Self {
        let terminal = Terminal::new(
            size,
            Arc::new(ElwoodTermConfig),
            "Elwood",
            "0.1.0",
            // The terminal needs a writer for output from the terminal itself
            // (e.g., responses to escape sequence queries). We use a sink.
            Box::new(std::io::sink()),
        );

        let pane = Self {
            pane_id,
            domain_id,
            terminal: Mutex::new(terminal),
            writer: Mutex::new(Box::new(std::io::sink())),
            bridge,
            seqno: AtomicUsize::new(0),
            dead: Mutex::new(false),
            title: Mutex::new("Elwood Agent".into()),
            input_buffer: Mutex::new(String::new()),
            state: Mutex::new(PaneState::Idle),
            pending_permission: Mutex::new(None),
        };

        // Write the initial banner
        pane.write_ansi(&formatter::format_prompt_banner());

        pane
    }

    /// Write ANSI-escaped text to the virtual terminal.
    fn write_ansi(&self, text: &str) {
        let mut terminal = self.terminal.lock();
        let actions = termwiz::escape::parser::Parser::new().parse_as_vec(text.as_bytes());
        terminal.perform_actions(actions);
        self.seqno.fetch_add(1, Ordering::Release);
    }

    /// Poll the RuntimeBridge for new responses and render them.
    ///
    /// This should be called periodically (e.g., from a timer or before rendering).
    /// It drains all available responses and writes them to the virtual terminal.
    pub fn poll_responses(&self) {
        loop {
            match self.bridge.try_recv_response() {
                Ok(Some(response)) => {
                    // Update state based on response type
                    match &response {
                        AgentResponse::ContentDelta(_)
                        | AgentResponse::ToolStart { .. }
                        | AgentResponse::ToolEnd { .. } => {
                            *self.state.lock() = PaneState::Running;
                        }
                        AgentResponse::PermissionRequest {
                            request_id,
                            tool_name,
                            description: _,
                        } => {
                            *self.state.lock() = PaneState::AwaitingPermission;
                            *self.pending_permission.lock() = Some(PendingPermission {
                                request_id: request_id.clone(),
                                tool_name: tool_name.clone(),
                            });
                        }
                        AgentResponse::TurnComplete { .. } => {
                            *self.state.lock() = PaneState::Idle;
                        }
                        AgentResponse::Error(_) => {
                            *self.state.lock() = PaneState::Idle;
                        }
                        AgentResponse::Shutdown => {
                            *self.dead.lock() = true;
                        }
                    }

                    // Update title based on state
                    let new_title = match *self.state.lock() {
                        PaneState::Idle => "Elwood Agent".to_string(),
                        PaneState::Running => "Elwood Agent [running]".to_string(),
                        PaneState::AwaitingPermission => {
                            "Elwood Agent [permission needed]".to_string()
                        }
                    };
                    *self.title.lock() = new_title;

                    let text = formatter::format_response(&response);
                    if !text.is_empty() {
                        self.write_ansi(&text);
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    // Channel closed — agent has shut down
                    *self.dead.lock() = true;
                    break;
                }
            }
        }
    }

    /// Handle a permission approval or denial.
    fn handle_permission_response(&self, granted: bool) {
        let pending = self.pending_permission.lock().take();
        if let Some(perm) = pending {
            // Show the user's choice
            let feedback = if granted {
                formatter::format_permission_granted(&perm.tool_name)
            } else {
                formatter::format_permission_denied(&perm.tool_name)
            };
            self.write_ansi(&feedback);

            // Send the response to the agent
            let _ = self.bridge.send_request(AgentRequest::PermissionResponse {
                request_id: perm.request_id,
                granted,
            });

            *self.state.lock() = PaneState::Running;
        }
    }

    /// Submit the current input buffer as a message to the agent.
    fn submit_input(&self) {
        let content = {
            let mut buf = self.input_buffer.lock();
            let content = buf.clone();
            buf.clear();
            content
        };

        if content.is_empty() {
            return;
        }

        // Echo the input to the terminal
        self.write_ansi(&format!("{content}\r\n"));

        // Send to agent via bridge
        let _ = self.bridge.send_request(AgentRequest::SendMessage { content });
    }
}

#[async_trait(?Send)]
impl mux::pane::Pane for ElwoodPane {
    fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    fn get_cursor_position(&self) -> StableCursorPosition {
        // Poll for new output before returning cursor position
        self.poll_responses();
        let mut terminal = self.terminal.lock();
        terminal_get_cursor_position(&mut terminal)
    }

    fn get_current_seqno(&self) -> SequenceNo {
        // Poll for new output
        self.poll_responses();
        self.seqno.load(Ordering::Acquire) as SequenceNo
    }

    fn get_changed_since(
        &self,
        lines: Range<StableRowIndex>,
        seqno: SequenceNo,
    ) -> RangeSet<StableRowIndex> {
        let mut terminal = self.terminal.lock();
        terminal_get_dirty_lines(&mut terminal, lines, seqno)
    }

    fn get_lines(&self, lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        let mut terminal = self.terminal.lock();
        terminal_get_lines(&mut terminal, lines)
    }

    fn with_lines_mut(
        &self,
        lines: Range<StableRowIndex>,
        with_lines: &mut dyn WithPaneLines,
    ) {
        let mut terminal = self.terminal.lock();
        terminal_with_lines_mut(&mut terminal, lines, with_lines);
    }

    fn for_each_logical_line_in_stable_range_mut(
        &self,
        lines: Range<StableRowIndex>,
        for_line: &mut dyn ForEachPaneLogicalLine,
    ) {
        let mut terminal = self.terminal.lock();
        terminal_for_each_logical_line_in_stable_range_mut(&mut terminal, lines, for_line);
    }

    fn get_logical_lines(&self, lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        mux::pane::impl_get_logical_lines_via_get_lines(self, lines)
    }

    fn get_dimensions(&self) -> RenderableDimensions {
        let mut terminal = self.terminal.lock();
        terminal_get_dimensions(&mut terminal)
    }

    fn get_title(&self) -> String {
        self.title.lock().clone()
    }

    fn send_paste(&self, text: &str) -> anyhow::Result<()> {
        // Treat pasted text as input
        self.input_buffer.lock().push_str(text);
        self.submit_input();
        Ok(())
    }

    fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
        Ok(None)
    }

    fn writer(&self) -> MappedMutexGuard<'_, dyn Write> {
        MutexGuard::map(self.writer.lock(), |w| {
            let w: &mut dyn Write = &mut **w;
            w
        })
    }

    fn resize(&self, size: TerminalSize) -> anyhow::Result<()> {
        let mut terminal = self.terminal.lock();
        terminal.resize(size);
        self.seqno.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn key_down(&self, key: KeyCode, mods: KeyModifiers) -> anyhow::Result<()> {
        // If awaiting permission, intercept y/n keys
        if *self.state.lock() == PaneState::AwaitingPermission {
            match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.handle_permission_response(true);
                    return Ok(());
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.handle_permission_response(false);
                    return Ok(());
                }
                KeyCode::Escape => {
                    // Escape also denies
                    self.handle_permission_response(false);
                    return Ok(());
                }
                _ => {
                    // Ignore other keys during permission prompt
                    return Ok(());
                }
            }
        }

        match key {
            KeyCode::Enter if mods.is_empty() => {
                self.submit_input();
            }
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                self.input_buffer.lock().push(c);
                // Echo the character
                self.write_ansi(&c.to_string());
            }
            KeyCode::Backspace if mods.is_empty() => {
                let mut buf = self.input_buffer.lock();
                if buf.pop().is_some() {
                    drop(buf);
                    // Move cursor back, overwrite with space, move back again
                    self.write_ansi("\x08 \x08");
                }
            }
            KeyCode::Escape => {
                // Cancel current operation
                let _ = self.bridge.send_request(AgentRequest::Cancel);
            }
            _ => {
                // Unhandled key
            }
        }
        Ok(())
    }

    fn key_up(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        Ok(())
    }

    fn perform_assignment(
        &self,
        _assignment: &config::keyassignment::KeyAssignment,
    ) -> PerformAssignmentResult {
        // TODO(Phase 4): Handle Elwood-specific key assignments
        PerformAssignmentResult::Unhandled
    }

    fn mouse_event(&self, _event: MouseEvent) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_dead(&self) -> bool {
        *self.dead.lock()
    }

    fn kill(&self) {
        let _ = self.bridge.send_request(AgentRequest::Shutdown);
        *self.dead.lock() = true;
    }

    fn palette(&self) -> ColorPalette {
        self.terminal.lock().palette()
    }

    fn domain_id(&self) -> DomainId {
        self.domain_id
    }

    fn is_mouse_grabbed(&self) -> bool {
        false
    }

    fn is_alt_screen_active(&self) -> bool {
        false
    }

    fn get_current_working_dir(&self, _policy: CachePolicy) -> Option<Url> {
        std::env::current_dir()
            .ok()
            .and_then(|p| Url::from_directory_path(p).ok())
    }

    fn can_close_without_prompting(&self, _reason: CloseReason) -> bool {
        true
    }

    fn copy_user_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("ELWOOD_PANE".into(), "true".into());
        vars
    }
}
