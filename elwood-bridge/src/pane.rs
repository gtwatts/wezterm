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
use crate::lua_api::{self, LuaEventArg, LuaEventDispatcher};
use crate::file_browser::FileTree;
use crate::fuzzy_finder::{self, FuzzyFinder, FileSource, SlashCommandSource, HistorySource, FuzzyAction};
use crate::semantic_bridge::SemanticBridge;
use crate::diff_viewer::{DiffViewer, ReviewAction};
use crate::editor::InputEditor;
use crate::git_info;
use crate::git_ui::{self, CommitView, StagingView};
use crate::history_search::{HistoryRecord, HistorySearch};
use crate::nl_classifier::NlClassifier;
use crate::notification::{self, ToastAction, ToastLevel, ToastManager};
use crate::observer::{ContentDetector, ContentType, NextCommandSuggester, PaneObserver};
use crate::palette::CommandPalette;
use crate::plan_mode;
use crate::plan_viewer::PlanViewer;
use crate::suggestion_overlay::SuggestionManager;
use crate::prediction_engine::{PredictionContext, PredictionEngine};
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
    /// Next-command prediction engine (rules + history bigrams + LLM).
    prediction_engine: Mutex<PredictionEngine>,
    /// Cross-pane observer for terminal awareness (reads sibling pane content).
    pane_observer: PaneObserver,
    /// File browser overlay (F2).
    file_browser: Mutex<Option<FileTree>>,
    /// Lua plugin event dispatcher for user-defined hooks.
    lua_events: Mutex<Option<LuaEventDispatcher>>,
    /// Suggestion overlay — shows error-fix suggestions from ContentDetector.
    suggestion_manager: Mutex<SuggestionManager>,
    /// Interactive git staging view (`/git stage`).
    staging_view: Mutex<Option<StagingView>>,
    /// Interactive git commit view (`/git commit`).
    commit_view: Mutex<Option<CommitView>>,
    /// Interactive plan viewer overlay. When `Some`, key events are routed here.
    plan_viewer: Mutex<Option<PlanViewer>>,
    /// Toast notification manager for proactive suggestions and status updates.
    toast_manager: Mutex<ToastManager>,
    /// Fuzzy finder overlay (Ctrl+F).
    fuzzy_finder: Mutex<Option<FuzzyFinder>>,
    /// Terminal session recorder (asciinema v2 format).
    recorder: Mutex<crate::recording::SessionRecorder>,
}

/// A pending permission request waiting for user approval.
#[derive(Debug, Clone)]
struct PendingPermission {
    request_id: String,
    tool_name: String,
}

