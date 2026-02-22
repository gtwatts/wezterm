//! # elwood-bridge
//!
//! Bridge between `elwood-core` (agentic coding agent) and WezTerm (terminal emulator).
//!
//! This crate implements WezTerm's `Domain` and `Pane` traits to embed the Elwood
//! agent as a native pane within the terminal. The agent renders through WezTerm's
//! GPU pipeline and can observe other panes' content in real time.
//!
//! ## Architecture
//!
//! ```text
//! WezTerm (smol)  ←→  RuntimeBridge (flume)  ←→  elwood-core (tokio)
//! ```
//!
//! - **ElwoodDomain**: Implements `Domain` — manages agent lifecycle
//! - **ElwoodPane**: Implements `Pane` — renders agent output via virtual terminal
//! - **RuntimeBridge**: Bridges smol↔tokio via flume channels
//! - **PaneObserver**: Reads content from other WezTerm panes
//! - **ANSI Formatter**: Converts `AgentEvent` to styled terminal output

pub mod autocorrect;
pub mod block;
pub mod commands;
pub mod completions;
pub mod config;
pub mod context;
pub mod diff;
pub mod diff_viewer;
pub mod domain;
pub mod editor;
pub mod file_browser;
pub mod fuzzy_finder;
pub mod git_info;
pub mod git_ui;
pub mod ide_bridge;
pub mod jobs;
pub mod history_search;
pub mod keybindings;
pub mod launch_config;
pub mod lua_api;
pub mod mcp;
pub mod model_router;
pub mod multi_agent;
pub mod nl_classifier;
pub mod notebook;
pub mod notification;
pub mod observer;
pub mod palette;
pub mod pane;
pub mod plan_mode;
pub mod plan_viewer;
pub mod prediction_engine;
pub mod pty_inner;
pub mod recording;
pub mod redaction;
pub mod runtime;
pub mod semantic_bridge;
pub mod session_export;
pub mod session_log;
pub mod shared_writer;
pub mod suggestion_overlay;
pub mod tools;
pub mod vim_mode;
pub mod workflow;

mod formatter;
pub mod markdown;
pub mod screen;
pub mod theme;

pub use domain::ElwoodDomain;
pub use git_info::{GitContext, GitInfo};
pub use observer::{ContentDetector, ContentType, ContextualContent, ErrorDetection, ErrorType, NextCommandSuggester, PaneObserver, Severity};
pub use suggestion_overlay::{Suggestion, SuggestionManager};
pub use notification::{Toast, ToastAction, ToastLevel, ToastManager};
pub use pane::ElwoodPane;
pub use runtime::RuntimeBridge;
pub use session_log::SessionLog;
