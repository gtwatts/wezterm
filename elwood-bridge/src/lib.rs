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

pub mod config;
pub mod domain;
pub mod keybindings;
pub mod lua_api;
pub mod observer;
pub mod pane;
pub mod runtime;
pub mod tools;

mod formatter;

pub use domain::ElwoodDomain;
pub use observer::{ContentDetector, ContentType, ContextualContent, PaneObserver};
pub use pane::ElwoodPane;
pub use runtime::RuntimeBridge;
