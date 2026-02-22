//! ElwoodPane — implements WezTerm's `Pane` trait for agent output.
//!
//! The pane wraps a `wezterm_term::Terminal` (virtual terminal) and renders
//! agent output as ANSI escape sequences. WezTerm's renderer calls
//! `get_lines()` which delegates to the virtual terminal, giving us full
//! rich text rendering through the existing GPU pipeline.
//!
//! ## Rendering Architecture
//!
//! The pane uses a full-screen TUI layout with fixed chrome (header, input box,
//! status bar) and a scrolling chat area. This matches the visual hierarchy of
//! elwood-cli's ratatui compositor.
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐  Row 1
//! │  Elwood Pro / project    1:chat  2:tools   22:14 │  Header (fixed)
//! ├──────────────────────────────────────────────────┤
//! │                                                  │
//! │  Elwood:  I will help you with...                │  Chat area
//! │  ⚙ ReadFile /src/main.rs                         │  (scroll region)
//! │  ✔ OK — 200 lines                               │
//! │                                                  │
//! ├──────────────────────────────────────────────────┤
//! │ ╭─ Message (Enter send, Esc cancel) ───────────╮ │  Input box (fixed)
//! │ │ Type a message...                            │ │
//! │ │                                              │ │
//! │ ╰──────────────────────────────────────────────╯ │
//! │  Ready · gemini-2.5-pro · 5.2K tok · 12s        │  Status bar (fixed)
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! Chat content is written into a terminal scroll region so it scrolls
//! naturally within the bounded area. Chrome updates (header, input, status)
//! use cursor save/restore to avoid disturbing the scroll position.

use crate::runtime::{AgentRequest, AgentResponse, InputMode, RuntimeBridge};
use crate::screen::{self, ScreenState};

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
use std::time::Instant;
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
/// Wraps a virtual terminal (`wezterm_term::Terminal`) and renders a
/// full-screen TUI layout with fixed chrome (header, input box, status bar)
/// and a scrolling chat region. The WezTerm GPU renderer reads from the
/// virtual terminal via `get_lines()`.
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
    /// Current input mode (Agent or Terminal).
    input_mode: Mutex<InputMode>,
    /// The pending permission request ID (when in AwaitingPermission state).
    pending_permission: Mutex<Option<PendingPermission>>,
    /// Full-screen layout state (dimensions, model, tokens, etc.).
    screen: Mutex<ScreenState>,
}

/// A pending permission request waiting for user approval.
#[derive(Debug, Clone)]
struct PendingPermission {
    request_id: String,
    tool_name: String,
}

