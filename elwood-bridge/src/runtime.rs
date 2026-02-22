//! Runtime bridge between WezTerm's smol executor and elwood-core's tokio runtime.
//!
//! WezTerm runs on smol; elwood-core runs on tokio. This module spawns a dedicated
//! background thread running a tokio `Runtime` and uses `flume` channels for
//! bidirectional communication. `flume` works with both async runtimes and sync code.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────┐    flume channels    ┌──────────────────┐
//! │  WezTerm (smol) │ ◄──────────────────► │  tokio thread    │
//! │  ElwoodPane      │   AgentRequest →     │  Agent loop      │
//! │  polls responses │   ← AgentResponse    │  Tool execution  │
//! └─────────────────┘                      └──────────────────┘
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Whether the input box is in Agent mode (natural language) or Terminal mode (shell commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Natural language messages sent to the LLM agent.
    #[default]
    Agent,
    /// Shell commands executed via `$SHELL -c`.
    Terminal,
}

/// Request from WezTerm (smol-side) to the agent (tokio-side).
#[derive(Debug, Clone)]
pub enum AgentRequest {
    /// Start a new agent session with the given prompt.
    Start {
        prompt: String,
        session_id: String,
        working_dir: Option<String>,
    },

    /// Send a follow-up message to an active agent session.
    SendMessage { content: String },

    /// Run a shell command (Terminal mode or `!` prefix).
    RunCommand {
        command: String,
        working_dir: Option<String>,
    },

    /// User responded to a permission request.
    PermissionResponse { request_id: String, granted: bool },

    /// User reviewed a proposed file edit (from diff viewer).
    ReviewFeedback {
        /// Path of the file being reviewed.
        file_path: String,
        /// Inline comments: (file, line_number, comment_text).
        comments: Vec<(String, usize, String)>,
        /// Whether the user approved the changes.
        approved: bool,
    },

    /// Cancel the current agent operation.
    Cancel,

    /// Agent wants to write keystrokes/text to the PTY.
    PtyWrite {
        /// The text to send to PTY stdin.
        input: String,
        /// Human-readable description of what the agent is doing.
        description: String,
    },

    /// Agent requests a screen snapshot from the PTY.
    PtyReadScreen,

    /// Agent requests to see content from other terminal panes.
    ObservePanes,

    /// Switch the active model to the named model.
    SwitchModel { model_name: String },

    /// Generate a structured implementation plan for the given description.
    GeneratePlan { description: String },

    /// Run a saved workflow's resolved steps sequentially.
    WorkflowRun {
        /// Workflow name (for display).
        name: String,
        /// Resolved (command, description, continue_on_error) triples.
        steps: Vec<(String, String, bool)>,
    },

    /// Run a command in the background (from `/bg` or `&` suffix).
    RunBackgroundCommand {
        command: String,
        working_dir: Option<String>,
    },

    /// Kill a running background job.
    KillJob { job_id: u32 },

    /// Gracefully shut down the agent runtime.
    Shutdown,
}

/// Response from the agent (tokio-side) back to WezTerm (smol-side).
#[derive(Debug, Clone)]
pub enum AgentResponse {
    /// Agent produced output text (streaming content delta).
    ContentDelta(String),

    /// Agent is using a tool.
    ToolStart {
        tool_name: String,
        tool_id: String,
        input_preview: String,
    },

    /// Tool execution completed.
    ToolEnd {
        tool_id: String,
        success: bool,
        output_preview: String,
    },

    /// Agent needs permission to perform an action.
    PermissionRequest {
        request_id: String,
        tool_name: String,
        description: String,
    },

    /// Agent turn completed (idle, waiting for next prompt).
    TurnComplete {
        /// Summary of what was accomplished.
        summary: Option<String>,
    },

    /// Shell command execution completed.
    CommandOutput {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
    },

    /// Agent proposes a file edit, shown as an interactive diff.
    FileEdit {
        /// Path to the file being edited.
        file_path: String,
        /// Original file content.
        old_content: String,
        /// Proposed new file content.
        new_content: String,
        /// Human-readable description of the edit.
        description: String,
    },

    /// Screen snapshot from the PTY terminal.
    PtyScreenSnapshot {
        lines: Vec<String>,
        cursor_x: usize,
        cursor_y: i64,
        cols: usize,
        rows: usize,
        alt_screen: bool,
    },

