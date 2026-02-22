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
use crate::commands::{self, CommandResult};
use crate::completions::CompletionEngine;
use crate::context;
use crate::diff;
use crate::semantic_bridge::SemanticBridge;
use crate::diff_viewer::{DiffViewer, ReviewAction};
use crate::editor::InputEditor;
use crate::git_info;
use crate::history_search::{HistoryRecord, HistorySearch};
use crate::nl_classifier::NlClassifier;
use crate::observer::{ContentDetector, ContentType, NextCommandSuggester};
use crate::palette::CommandPalette;
use crate::pty_inner::InnerPty;
use crate::runtime::{AgentRequest, AgentResponse, InputMode, RuntimeBridge};
use crate::screen::{self, ScreenState};
use crate::session_log::SessionLog;
use crate::shared_writer::SharedWriter;

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
use portable_pty::PtySize;
use rangeset::RangeSet;
use std::collections::HashMap;
use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;
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
    terminal: Arc<Mutex<Terminal>>,
    /// Swappable writer: points to sink in agent mode, PTY stdin in terminal mode.
    /// The Terminal's internal writer is a clone of this; swapping the destination
    /// transparently reroutes Terminal::key_down() writes.
    shared_writer: SharedWriter,
    /// Mutex-wrapped writer for the Pane trait's writer() method.
    pane_writer: Mutex<Box<dyn Write + Send>>,
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
    /// Session log for markdown export.
    session_log: Mutex<SessionLog>,
    /// Embedded PTY. `None` until the user first enters terminal mode (Ctrl+T).
    inner_pty: Mutex<Option<InnerPty>>,
    /// Active diff viewer for code review. When `Some`, key events are routed here.
    diff_viewer: Mutex<Option<DiffViewer>>,
    /// NL classifier for auto-detecting input mode on submit.
    nl_classifier: NlClassifier,
    /// Completion engine for ghost text suggestions.
    completion_engine: Mutex<CompletionEngine>,
    /// Command palette overlay (Ctrl+P).
    palette: Mutex<CommandPalette>,
    /// Fuzzy history search overlay (Ctrl+R).
    history_search: Mutex<HistorySearch>,
    /// Semantic bridge for code-aware completions and context.
    semantic_bridge: Mutex<Option<SemanticBridge>>,
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
        let shared_writer = SharedWriter::new();

        let terminal = Terminal::new(
            size,
            Arc::new(ElwoodTermConfig),
            "Elwood",
            "0.1.0",
            // Terminal's internal writer goes through SharedWriter so that
            // key_down() writes route to sink (agent mode) or PTY (terminal mode).
            Box::new(shared_writer.clone()),
        );

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let mut screen_state = ScreenState::default();
        screen_state.width = size.cols as u16;
        screen_state.height = size.rows as u16;
        screen_state.git_info = git_info::get_git_info(&cwd);

        // A separate clone of the shared_writer for the Pane::writer() trait method.
        let pane_writer_clone = shared_writer.clone();

        let pane = Self {
            pane_id,
            domain_id,
            terminal: Arc::new(Mutex::new(terminal)),
            shared_writer,
            pane_writer: Mutex::new(Box::new(pane_writer_clone)),
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
            session_log: Mutex::new(SessionLog::new(cwd.clone())),
            inner_pty: Mutex::new(None),
            diff_viewer: Mutex::new(None),
            nl_classifier: NlClassifier::new(),
            completion_engine: Mutex::new(CompletionEngine::new()),
            palette: Mutex::new(CommandPalette::new()),
            history_search: Mutex::new(HistorySearch::new()),
            semantic_bridge: {
                let mut bridge = SemanticBridge::new(cwd);
                bridge.initialize();
                Mutex::new(Some(bridge))
            },
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
                        AgentResponse::FileEdit {
                            file_path,
                            old_content,
                            new_content,
                            description,
                        } => {
                            // Compute the diff and open the diff viewer
                            let file_diff = diff::compute_file_diff(
                                Some(file_path),
                                file_path,
                                old_content,
                                new_content,
                                3,
                            );
                            let viewer = DiffViewer::new(
                                vec![file_diff],
                                description.clone(),
                            );
                            // Render the diff into the chat area
                            let width = self.screen.lock().width as usize;
                            let rendered = viewer.render(width);
                            *self.diff_viewer.lock() = Some(viewer);
                            self.write_ansi(&rendered);
                            // Mark idle — user now reviews
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
                        AgentResponse::PtyScreenSnapshot { .. } => {
                            // PTY screen snapshots are handled by the PTY pane, not here
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

                    // Log to session
                    match &response {
                        AgentResponse::ContentDelta(text) => {
                            self.session_log.lock().log_agent(text);
                        }
                        AgentResponse::ToolStart { tool_name, input_preview, .. } => {
                            self.session_log.lock().log_tool(tool_name, &format!("started: {input_preview}"));
                        }
                        AgentResponse::ToolEnd { success, output_preview, .. } => {
                            let status = if *success { "OK" } else { "FAIL" };
                            self.session_log.lock().log_tool("ToolEnd", &format!("{status}: {output_preview}"));
                        }
                        AgentResponse::CommandOutput { stdout, stderr, exit_code, .. } => {
                            self.session_log.lock().log_command_output(stdout, stderr, *exit_code);
                            // Refresh git info after commands (may have changed branch/state)
                            if let Some(cwd) = std::env::current_dir().ok() {
                                let mut ss = self.screen.lock();
                                ss.git_info = git_info::get_git_info(&cwd);
                            }
                            // Refresh semantic index after commands (may have changed files)
                            if let Some(ref mut bridge) = *self.semantic_bridge.lock() {
                                bridge.refresh();
                            }
                        }
                        AgentResponse::Error(msg) => {
                            self.session_log.lock().log_system(&format!("Error: {msg}"));
                        }
                        _ => {}
                    }

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

        // Check if the PTY child has exited (detected by reader thread EOF)
        {
            let mode = self.input_editor.lock().mode();
            if mode == InputMode::Terminal {
                let pty_dead = {
                    let pty_guard = self.inner_pty.lock();
                    pty_guard.as_ref().is_some_and(|p| p.is_dead())
                };
                if pty_dead {
                    self.handle_pty_exit();
                }
            }
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
    ///
    /// When switching to Terminal mode for the first time, lazily spawns a
    /// PTY with the user's shell. The SharedWriter is swapped to route
    /// Terminal::key_down() writes to the PTY stdin.
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

        match new_mode {
            InputMode::Terminal => {
                // Spawn PTY on first entry to terminal mode
                let mut pty_guard = self.inner_pty.lock();
                if pty_guard.is_none() {
                    let ss = self.screen.lock();
                    let size = PtySize {
                        rows: ss.height,
                        cols: ss.width,
                        pixel_width: 0,
                        pixel_height: 0,
                    };
                    drop(ss);

                    let cwd = std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."));

                    match InnerPty::spawn(size, &cwd, &self.terminal, &self.shared_writer) {
                        Ok(inner) => {
                            *pty_guard = Some(inner);
                            tracing::info!("PTY spawned for terminal mode");
                        }
                        Err(e) => {
                            tracing::error!("Failed to spawn PTY: {e}");
                            self.write_ansi(&screen::format_error(
                                &format!("Failed to spawn PTY: {e}"),
                            ));
                        }
                    }
                } else {
                    // PTY already exists — swap writer back to PTY
                    // (it was swapped to sink when we left terminal mode)
                    // Actually: in current design the shared_writer stays pointed
                    // at the PTY master writer once spawned. We only swap to sink
                    // when the PTY dies. So nothing to do here.
                }
            }
            InputMode::Agent => {
                // Switching back to agent mode.
                // The PTY stays alive in the background. Writer stays pointed
                // at the PTY so that if the user switches back, keystrokes
                // route correctly. Agent mode routes keys to InputEditor
                // before they reach Terminal::key_down(), so the writer
                // destination doesn't matter in agent mode.
            }
        }

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

        // Record to completion engine and history search
        self.record_submission(&command, InputMode::Terminal);

        // Log to session
        self.session_log.lock().log_command(&command);

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

        // Record to completion engine and history search
        self.record_submission(&content, InputMode::Agent);

        // ── Slash command routing ────────────────────────────────────
        if let Some((cmd_name, args)) = commands::parse_command(&content) {
            let model_name = self.screen.lock().model_name.clone();
            let result = commands::execute_command(cmd_name, args, &model_name);
            self.handle_command_result(&content, result);
            return;
        }

        // ── @ context attachment (with @symbol: support) ─────────────
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let bridge_guard = self.semantic_bridge.lock();
        let (attachments, augmented_content) =
            context::resolve_and_build_prompt_with_symbols(&content, &cwd, bridge_guard.as_ref());
        drop(bridge_guard);

        // Log to session
        self.session_log.lock().log_user(&content);

        // Write user prompt into chat area (show original, not augmented)
        let display_content = if attachments.is_empty() {
            content.clone()
        } else {
            let labels: Vec<&str> = attachments.iter().map(|a| a.label.as_str()).collect();
            format!("{content}\n  [attached: {}]", labels.join(", "))
        };
        self.write_ansi(&screen::format_user_prompt(&display_content));

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

        // Send to agent via bridge (use augmented content with file context)
        let _ = self.bridge.send_request(AgentRequest::SendMessage {
            content: augmented_content,
        });
    }

    /// Handle the result of a slash command execution.
    fn handle_command_result(&self, original_input: &str, result: CommandResult) {
        // Echo the command in chat
        self.write_ansi(&screen::format_command_prompt(original_input));

        match result {
            CommandResult::ChatMessage(msg) => {
                self.write_ansi(&screen::format_command_response(&msg));
            }
            CommandResult::AgentRequest(request) => {
                self.write_ansi(&screen::format_assistant_prefix());
                {
                    let mut ss = self.screen.lock();
                    ss.is_running = true;
                    ss.task_start = Some(Instant::now());
                    ss.task_elapsed_frozen = None;
                }
                *self.state.lock() = PaneState::Running;
                self.refresh_status_bar();
                let _ = self.bridge.send_request(request);
            }
            CommandResult::ClearChat => {
                self.write_ansi("\x1b[2J");
                let ss = self.screen.lock();
                let full = screen::render_full_screen(&ss);
                drop(ss);
                self.write_ansi(&full);
            }
            CommandResult::ExportSession(path) => {
                self.handle_export_to_path(&path);
            }
            CommandResult::OpenDiffViewer { staged } => {
                self.open_git_diff_viewer(staged);
            }
            CommandResult::Unknown(name) => {
                self.write_ansi(&screen::format_command_response(&format!(
                    "Unknown command: /{name}\nType /help for available commands."
                )));
            }
        }
    }

    /// Export session to a specific path (or default).
    fn handle_export_to_path(&self, path: &str) {
        if path.is_empty() {
            match self.session_log.lock().export_to_file() {
                Ok(p) => {
                    let info = "\x1b[38;2;125;207;255m";
                    self.write_ansi(&format!(
                        "\r\n{info}  Session exported to: {}{RESET}\r\n",
                        p.display(),
                        RESET = "\x1b[0m",
                    ));
                }
                Err(e) => {
                    let err = "\x1b[38;2;247;118;142m";
                    self.write_ansi(&format!(
                        "\r\n{err}  Export failed: {e}{RESET}\r\n",
                        RESET = "\x1b[0m",
                    ));
                }
            }
            return;
        }

        let target = PathBuf::from(path);
        let markdown = self.session_log.lock().export_markdown();

        match std::fs::write(&target, &markdown) {
            Ok(()) => {
                let info = "\x1b[38;2;125;207;255m";
                self.write_ansi(&format!(
                    "\r\n{info}  Session exported to: {}{RESET}\r\n",
                    target.display(),
                    RESET = "\x1b[0m",
                ));
            }
            Err(e) => {
                let err = "\x1b[38;2;247;118;142m";
                self.write_ansi(&format!(
                    "\r\n{err}  Export failed: {e}{RESET}\r\n",
                    RESET = "\x1b[0m",
                ));
            }
        }
    }

    /// Sync the InputEditor's current state into ScreenState for rendering.
    fn sync_editor_to_screen(&self) {
        let editor = self.input_editor.lock();
        let mut ss = self.screen.lock();
        ss.input_lines = editor.lines().to_vec();
        ss.cursor_row = editor.cursor_row();
        ss.cursor_col = editor.cursor_col();
        ss.ghost_text = editor.ghost_text().map(String::from);
        // Keep legacy field in sync for single-line fallback
        ss.input_text = editor.lines().first().cloned().unwrap_or_default();
    }

    /// Update ghost text suggestion based on current editor content.
    fn update_ghost_text(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let input = self.input_editor.lock().content();

        // Try completions with symbol index for richer suggestions
        let bridge_guard = self.semantic_bridge.lock();
        let completions = self
            .completion_engine
            .lock()
            .get_completions_with_symbols(&input, &cwd, bridge_guard.as_ref());
        drop(bridge_guard);

        let ghost = completions.first().and_then(|c| {
            c.text.strip_prefix(&*input).map(|suffix| suffix.to_string())
        }).filter(|s| !s.is_empty());
        self.input_editor.lock().set_ghost_text(ghost);
    }

    /// Record submitted text to the completion engine and history search.
    fn record_submission(&self, text: &str, mode: InputMode) {
        self.completion_engine.lock().add_history(text.to_string());

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
        self.history_search.lock().add_entry(HistoryRecord {
            text: text.to_string(),
            timestamp,
            mode,
            directory: cwd,
            use_count: 1,
        });
    }

    /// Execute a palette command string (e.g. "/help", "toggle_mode", "quick_fix").
    fn execute_palette_command(&self, command: &str) {
        match command {
            "toggle_mode" => self.toggle_input_mode(),
            "quick_fix" => self.handle_quick_fix(),
            "nav_prev" => self.navigate_block_prev(),
            "nav_next" => self.navigate_block_next(),
            "history_search" => {
                self.history_search.lock().open();
                self.render_history_search_overlay();
            }
            cmd if cmd.starts_with('/') => {
                // Route slash commands through the normal command handler
                if let Some((cmd_name, args)) = commands::parse_command(cmd) {
                    let model_name = self.screen.lock().model_name.clone();
                    let result = commands::execute_command(cmd_name, args, &model_name);
                    self.handle_command_result(cmd, result);
                }
            }
            _ => {}
        }
    }

    /// Render the palette overlay into the virtual terminal.
    fn render_palette_overlay(&self) {
        let palette = self.palette.lock();
        let width = self.screen.lock().width;
        let rendered = palette.render(width);
        drop(palette);
        if !rendered.is_empty() {
            self.write_ansi(&rendered);
        }
    }

    /// Render the history search overlay into the virtual terminal.
    fn render_history_search_overlay(&self) {
        let hs = self.history_search.lock();
        let width = self.screen.lock().width;
        let rendered = hs.render(width);
        drop(hs);
        if !rendered.is_empty() {
            self.write_ansi(&rendered);
        }
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

    // ── PTY lifecycle ──────────────────────────────────────────────────────

    /// Handle a key event while the diff viewer is active.
    ///
    /// Routes keys for navigation (j/k/n/]/[), actions (y/n/c/q), comment
    /// input, and hunk collapse (Space). Comment mode is checked first so
    /// that typing characters goes to the comment buffer.
    fn handle_diff_viewer_key(&self, key: KeyCode, mods: KeyModifiers) -> anyhow::Result<()> {
        let action = {
            let mut viewer_guard = self.diff_viewer.lock();
            let viewer = match viewer_guard.as_mut() {
                Some(v) => v,
                None => return Ok(()),
            };

            // ── Comment input mode (checked first) ──────────────
            if viewer.commenting {
                match key {
                    KeyCode::Enter if mods.is_empty() => viewer.submit_comment(),
                    KeyCode::Escape => viewer.cancel_comment(),
                    KeyCode::Backspace => viewer.comment_backspace(),
                    KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                        viewer.comment_insert_char(c);
                    }
                    _ => {}
                }
                return self.rerender_diff_viewer_locked(viewer_guard);
            }

            // ── Normal navigation / actions ─────────────────────
            match key {
                KeyCode::Char('y') | KeyCode::Char('Y') if mods.is_empty() => {
                    Some(ReviewAction::Approve)
                }
                KeyCode::Char('n') | KeyCode::Char('N') if mods.is_empty() => {
                    Some(ReviewAction::Reject)
                }
                KeyCode::Char('c') if mods.is_empty() => {
                    viewer.start_comment();
                    None
                }
                KeyCode::Char('j') | KeyCode::DownArrow if mods.is_empty() => {
                    viewer.move_down();
                    None
                }
                KeyCode::Char('k') | KeyCode::UpArrow if mods.is_empty() => {
                    viewer.move_up();
                    None
                }
                KeyCode::Char(']') if mods.is_empty() => {
                    viewer.next_file();
                    None
                }
                KeyCode::Char('[') if mods.is_empty() => {
                    viewer.prev_file();
                    None
                }
                KeyCode::Char(' ') if mods.is_empty() => {
                    viewer.toggle_hunk_collapse();
                    None
                }
                KeyCode::Char('q') | KeyCode::Escape => {
                    drop(viewer_guard);
                    *self.diff_viewer.lock() = None;
                    self.write_ansi(
                        "\r\n\x1b[38;2;86;95;137m\x1b[2m[Diff viewer closed]\x1b[0m\r\n",
                    );
                    return Ok(());
                }
                _ => None,
            }
        };

        if let Some(review_action) = action {
            // Extract comments and send review feedback
            let (file_path, comments, approved) = {
                let viewer = self.diff_viewer.lock();
                let v = viewer.as_ref().expect("diff_viewer should be Some");
                let fp = v
                    .current_diff()
                    .map(|d| d.new_path.clone())
                    .unwrap_or_default();
                let comments_vec: Vec<(String, usize, String)> = v
                    .comments
                    .iter()
                    .map(|c| {
                        let f = v
                            .diffs
                            .get(c.file_idx)
                            .map(|d| d.new_path.clone())
                            .unwrap_or_default();
                        (f, c.line_no, c.text.clone())
                    })
                    .collect();
                let approved = matches!(review_action, ReviewAction::Approve);
                (fp, comments_vec, approved)
            };

            *self.diff_viewer.lock() = None;

            let _ = self.bridge.send_request(AgentRequest::ReviewFeedback {
                file_path,
                comments,
                approved,
            });

            let msg = if approved {
                "\r\n\x1b[38;2;158;206;106m\x1b[1m\u{2714} Changes approved\x1b[0m\r\n"
            } else {
                "\r\n\x1b[38;2;247;118;142m\x1b[1m\u{2717} Changes rejected\x1b[0m\r\n"
            };
            self.write_ansi(msg);
            self.refresh_status_bar();
        } else {
            // Re-render after navigation
            let viewer = self.diff_viewer.lock();
            if let Some(ref v) = *viewer {
                let width = self.screen.lock().width as usize;
                let rendered = v.render(width);
                drop(viewer);
                self.write_ansi(&rendered);
            }
        }

        Ok(())
    }

    /// Re-render the diff viewer from a held lock, then write to terminal.
    fn rerender_diff_viewer_locked(
        &self,
        viewer_guard: parking_lot::MutexGuard<'_, Option<DiffViewer>>,
    ) -> anyhow::Result<()> {
        if let Some(ref v) = *viewer_guard {
            let width = self.screen.lock().width as usize;
            let rendered = v.render(width);
            drop(viewer_guard);
            self.write_ansi(&rendered);
        }
        Ok(())
    }

    /// Open the diff viewer for git diff output (from `/diff` command).
    fn open_git_diff_viewer(&self, staged: bool) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        match diff::git_diff(&cwd, staged) {
            Ok(diffs) => {
                if diffs.is_empty() {
                    self.write_ansi(&screen::format_command_response(
                        "No changes (working tree clean).",
                    ));
                    return;
                }

                let desc = if staged {
                    "Staged changes"
                } else {
                    "Working directory changes"
                };
                let viewer = DiffViewer::new(diffs, desc.to_string());
                let width = self.screen.lock().width as usize;
                let rendered = viewer.render(width);
                *self.diff_viewer.lock() = Some(viewer);
                self.write_ansi(&rendered);
            }
            Err(e) => {
                self.write_ansi(&screen::format_error(&format!(
                    "Failed to get git diff: {e}"
                )));
            }
        }
    }

    /// Handle the PTY child process exit.
    ///
    /// Switches back to Agent mode, resets the SharedWriter to sink,
    /// cleans up the InnerPty, and shows the exit status in the chat area.
    fn handle_pty_exit(&self) {
        // Get exit status before cleanup
        let exit_info = {
            let mut pty_guard = self.inner_pty.lock();
            let info = pty_guard.as_mut().and_then(|p| {
                p.try_wait().map(|status| format!("Shell exited ({})", status))
            }).unwrap_or_else(|| "Shell exited".to_string());
            // Kill and drop the inner PTY
            *pty_guard = None;
            info
        };

        // Reset writer to sink
        self.shared_writer.swap_to_sink();

        // Switch back to agent mode
        {
            let mut editor = self.input_editor.lock();
            editor.set_mode(InputMode::Agent);
        }
        {
            let mut ss = self.screen.lock();
            ss.input_mode = InputMode::Agent;
        }

        // Show exit info in chat area
        self.write_ansi(&format!(
            "\r\n\x1b[38;2;86;95;137m\x1b[3m  {exit_info}\x1b[0m\r\n"
        ));

        self.refresh_input_box();
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
        // FileEdit rendering is handled directly in poll_responses
        AgentResponse::FileEdit { .. } => String::new(),
        // PtyScreenSnapshot is handled internally
        AgentResponse::PtyScreenSnapshot { .. } => String::new(),
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
        // In terminal mode with active PTY, send paste through Terminal
        let mode = self.input_editor.lock().mode();
        if mode == InputMode::Terminal && self.inner_pty.lock().is_some() {
            let mut terminal = self.terminal.lock();
            terminal.send_paste(text)?;
            self.seqno.fetch_add(1, Ordering::Release);
            return Ok(());
        }

        // Agent mode: treat pasted text as input editor content
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
        MutexGuard::map(self.pane_writer.lock(), |w| {
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

        // Resize the inner PTY if present
        {
            let pty_guard = self.inner_pty.lock();
            if let Some(ref inner) = *pty_guard {
                let pty_size = PtySize {
                    rows: size.rows as u16,
                    cols: size.cols as u16,
                    pixel_width: size.pixel_width as u16,
                    pixel_height: size.pixel_height as u16,
                };
                if let Err(e) = inner.resize(pty_size) {
                    tracing::warn!("Failed to resize PTY: {e}");
                }
            }
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

        // ── Diff viewer mode: route keys to the diff viewer ────────────
        if self.diff_viewer.lock().is_some() {
            return self.handle_diff_viewer_key(key, mods);
        }

        // ── Command palette mode: route all keys to palette ──────────
        {
            let palette_open = self.palette.lock().is_open();
            if palette_open {
                match key {
                    KeyCode::Escape => {
                        self.palette.lock().close();
                    }
                    KeyCode::Enter => {
                        let command = self.palette.lock().selected_command()
                            .map(String::from);
                        self.palette.lock().close();
                        if let Some(cmd) = command {
                            self.execute_palette_command(&cmd);
                        }
                    }
                    KeyCode::UpArrow => {
                        self.palette.lock().select_prev();
                        self.render_palette_overlay();
                    }
                    KeyCode::DownArrow => {
                        self.palette.lock().select_next();
                        self.render_palette_overlay();
                    }
                    KeyCode::Backspace => {
                        self.palette.lock().backspace();
                        self.render_palette_overlay();
                    }
                    KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                        self.palette.lock().type_char(c);
                        self.render_palette_overlay();
                    }
                    _ => {}
                }
                return Ok(());
            }
        }

        // ── History search mode: route all keys to history search ──────
        {
            let hs_open = self.history_search.lock().is_open();
            if hs_open {
                match key {
                    KeyCode::Escape => {
                        self.history_search.lock().close();
                    }
                    KeyCode::Enter => {
                        let text = self.history_search.lock().selected_text()
                            .map(String::from);
                        self.history_search.lock().close();
                        if let Some(selected) = text {
                            // Insert selected text into editor
                            let mut editor = self.input_editor.lock();
                            editor.clear();
                            for c in selected.chars() {
                                editor.insert_char(c);
                            }
                            drop(editor);
                            self.sync_editor_to_screen();
                            self.refresh_input_box();
                        }
                    }
                    KeyCode::UpArrow => {
                        self.history_search.lock().select_prev();
                        self.render_history_search_overlay();
                    }
                    KeyCode::DownArrow => {
                        self.history_search.lock().select_next();
                        self.render_history_search_overlay();
                    }
                    KeyCode::Backspace => {
                        self.history_search.lock().backspace();
                        self.render_history_search_overlay();
                    }
                    KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                        self.history_search.lock().type_char(c);
                        self.render_history_search_overlay();
                    }
                    _ => {}
                }
                return Ok(());
            }
        }

        // Ctrl+P toggles command palette
        if key == KeyCode::Char('p') && mods == KeyModifiers::CTRL {
            self.palette.lock().toggle();
            self.render_palette_overlay();
            return Ok(());
        }

        // Ctrl+R opens history search
        if key == KeyCode::Char('r') && mods == KeyModifiers::CTRL {
            self.history_search.lock().open();
            self.render_history_search_overlay();
            return Ok(());
        }

        // Ctrl+T toggles input mode (always intercepted, never forwarded to PTY)
        if key == KeyCode::Char('t') && mods == KeyModifiers::CTRL {
            self.toggle_input_mode();
            return Ok(());
        }

        // ── Terminal mode with active PTY: forward keys to PTY via Terminal ──
        let mode = self.input_editor.lock().mode();
        if mode == InputMode::Terminal && self.inner_pty.lock().is_some() {
            // Check if PTY child has exited — auto-switch back to agent mode
            {
                let pty_guard = self.inner_pty.lock();
                if let Some(ref pty) = *pty_guard {
                    if pty.is_dead() {
                        drop(pty_guard);
                        self.handle_pty_exit();
                        return Ok(());
                    }
                }
            }

            // Forward keystroke through Terminal::key_down(), which encodes it
            // and writes to SharedWriter -> PTY stdin.
            let mut terminal = self.terminal.lock();
            terminal.key_down(key, mods)?;
            self.seqno.fetch_add(1, Ordering::Release);
            return Ok(());
        }

        // ── Agent mode: route keys to InputEditor ────────────────────────────

        // Ctrl+F: quick-fix — send last detected error to the agent
        if key == KeyCode::Char('f') && mods == KeyModifiers::CTRL {
            self.handle_quick_fix();
            return Ok(());
        }

        // Tab: accept full ghost text suggestion (if at end of line and ghost present)
        if key == KeyCode::Tab && mods.is_empty() {
            let accepted = {
                let mut editor = self.input_editor.lock();
                if editor.at_end_of_line() {
                    editor.accept_suggestion()
                } else {
                    false
                }
            };
            if accepted {
                self.sync_editor_to_screen();
                self.refresh_input_box();
                return Ok(());
            }
        }

        // Right arrow at end of line: accept one word from ghost text
        if key == KeyCode::RightArrow && mods.is_empty() {
            let accepted = {
                let mut editor = self.input_editor.lock();
                if editor.at_end_of_line() && editor.ghost_text().is_some() {
                    editor.accept_next_word()
                } else {
                    false
                }
            };
            if accepted {
                self.update_ghost_text();
                self.sync_editor_to_screen();
                self.refresh_input_box();
                return Ok(());
            }
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
                            // NL auto-detection: classify input and auto-route
                            // high-confidence terminal commands to shell
                            let content = self.input_editor.lock().content();
                            let classification = self.nl_classifier.classify(&content);
                            if classification.mode == InputMode::Terminal
                                && classification.confidence >= 0.5
                            {
                                self.submit_command();
                            } else {
                                self.submit_input();
                            }
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
            // Update ghost text suggestion from completion engine
            self.update_ghost_text();
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
        if *self.dead.lock() {
            return true;
        }
        // Check if the PTY child has exited while we are in terminal mode
        let pty_guard = self.inner_pty.lock();
        if let Some(ref pty) = *pty_guard {
            if pty.is_dead() {
                // Don't auto-switch here — that's done in poll_responses/key_down
                return false; // Pane is not dead, just the PTY child
            }
        }
        false
    }

    fn kill(&self) {
        // Kill the inner PTY if present
        {
            let mut pty_guard = self.inner_pty.lock();
            if let Some(ref mut pty) = *pty_guard {
                pty.kill();
            }
            *pty_guard = None;
        }
        self.shared_writer.swap_to_sink();

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
        // When PTY is active, check if the running program grabbed the mouse
        if self.inner_pty.lock().is_some() {
            return self.terminal.lock().is_mouse_grabbed();
        }
        false
    }

    fn is_alt_screen_active(&self) -> bool {
        // When PTY is active, check if a full-screen program (vim, htop) is running
        if self.inner_pty.lock().is_some() {
            return self.terminal.lock().is_alt_screen_active();
        }
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
