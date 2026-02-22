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

use crate::block::BlockManager;
use crate::editor::InputEditor;
use crate::observer::{ContentDetector, ContentType, NextCommandSuggester};
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

/// Context captured from the last error detection, used to build a Ctrl+F quick-fix prompt.
#[derive(Debug, Clone)]
struct LastDetection {
    /// The command that produced the output.
    command: String,
    /// Combined stdout+stderr that was analyzed.
    output: String,
    /// The human-readable label for the error type (e.g. "Compiler error").
    label: String,
    /// The source file extracted from the error, if any.
    source_file: Option<String>,
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
    /// Multi-line input editor (replaces the old `input_buffer: Mutex<String>`).
    input_editor: Mutex<InputEditor>,
    /// Current pane operational state.
    state: Mutex<PaneState>,
    /// The pending permission request ID (when in AwaitingPermission state).
    pending_permission: Mutex<Option<PendingPermission>>,
    /// Full-screen layout state (dimensions, model, tokens, etc.).
    screen: Mutex<ScreenState>,
    /// Block manager — tracks agent response / command blocks for navigation.
    block_manager: Mutex<BlockManager>,
    /// Last Active AI detection — populated when errors are found in command output.
    /// Consumed by Ctrl+F to build a quick-fix prompt.
    last_detection: Mutex<Option<LastDetection>>,
    /// Shared content detector — pre-compiled regexes.
    detector: ContentDetector,
    /// Next-command suggester.
    suggester: NextCommandSuggester,
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
            input_editor: Mutex::new(InputEditor::new(InputMode::default())),
            state: Mutex::new(PaneState::Idle),
            pending_permission: Mutex::new(None),
            screen: Mutex::new(screen_state),
            block_manager: Mutex::new(BlockManager::new()),
            last_detection: Mutex::new(None),
            detector: ContentDetector::new(),
            suggester: NextCommandSuggester::new(),
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
                            // Close the current agent block (exit 0 — successful turn)
                            self.block_manager.lock().finish_block(Some(0));
                        }
                        AgentResponse::CommandOutput {
                            command,
                            stdout,
                            stderr,
                            exit_code,
                        } => {
                            let code = *exit_code;
                            let success = code == Some(0);

                            // ── Active AI: run observer detection ──────────
                            // Combine stdout + stderr into a single line list for the detector.
                            let combined: String = if stderr.is_empty() {
                                stdout.clone()
                            } else if stdout.is_empty() {
                                stderr.clone()
                            } else {
                                format!("{stdout}\n{stderr}")
                            };
                            let lines: Vec<String> =
                                combined.lines().map(|l| l.to_string()).collect();
                            let detections = self.detector.detect(self.pane_id, &lines, Instant::now());

                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                            ss.task_elapsed_frozen = ss.task_start
                                .map(|s| s.elapsed().as_secs());
                            ss.task_start = None;
                            drop(ss);

                            // Close the current block with the shell exit code
                            self.block_manager.lock().finish_block(code);

                            // Store the primary detection for Ctrl+F quick-fix
                            let primary = detections.first();
                            *self.last_detection.lock() = primary.map(|d| {
                                let label = match d.content_type {
                                    ContentType::CompilerError => "Compiler error",
                                    ContentType::TestFailure => "Test failure",
                                    ContentType::StackTrace => "Stack trace",
                                    ContentType::CommandOutput => "Command error",
                                    ContentType::Unknown => "Error",
                                };
                                LastDetection {
                                    command: command.clone(),
                                    output: combined.clone(),
                                    label: label.to_string(),
                                    source_file: d.source_file.clone(),
                                }
                            });

                            // Render Active AI suggestion if errors detected
                            if let Some(d) = primary {
                                let label = match d.content_type {
                                    ContentType::CompilerError => "Compiler error",
                                    ContentType::TestFailure => "Test failure",
                                    ContentType::StackTrace => "Stack trace",
                                    ContentType::CommandOutput => "Command error",
                                    ContentType::Unknown => "Error",
                                };
                                self.write_ansi(&screen::format_suggestion(label));
                            } else if let Some(suggestion) = self.suggester.suggest(command, success) {
                                // No errors but there is a next-command suggestion
                                self.write_ansi(&screen::format_next_command_suggestion(suggestion));
                            }
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
            let mut editor = self.input_editor.lock();
            let new = match editor.mode() {
                InputMode::Agent => InputMode::Terminal,
                InputMode::Terminal => InputMode::Agent,
            };
            editor.set_mode(new);
            new
        };

        // Sync to screen state and refresh chrome
        let mut ss = self.screen.lock();
        ss.input_mode = new_mode;
        drop(ss);
        self.refresh_input_box();
        self.refresh_status_bar();
    }

    /// Submit the current input buffer as a shell command.
    fn submit_command(&self) {
        let command = match self.input_editor.lock().submit() {
            Some(c) => c,
            None => return,
        };

        // Clear input box screen state
        self.sync_editor_to_screen();
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
        let content = match self.input_editor.lock().submit() {
            Some(c) => c,
            None => return,
        };

        // Clear input box screen state
        self.sync_editor_to_screen();
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

    /// Sync the InputEditor's current state into ScreenState for rendering.
    fn sync_editor_to_screen(&self) {
        let editor = self.input_editor.lock();
        let mut ss = self.screen.lock();
        ss.input_lines = editor.lines().to_vec();
        ss.cursor_row = editor.cursor_row();
        ss.cursor_col = editor.cursor_col();
        // Keep legacy field in sync for single-line fallback
        ss.input_text = editor.lines().first().cloned().unwrap_or_default();
    }

    // ── Active AI quick-fix ─────────────────────────────────────────────────

    /// Handle `Ctrl+F` — send the last detected error to the agent with a fix prompt.
    ///
    /// If there is no pending detection (i.e. the last command succeeded without
    /// errors), this is a no-op with a brief informational message.
    fn handle_quick_fix(&self) {
        let detection = self.last_detection.lock().take();
        match detection {
            None => {
                // Nothing to fix — tell the user
                // INFO color: #7DCFFF = rgb(125, 207, 255) — matches screen.rs INFO constant
                let info = "\x1b[38;2;125;207;255m";
                self.write_ansi(&format!(
                    "\r\n{info}  [!] No error detected from last command. Run a command first, then press Ctrl+F.\x1b[0m\r\n",
                ));
            }
            Some(det) => {
                // Build a fix prompt that includes the command, output, and file context
                let file_ctx = det
                    .source_file
                    .as_deref()
                    .map(|f| format!(" in `{f}`"))
                    .unwrap_or_default();

                let prompt = format!(
                    "Fix the {label}{file_ctx}.\n\nCommand: `{command}`\n\nOutput:\n```\n{output}\n```\n\nPlease identify the root cause and apply a fix.",
                    label = det.label,
                    file_ctx = file_ctx,
                    command = det.command,
                    output = if det.output.len() > 4096 {
                        &det.output[..4096]
                    } else {
                        &det.output
                    },
                );

                // Echo the intent into the chat area so the user knows what was sent
                self.write_ansi(&screen::format_user_prompt(&format!(
                    "[Quick Fix] {}{}",
                    det.label, file_ctx
                )));
                self.write_ansi(&screen::format_assistant_prefix());

                // Mark as running and send to agent
                {
                    let mut ss = self.screen.lock();
                    ss.is_running = true;
                    ss.task_start = Some(Instant::now());
                    ss.task_elapsed_frozen = None;
                }
                *self.state.lock() = PaneState::Running;

                let _ = self.bridge.send_request(AgentRequest::SendMessage { content: prompt });
            }
        }
        self.refresh_status_bar();
    }

    // ── Block navigation ────────────────────────────────────────────────────

    /// Return the current cursor row in the virtual terminal (stable index).
    fn current_row(&self) -> StableRowIndex {
        let mut terminal = self.terminal.lock();
        terminal_get_cursor_position(&mut terminal).y
    }

    /// Navigate to the previous block (Ctrl+Up).
    fn navigate_block_prev(&self) {
        let current = self.current_row();
        if let Some(target_row) = self.block_manager.lock().navigate_prev(current) {
            self.scroll_to_row(target_row);
        }
    }

    /// Navigate to the next block (Ctrl+Down).
    fn navigate_block_next(&self) {
        let current = self.current_row();
        if let Some(target_row) = self.block_manager.lock().navigate_next(current) {
            self.scroll_to_row(target_row);
        }
    }

    /// Scroll the virtual terminal so that `row` is approximately visible.
    ///
    /// Emits a cursor-positioning escape; the WezTerm renderer follows the
    /// cursor position when re-drawing the pane.
    fn scroll_to_row(&self, row: StableRowIndex) {
        let ss = self.screen.lock();
        let chat_top = ss.chat_top();
        let chat_bottom = ss.chat_bottom();
        let height = ss.height.max(1);
        drop(ss);

        let visible_row = row.max(0) as u16;
        let target_row = (chat_top + visible_row % height)
            .min(chat_bottom)
            .max(chat_top);

        let escape = format!("\x1b[s\x1b[{};1H\x1b[u", target_row);
        self.write_ansi(&escape);
    }

    /// Copy the output zone of the block at the current cursor row.
    ///
    /// Writes a brief confirmation message into the chat area.  Full clipboard
    /// integration requires access to the WezTerm clipboard API (wezterm-gui);
    /// that is left as a comment for future wiring.
    fn copy_current_block_output(&self) {
        let current = self.current_row();
        let output_text = {
            let mgr = self.block_manager.lock();
            mgr.get_block_at_row(current)
                .and_then(|b| b.output_zone)
                .map(|z| {
                    let mut terminal = self.terminal.lock();
                    let (_, lines) = terminal_get_lines(&mut terminal, z.start_y..z.end_y + 1);
                    lines
                        .iter()
                        .map(|l| l.as_str().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
        };

        if let Some(text) = output_text {
            let msg = format!(
                "\r\n\x1b[2m[Copied {} bytes from block]\x1b[0m\r\n",
                text.len(),
            );
            self.write_ansi(&msg);
            // Future: pass `text` to the clipboard via a callback registered
            // at ElwoodDomain construction time.
            let _ = text;
        }
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
        // Treat pasted text as input — insert each character into the editor
        {
            let mut editor = self.input_editor.lock();
            for ch in text.chars() {
                if ch == '\n' || ch == '\r' {
                    editor.insert_newline();
                } else {
                    editor.insert_char(ch);
                }
            }
        }
        self.sync_editor_to_screen();
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

        // Ctrl+F: quick-fix — send last detected error to the agent
        if key == KeyCode::Char('f') && mods == KeyModifiers::CTRL {
            self.handle_quick_fix();
            return Ok(());
        }

        let mut editor_changed = true;

        match key {
            // ── Submit ───────────────────────────────────────────────
            KeyCode::Enter if mods.is_empty() => {
                let mode = self.input_editor.lock().mode();
                match mode {
                    InputMode::Agent => {
                        // Check for `!` prefix — run as command
                        let starts_with_bang = self
                            .input_editor
                            .lock()
                            .lines()
                            .first()
                            .map(|l| l.starts_with('!'))
                            .unwrap_or(false);
                        if starts_with_bang {
                            // Strip the leading `!` from the first line
                            {
                                let mut ed = self.input_editor.lock();
                                let first = ed.lines()[0].trim_start_matches('!').to_string();
                                ed.clear();
                                for c in first.chars() {
                                    ed.insert_char(c);
                                }
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
                editor_changed = false; // sync already done in submit_*
            }

            // ── Multi-line newline (Shift+Enter) ─────────────────────
            KeyCode::Enter if mods == KeyModifiers::SHIFT => {
                self.input_editor.lock().insert_newline();
            }

            // ── Cancel ───────────────────────────────────────────────
            KeyCode::Escape => {
                let _ = self.bridge.send_request(AgentRequest::Cancel);
                editor_changed = false;
            }

            // ── Backspace ────────────────────────────────────────────
            KeyCode::Backspace if mods.is_empty() => {
                self.input_editor.lock().backspace();
            }

            // ── Delete word backward (Ctrl+W) ────────────────────────
            KeyCode::Char('w') if mods == KeyModifiers::CTRL => {
                self.input_editor.lock().delete_word_backward();
            }

            // ── Delete to line start (Ctrl+U) ────────────────────────
            KeyCode::Char('u') if mods == KeyModifiers::CTRL => {
                self.input_editor.lock().delete_to_line_start();
            }

            // ── Start of line (Ctrl+A or Home) ────────────────────────
            KeyCode::Char('a') if mods == KeyModifiers::CTRL => {
                self.input_editor.lock().move_to_line_start();
            }
            KeyCode::Home if mods.is_empty() => {
                self.input_editor.lock().move_to_line_start();
            }

            // ── End of line (Ctrl+E or End) ───────────────────────────
            KeyCode::Char('e') if mods == KeyModifiers::CTRL => {
                self.input_editor.lock().move_to_line_end();
            }
            KeyCode::End if mods.is_empty() => {
                self.input_editor.lock().move_to_line_end();
            }

            // ── Cursor left ───────────────────────────────────────────
            KeyCode::LeftArrow if mods.is_empty() => {
                self.input_editor.lock().move_left();
            }

            // ── Cursor right ──────────────────────────────────────────
            KeyCode::RightArrow if mods.is_empty() => {
                self.input_editor.lock().move_right();
            }

            // ── Cursor up / history prev ──────────────────────────────
            KeyCode::UpArrow if mods.is_empty() => {
                self.input_editor.lock().move_up();
            }

            // ── Cursor down / history next ────────────────────────────
            KeyCode::DownArrow if mods.is_empty() => {
                self.input_editor.lock().move_down();
            }

            // ── Delete word backward (Alt+Backspace) ──────────────────
            KeyCode::Backspace if mods == KeyModifiers::ALT => {
                self.input_editor.lock().delete_word_backward();
            }

            // ── Block navigation (Ctrl+Up / Ctrl+Down) ───────────────
            KeyCode::UpArrow if mods == KeyModifiers::CTRL => {
                self.navigate_block_prev();
                editor_changed = false;
            }
            KeyCode::DownArrow if mods == KeyModifiers::CTRL => {
                self.navigate_block_next();
                editor_changed = false;
            }

            // ── Copy current block output (Ctrl+Shift+C) ─────────────
            KeyCode::Char('c') if mods == KeyModifiers::CTRL | KeyModifiers::SHIFT => {
                self.copy_current_block_output();
                editor_changed = false;
            }

            // ── Regular character input ───────────────────────────────
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                self.input_editor.lock().insert_char(c);
            }

            _ => {
                editor_changed = false;
            }
        }

        if editor_changed {
            self.sync_editor_to_screen();
            self.refresh_input_box();
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