    /// Snapshots of content from sibling terminal panes.
    PaneSnapshots {
        /// Serialized pane snapshots: (pane_id, title, lines, dimensions).
        snapshots: Vec<PaneSnapshotInfo>,
    },

    /// The active model was switched.
    ModelSwitched { model_name: String },

    /// Agent generated a structured implementation plan.
    PlanGenerated {
        /// The raw markdown plan text from the LLM.
        plan_markdown: String,
    },

    /// Cumulative cost update (sent after each agent turn).
    CostUpdate {
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    },

    /// A single workflow step completed.
    WorkflowStepResult {
        /// Workflow name.
        workflow_name: String,
        /// 0-based step index.
        step_index: usize,
        /// Total steps in the workflow.
        total_steps: usize,
        /// The resolved command that was run.
        command: String,
        /// Step description.
        description: String,
        /// Standard output.
        stdout: String,
        /// Standard error.
        stderr: String,
        /// Process exit code.
        exit_code: Option<i32>,
        /// Whether this was the last step (workflow complete).
        is_last: bool,
    },

    /// Background job status update.
    JobUpdate {
        /// The job ID.
        job_id: u32,
        /// New status: "running", "completed", "failed", "cancelled".
        status: String,
        /// Optional output chunk (stdout or stderr line).
        output_chunk: Option<String>,
        /// Whether this is stderr (vs stdout).
        is_stderr: bool,
        /// Exit code (set when status is "completed" or "failed").
        exit_code: Option<i32>,
        /// Process ID (set on first "running" update).
        pid: Option<u32>,
    },

    /// An error occurred in the agent.
    Error(String),

    /// Agent runtime is shutting down.
    Shutdown,
}

/// Serializable summary of a pane snapshot for the bridge protocol.
#[derive(Debug, Clone)]
pub struct PaneSnapshotInfo {
    /// The WezTerm pane ID.
    pub pane_id: usize,
    /// The pane's title.
    pub title: String,
    /// The visible text lines.
    pub lines: Vec<String>,
    /// Terminal dimensions (cols, rows).
    pub dimensions: (usize, usize),
    /// Cursor row position.
    pub cursor_row: i64,
}

/// Bridges the WezTerm (smol) and elwood-core (tokio) async runtimes.
///
/// The bridge owns a background thread running a tokio `Runtime`. Communication
/// happens via `flume` channels which work with both runtimes (and sync code).
pub struct RuntimeBridge {
    /// Send requests to the agent (tokio-side).
    request_tx: flume::Sender<AgentRequest>,

    /// Receive responses from the agent (smol-side polls this).
    response_rx: flume::Receiver<AgentResponse>,

    /// The background tokio thread handle.
    _tokio_thread: thread::JoinHandle<()>,

    /// Whether the bridge has been shut down.
    shutdown: Arc<AtomicBool>,
}

impl RuntimeBridge {
    /// Create a new RuntimeBridge. Spawns a dedicated thread running tokio.
    ///
    /// The `agent_loop` closure runs on the tokio thread. It receives:
    /// - A receiver for `AgentRequest` messages from WezTerm
    /// - A sender for `AgentResponse` messages back to WezTerm
    ///
    /// The closure should run the agent loop, processing requests and sending
    /// responses until it receives `AgentRequest::Shutdown`.
    pub fn new<F>(agent_loop: F) -> Self
    where
        F: FnOnce(flume::Receiver<AgentRequest>, flume::Sender<AgentResponse>)
            + Send
            + 'static,
    {
        let (request_tx, request_rx) = flume::unbounded::<AgentRequest>();
        let (response_tx, response_rx) = flume::unbounded::<AgentResponse>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::clone(&shutdown);

        let tokio_thread = thread::Builder::new()
            .name("elwood-tokio-runtime".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("elwood-tokio-worker")
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime for elwood bridge");

                rt.block_on(async {
                    agent_loop(request_rx, response_tx);
                });

                shutdown_flag.store(true, Ordering::Release);
            })
            .expect("failed to spawn elwood tokio thread");

