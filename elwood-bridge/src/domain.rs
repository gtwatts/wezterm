//! ElwoodDomain — implements WezTerm's `Domain` trait for agent panes.
//!
//! A Domain in WezTerm represents a source of panes (local PTY, SSH, WSL, etc.).
//! `ElwoodDomain` is a new domain type that spawns `ElwoodPane` instances — panes
//! that run the Elwood AI agent instead of a shell process.

use crate::pane::ElwoodPane;
use crate::runtime::{AgentRequest, AgentResponse, RuntimeBridge};

use anyhow::Context;
use async_trait::async_trait;
use mux::domain::{alloc_domain_id, DomainId, DomainState};
use mux::pane::{alloc_pane_id, Pane};
use mux::window::WindowId;
use mux::Mux;
use parking_lot::Mutex;
use portable_pty::CommandBuilder;
use std::sync::Arc;
use wezterm_term::TerminalSize;

/// State of the Elwood domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalState {
    /// Domain is registered but the tokio runtime is not yet started.
    Detached,
    /// Domain is active with a running tokio runtime.
    Attached,
}

/// WezTerm Domain implementation for Elwood agent panes.
///
/// When attached, this domain maintains a `RuntimeBridge` that runs elwood-core's
/// tokio runtime on a background thread. Each spawned pane creates an `ElwoodPane`
/// that communicates with the agent via the bridge.
pub struct ElwoodDomain {
    domain_id: DomainId,
    name: String,
    state: Mutex<InternalState>,
    bridge: Mutex<Option<Arc<RuntimeBridge>>>,
}

impl ElwoodDomain {
    /// Create a new ElwoodDomain.
    pub fn new() -> Self {
        Self {
            domain_id: alloc_domain_id(),
            name: "elwood".to_string(),
            state: Mutex::new(InternalState::Detached),
            bridge: Mutex::new(None),
        }
    }

    /// Create with a custom name.
    pub fn with_name(name: &str) -> Self {
        Self {
            domain_id: alloc_domain_id(),
            name: name.to_string(),
            state: Mutex::new(InternalState::Detached),
            bridge: Mutex::new(None),
        }
    }

    /// Get the current RuntimeBridge, if attached.
    fn get_bridge(&self) -> Option<Arc<RuntimeBridge>> {
        self.bridge.lock().clone()
    }

    /// Start the tokio runtime thread and agent loop.
    fn start_runtime(&self) -> Arc<RuntimeBridge> {
        let bridge = Arc::new(RuntimeBridge::new_async(|request_rx, response_tx| {
            agent_runtime_loop(request_rx, response_tx)
        }));

        *self.bridge.lock() = Some(Arc::clone(&bridge));
        *self.state.lock() = InternalState::Attached;
        bridge
    }
}

/// Create the LLM provider based on configuration.
///
/// Tries in order:
/// 1. Gemini OAuth (from `~/.gemini/oauth_creds.json`)
/// 2. Auto-detect from environment variables
fn create_provider(
    config: &crate::config::ElwoodConfig,
) -> anyhow::Result<Arc<dyn elwood_core::provider::LlmProvider>> {
    use elwood_core::provider::{GeminiProvider, ProviderFactory};

    // If configured for gemini, try OAuth first
    if config.provider == "gemini" {
        let cred_path = dirs_next::home_dir()
            .unwrap_or_default()
            .join(".gemini")
            .join("oauth_creds.json");

        if cred_path.exists() {
            tracing::info!("Using Gemini OAuth from {}", cred_path.display());
            return Ok(Arc::new(GeminiProvider::new_oauth(cred_path)));
        }
    }

    // Fallback: auto-detect from environment
    ProviderFactory::from_env()
        .map_err(|e| anyhow::anyhow!("No LLM provider available: {e}"))
}

