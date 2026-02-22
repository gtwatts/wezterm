//! ElwoodDomain — implements WezTerm's `Domain` trait for agent panes.
//!
//! A Domain in WezTerm represents a source of panes (local PTY, SSH, WSL, etc.).
//! `ElwoodDomain` is a new domain type that spawns `ElwoodPane` instances — panes
//! that run the Elwood AI agent instead of a shell process.

use crate::pane::ElwoodPane;
use crate::runtime::{AgentRequest, AgentResponse, RuntimeBridge};
use crate::semantic_bridge::SemanticBridge;

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

/// Create the LLM provider for a given provider name.
///
/// Tries in order:
/// 1. Gemini OAuth (from `~/.gemini/oauth_creds.json`) if provider is "gemini"
/// 2. Auto-detect from environment variables
fn create_provider_for(
    provider_name: &str,
) -> anyhow::Result<Arc<dyn elwood_core::provider::LlmProvider>> {
    use elwood_core::provider::{GeminiProvider, ProviderFactory};

    // If configured for gemini, try OAuth first
    if provider_name == "gemini" {
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

    // Initialize the model router from config
    let mut model_router = config.model_router();

    // Create provider for the initial active model
    let mut provider: Arc<dyn elwood_core::provider::LlmProvider> =
        match create_provider_for(&model_router.active_model().provider) {
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
                            let _ =
                                response_tx.send(AgentResponse::TurnComplete { summary: None });
                        }
                        AgentRequest::RunCommand {
                            command,
                            working_dir,
                        } => {
                            // Commands still work even without an LLM provider
                            let tx = response_tx.clone();
                            tokio::spawn(async move {
                                let shell = std::env::var("SHELL")
                                    .unwrap_or_else(|_| "bash".to_string());
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
                                )
                                .await;
                                let response = match result {
                                    Ok(Ok(output)) => AgentResponse::CommandOutput {
                                        command: command.clone(),
                                        stdout: String::from_utf8_lossy(&output.stdout)
                                            .to_string(),
                                        stderr: String::from_utf8_lossy(&output.stderr)
                                            .to_string(),
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
                        AgentRequest::WorkflowRun { name, steps } => {
                            // Workflows work without an LLM provider
                            let tx = response_tx.clone();
                            tokio::spawn(async move {
                                let shell = std::env::var("SHELL")
                                    .unwrap_or_else(|_| "bash".to_string());
                                let total = steps.len();
                                for (i, (command, description, continue_on_error)) in
                                    steps.iter().enumerate()
                                {
                                    let mut cmd = tokio::process::Command::new(&shell);
                                    cmd.arg("-c").arg(command);
                                    cmd.stdout(std::process::Stdio::piped());
                                    cmd.stderr(std::process::Stdio::piped());

                                    let timeout = std::time::Duration::from_secs(300);
                                    let result =
                                        tokio::time::timeout(timeout, cmd.output()).await;

                                    let (stdout, stderr, exit_code) = match result {
                                        Ok(Ok(o)) => (
                                            String::from_utf8_lossy(&o.stdout).to_string(),
                                            String::from_utf8_lossy(&o.stderr).to_string(),
                                            o.status.code(),
                                        ),
                                        Ok(Err(e)) => (
                                            String::new(),
                                            format!("Failed to execute: {e}"),
                                            Some(-1),
                                        ),
                                        Err(_) => (
                                            String::new(),
                                            "Command timed out (5 minute limit)".to_string(),
                                            Some(-1),
                                        ),
                                    };

                                    let failed = exit_code.unwrap_or(1) != 0;
                                    let is_last =
                                        i + 1 == total || (failed && !continue_on_error);

                                    let _ = tx.send(AgentResponse::WorkflowStepResult {
                                        workflow_name: name.clone(),
                                        step_index: i,
                                        total_steps: total,
                                        command: command.clone(),
                                        description: description.clone(),
                                        stdout,
                                        stderr,
                                        exit_code,
                                        is_last,
                                    });

                                    if failed && !continue_on_error {
                                        break;
                                    }
                                }
                            });
                        }
                        _ => {}
                    }
                }
                return;
            }
        };

    // Create tool registry with default permissions
    let mut tools_inner = ToolRegistry::new(PermissionConfig::default());

    // Initialize MCP client manager and register discovered tools
    let mut mcp_manager = crate::mcp::McpClientManager::new();
    mcp_manager.connect_all(&config.mcp).await;

    if mcp_manager.server_count() > 0 {
        tracing::info!(
            "MCP: {} servers connected, {} tools discovered",
            mcp_manager.server_count(),
            mcp_manager.discovered_tools().len(),
        );

        for (server_name, tool_def) in mcp_manager.discovered_tools() {
            if let Some(client) = mcp_manager.get_client(server_name) {
                let adapter = crate::mcp::McpToolAdapter::new(
                    server_name,
                    tool_def,
                    Arc::clone(client),
                );
                tools_inner.register(Arc::new(adapter));
            }
        }
    }

    let tools = Arc::new(tools_inner);

    // Start MCP server if enabled in config
    let _mcp_server_handle = if config.mcp.server_enabled {
        tracing::info!("MCP server enabled — starting stdio server");
        Some(crate::mcp::server::spawn_server(None))
    } else {
        None
    };

    // Conversation history persists across turns within a session
    let mut messages: Vec<Message> = Vec::new();

    // Cancellation token — recreated for each agent turn
    let mut cancel = CancellationToken::new();

    // Initialize semantic bridge for code-aware context enrichment
    let semantic_bridge = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut bridge = SemanticBridge::new(cwd);
        // Initialize in a blocking spawn to avoid blocking the event loop
        tokio::task::spawn_blocking(move || {
            bridge.initialize();
            bridge
        })
        .await
        .ok()
    };

    tracing::info!(
        "Elwood agent runtime started (provider={}, model={}, models={}, symbols={})",
        model_router.active_model().provider,
        model_router.active_model().name,
        model_router.model_count(),
        semantic_bridge.as_ref().map(|b| b.symbol_count()).unwrap_or(0),
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
                    &mut model_router,
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

                    let mut parts = Vec::new();

                    if !git_ctx.branch.is_empty() {
                        parts.push(format!(
                            "[Git Context]\n{}",
                            git_ctx.format_context()
                        ));
                    }

                    // Enrich with relevant code context from semantic bridge
                    if let Some(ref bridge) = semantic_bridge {
                        let snippets = bridge.find_relevant_context(&content, 2048);
                        if !snippets.is_empty() {
                            let mut ctx = String::from("[Relevant Code]\n");
                            for snippet in &snippets {
                                ctx.push_str(&format!(
                                    "<code ref=\"{}\" relevance=\"{:.2}\">\n{}\n</code>\n",
                                    snippet.id, snippet.score, snippet.text,
                                ));
                            }
                            parts.push(ctx);
                        }
                    }

                    parts.push(format!("[User Message]\n{content}"));
                    parts.join("\n")
                };

                // Append user message to ongoing conversation
                messages.push(Message::user(&enriched));

                run_agent_turn(
                    &mut model_router,
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

            AgentRequest::ObservePanes => {
                // Pane observation is handled on the WezTerm/pane side
                // (requires Mux access which is only available on the smol thread).
                tracing::debug!("ObservePanes request received in agent loop (ignored)");
            }

            AgentRequest::GeneratePlan { description } => {
                // Plan generation: send as a regular agent turn with a planning prompt
                cancel = CancellationToken::new();
                let plan_prompt = format!(
                    "Please create a detailed step-by-step implementation plan for:\n\n{description}\n\n\
                     Format your response as a numbered list of steps, each with:\n\
                     - A clear action title\n\
                     - Brief description of what to do\n\
                     - Files to modify (if applicable)"
                );
                messages.push(Message::user(&plan_prompt));

                run_agent_turn(
                    &mut model_router,
                    &provider,
                    &tools,
                    &cancel,
                    &mut messages,
                    &response_tx,
                )
                .await;
            }

            AgentRequest::SwitchModel { model_name } => {
                if model_router.switch_to(&model_name) {
                    let active = model_router.active_model();
                    tracing::info!(
                        "Switched to model: {} (provider={})",
                        active.name,
                        active.provider,
                    );

                    // Create a new provider for the switched model
                    match create_provider_for(&active.provider) {
                        Ok(new_provider) => {
                            provider = new_provider;
                            let _ = response_tx.send(AgentResponse::ModelSwitched {
                                model_name: active.name.clone(),
                            });
                        }
                        Err(e) => {
                            tracing::error!("Failed to create provider for {}: {e}", active.provider);
                            let _ = response_tx.send(AgentResponse::Error(format!(
                                "Failed to switch to {}: {e}",
                                model_name,
                            )));
                        }
                    }
                } else {
                    let _ = response_tx.send(AgentResponse::Error(format!(
                        "Unknown model: {model_name}. Use /model list to see available models."
                    )));
                }
            }

            AgentRequest::WorkflowRun { name, steps } => {
                tracing::info!("Running workflow: {name} ({} steps)", steps.len());
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
                let total = steps.len();

                for (i, (command, description, continue_on_error)) in steps.iter().enumerate() {
                    let mut cmd = tokio::process::Command::new(&shell);
                    cmd.arg("-c").arg(command);
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::piped());

                    let timeout = std::time::Duration::from_secs(300);
                    let result = tokio::time::timeout(timeout, cmd.output()).await;

                    let (stdout, stderr, exit_code) = match result {
                        Ok(Ok(o)) => (
                            String::from_utf8_lossy(&o.stdout).to_string(),
                            String::from_utf8_lossy(&o.stderr).to_string(),
                            o.status.code(),
                        ),
                        Ok(Err(e)) => (
                            String::new(),
                            format!("Failed to execute: {e}"),
                            Some(-1),
                        ),
                        Err(_) => (
                            String::new(),
                            "Command timed out (5 minute limit)".to_string(),
                            Some(-1),
                        ),
                    };

                    let failed = exit_code.unwrap_or(1) != 0;
                    let is_last = i + 1 == total || (failed && !continue_on_error);

                    let _ = response_tx.send(AgentResponse::WorkflowStepResult {
                        workflow_name: name.clone(),
                        step_index: i,
                        total_steps: total,
                        command: command.clone(),
                        description: description.clone(),
                        stdout,
                        stderr,
                        exit_code,
                        is_last,
                    });

                    if failed && !continue_on_error {
                        break;
                    }
                }
            }

            AgentRequest::Shutdown => {
                tracing::info!("Elwood agent runtime shutting down");
                cancel.cancel();
                // Shut down all MCP server connections
                mcp_manager.shutdown_all().await;
                let _ = response_tx.send(AgentResponse::Shutdown);
                break;
            }

            AgentRequest::RunBackgroundCommand { command, working_dir } => {
                tracing::info!("Starting background job: {command}");
                let tx = response_tx.clone();
                // Assign a temporary job_id from a simple counter.
                // The pane-side JobManager assigns the real ID; we use a
                // placeholder here and the pane maps it on receipt.
                static BG_JOB_COUNTER: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(1);
                let job_id = BG_JOB_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                tokio::spawn(async move {
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
                    let mut cmd = tokio::process::Command::new(&shell);
                    cmd.arg("-c").arg(&command);
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::piped());
                    if let Some(ref dir) = working_dir {
                        cmd.current_dir(dir);
                    }

                    let child = cmd.spawn();
                    match child {
                        Ok(mut child) => {
                            let pid = child.id();
                            // Notify: job started with PID
                            let _ = tx.send(AgentResponse::JobUpdate {
                                job_id,
                                status: "running".to_string(),
                                output_chunk: Some(command.clone()),
                                is_stderr: false,
                                exit_code: None,
                                pid,
                            });

                            // Stream stdout
                            let stdout = child.stdout.take();
                            let stderr = child.stderr.take();
                            let tx_out = tx.clone();
                            let tx_err = tx.clone();

                            let stdout_handle = tokio::spawn(async move {
                                if let Some(stdout) = stdout {
                                    use tokio::io::{AsyncBufReadExt, BufReader};
                                    let reader = BufReader::new(stdout);
                                    let mut lines = reader.lines();
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        let _ = tx_out.send(AgentResponse::JobUpdate {
                                            job_id,
                                            status: "running".to_string(),
                                            output_chunk: Some(line),
                                            is_stderr: false,
                                            exit_code: None,
                                            pid: None,
                                        });
                                    }
                                }
                            });

                            let stderr_handle = tokio::spawn(async move {
                                if let Some(stderr) = stderr {
                                    use tokio::io::{AsyncBufReadExt, BufReader};
                                    let reader = BufReader::new(stderr);
                                    let mut lines = reader.lines();
                                    while let Ok(Some(line)) = lines.next_line().await {
                                        let _ = tx_err.send(AgentResponse::JobUpdate {
                                            job_id,
                                            status: "running".to_string(),
                                            output_chunk: Some(line),
                                            is_stderr: true,
                                            exit_code: None,
                                            pid: None,
                                        });
                                    }
                                }
                            });

                            // Wait for process to complete
                            let exit_status = child.wait().await;
                            let _ = stdout_handle.await;
                            let _ = stderr_handle.await;

                            let (status, code) = match exit_status {
                                Ok(s) => {
                                    let c = s.code().unwrap_or(-1);
                                    if c == 0 {
                                        ("completed".to_string(), c)
                                    } else {
                                        ("failed".to_string(), c)
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AgentResponse::JobUpdate {
                                        job_id,
                                        status: "running".to_string(),
                                        output_chunk: Some(format!("Process error: {e}")),
                                        is_stderr: true,
                                        exit_code: None,
                                        pid: None,
                                    });
                                    ("failed".to_string(), -1)
                                }
                            };

                            let _ = tx.send(AgentResponse::JobUpdate {
                                job_id,
                                status,
                                output_chunk: None,
                                is_stderr: false,
                                exit_code: Some(code),
                                pid: None,
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(AgentResponse::JobUpdate {
                                job_id,
                                status: "failed".to_string(),
                                output_chunk: Some(format!("Failed to spawn: {e}")),
                                is_stderr: true,
                                exit_code: Some(-1),
                                pid: None,
                            });
                        }
                    }
                });
            }

            AgentRequest::KillJob { job_id } => {
                // Kill is handled on the pane side (it has the JobManager with PIDs).
                // If it arrives here, log and ignore.
                tracing::debug!("KillJob({job_id}) received in agent loop — handled on pane side");
            }
        }
    }
}