        Self {
            request_tx,
            response_rx,
            _tokio_thread: tokio_thread,
            shutdown,
        }
    }

    /// Create a new RuntimeBridge with an async agent loop.
    ///
    /// Like [`new`](Self::new) but the closure returns a `Future`, allowing
    /// `await` for async operations like `CoreAgent::execute`.
    pub fn new_async<F, Fut>(agent_loop: F) -> Self
    where
        F: FnOnce(flume::Receiver<AgentRequest>, flume::Sender<AgentResponse>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let (request_tx, request_rx) = flume::unbounded::<AgentRequest>();
        let (response_tx, response_rx) = flume::unbounded::<AgentResponse>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_flag = Arc::clone(&shutdown);

        let tokio_thread = thread::Builder::new()
            .name("elwood-tokio-runtime".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("elwood-tokio-worker")
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime for elwood bridge");

                rt.block_on(agent_loop(request_rx, response_tx));

                shutdown_flag.store(true, Ordering::Release);
            })
            .expect("failed to spawn elwood tokio thread");

        Self {
            request_tx,
            response_rx,
            _tokio_thread: tokio_thread,
            shutdown,
        }
    }

    /// Send a request to the agent (non-blocking).
    ///
    /// Returns `Err` if the agent runtime has shut down.
    pub fn send_request(&self, request: AgentRequest) -> Result<(), flume::SendError<AgentRequest>> {
        self.request_tx.send(request)
    }

    /// Try to receive a response from the agent (non-blocking).
    ///
    /// Returns `Ok(Some(response))` if a response is available,
    /// `Ok(None)` if no response is ready yet, or `Err` if the channel is closed.
    pub fn try_recv_response(&self) -> Result<Option<AgentResponse>, flume::RecvError> {
        match self.response_rx.try_recv() {
            Ok(resp) => Ok(Some(resp)),
            Err(flume::TryRecvError::Empty) => Ok(None),
            Err(flume::TryRecvError::Disconnected) => Err(flume::RecvError::Disconnected),
        }
    }

    /// Receive a response from the agent (blocking).
    pub fn recv_response(&self) -> Result<AgentResponse, flume::RecvError> {
        self.response_rx.recv()
    }

    /// Get an async receiver that works with smol's executor.
    ///
    /// `flume::Receiver::recv_async()` returns a future compatible with any runtime.
    pub fn response_receiver(&self) -> &flume::Receiver<AgentResponse> {
        &self.response_rx
    }

    /// Check if the bridge has been shut down.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Request a graceful shutdown of the agent runtime.
    pub fn request_shutdown(&self) {
        let _ = self.request_tx.send(AgentRequest::Shutdown);
    }
}

impl Drop for RuntimeBridge {
    fn drop(&mut self) {
        if !self.is_shutdown() {
            self.request_shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_send_recv() {
        let bridge = RuntimeBridge::new(|request_rx, response_tx| {
            // Simple echo agent loop
            while let Ok(req) = request_rx.recv() {
                match req {
                    AgentRequest::SendMessage { content } => {
                        let _ = response_tx.send(AgentResponse::ContentDelta(
                            format!("echo: {content}"),
                        ));
                        let _ = response_tx.send(AgentResponse::TurnComplete { summary: None });
                    }
                    AgentRequest::Shutdown => {
                        let _ = response_tx.send(AgentResponse::Shutdown);
                        break;
                    }
                    _ => {}
                }
            }
        });

        bridge
            .send_request(AgentRequest::SendMessage {
                content: "hello".into(),
            })
            .unwrap();

        let resp = bridge.recv_response().unwrap();
        match resp {
            AgentResponse::ContentDelta(s) => assert_eq!(s, "echo: hello"),
            other => panic!("unexpected response: {other:?}"),
        }

        bridge.request_shutdown();
        // Drain remaining responses
        while let Ok(resp) = bridge.recv_response() {
            if matches!(resp, AgentResponse::Shutdown) {
                break;
            }
        }
    }

    #[test]
    fn test_bridge_shutdown_flag() {
        let bridge = RuntimeBridge::new(|request_rx, response_tx| {
            while let Ok(req) = request_rx.recv() {
                if matches!(req, AgentRequest::Shutdown) {
                    let _ = response_tx.send(AgentResponse::Shutdown);
                    break;
                }
            }
        });

        assert!(!bridge.is_shutdown());
        bridge.request_shutdown();

        // Wait for shutdown
        while let Ok(resp) = bridge.recv_response() {
            if matches!(resp, AgentResponse::Shutdown) {
                break;
            }
        }
        // Give the thread a moment to set the flag
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(bridge.is_shutdown());
    }
}
