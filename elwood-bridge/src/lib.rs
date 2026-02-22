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

pub mod block;
pub mod commands;
pub mod completions;
pub mod config;
pub mod context;
pub mod diff;
pub mod diff_viewer;
pub mod domain;
pub mod editor;
pub mod git_info;
pub mod history_search;
pub mod keybindings;
pub mod lua_api;
pub mod nl_classifier;
pub mod observer;
pub mod palette;
pub mod pane;
pub mod pty_inner;
pub mod runtime;
pub mod session_log;
pub mod shared_writer;
pub mod tools;

mod formatter;
pub mod screen;

pub use domain::ElwoodDomain;
pub use git_info::{GitContext, GitInfo};
pub use observer::{ContentDetector, ContentType, ContextualContent, NextCommandSuggester, PaneObserver};
pub use pane::ElwoodPane;
pub use runtime::RuntimeBridge;
pub use session_log::SessionLog;