impl ElwoodPane {
    /// Create a new ElwoodPane with a virtual terminal and full-screen TUI.
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
            Box::new(std::io::sink()),
        );

        let mut screen_state = ScreenState::default();
        screen_state.width = size.cols as u16;
        screen_state.height = size.rows as u16;

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
            input_mode: Mutex::new(InputMode::default()),
            pending_permission: Mutex::new(None),
            screen: Mutex::new(screen_state),
        };

        // Render the full-screen TUI layout (includes welcome message)
        {
            let ss = pane.screen.lock();
            pane.write_ansi(&screen::render_full_screen(&ss));
        }

        pane
    }

    /// Write ANSI-escaped text to the virtual terminal.
    fn write_ansi(&self, text: &str) {
        let mut terminal = self.terminal.lock();
        let actions = termwiz::escape::parser::Parser::new().parse_as_vec(text.as_bytes());
        terminal.perform_actions(actions);
        self.seqno.fetch_add(1, Ordering::Release);
    }

    /// Redraw the fixed chrome (header, input box, status bar) without
    /// disturbing the chat scroll position.
    #[allow(dead_code)]
    fn redraw_chrome(&self) {
        let ss = self.screen.lock();
        let mut out = String::with_capacity(2048);
        out.push_str("\x1b[s"); // save cursor
        out.push_str(&screen::render_header(&ss));
        out.push_str(&screen::render_input_box(&ss));
        out.push_str(&screen::render_status_bar(&ss));
        out.push_str("\x1b[u"); // restore cursor
        drop(ss);
        self.write_ansi(&out);
    }

    /// Update just the status bar (lightweight refresh for timer/state changes).
    fn refresh_status_bar(&self) {
        let ss = self.screen.lock();
        let mut out = String::with_capacity(512);
        out.push_str("\x1b[s"); // save cursor
        out.push_str(&screen::render_status_bar(&ss));
        out.push_str("\x1b[u"); // restore cursor
        drop(ss);
        self.write_ansi(&out);
    }

    /// Update the input box (after keystroke or clear).
    fn refresh_input_box(&self) {
        let ss = self.screen.lock();
        let mut out = String::with_capacity(512);
        out.push_str("\x1b[s"); // save cursor
        out.push_str(&screen::render_input_box(&ss));
        out.push_str("\x1b[u"); // restore cursor
        drop(ss);
        self.write_ansi(&out);
    }

    /// Poll the RuntimeBridge for new responses and render them.
    ///
    /// Content is written into the scroll region. Chrome is updated via
    /// cursor save/restore so the scroll position is preserved.
    pub fn poll_responses(&self) {
        let mut any_update = false;

        loop {
            match self.bridge.try_recv_response() {
                Ok(Some(response)) => {
                    any_update = true;

                    // Update pane state and screen state based on response type
                    match &response {
                        AgentResponse::ContentDelta(_) => {
                            *self.state.lock() = PaneState::Running;
                            let mut ss = self.screen.lock();
                            ss.is_running = true;
                            if ss.task_start.is_none() {
                                ss.task_start = Some(Instant::now());
                            }
                        }
                        AgentResponse::ToolStart {
                            tool_name,
                            tool_id: _,
                            input_preview: _,
                        } => {
                            *self.state.lock() = PaneState::Running;
                            let mut ss = self.screen.lock();
                            ss.is_running = true;
                            ss.active_tool = Some(tool_name.clone());
                            ss.tool_start = Some(Instant::now());
                            if ss.task_start.is_none() {
                                ss.task_start = Some(Instant::now());
                            }
                        }
                        AgentResponse::ToolEnd { .. } => {
                            *self.state.lock() = PaneState::Running;
                            let mut ss = self.screen.lock();
                            ss.active_tool = None;
                            ss.tool_start = None;
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
                            let mut ss = self.screen.lock();
                            ss.awaiting_permission = true;
                            ss.active_tool = None;
                            ss.tool_start = None;
                        }
                        AgentResponse::TurnComplete { .. } => {
                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.awaiting_permission = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                            // Freeze elapsed time
                            ss.task_elapsed_frozen = ss.task_start
                                .map(|s| s.elapsed().as_secs());
                            ss.task_start = None;
                        }
                        AgentResponse::CommandOutput { .. } => {
                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                            ss.task_elapsed_frozen = ss.task_start
                                .map(|s| s.elapsed().as_secs());
                            ss.task_start = None;
                        }
                        AgentResponse::Error(_) => {
                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.awaiting_permission = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                        }
                        AgentResponse::Shutdown => {
                            *self.dead.lock() = true;
                        }
                    }

                    // Update window title
                    let new_title = match *self.state.lock() {
                        PaneState::Idle => "Elwood Agent".to_string(),
                        PaneState::Running => "Elwood Agent [running]".to_string(),
                        PaneState::AwaitingPermission => {
                            "Elwood Agent [permission needed]".to_string()
                        }
                    };
                    *self.title.lock() = new_title;

                    // Render content into the scroll region
                    let text = format_response_for_chat(&response);
                    if !text.is_empty() {
                        self.write_ansi(&text);
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    *self.dead.lock() = true;
                    break;
                }
            }
        }

        if any_update {
            self.refresh_status_bar();
        }
    }

    /// Handle a permission approval or denial.
    fn handle_permission_response(&self, granted: bool) {
        let pending = self.pending_permission.lock().take();
        if let Some(perm) = pending {
            // Show the user's choice in the chat area
            let feedback = if granted {
                screen::format_permission_granted(&perm.tool_name)
            } else {
                screen::format_permission_denied(&perm.tool_name)
            };
            self.write_ansi(&feedback);

            // Send the response to the agent
            let _ = self.bridge.send_request(AgentRequest::PermissionResponse {
                request_id: perm.request_id,
                granted,
            });

            *self.state.lock() = PaneState::Running;
            {
                let mut ss = self.screen.lock();
                ss.awaiting_permission = false;
                ss.is_running = true;
            }
            self.refresh_status_bar();
        }
    }

    /// Toggle between Agent and Terminal input modes.
    fn toggle_input_mode(&self) {
        let new_mode = {
            let mut mode = self.input_mode.lock();
            *mode = match *mode {
                InputMode::Agent => InputMode::Terminal,
                InputMode::Terminal => InputMode::Agent,
            };
            *mode
        };

        // Sync to screen state and refresh chrome
        self.screen.lock().input_mode = new_mode;
        self.refresh_input_box();
        self.refresh_status_bar();
    }

    /// Submit the current input buffer as a shell command.
    fn submit_command(&self) {
        let command = {
            let mut buf = self.input_buffer.lock();
            let cmd = buf.clone();
            buf.clear();
            cmd
        };

        if command.is_empty() {
            return;
        }

        // Clear input box
        {
            self.screen.lock().input_text.clear();
        }
        self.refresh_input_box();

        // Write "$ command" prompt into chat area
        self.write_ansi(&screen::format_command_prompt(&command));

        // Update state — mark as running
        {
            let mut ss = self.screen.lock();
            ss.is_running = true;
            ss.task_start = Some(Instant::now());
            ss.task_elapsed_frozen = None;
        }
        *self.state.lock() = PaneState::Running;
        self.refresh_status_bar();

        // Send RunCommand to the bridge
        let working_dir = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        let _ = self.bridge.send_request(AgentRequest::RunCommand {
            command,
            working_dir,
        });
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

        // Clear input box
        {
            let mut ss = self.screen.lock();
            ss.input_text.clear();
        }
        self.refresh_input_box();

        // Write user prompt into chat area (scroll region)
        self.write_ansi(&screen::format_user_prompt(&content));

        // Write the "Elwood" prefix before streaming starts
        self.write_ansi(&screen::format_assistant_prefix());

        // Update state
        {
            let mut ss = self.screen.lock();
            ss.is_running = true;
            ss.task_start = Some(Instant::now());
            ss.task_elapsed_frozen = None;
        }
        *self.state.lock() = PaneState::Running;
        self.refresh_status_bar();

        // Send to agent via bridge
        let _ = self.bridge.send_request(AgentRequest::SendMessage { content });
    }
}

