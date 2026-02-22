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

    /// User responded to a permission request.
    PermissionResponse { request_id: String, granted: bool },

    /// Cancel the current agent operation.
    Cancel,

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

    /// An error occurred in the agent.
    Error(String),

    /// Agent runtime is shutting down.
    Shutdown,
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