/// Execute a single agent turn: create a CoreAgent, run execute(), and translate events.
async fn run_agent_turn(
    model_router: &mut crate::model_router::ModelRouter,
    provider: &Arc<dyn elwood_core::provider::LlmProvider>,
    tools: &Arc<elwood_core::tools::ToolRegistry>,
    cancel: &tokio_util::sync::CancellationToken,
    messages: &mut Vec<elwood_core::provider::Message>,
    response_tx: &flume::Sender<AgentResponse>,
) {
    use elwood_core::agent::{AgentDef, CoreAgent};
    use elwood_core::output::{AgentEvent, ChannelOutput};

    let active_model = model_router.active_model();

    // Create a tokio mpsc channel for AgentEvents
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let output: Arc<dyn elwood_core::output::AgentOutput> = Arc::new(ChannelOutput::new(event_tx));

    let agent_def = AgentDef {
        name: "elwood".to_string(),
        model: Some(active_model.name.clone()),
        provider: Some(active_model.provider.clone()),
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

        // Token/cost tracking — forward to the bridge for model router
        AgentEvent::TokenUsage { usage, .. } => Some(AgentResponse::CostUpdate {
            input_tokens: usage.prompt_tokens as u64,
            output_tokens: usage.completion_tokens as u64,
            cost_usd: 0.0, // cost computed on the pane side from model pricing
        }),
        AgentEvent::CostUpdate { total_cost, .. } => Some(AgentResponse::CostUpdate {
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: total_cost,
        }),

        // Events we don't surface to the terminal
        AgentEvent::SessionStarted { .. }
        | AgentEvent::SessionResumed { .. }
        | AgentEvent::AgentStarted { .. }
        | AgentEvent::AgentThinking { .. }
        | AgentEvent::AgentCompleted { .. }
        | AgentEvent::ContentComplete { .. }
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