/// Format an `AgentResponse` for the chat scroll region.
/// Uses the screen module's formatting functions for rich ANSI output.
fn format_response_for_chat(response: &AgentResponse) -> String {
    match response {
        AgentResponse::ContentDelta(text) => screen::format_content(text),
        AgentResponse::ToolStart {
            tool_name,
            tool_id: _,
            input_preview,
        } => screen::format_tool_start(tool_name, input_preview),
        AgentResponse::ToolEnd {
            tool_id: _,
            success,
            output_preview,
        } => screen::format_tool_end(*success, output_preview),
        AgentResponse::PermissionRequest {
            request_id: _,
            tool_name,
            description,
        } => screen::format_permission_request(tool_name, description),
        AgentResponse::CommandOutput {
            command,
            stdout,
            stderr,
            exit_code,
        } => screen::format_command_output(command, stdout, stderr, *exit_code),
        AgentResponse::TurnComplete { summary } => {
            screen::format_turn_complete(summary.as_deref())
        }
        AgentResponse::Error(msg) => screen::format_error(msg),
        AgentResponse::Shutdown => screen::format_shutdown(),
    }
}

#[async_trait(?Send)]
impl mux::pane::Pane for ElwoodPane {
    fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    fn get_cursor_position(&self) -> StableCursorPosition {
        self.poll_responses();
        let mut terminal = self.terminal.lock();
        terminal_get_cursor_position(&mut terminal)
    }

    fn get_current_seqno(&self) -> SequenceNo {
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
        // Treat pasted text as input — add to buffer and update input box
        {
            self.input_buffer.lock().push_str(text);
            let buf = self.input_buffer.lock().clone();
            self.screen.lock().input_text = buf;
        }
        self.refresh_input_box();
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
        // Resize the virtual terminal
        {
            let mut terminal = self.terminal.lock();
            terminal.resize(size);
        }

        // Update screen state dimensions and re-render the full layout
        {
            let mut ss = self.screen.lock();
            ss.width = size.cols as u16;
            ss.height = size.rows as u16;
            // Re-render the full chrome
            let full = screen::render_full_screen(&ss);
            drop(ss);
            self.write_ansi(&full);
        }

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
                    self.handle_permission_response(false);
                    return Ok(());
                }
                _ => {
                    return Ok(());
                }
            }
        }

        // Ctrl+T toggles input mode (before other handling)
        if key == KeyCode::Char('t') && mods == KeyModifiers::CTRL {
            self.toggle_input_mode();
            return Ok(());
        }

        match key {
            KeyCode::Enter if mods.is_empty() => {
                let mode = *self.input_mode.lock();
                match mode {
                    InputMode::Agent => {
                        // Check for `!` prefix — run as command
                        let starts_with_bang = self.input_buffer.lock().starts_with('!');
                        if starts_with_bang {
                            // Strip the `!` prefix before submitting as command
                            {
                                let mut buf = self.input_buffer.lock();
                                *buf = buf.trim_start_matches('!').to_string();
                            }
                            self.submit_command();
                        } else {
                            self.submit_input();
                        }
                    }
                    InputMode::Terminal => {
                        self.submit_command();
                    }
                }
            }
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                // Add character to input buffer and update the input box
                self.input_buffer.lock().push(c);
                {
                    let buf = self.input_buffer.lock().clone();
                    self.screen.lock().input_text = buf;
                }
                self.refresh_input_box();
            }
            KeyCode::Backspace if mods.is_empty() => {
                let mut buf = self.input_buffer.lock();
                if buf.pop().is_some() {
                    let new_text = buf.clone();
                    drop(buf);
                    self.screen.lock().input_text = new_text;
                    self.refresh_input_box();
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