/// The core async agent runtime loop.
///
/// Runs on the dedicated tokio thread inside the RuntimeBridge. Processes
/// `AgentRequest` messages and translates `AgentEvent`s into `AgentResponse`s.
async fn agent_runtime_loop(
    request_rx: flume::Receiver<AgentRequest>,
    response_tx: flume::Sender<AgentResponse>,
) {
    use elwood_core::config::PermissionConfig;
    use elwood_core::provider::Message;
    use elwood_core::tools::ToolRegistry;
    use tokio_util::sync::CancellationToken;

    let config = crate::config::ElwoodConfig::load();

    // Create provider
    let provider = match create_provider(&config) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to create provider: {e}");
            let _ = response_tx.send(AgentResponse::Error(format!(
                "Failed to initialize LLM provider: {e}"
            )));
            // Still run a degraded loop that reports the error for each request
            while let Ok(req) = request_rx.recv_async().await {
                match req {
                    AgentRequest::Shutdown => {
                        let _ = response_tx.send(AgentResponse::Shutdown);
                        break;
                    }
                    AgentRequest::SendMessage { .. } | AgentRequest::Start { .. } => {
                        let _ = response_tx.send(AgentResponse::Error(format!(
                            "No LLM provider configured: {e}"
                        )));
                        let _ = response_tx.send(AgentResponse::TurnComplete { summary: None });
                    }
                    AgentRequest::RunCommand { command, working_dir } => {
                        // Commands still work even without an LLM provider
                        let tx = response_tx.clone();
                        tokio::spawn(async move {
                            let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
                            let mut cmd = tokio::process::Command::new(&shell);
                            cmd.arg("-c").arg(&command);
                            cmd.stdout(std::process::Stdio::piped());
                            cmd.stderr(std::process::Stdio::piped());
                            if let Some(ref dir) = working_dir {
                                cmd.current_dir(dir);
                            }
                            let result = tokio::time::timeout(
                                std::time::Duration::from_secs(300),
                                cmd.output(),
                            ).await;
                            let response = match result {
                                Ok(Ok(output)) => AgentResponse::CommandOutput {
                                    command: command.clone(),
                                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                                    exit_code: output.status.code(),
                                },
                                Ok(Err(e)) => AgentResponse::CommandOutput {
                                    command: command.clone(),
                                    stdout: String::new(),
                                    stderr: format!("Failed to execute: {e}"),
                                    exit_code: Some(-1),
                                },
                                Err(_) => AgentResponse::CommandOutput {
                                    command: command.clone(),
                                    stdout: String::new(),
                                    stderr: "Command timed out (5 minute limit)".to_string(),
                                    exit_code: Some(-1),
                                },
                            };
                            let _ = tx.send(response);
                        });
                    }
                    _ => {}
                }
            }
            return;
        }
    };

    // Create tool registry with default permissions
    let tools = Arc::new(ToolRegistry::new(PermissionConfig::default()));

    // Conversation history persists across turns within a session
    let mut messages: Vec<Message> = Vec::new();

    // Cancellation token — recreated for each agent turn
    let mut cancel = CancellationToken::new();

    tracing::info!(
        "Elwood agent runtime started (provider={}, model={})",
        config.provider,
        config.model
    );

    while let Ok(req) = request_rx.recv_async().await {
        match req {
            AgentRequest::Start {
                prompt,
                session_id,
                working_dir,
            } => {
                tracing::info!("Agent session started: {session_id}");

                // Reset conversation for new session
                messages.clear();
                cancel = CancellationToken::new();

                // Set working directory if provided
                if let Some(ref dir) = working_dir {
                    let _ = std::env::set_current_dir(dir);
                }

                // Add user message
                messages.push(Message::user(&prompt));

                run_agent_turn(
                    &config,
                    &provider,
                    &tools,
                    &cancel,
                    &mut messages,
                    &response_tx,
                )
                .await;
            }

            AgentRequest::SendMessage { content } => {
                // Fresh cancellation token for new turn
                cancel = CancellationToken::new();

                // Collect git context and prepend to first message of each turn
                let enriched = {
                    let cwd = std::env::current_dir()
                        .unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let git_ctx = crate::git_info::get_git_context(&cwd);
                    if git_ctx.branch.is_empty() {
                        content.clone()
                    } else {
                        format!(
                            "[Git Context]\n{}\n[User Message]\n{content}",
                            git_ctx.format_context()
                        )
                    }
                };

                // Append user message to ongoing conversation
                messages.push(Message::user(&enriched));

                run_agent_turn(
                    &config,
                    &provider,
                    &tools,
                    &cancel,
                    &mut messages,
                    &response_tx,
                )
                .await;
            }

            AgentRequest::RunCommand {
                command,
                working_dir,
            } => {
                tracing::info!("Running shell command: {command}");
                let tx = response_tx.clone();
                tokio::spawn(async move {
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());

                    let mut cmd = tokio::process::Command::new(&shell);
                    cmd.arg("-c").arg(&command);
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::piped());

                    if let Some(ref dir) = working_dir {
                        cmd.current_dir(dir);
                    }

                    let timeout_duration = std::time::Duration::from_secs(300); // 5 minutes
                    let result = tokio::time::timeout(timeout_duration, cmd.output()).await;

                    let response = match result {
                        Ok(Ok(output)) => AgentResponse::CommandOutput {
                            command: command.clone(),
                            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                            exit_code: output.status.code(),
                        },
                        Ok(Err(e)) => AgentResponse::CommandOutput {
                            command: command.clone(),
                            stdout: String::new(),
                            stderr: format!("Failed to execute: {e}"),
                            exit_code: Some(-1),
                        },
                        Err(_) => AgentResponse::CommandOutput {
                            command: command.clone(),
                            stdout: String::new(),
                            stderr: "Command timed out (5 minute limit)".to_string(),
                            exit_code: Some(-1),
                        },
                    };
                    let _ = tx.send(response);
                });
            }

            AgentRequest::Cancel => {
                tracing::info!("Agent cancellation requested");
                cancel.cancel();
            }

            AgentRequest::PermissionResponse { .. } => {
                // Permission handling will be wired when ToolRegistry
                // permission handler is integrated
            }

            AgentRequest::ReviewFeedback { .. } => {
                // Review feedback is handled on the WezTerm side (diff viewer)
            }

            AgentRequest::PtyWrite { .. } | AgentRequest::PtyReadScreen => {
                // PTY interactions are handled on the WezTerm/pane side,
                // not in the agent loop. These should not normally arrive here.
                tracing::debug!("PTY request received in agent loop (ignored)");
            }

            AgentRequest::Shutdown => {
                tracing::info!("Elwood agent runtime shutting down");
                cancel.cancel();
                let _ = response_tx.send(AgentResponse::Shutdown);
                break;
            }
        }
    }
}