/// Generate a simple conventional commit message from staged file statuses.
fn generate_simple_commit_message(files: &[git_ui::GitFileStatus]) -> String {
    let staged: Vec<_> = files.iter().filter(|f| f.staged).collect();
    if staged.is_empty() { return String::new(); }
    let (mut added, mut modified, mut deleted) = (0usize, 0usize, 0usize);
    for f in &staged {
        match f.status {
            git_ui::FileStatus::Added | git_ui::FileStatus::Untracked => added += 1,
            git_ui::FileStatus::Modified | git_ui::FileStatus::Renamed | git_ui::FileStatus::Copied => modified += 1,
            git_ui::FileStatus::Deleted => deleted += 1,
        }
    }
    let verb = if added >= modified && added >= deleted { "add" } else if deleted >= modified { "remove" } else { "update" };
    if staged.len() == 1 {
        let name = staged[0].path.rsplit('/').next().unwrap_or(&staged[0].path);
        format!("{verb}: {name}")
    } else {
        let first_dir = staged[0].path.split('/').next().unwrap_or("");
        let all_same_dir = staged.iter().all(|f| f.path.starts_with(first_dir));
        if all_same_dir && !first_dir.is_empty() && first_dir != staged[0].path {
            format!("{verb}: {} files in {first_dir}/", staged.len())
        } else {
            format!("{verb}: {} files", staged.len())
        }
    }
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
            prediction_engine: Mutex::new(PredictionEngine::new()),
            pane_observer: PaneObserver::new(pane_id),
            file_browser: Mutex::new(None),
            lua_events: Mutex::new(LuaEventDispatcher::try_new()),
            suggestion_manager: Mutex::new(SuggestionManager::new()),
            staging_view: Mutex::new(None),
            commit_view: Mutex::new(None),
            plan_viewer: Mutex::new(None),
            toast_manager: Mutex::new(ToastManager::new()),
            fuzzy_finder: Mutex::new(None),
            recorder: Mutex::new(crate::recording::SessionRecorder::new()),
        };

        // Start observing sibling panes for cross-pane awareness.
        // subscribe_all() watches all panes (empty subscription = observe everything).
        pane.pane_observer.subscribe_all();
        pane.pane_observer.start_observing();

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

    /// Render the toast notification overlay if any toasts are visible.
    fn render_toasts(&self) {
        let tm = self.toast_manager.lock();
        if !tm.has_visible() {
            return;
        }
        let toasts = tm.visible_toasts();
        let ss = self.screen.lock();
        let overlay = notification::render_toast_overlay(toasts, ss.width, ss.chat_top());
        drop(ss);
        drop(tm);
        self.write_ansi(&overlay);
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
                            // Toast: agent turn completed
                            if let Some(elapsed) = ss.task_elapsed_frozen {
                                if elapsed >= 5 {
                                    self.toast_manager.lock().push(
                                        "Agent turn complete",
                                        ToastLevel::Success,
                                        None,
                                        None,
                                    );
                                    self.render_toasts();
                                }
                            }
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

                            // Record command in prediction engine for bigram tracking
                            self.prediction_engine.lock().record_command(command, code);

                            // Populate suggestion overlay with structured detections
                            let error_detections = self.detector.detect_errors(&lines);
                            {
                                let mut sm = self.suggestion_manager.lock();
                                sm.clear();
                                if !error_detections.is_empty() {
                                    sm.add_batch(&error_detections);
                                }
                            }

                            // Toast: errors in output or long-running success
                            if !error_detections.is_empty() {
                                let first_msg: String =
                                    error_detections[0].message.chars().take(40).collect();
                                self.toast_manager.lock().push_with_detail(
                                    format!("Error in `{}`", truncate_cmd(command, 20)),
                                    first_msg,
                                    ToastLevel::Error,
                                    None,
                                    Some(ToastAction::SendToAgent(format!(
                                        "Fix the error from `{command}`"
                                    ))),
                                );
                                self.render_toasts();
                            } else if success {
                                let task_elapsed = self.screen.lock().task_elapsed_frozen;
                                if let Some(secs) = task_elapsed {
                                    if secs >= 5 {
                                        self.toast_manager.lock().push(
                                            format!("Command finished ({secs}s)"),
                                            ToastLevel::Info,
                                            None,
                                            None,
                                        );
                                        self.render_toasts();
                                    }
                                }
                            }

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

                                // Render the structured suggestion overlay
                                let sm = self.suggestion_manager.lock();
                                if let Some(active) = sm.active() {
                                    let ss = self.screen.lock();
                                    let overlay = screen::render_suggestion_overlay(
                                        &ss,
                                        active,
                                        sm.visible_count(),
                                    );
                                    drop(ss);
                                    drop(sm);
                                    self.write_ansi(&overlay);
                                }
                            } else if let Some(suggestion) = self.suggester.suggest(command, success) {
                                // No errors but there is a next-command suggestion
                                self.write_ansi(&screen::format_next_command_suggestion(suggestion));
                            }

                            // Set prediction as ghost text in Terminal mode
                            if self.input_editor.lock().mode() == InputMode::Terminal {
                                let cwd = std::env::current_dir()
                                    .unwrap_or_else(|_| PathBuf::from("."));
                                let git_branch = self.screen.lock().git_info
                                    .as_ref()
                                    .map(|gi| gi.branch.clone());
                                let recent: Vec<String> = self.completion_engine.lock()
                                    .ghost_text("", &cwd)
                                    .into_iter()
                                    .collect();
                                let pred_ctx = PredictionContext {
                                    last_command: command.clone(),
                                    last_exit_code: code,
                                    working_dir: cwd,
                                    git_branch,
                                    recent_commands: recent,
                                };
                                if let Some(prediction) = self.prediction_engine.lock().predict(&pred_ctx) {
                                    self.input_editor.lock().set_ghost_text(
                                        Some(prediction.command),
                                    );
                                    self.sync_editor_to_screen();
                                    self.refresh_input_box();
                                }
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
                        AgentResponse::Error(ref msg) => {
                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.awaiting_permission = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                            drop(ss);
                            // Toast: agent error
                            self.toast_manager.lock().push(
                                msg.chars().take(50).collect::<String>(),
                                ToastLevel::Error,
                                None,
                                None,
                            );
                            self.render_toasts();
                        }
                        AgentResponse::PtyScreenSnapshot { .. } => {
                            // PTY screen snapshots are handled by the PTY pane, not here
                        }
                        AgentResponse::PaneSnapshots { .. } => {
                            // Pane snapshots are handled by the observer, not here
                        }
                        AgentResponse::ModelSwitched { ref model_name } => {
                            let mut ss = self.screen.lock();
                            ss.model_name = model_name.clone();
                        }
                        AgentResponse::PlanGenerated { ref plan_markdown } => {
                            *self.state.lock() = PaneState::Idle;
                            let mut ss = self.screen.lock();
                            ss.is_running = false;
                            ss.active_tool = None;
                            ss.tool_start = None;
                            ss.task_elapsed_frozen = ss.task_start
                                .map(|s| s.elapsed().as_secs());
                            ss.task_start = None;
                            let width = ss.width as usize;
                            drop(ss);

                            // Parse the plan markdown into a structured plan
                            let plan = plan_mode::parse_llm_plan(plan_markdown);

                            // Save the plan to disk
                            match plan_mode::save_plan(&plan) {
                                Ok(path) => {
                                    tracing::info!("Plan saved to {}", path.display());
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to save plan: {e}");
                                }
                            }

                            // Render the inline preview first
                            let inline = plan_mode::render_plan_inline(&plan, width);
                            self.write_ansi(&inline);

                            // Open the plan viewer overlay for approval
                            let viewer = PlanViewer::new(plan);
                            let rendered = viewer.render(self.screen.lock().width);
                            *self.plan_viewer.lock() = Some(viewer);
                            self.write_ansi(&rendered);
                        }
                        AgentResponse::CostUpdate {
                            input_tokens,
                            output_tokens,
                            cost_usd,
                        } => {
                            let mut ss = self.screen.lock();
                            ss.tokens_used = ss.tokens_used.saturating_add(
                                *input_tokens as usize + *output_tokens as usize,
                            );
                            ss.cost += cost_usd;
                        }
                        AgentResponse::WorkflowStepResult {
                            ref exit_code, is_last, ..
                        } => {
                            if *is_last {
                                // Workflow finished — return to idle
                                *self.state.lock() = PaneState::Idle;
                                let mut ss = self.screen.lock();
                                ss.is_running = false;
                                ss.active_tool = None;
                                ss.tool_start = None;
                                ss.task_elapsed_frozen = ss.task_start
                                    .map(|s| s.elapsed().as_secs());
                                ss.task_start = None;
                            } else {
                                // Show step progress in active tool slot
                                let mut ss = self.screen.lock();
                                if let AgentResponse::WorkflowStepResult {
                                    ref workflow_name,
                                    step_index,
                                    total_steps,
                                    ..
                                } = response {
                                    ss.active_tool = Some(format!(
                                        "{workflow_name} [{}/{}]",
                                        step_index + 1,
                                        total_steps,
                                    ));
                                }
                                let _ = exit_code; // suppress unused warning
                            }
                        }
                        AgentResponse::Shutdown => {
                            *self.dead.lock() = true;
                        }
                        AgentResponse::JobUpdate { .. } => {
                            // Handled by jobs panel (not yet wired)
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

                    // ── Lua plugin hooks ──────────────────────────────
                    self.dispatch_lua_event(&response);

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

    /// Dispatch a Lua plugin event for the given agent response.
    ///
    /// All Lua calls happen inside the `lua_events` lock. Side effects
    /// (rendering notifications, updating state, sending bridge requests)
    /// happen *after* the lock is released to avoid nested-lock deadlocks.
    fn dispatch_lua_event(&self, response: &AgentResponse) {
        let pid = self.pane_id as u64;

        // Collect the active tool name outside the lua lock if needed.
        let active_tool = match response {
            AgentResponse::ToolEnd { .. } => {
                self.screen.lock().active_tool.clone().unwrap_or_default()
            }
            _ => String::new(),
        };

        // Hold the lua lock only for dispatching; collect results into locals.
        let (notifications, approve_tool, approve_permission) = {
            let guard = self.lua_events.lock();
            let lua = match guard.as_ref() {
                Some(l) => l,
                None => return,
            };

            match response {
                AgentResponse::ContentDelta(text) => {
                    let n = lua.dispatch(
                        lua_api::EVENT_AGENT_MESSAGE,
                        pid,
                        &[LuaEventArg::Str(text.clone())],
                    );
                    (n, None, None)
                }
                AgentResponse::ToolStart { tool_name, input_preview, .. } => {
                    let (r, n) = lua.dispatch_with_result(
                        lua_api::EVENT_TOOL_START,
                        pid,
                        &[
                            LuaEventArg::Str(tool_name.clone()),
                            LuaEventArg::Str(input_preview.clone()),
                        ],
                    );
                    let approved = if r.approve == Some(true) {
                        Some(tool_name.clone())
                    } else {
                        None
                    };
                    (n, approved, None)
                }
                AgentResponse::ToolEnd { success, output_preview, .. } => {
                    let n = lua.dispatch(
                        lua_api::EVENT_TOOL_END,
                        pid,
                        &[
                            LuaEventArg::Str(active_tool),
                            LuaEventArg::Bool(*success),
                            LuaEventArg::Str(output_preview.clone()),
                        ],
                    );
                    (n, None, None)
                }
                AgentResponse::CommandOutput { command, exit_code, .. } => {
                    let n = lua.dispatch(
                        lua_api::EVENT_COMMAND_COMPLETE,
                        pid,
                        &[
                            LuaEventArg::Str(command.clone()),
                            LuaEventArg::OptInt(exit_code.map(|c| c as i64)),
                        ],
                    );
                    (n, None, None)
                }
                AgentResponse::PermissionRequest { request_id, tool_name, description } => {
                    let (r, n) = lua.dispatch_with_result(
                        lua_api::EVENT_PERMISSION_REQUEST,
                        pid,
                        &[
                            LuaEventArg::Str(tool_name.clone()),
                            LuaEventArg::Str(description.clone()),
                        ],
                    );
                    let approved = if r.approve == Some(true) {
                        Some((request_id.clone(), tool_name.clone()))
                    } else {
                        None
                    };
                    (n, None, approved)
                }
                AgentResponse::Error(msg) => {
                    let n = lua.dispatch(
                        lua_api::EVENT_ERROR_DETECTED,
                        pid,
                        &[
                            LuaEventArg::Str("agent_error".into()),
                            LuaEventArg::Str(msg.clone()),
                        ],
                    );
                    (n, None, None)
                }
                _ => return,
            }
        };
        // lua_events lock is dropped here

        // Side effects: render notifications
        self.render_lua_notifications(&notifications);

        // Log auto-approved tool starts
        if let Some(tool_name) = approve_tool {
            log::debug!("Lua hook auto-approved tool_start for {tool_name}");
        }

        // Auto-approve permission requests from Lua hooks
        if let Some((request_id, tool_name)) = approve_permission {
            log::info!("Lua hook auto-approved permission for {tool_name}");
            let _ = self.bridge.send_request(AgentRequest::PermissionResponse {
                request_id,
                granted: true,
            });
            *self.state.lock() = PaneState::Running;
            {
                let mut ss = self.screen.lock();
                ss.awaiting_permission = false;
                ss.is_running = true;
            }
            *self.pending_permission.lock() = None;
        }
    }

    /// Render notifications from Lua hooks into the chat area.
    fn render_lua_notifications(&self, notifications: &[String]) {
        for notification in notifications {
            self.write_ansi(&format!("\r\n\x1b[38;2;125;207;255m  [hook] {notification}\x1b[0m\r\n"));
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
        let (old_str, new_str) = match new_mode {
            InputMode::Agent => ("Terminal", "Agent"),
            InputMode::Terminal => ("Agent", "Terminal"),
        };
        let mut ss = self.screen.lock();
        ss.input_mode = new_mode;
        drop(ss);

        // Dispatch mode_change Lua hook
        let mode_notifs = {
            let guard = self.lua_events.lock();
            match guard.as_ref() {
                Some(lua) => lua.dispatch(
                    lua_api::EVENT_MODE_CHANGE,
                    self.pane_id as u64,
                    &[LuaEventArg::Str(old_str.into()), LuaEventArg::Str(new_str.into())],
                ),
                None => Vec::new(),
            }
        };
        self.render_lua_notifications(&mode_notifs);

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

        // ── Cross-pane context injection ────────────────────────────
        // Include recent content from sibling panes so the agent has
        // awareness of what's happening in other terminal tabs/splits.
        let pane_context = self.pane_observer.format_context_for_agent(50);
        let final_content = if pane_context.is_empty() {
            augmented_content
        } else {
            format!("{pane_context}\n{augmented_content}")
        };

        // Send to agent via bridge (use augmented content with file + pane context)
        let _ = self.bridge.send_request(AgentRequest::SendMessage {
            content: final_content,
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
            CommandResult::GitStatus => {
                self.handle_git_status();
            }
            CommandResult::OpenStagingView => {
                self.open_staging_view();
            }
            CommandResult::OpenCommitFlow => {
                self.open_commit_flow();
            }
            CommandResult::GitPush => {
                self.handle_git_push();
            }
            CommandResult::GitLog { count } => {
                self.handle_git_log(count);
            }
            CommandResult::ListPanes => {
                self.handle_list_panes();
            }
            CommandResult::ListPlans => {
                self.handle_list_plans();
            }
            CommandResult::ResumePlan { id_prefix } => {
                self.handle_resume_plan(&id_prefix);
            }
            CommandResult::SwitchModel { model_name } => {
                let _ = self.bridge.send_request(AgentRequest::SwitchModel {
                    model_name,
                });
            }
            CommandResult::ExportFormatted { path, format } => { self.write_ansi(&screen::format_command_response(&format!("Export as {format} to: {path}"))); }
            CommandResult::ImportSession { path } => { self.write_ansi(&screen::format_command_response(&format!("Import session from: {path}"))); }
            CommandResult::ListBookmarks => {
                self.handle_list_bookmarks();
            }
            CommandResult::ExportBlock { index } => {
                self.handle_export_block(index);
            }
            CommandResult::RecordStart { filename } => {
                self.handle_record_start(filename.as_deref());
            }
            CommandResult::RecordStop => {
                self.handle_record_stop();
            }
            CommandResult::RecordPause => {
                self.handle_record_pause();
            }
            CommandResult::RecordResume => {
                self.handle_record_resume();
            }
            CommandResult::WorkflowResult(wf_result) => {
                use crate::workflow::WorkflowCommandResult;
                match wf_result {
                    WorkflowCommandResult::ChatMessage(msg) => {
                        self.write_ansi(&screen::format_command_response(&msg));
                    }
                    WorkflowCommandResult::RunSteps { name, steps } => {
                        // Show workflow header in chat
                        let step_count = steps.len();
                        self.write_ansi(&screen::format_command_response(
                            &format!("Running workflow '{name}' ({step_count} steps)..."),
                        ));
                        // Mark as running
                        {
                            let mut ss = self.screen.lock();
                            ss.is_running = true;
                            ss.task_start = Some(Instant::now());
                            ss.task_elapsed_frozen = None;
                        }
                        *self.state.lock() = PaneState::Running;
                        self.refresh_status_bar();
                        // Send to agent runtime for execution
                        let _ = self.bridge.send_request(AgentRequest::WorkflowRun {
                            name,
                            steps,
                        });
                    }
                }
            }
            CommandResult::Unknown(name) => {
                self.write_ansi(&screen::format_command_response(&format!(
                    "Unknown command: /{name}\nType /help for available commands."
                )));
            }
            CommandResult::OpenJobsPanel
            | CommandResult::RunBackground { .. }
            | CommandResult::KillJob { .. } => {
                // Handled by jobs panel integration (not yet wired)
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

    /// Handle the `/panes` command — list sibling terminal panes with previews.
    fn handle_list_panes(&self) {
        let panes = PaneObserver::list_panes();
        let own_id = self.pane_id;

        if panes.len() <= 1 {
            self.write_ansi(&screen::format_command_response(
                "No sibling panes found. Split the terminal to see other panes.",
            ));
            return;
        }

        let mut msg = String::from("Sibling terminal panes:\n\n");
        for info in &panes {
            if info.pane_id == own_id {
                continue;
            }
            let process = info
                .foreground_process
                .as_deref()
                .unwrap_or("(unknown)");
            let cwd_str = info.cwd.as_deref().unwrap_or("");
            let status = if info.is_dead { " [dead]" } else { "" };
            msg.push_str(&format!(
                "  Pane {} | {}x{} | {} | {}{}\n",
                info.pane_id, info.cols, info.rows, process, cwd_str, status,
            ));

            if let Some(snap) = self.pane_observer.get_pane_content(info.pane_id) {
                let start = snap.lines.len().saturating_sub(5);
                for line in &snap.lines[start..] {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        msg.push_str(&format!("    | {trimmed}\n"));
                    }
                }
                msg.push('\n');
            }
        }

        let errors = self.pane_observer.detect_errors_in_siblings();
        if !errors.is_empty() {
            msg.push_str("Detected errors:\n");
            for (pane_id, summary) in &errors {
                msg.push_str(&format!("  [Pane {pane_id}] {summary}\n"));
            }
        }

        self.write_ansi(&screen::format_command_response(&msg));
    }

    /// `/record start` — begin terminal recording.
    fn handle_record_start(&self, filename: Option<&str>) {
        let mut rec = self.recorder.lock();
        if rec.is_recording() {
            drop(rec);
            self.write_ansi(&screen::format_command_response(
                "Already recording. Use /record stop first."
            ));
            return;
        }
        let ss = self.screen.lock();
        let (w, h) = (ss.width as u32, ss.height as u32);
        drop(ss);

        let path = if let Some(name) = filename {
            let p = std::path::PathBuf::from(name);
            rec.start_to(p.clone(), w, h);
            p
        } else {
            rec.start(w, h)
        };
        drop(rec);

        {
            let mut ss = self.screen.lock();
            ss.recording_active = true;
            ss.recording_paused = false;
        }
        self.refresh_status_bar();
        self.write_ansi(&screen::format_command_response(&format!(
            "Recording to {}", path.display()
        )));
    }

    /// `/record stop` — stop terminal recording.
    fn handle_record_stop(&self) {
        let mut rec = self.recorder.lock();
        if !rec.is_recording() {
            drop(rec);
            self.write_ansi(&screen::format_command_response(
                "Not recording. Use /record start first."
            ));
            return;
        }
        let events = rec.event_count();
        let duration = rec.elapsed_secs();
        let path = rec.stop();
        drop(rec);

        {
            let mut ss = self.screen.lock();
            ss.recording_active = false;
            ss.recording_paused = false;
        }
        self.refresh_status_bar();

        let path_str = path.map(|p| p.display().to_string()).unwrap_or_default();
        self.write_ansi(&screen::format_command_response(&format!(
            "Recording saved to {path_str} ({events} events, {duration:.1}s)"
        )));
    }

    /// `/record pause` — pause terminal recording.
    fn handle_record_pause(&self) {
        let mut rec = self.recorder.lock();
        if !rec.is_recording() {
            drop(rec);
            self.write_ansi(&screen::format_command_response("Not recording."));
            return;
        }
        if rec.is_paused() {
            drop(rec);
            self.write_ansi(&screen::format_command_response("Recording already paused."));
            return;
        }
        rec.pause();
        drop(rec);
        {
            let mut ss = self.screen.lock();
            ss.recording_paused = true;
        }
        self.refresh_status_bar();
        self.write_ansi(&screen::format_command_response("Recording paused"));
    }

    /// `/record resume` — resume terminal recording.
    fn handle_record_resume(&self) {
        let mut rec = self.recorder.lock();
        if !rec.is_recording() {
            drop(rec);
            self.write_ansi(&screen::format_command_response("Not recording."));
            return;
        }
        if !rec.is_paused() {
            drop(rec);
            self.write_ansi(&screen::format_command_response("Recording is not paused."));
            return;
        }
        rec.resume();
        drop(rec);
        {
            let mut ss = self.screen.lock();
            ss.recording_paused = false;
        }
        self.refresh_status_bar();
        self.write_ansi(&screen::format_command_response("Recording resumed"));
    }

    /// `/bookmarks` — list all bookmarked blocks with their header summaries.
    fn handle_list_bookmarks(&self) {
        let mgr = self.block_manager.lock();
        let bookmarked = mgr.bookmarked_blocks_with_index();

        if bookmarked.is_empty() {
            self.write_ansi(&screen::format_command_response(
                "No bookmarked blocks.\nUse ] / [ to navigate blocks, then 'b' to bookmark.",
            ));
            return;
        }

        let mut msg = format!("Bookmarked blocks ({}):\n\n", bookmarked.len());
        for (idx, block) in &bookmarked {
            let exit_str = match block.exit_code {
                Some(0) => " [ok]".to_string(),
                Some(n) => format!(" [exit {n}]"),
                None => String::new(),
            };
            let dur_str = block
                .duration_secs()
                .map(|d| format!(" ({d:.1}s)"))
                .unwrap_or_default();
            msg.push_str(&format!(
                "  [{idx}] Block #{}{exit_str}{dur_str}\n",
                block.id,
            ));
        }
        msg.push_str("\nUse /export block <index> to export a block as markdown.");
        drop(mgr);
        self.write_ansi(&screen::format_command_response(&msg));
    }

    /// `/export block [index]` — export a block as markdown.
    fn handle_export_block(&self, index: Option<usize>) {
        let mgr = self.block_manager.lock();

        // Determine which block to export: explicit index, selected, or last
        let target_index = index
            .or_else(|| mgr.selected_index())
            .unwrap_or_else(|| mgr.len().saturating_sub(1));

        match mgr.export_block_markdown(target_index) {
            Some(markdown) => {
                drop(mgr);
                self.write_ansi(&screen::format_command_response(&markdown));
            }
            None => {
                drop(mgr);
                self.write_ansi(&screen::format_command_response(&format!(
                    "No block at index {target_index}.\nUse /bookmarks to see available blocks."
                )));
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
            "fuzzy_finder" => {
                self.open_fuzzy_finder();
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

    // ── Fuzzy finder (Ctrl+F) ──────────────────────────────────────────────

    /// Open the fuzzy finder overlay with all sources.
    fn open_fuzzy_finder(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Collect history texts for the HistorySource
        let history_texts: Vec<String> = self
            .history_search
            .lock()
            .entries()
            .iter()
            .rev()
            .take(500)
            .map(|r| r.text.clone())
            .collect();

        let sources: Vec<Box<dyn fuzzy_finder::FuzzySource>> = vec![
            Box::new(FileSource::new(cwd)),
            Box::new(SlashCommandSource::new()),
            Box::new(HistorySource::from_texts(history_texts)),
        ];
        *self.fuzzy_finder.lock() = Some(FuzzyFinder::new(sources));
        self.render_fuzzy_finder_overlay();
    }

    /// Render the fuzzy finder overlay into the virtual terminal.
    fn render_fuzzy_finder_overlay(&self) {
        let ff = self.fuzzy_finder.lock();
        if let Some(ref finder) = *ff {
            let ss = self.screen.lock();
            let rendered = finder.render(ss.width as usize, ss.height as usize);
            drop(ss);
            drop(ff);
            if !rendered.is_empty() {
                self.write_ansi(&rendered);
            }
        }
    }

    /// Handle a key event while the fuzzy finder is open.
    fn handle_fuzzy_finder_key(&self, key: KeyCode, mods: KeyModifiers) {
        match key {
            KeyCode::Escape => {
                *self.fuzzy_finder.lock() = None;
            }
            KeyCode::Enter => {
                let action = self
                    .fuzzy_finder
                    .lock()
                    .as_ref()
                    .and_then(|f| f.selected_action().cloned());
                *self.fuzzy_finder.lock() = None;
                if let Some(action) = action {
                    self.execute_fuzzy_action(action);
                }
            }
            KeyCode::UpArrow => {
                if let Some(ref mut f) = *self.fuzzy_finder.lock() {
                    f.select_prev();
                }
                self.render_fuzzy_finder_overlay();
            }
            KeyCode::DownArrow => {
                if let Some(ref mut f) = *self.fuzzy_finder.lock() {
                    f.select_next();
                }
                self.render_fuzzy_finder_overlay();
            }
            KeyCode::Tab if mods.is_empty() => {
                if let Some(ref mut f) = *self.fuzzy_finder.lock() {
                    f.cycle_tab();
                }
                self.render_fuzzy_finder_overlay();
            }
            KeyCode::Backspace => {
                if let Some(ref mut f) = *self.fuzzy_finder.lock() {
                    f.backspace();
                }
                self.render_fuzzy_finder_overlay();
            }
            KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                if let Some(ref mut f) = *self.fuzzy_finder.lock() {
                    f.type_char(c);
                }
                self.render_fuzzy_finder_overlay();
            }
            _ => {}
        }
    }

    /// Execute the action from a selected fuzzy finder item.
    fn execute_fuzzy_action(&self, action: FuzzyAction) {
        match action {
            FuzzyAction::InsertText(text) => {
                let mut editor = self.input_editor.lock();
                editor.clear();
                for ch in text.chars() {
                    editor.insert_char(ch);
                }
                drop(editor);
                self.sync_editor_to_screen();
            }
            FuzzyAction::OpenFile(path) => {
                // Insert @file reference into the input
                let display = path.to_string_lossy();
                let text = format!("@{display} ");
                let mut editor = self.input_editor.lock();
                for ch in text.chars() {
                    editor.insert_char(ch);
                }
                drop(editor);
                self.sync_editor_to_screen();
            }
            FuzzyAction::AttachContext(ctx) => {
                let text = format!("@{ctx} ");
                let mut editor = self.input_editor.lock();
                for ch in text.chars() {
                    editor.insert_char(ch);
                }
                drop(editor);
                self.sync_editor_to_screen();
            }
            FuzzyAction::ScrollToBlock(idx) => {
                let mut bm = self.block_manager.lock();
                bm.select(idx);
            }
            FuzzyAction::ExecuteCommand(cmd) => {
                self.execute_palette_command(&cmd);
            }
        }
    }

    // ── File browser (F2) ───────────────────────────────────────────────────
    fn toggle_file_browser(&self) {
        let mut fb = self.file_browser.lock();
        if fb.is_some() { *fb = None; } else {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            *fb = Some(FileTree::new(cwd));
            drop(fb); self.render_file_browser_overlay();
        }
    }
    fn render_file_browser_overlay(&self) {
        let mut fb = self.file_browser.lock();
        if let Some(ref mut tree) = *fb {
            let ss = self.screen.lock();
            let rendered = tree.render(ss.width, ss.height);
            drop(ss); drop(fb); self.write_ansi(&rendered);
        }
    }
    fn handle_file_browser_key(&self, key: KeyCode, mods: KeyModifiers) {
        let filter_active = self.file_browser.lock().as_ref().map(|t| t.filter_active).unwrap_or(false);
        if filter_active {
            match key {
                KeyCode::Escape => { if let Some(ref mut t) = *self.file_browser.lock() { t.filter_active = false; t.filter_clear(); } self.render_file_browser_overlay(); }
                KeyCode::Enter => { if let Some(ref mut t) = *self.file_browser.lock() { t.filter_active = false; } self.render_file_browser_overlay(); }
                KeyCode::Backspace => { if let Some(ref mut t) = *self.file_browser.lock() { t.filter_backspace(); } self.render_file_browser_overlay(); }
                KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => { if let Some(ref mut t) = *self.file_browser.lock() { t.filter_insert_char(c); } self.render_file_browser_overlay(); }
                _ => {}
            }
            return;
        }
        match key {
            KeyCode::Escape | KeyCode::Char('q') | KeyCode::Function(2) => { *self.file_browser.lock() = None; }
            KeyCode::UpArrow | KeyCode::Char('k') if mods.is_empty() => { if let Some(ref mut t) = *self.file_browser.lock() { t.move_up(); } self.render_file_browser_overlay(); }
            KeyCode::DownArrow | KeyCode::Char('j') if mods.is_empty() => { if let Some(ref mut t) = *self.file_browser.lock() { t.move_down(); } self.render_file_browser_overlay(); }
            KeyCode::Enter if mods.is_empty() => { self.file_browser_action_open(); }
            KeyCode::Char(' ') if mods.is_empty() => {
                if let Some(ref mut t) = *self.file_browser.lock() {
                    let is_dir = t.selected_entry().map(|e| e.entry_type == crate::file_browser::EntryType::Directory).unwrap_or(false);
                    if is_dir { t.toggle_expand(); } else { t.show_preview = !t.show_preview; }
                }
                self.render_file_browser_overlay();
            }
            KeyCode::Char('@') if mods.is_empty() || mods == KeyModifiers::SHIFT => { self.file_browser_action_attach(); }
            KeyCode::Char('/') if mods.is_empty() => { if let Some(ref mut t) = *self.file_browser.lock() { t.filter_active = true; } self.render_file_browser_overlay(); }
            _ => {}
        }
    }
    fn file_browser_action_open(&self) {
        let info = { let mut fb = self.file_browser.lock(); fb.as_mut().and_then(|tree| { let entry = tree.selected_entry()?; let et = entry.entry_type; let p = entry.path.clone(); if et == crate::file_browser::EntryType::Directory { tree.toggle_expand(); None } else { Some(p) } }) };
        if info.is_none() { self.render_file_browser_overlay(); return; }
        let path = info.unwrap();
        *self.file_browser.lock() = None;
        let editor_cmd = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let command = format!("{editor_cmd} {}", path.display());
        self.write_ansi(&screen::format_command_prompt(&command));
        let working_dir = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
        let _ = self.bridge.send_request(AgentRequest::RunCommand { command, working_dir });
    }
    fn file_browser_action_attach(&self) {
        let path = self.file_browser.lock().as_ref().and_then(|tree| { tree.selected_entry().filter(|e| e.entry_type == crate::file_browser::EntryType::File).map(|e| e.path.clone()) });
        if let Some(path) = path {
            *self.file_browser.lock() = None;
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let rel = path.strip_prefix(&cwd).unwrap_or(&path).to_string_lossy().to_string();
            let at_ref = format!("@{rel} ");
            { let mut ed = self.input_editor.lock(); for c in at_ref.chars() { ed.insert_char(c); } }
            self.sync_editor_to_screen(); self.refresh_input_box();
            self.write_ansi(&format!("
[38;2;86;95;137m[2m[Attached: @{rel}][0m
"));
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

    // ── Git UI overlay handlers (staging view, commit view) ────────────

    fn handle_git_status(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match git_ui::format_git_status(&cwd) {
            Ok(o) => self.write_ansi(&format!("\r\n{o}")),
            Err(e) => self.write_ansi(&screen::format_error(&format!("git status failed: {e}"))),
        }
    }

    fn handle_git_log(&self, count: usize) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match git_ui::format_git_log(&cwd, count) {
            Ok(o) => self.write_ansi(&format!("\r\n{o}")),
            Err(e) => self.write_ansi(&screen::format_error(&format!("git log failed: {e}"))),
        }
    }

    fn handle_git_push(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let r = git_ui::git_push(&cwd);
        self.write_ansi(&git_ui::format_git_push_result(r));
        self.screen.lock().git_info = git_info::get_git_info(&cwd);
        self.refresh_status_bar();
    }

    fn open_staging_view(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match StagingView::new(&cwd) {
            Ok(v) => { let w = self.screen.lock().width as usize; let rendered = v.render(w); *self.staging_view.lock() = Some(v); self.write_ansi(&rendered); }
            Err(e) => self.write_ansi(&screen::format_command_response(&e)),
        }
    }

    fn open_commit_flow(&self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let files = match git_ui::get_file_statuses(&cwd) { Ok(f) => f, Err(e) => { self.write_ansi(&screen::format_error(&format!("git status: {e}"))); return; } };
        let sc = files.iter().filter(|f| f.staged).count();
        if sc == 0 { self.write_ansi(&screen::format_command_response("No files staged. Opening staging view...")); self.open_staging_view(); return; }
        let msg = generate_simple_commit_message(&files);
        let cv = CommitView::new(&cwd, msg, sc);
        let w = self.screen.lock().width as usize;
        let rendered = cv.render(w);
        *self.commit_view.lock() = Some(cv);
        self.write_ansi(&rendered);
    }

    fn handle_staging_view_key(&self, key: KeyCode, mods: KeyModifiers) {
        let act = { let mut g = self.staging_view.lock(); let v = match g.as_mut() { Some(v) => v, None => return }; match key { KeyCode::Char(' ') if mods.is_empty() => { v.toggle_current(); None } KeyCode::Char('a') | KeyCode::Char('A') if mods.is_empty() => { v.toggle_all(); None } KeyCode::Char('j') | KeyCode::DownArrow if mods.is_empty() => { v.move_down(); None } KeyCode::Char('k') | KeyCode::UpArrow if mods.is_empty() => { v.move_up(); None } KeyCode::Enter if mods.is_empty() => Some(("ok", v.staged_paths().len())), KeyCode::Escape => Some(("esc", 0)), _ => None, } };
        if let Some((a, n)) = act { *self.staging_view.lock() = None; if a == "ok" { self.write_ansi(&format!("\r\n\x1b[38;2;158;206;106m\x1b[1m{n} file{} staged\x1b[0m\r\n", if n == 1 { "" } else { "s" })); let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")); self.screen.lock().git_info = git_info::get_git_info(&cwd); self.refresh_status_bar(); } else { self.write_ansi("\r\n\x1b[38;2;86;95;137m\x1b[2m[Staging cancelled]\x1b[0m\r\n"); } }
        else { let g = self.staging_view.lock(); if let Some(ref v) = *g { let w = self.screen.lock().width as usize; let rendered = v.render(w); drop(g); self.write_ansi(&rendered); } }
    }

    fn handle_commit_view_key(&self, key: KeyCode, mods: KeyModifiers) {
        let act = { let mut g = self.commit_view.lock(); let v = match g.as_mut() { Some(v) => v, None => return }; if v.editing { match key { KeyCode::Escape => { v.editing = false; None } KeyCode::Enter if mods == KeyModifiers::SHIFT => { v.insert_newline(); None } KeyCode::Enter if mods.is_empty() => Some("commit"), KeyCode::Backspace => { v.backspace(); None } KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => { v.insert_char(c); None } _ => None, } } else { match key { KeyCode::Enter if mods.is_empty() => Some("commit"), KeyCode::Char('e') | KeyCode::Char('E') if mods.is_empty() => { v.start_edit(); None } KeyCode::Escape => Some("cancel"), _ => None, } } };
        if let Some(a) = act { if a == "commit" { let res = { self.commit_view.lock().as_ref().map(|v| v.commit()) }; *self.commit_view.lock() = None; match res { Some(Ok(out)) => { self.write_ansi(&format!("\r\n\x1b[38;2;158;206;106m\x1b[1mCommit successful\x1b[0m\r\n\x1b[38;2;192;202;245m{out}\x1b[0m\r\n")); let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")); self.screen.lock().git_info = git_info::get_git_info(&cwd); self.refresh_status_bar(); } Some(Err(e)) => self.write_ansi(&screen::format_error(&format!("Commit failed: {e}"))), None => {} } } else { *self.commit_view.lock() = None; self.write_ansi("\r\n\x1b[38;2;86;95;137m\x1b[2m[Commit cancelled]\x1b[0m\r\n"); } }
        else { let g = self.commit_view.lock(); if let Some(ref v) = *g { let w = self.screen.lock().width as usize; let rendered = v.render(w); drop(g); self.write_ansi(&rendered); } }
    }

    // ── Plan mode ───────────────────────────────────────────────────────
    fn handle_list_plans(&self) {
        let plans = plan_mode::list_plans();
        if plans.is_empty() {
            self.write_ansi(&screen::format_command_response("No saved plans. Use /plan <description> to create one."));
            return;
        }
        let mut msg = String::from("Saved plans:\n\n");
        for (path, title, status) in &plans {
            let id = path.file_stem().and_then(|n| n.to_str()).and_then(|n| n.get(..15)).unwrap_or("???");
            msg.push_str(&format!("  {id}  [{status}]  {title}\n"));
        }
        msg.push_str("\nUse /plan resume <id-prefix> to resume a plan.");
        self.write_ansi(&screen::format_command_response(&msg));
    }

    fn handle_resume_plan(&self, id_prefix: &str) {
        let plans = plan_mode::list_plans();
        let matching: Vec<_> = plans.iter().filter(|(path, _, _)| {
            path.file_stem().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(id_prefix))
        }).collect();
        match matching.len() {
            0 => { self.write_ansi(&screen::format_command_response(&format!("No plan found matching '{id_prefix}'."))); }
            1 => {
                let (path, _, _) = matching[0];
                match plan_mode::load_plan(path) {
                    Ok(plan) => {
                        let viewer = PlanViewer::new(plan);
                        let rendered = viewer.render(self.screen.lock().width);
                        *self.plan_viewer.lock() = Some(viewer);
                        self.write_ansi(&rendered);
                    }
                    Err(e) => { self.write_ansi(&screen::format_error(&format!("Failed to load plan: {e}"))); }
                }
            }
            _ => {
                let mut msg = format!("Multiple plans match '{id_prefix}'. Be more specific:\n\n");
                for (path, title, status) in &matching {
                    let id = path.file_stem().and_then(|n| n.to_str()).unwrap_or("???");
                    msg.push_str(&format!("  {id}  [{status}]  {title}\n"));
                }
                self.write_ansi(&screen::format_command_response(&msg));
            }
        }
    }

    fn handle_plan_viewer_key(&self, key: KeyCode, mods: KeyModifiers) -> anyhow::Result<()> {
        let action = {
            let mut vg = self.plan_viewer.lock();
            let viewer = match vg.as_mut() { Some(v) => v, None => return Ok(()) };
            if viewer.editing {
                match key {
                    KeyCode::Enter if mods.is_empty() => viewer.submit_edit(),
                    KeyCode::Escape => viewer.cancel_edit(),
                    KeyCode::Backspace => viewer.edit_backspace(),
                    KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => viewer.edit_insert_char(c),
                    _ => {}
                }
                let rendered = viewer.render(self.screen.lock().width);
                drop(vg);
                self.write_ansi(&rendered);
                return Ok(());
            }
            match key {
                KeyCode::Enter if mods.is_empty() => {
                    viewer.plan.status = plan_mode::PlanStatus::Approved;
                    let _ = plan_mode::save_plan(&viewer.plan);
                    Some(crate::plan_viewer::PlanAction::Approve)
                }
                KeyCode::Char('q') | KeyCode::Escape => Some(crate::plan_viewer::PlanAction::Cancel),
                KeyCode::Char('j') | KeyCode::DownArrow if mods.is_empty() => { viewer.move_down(); None }
                KeyCode::Char('k') | KeyCode::UpArrow if mods.is_empty() => { viewer.move_up(); None }
                KeyCode::Char(' ') if mods.is_empty() => { viewer.toggle_current(); None }
                KeyCode::Char('e') if mods.is_empty() => { viewer.start_edit(); None }
                _ => None,
            }
        };
        if let Some(pa) = action {
            match pa {
                crate::plan_viewer::PlanAction::Approve => {
                    let plan = { self.plan_viewer.lock().as_ref().map(|v| v.plan.clone()) };
                    *self.plan_viewer.lock() = None;
                    self.write_ansi("\r\n\x1b[38;2;158;206;106m\x1b[1m\u{2714} Plan approved\x1b[0m\r\n");
                    if let Some(mut plan) = plan {
                        plan.status = plan_mode::PlanStatus::InProgress;
                        let _ = plan_mode::save_plan(&plan);
                        if let Some(idx) = plan.next_step_index() {
                            let prompt = format!("Execute step {} of the plan: {}", idx + 1, plan.steps[idx].description);
                            self.write_ansi(&screen::format_assistant_prefix());
                            { let mut ss = self.screen.lock(); ss.is_running = true; ss.task_start = Some(Instant::now()); ss.task_elapsed_frozen = None; }
                            *self.state.lock() = PaneState::Running;
                            self.refresh_status_bar();
                            let _ = self.bridge.send_request(AgentRequest::SendMessage { content: prompt });
                        }
                    }
                }
                crate::plan_viewer::PlanAction::Cancel => {
                    *self.plan_viewer.lock() = None;
                    self.write_ansi("\r\n\x1b[38;2;86;95;137m\x1b[2m[Plan viewer closed]\x1b[0m\r\n");
                }
            }
        } else {
            let vg = self.plan_viewer.lock();
            if let Some(ref v) = *vg {
                let rendered = v.render(self.screen.lock().width);
                drop(vg);
                self.write_ansi(&rendered);
            }
        }
        Ok(())
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
        // PaneSnapshots are handled by the observer
        AgentResponse::PaneSnapshots { .. } => String::new(),
        AgentResponse::WorkflowStepResult {
            step_index,
            total_steps,
            command,
            description,
            stdout,
            stderr,
            exit_code,
            is_last,
            ..
        } => {
            let step_label = format!(
                "[{}/{}] {}",
                step_index + 1,
                total_steps,
                if description.is_empty() { command.as_str() } else { description.as_str() },
            );
            let mut out = screen::format_command_prompt(&step_label);
            out.push_str(&screen::format_command_output(command, stdout, stderr, *exit_code));
            if *is_last {
                out.push_str(&screen::format_turn_complete(Some("Workflow complete")));
            }
            out
        }
        AgentResponse::Error(msg) => screen::format_error(msg),
        AgentResponse::Shutdown => screen::format_shutdown(),
        // Other response types handled by subsystems
        _ => String::new(),
    }
}


/// Truncate a command string for display in toast messages.
fn truncate_cmd(cmd: &str, max: usize) -> String {
    if cmd.len() <= max {
        cmd.to_string()
    } else {
        let mut end = max;
        while !cmd.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &cmd[..end])
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

        // ── Plan viewer mode: route keys to plan viewer ────────────
        if self.plan_viewer.lock().is_some() {
            return self.handle_plan_viewer_key(key, mods);
        }

        // ── File browser mode: route keys to file browser ────────────
        if self.file_browser.lock().is_some() {
            self.handle_file_browser_key(key, mods);
            return Ok(());
        }

        // ── Git staging view: route keys to staging view ────────────
        if self.staging_view.lock().is_some() {
            self.handle_staging_view_key(key, mods);
            return Ok(());
        }

        // ── Git commit view: route keys to commit view ──────────────
        if self.commit_view.lock().is_some() {
            self.handle_commit_view_key(key, mods);
            return Ok(());
        }

        // ── Suggestion overlay mode: Enter/Esc/Tab ──────────────────
        if self.suggestion_manager.lock().has_visible() {
            match key {
                KeyCode::Enter => {
                    let fix = self.suggestion_manager.lock().accept();
                    if let Some(fix_cmd) = fix {
                        // Insert the fix command into the input editor
                        let mut editor = self.input_editor.lock();
                        editor.clear();
                        for ch in fix_cmd.chars() {
                            editor.insert_char(ch);
                        }
                        drop(editor);
                        self.sync_editor_to_screen();
                    }
                    return Ok(());
                }
                KeyCode::Escape => {
                    self.suggestion_manager.lock().dismiss();
                    return Ok(());
                }
                KeyCode::Tab if mods.is_empty() => {
                    self.suggestion_manager.lock().next();
                    // Re-render the overlay with the new active suggestion
                    let sm = self.suggestion_manager.lock();
                    if let Some(active) = sm.active() {
                        let ss = self.screen.lock();
                        let overlay = screen::render_suggestion_overlay(
                            &ss,
                            active,
                            sm.visible_count(),
                        );
                        drop(ss);
                        drop(sm);
                        self.write_ansi(&overlay);
                    }
                    return Ok(());
                }
                _ => {
                    // Any other key dismisses the overlay and falls through
                    self.suggestion_manager.lock().clear();
                }
            }
        }

        // ── Toast notification mode: Esc dismiss, Enter accept action ──
        if self.toast_manager.lock().has_visible() {
            match key {
                KeyCode::Escape => {
                    self.toast_manager.lock().dismiss_top();
                    return Ok(());
                }
                KeyCode::Enter => {
                    let action = self.toast_manager.lock().accept_top();
                    if let Some(ToastAction::RunCommand(cmd)) = action {
                        let mut editor = self.input_editor.lock();
                        editor.clear();
                        for ch in cmd.chars() {
                            editor.insert_char(ch);
                        }
                        drop(editor);
                        self.sync_editor_to_screen();
                    } else if let Some(ToastAction::SendToAgent(msg)) = action {
                        let mut editor = self.input_editor.lock();
                        editor.clear();
                        for ch in msg.chars() {
                            editor.insert_char(ch);
                        }
                        drop(editor);
                        self.sync_editor_to_screen();
                    }
                    return Ok(());
                }
                _ => {
                    // Other keys dismiss toasts and fall through
                    self.toast_manager.lock().clear();
                }
            }
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

        // ── Fuzzy finder mode: route all keys to fuzzy finder ──────
        if self.fuzzy_finder.lock().is_some() {
            self.handle_fuzzy_finder_key(key, mods);
            return Ok(());
        }

        // F2 toggles file browser
        if key == KeyCode::Function(2) && mods.is_empty() {
            self.toggle_file_browser();
            return Ok(());
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

        // Ctrl+F: open fuzzy finder overlay
        if key == KeyCode::Char('f') && mods == KeyModifiers::CTRL {
            self.open_fuzzy_finder();
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

            // ── Selected-block navigation ([ and ]) ──────────────────
            // Only when input is empty so we don't eat characters mid-typing.
            KeyCode::Char('[') if mods.is_empty() && self.input_editor.lock().is_empty() => {
                let mut mgr = self.block_manager.lock();
                mgr.navigate_prev_selected();
                if let Some(first_row) = mgr.selected_block().and_then(|b| b.first_row()) {
                    drop(mgr);
                    self.scroll_to_row(first_row);
                }
                editor_changed = false;
            }
            KeyCode::Char(']') if mods.is_empty() && self.input_editor.lock().is_empty() => {
                let mut mgr = self.block_manager.lock();
                mgr.navigate_next_selected();
                if let Some(first_row) = mgr.selected_block().and_then(|b| b.first_row()) {
                    drop(mgr);
                    self.scroll_to_row(first_row);
                }
                editor_changed = false;
            }

            // ── Toggle collapse on selected block (c when input empty) ──
            KeyCode::Char('c') if mods.is_empty() && self.input_editor.lock().is_empty() => {
                let mut mgr = self.block_manager.lock();
                if let Some(idx) = mgr.selected_index() {
                    mgr.toggle_collapse_at(idx);
                }
                editor_changed = false;
            }

            // ── Toggle bookmark on selected block (b when input empty) ──
            KeyCode::Char('b') if mods.is_empty() && self.input_editor.lock().is_empty() => {
                let mut mgr = self.block_manager.lock();
                if let Some(idx) = mgr.selected_index() {
                    mgr.toggle_bookmark_at(idx);
                }
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