/// Execute a single agent turn: create a CoreAgent, run execute(), and translate events.
async fn run_agent_turn(
    config: &crate::config::ElwoodConfig,
    provider: &Arc<dyn elwood_core::provider::LlmProvider>,
    tools: &Arc<elwood_core::tools::ToolRegistry>,
    cancel: &tokio_util::sync::CancellationToken,
    messages: &mut Vec<elwood_core::provider::Message>,
    response_tx: &flume::Sender<AgentResponse>,
) {
    use elwood_core::agent::{AgentDef, CoreAgent};
    use elwood_core::output::{AgentEvent, ChannelOutput};

    // Create a tokio mpsc channel for AgentEvents
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let output: Arc<dyn elwood_core::output::AgentOutput> = Arc::new(ChannelOutput::new(event_tx));

    let agent_def = AgentDef {
        name: "elwood".to_string(),
        model: Some(config.model.clone()),
        provider: Some(config.provider.clone()),
        ..AgentDef::default()
    };

    let agent = CoreAgent::new(
        agent_def,
        Arc::clone(provider),
        Arc::clone(tools),
        output,
        cancel.clone(),
    );

    // Spawn a task to drain AgentEvents and translate them to AgentResponses
    let drain_tx = response_tx.clone();
    let drain_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let response = translate_event(event);
            if let Some(resp) = response {
                if drain_tx.send(resp).is_err() {
                    break; // receiver dropped
                }
            }
        }
    });

    // Run the agent
    match agent.execute(messages).await {
        Ok(state) => {
            let summary = format!(
                "Completed in {} steps ({} tool calls)",
                state.step, state.tool_calls
            );
            tracing::debug!("{summary}");

            // Drop the agent to close the ChannelOutput sender, unblocking the drain task
            drop(agent);

            // Wait for event drain to finish
            let _ = drain_handle.await;

            let _ = response_tx.send(AgentResponse::TurnComplete {
                summary: Some(summary),
            });
        }
        Err(e) => {
            tracing::error!("Agent execution failed: {e}");
            drop(agent);
            let _ = drain_handle.await;
            let _ = response_tx.send(AgentResponse::Error(format!("Agent error: {e}")));
            let _ = response_tx.send(AgentResponse::TurnComplete { summary: None });
        }
    }
}

/// Translate an elwood-core AgentEvent into an AgentResponse for the bridge.
///
/// Returns `None` for events that don't map to a bridge response (e.g. internal
/// token usage, swarm events, etc.).
fn translate_event(event: elwood_core::output::AgentEvent) -> Option<AgentResponse> {
    use elwood_core::output::AgentEvent;

    match event {
        // Content streaming
        AgentEvent::ContentDelta { delta, .. } => Some(AgentResponse::ContentDelta(delta)),
        AgentEvent::ReasoningDelta { delta, .. } => {
            // Show reasoning as content with a prefix
            Some(AgentResponse::ContentDelta(delta))
        }

        // Tool lifecycle
        AgentEvent::ToolCallStarted {
            tool_name,
            tool_call_id,
            arguments,
            ..
        } => Some(AgentResponse::ToolStart {
            tool_name,
            tool_id: tool_call_id,
            input_preview: truncate_preview(&arguments, 200),
        }),
        AgentEvent::ToolCallCompleted {
            tool_call_id,
            result,
            success,
            ..
        } => Some(AgentResponse::ToolEnd {
            tool_id: tool_call_id,
            success,
            output_preview: truncate_preview(&result, 200),
        }),
        AgentEvent::ToolCallFailed {
            tool_call_id,
            error,
            ..
        } => Some(AgentResponse::ToolEnd {
            tool_id: tool_call_id,
            success: false,
            output_preview: truncate_preview(&error, 200),
        }),

        // Permission requests
        AgentEvent::PermissionRequested {
            tool_name,
            tool_call_id,
            arguments,
            ..
        } => Some(AgentResponse::PermissionRequest {
            request_id: tool_call_id,
            tool_name,
            description: truncate_preview(&arguments, 500),
        }),

        // Errors and warnings
        AgentEvent::AgentFailed { error, .. } => Some(AgentResponse::Error(error)),
        AgentEvent::Error { message } => Some(AgentResponse::Error(message)),
        AgentEvent::Warning { message } => {
            Some(AgentResponse::ContentDelta(format!("[warning] {message}\n")))
        }

        // Status messages
        AgentEvent::Status { message } => {
            Some(AgentResponse::ContentDelta(format!("[status] {message}\n")))
        }

        // Events we don't surface to the terminal
        AgentEvent::SessionStarted { .. }
        | AgentEvent::SessionResumed { .. }
        | AgentEvent::AgentStarted { .. }
        | AgentEvent::AgentThinking { .. }
        | AgentEvent::AgentCompleted { .. }
        | AgentEvent::ContentComplete { .. }
        | AgentEvent::TokenUsage { .. }
        | AgentEvent::CostUpdate { .. }
        | AgentEvent::SwarmStarted { .. }
        | AgentEvent::SwarmCompleted { .. }
        | AgentEvent::RecoveryAttempt { .. }
        | AgentEvent::RecoveryFallback { .. }
        | AgentEvent::TaskDeclared { .. }
        | AgentEvent::TaskStatusChanged { .. }
        | AgentEvent::TokenBudget { .. }
        | AgentEvent::PermissionGranted { .. }
        | AgentEvent::PermissionDenied { .. }
        | AgentEvent::SnapshotAvailable { .. } => None,
    }
}

/// Truncate a string to a maximum length, respecting UTF-8 char boundaries.
fn truncate_preview(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[async_trait(?Send)]
impl mux::domain::Domain for ElwoodDomain {
    async fn spawn_pane(
        &self,
        size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let bridge = self
            .get_bridge()
            .or_else(|| Some(self.start_runtime()))
            .context("failed to get runtime bridge")?;

        let pane_id = alloc_pane_id();
        let pane: Arc<dyn Pane> = Arc::new(ElwoodPane::new(
            pane_id,
            self.domain_id,
            size,
            bridge,
        ));

        let mux = Mux::get();
        mux.add_pane(&pane)?;

        Ok(pane)
    }

    fn spawnable(&self) -> bool {
        true
    }

    fn detachable(&self) -> bool {
        true
    }

    fn domain_id(&self) -> DomainId {
        self.domain_id
    }

    fn domain_name(&self) -> &str {
        &self.name
    }

    async fn domain_label(&self) -> String {
        "Elwood Agent".to_string()
    }

    async fn attach(&self, _window_id: Option<WindowId>) -> anyhow::Result<()> {
        if self.get_bridge().is_none() {
            self.start_runtime();
        }
        Ok(())
    }

    fn detach(&self) -> anyhow::Result<()> {
        if let Some(bridge) = self.get_bridge() {
            bridge.request_shutdown();
        }
        *self.bridge.lock() = None;
        *self.state.lock() = InternalState::Detached;
        Ok(())
    }

    fn state(&self) -> DomainState {
        match *self.state.lock() {
            InternalState::Attached => DomainState::Attached,
            InternalState::Detached => DomainState::Detached,
        }
    }
}
