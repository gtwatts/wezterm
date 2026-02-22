//! Lua plugin hooks for controlling the Elwood agent from user-defined scripts.
//!
//! Provides an event-driven Lua API where users register callbacks in
//! `~/.elwood/hooks.lua` (or any configured path). The Elwood runtime
//! dispatches events at key points -- agent messages, tool lifecycle,
//! permission requests, mode changes, etc.
//!
//! # Example `~/.elwood/hooks.lua`
//!
//! ```lua
//! local elwood = require("elwood")
//!
//! -- Auto-approve safe read-only tools
//! elwood.on("tool_start", function(pane, tool_name, args)
//!     if tool_name == "ReadFileTool" or tool_name == "GlobTool" then
//!         return { approve = true }
//!     end
//! end)
//!
//! -- Notify on command failure
//! elwood.on("command_complete", function(pane, command, exit_code)
//!     if exit_code ~= 0 then
//!         pane:notify("Command failed: " .. command)
//!     end
//! end)
//! ```
//!
//! # Supported Events
//!
//! | Event | Arguments | Return |
//! |-------|-----------|--------|
//! | `agent_message` | `(pane, text)` | -- |
//! | `tool_start` | `(pane, tool_name, args)` | `{approve=true}` to auto-approve |
//! | `tool_end` | `(pane, tool_name, success, output)` | -- |
//! | `command_complete` | `(pane, command, exit_code)` | -- |
//! | `error_detected` | `(pane, error_type, message)` | -- |
//! | `mode_change` | `(pane, old_mode, new_mode)` | -- |
//! | `permission_request` | `(pane, tool_name, description)` | `{approve=true}` to auto-approve |

use crate::keybindings;

use mlua::{
    Function, IntoLua, Lua, MultiValue, RegistryKey, Result as LuaResult, Table, UserData,
    UserDataMethods, Value,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;

/// A lightweight Lua context for a pane, passed to event callbacks.
///
/// Since Lua UserData instances are shared by reference, we wrap the
/// mutable notifications in an `Arc<Mutex<>>` so the Lua callback can
/// append notifications and we can read them back after the call.
#[derive(Debug, Clone)]
pub struct LuaPaneContext {
    /// The WezTerm pane ID.
    pub pane_id: u64,
    /// Notifications queued by Lua callbacks (shared with the dispatch caller).
    pub notifications: Arc<Mutex<Vec<String>>>,
}

impl LuaPaneContext {
    /// Create a new pane context for the given pane ID.
    pub fn new(pane_id: u64) -> Self {
        Self {
            pane_id,
            notifications: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Drain and return all queued notifications.
    pub fn take_notifications(&self) -> Vec<String> {
        std::mem::take(&mut *self.notifications.lock())
    }
}

impl UserData for LuaPaneContext {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method("notify", |_, this, msg: String| {
            this.notifications.lock().push(msg);
            Ok(())
        });

        methods.add_method("pane_id", |_, this, ()| Ok(this.pane_id));
    }
}

/// Result from dispatching an event that can return values (e.g. auto-approve).
#[derive(Debug, Clone, Default)]
pub struct DispatchResult {
    /// If a callback returned `{approve = true}`, this is `true`.
    pub approve: Option<bool>,
}

/// The Elwood Lua event system.
///
/// Manages an embedded Lua runtime, loads user hook scripts, and dispatches
/// events to registered callbacks.
pub struct ElwoodLuaEvents {
    lua: Lua,
    /// Registry keys for callbacks, keyed by event name.
    callbacks: HashMap<String, Vec<RegistryKey>>,
}

impl ElwoodLuaEvents {
    /// Create a new Lua event system.
    ///
    /// Initializes a Lua 5.4 runtime and creates the `elwood` module
    /// with the `on()` registration function.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();

        // Create the `elwood` module table in the registry so `require("elwood")` works.
        // Scope the borrows so `lua` can be moved into Self afterwards.
        {
            let elwood_mod = lua.create_table()?;

            // _elwood_callbacks: accumulator table for callbacks registered during script load
            lua.globals()
                .set("_elwood_callbacks", lua.create_table()?)?;
            elwood_mod.set("on", lua.create_function(elwood_on_stub)?)?;

            // Register as a loaded package so `require("elwood")` returns it.
            let loaded: Table = lua
                .globals()
                .get::<_, Table>("package")?
                .get::<_, Table>("loaded")?;
            loaded.set("elwood", elwood_mod)?;
        }

        Ok(Self {
            lua,
            callbacks: HashMap::new(),
        })
    }

    /// Load and execute a Lua hook script file.
    ///
    /// The script can call `elwood.on("event_name", function(...) end)` to
    /// register callbacks. Multiple scripts can be loaded; callbacks accumulate.
    pub fn load_file(&mut self, path: &std::path::Path) -> LuaResult<()> {
        let source = std::fs::read_to_string(path).map_err(|e| {
            mlua::Error::external(format!("failed to read {}: {e}", path.display()))
        })?;
        self.load_source(&source, path.to_string_lossy().as_ref())
    }

    /// Load and execute a Lua hook script from a string.
    pub fn load_source(&mut self, source: &str, chunk_name: &str) -> LuaResult<()> {
        // Execute the script. It will call `elwood.on(...)` which stores
        // callbacks in `_elwood_callbacks`.
        self.lua.load(source).set_name(chunk_name).exec()?;

        // Harvest callbacks from the global accumulator table.
        let cb_table: Table = self.lua.globals().get::<_, Table>("_elwood_callbacks")?;
        for pair in cb_table.pairs::<String, Table>() {
            let (event_name, funcs) = pair?;
            let entry = self.callbacks.entry(event_name).or_default();
            for func in funcs.sequence_values::<Function>() {
                let func = func?;
                let key = self.lua.create_registry_value(func)?;
                entry.push(key);
            }
        }

        // Clear the accumulator for the next load_source call.
        self.lua
            .globals()
            .set("_elwood_callbacks", self.lua.create_table()?)?;

        Ok(())
    }

    /// Load the default hooks file from `~/.elwood/hooks.lua`, if it exists.
    ///
    /// Returns `Ok(true)` if a file was loaded, `Ok(false)` if none was found.
    pub fn load_default(&mut self) -> LuaResult<bool> {
        let path = default_hooks_path();
        if path.exists() {
            self.load_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Dispatch an event to all registered callbacks (fire-and-forget).
    ///
    /// Returns any notifications queued by callbacks via `pane:notify()`.
    pub fn dispatch(&self, event: &str, pane_id: u64, args: &[LuaEventArg]) -> Vec<String> {
        let handlers = match self.callbacks.get(event) {
            Some(h) if !h.is_empty() => h,
            _ => return Vec::new(),
        };

        let pane_ctx = LuaPaneContext::new(pane_id);

        for key in handlers {
            let func: Function = match self.lua.registry_value(key) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("Failed to retrieve Lua callback for {event}: {e}");
                    continue;
                }
            };

            if let Err(e) = self.call_handler(&func, &pane_ctx, args) {
                log::warn!("Lua hook {event} error: {e}");
            }
        }

        pane_ctx.take_notifications()
    }

    /// Dispatch an event that can return a result (e.g. `{approve = true}`).
    ///
    /// The first callback to return a table with `approve` set wins.
    pub fn dispatch_with_result(
        &self,
        event: &str,
        pane_id: u64,
        args: &[LuaEventArg],
    ) -> (DispatchResult, Vec<String>) {
        let handlers = match self.callbacks.get(event) {
            Some(h) if !h.is_empty() => h,
            _ => return (DispatchResult::default(), Vec::new()),
        };

        let pane_ctx = LuaPaneContext::new(pane_id);
        let mut result = DispatchResult::default();

        for key in handlers {
            let func: Function = match self.lua.registry_value(key) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("Failed to retrieve Lua callback for {event}: {e}");
                    continue;
                }
            };

            match self.call_handler_with_result(&func, &pane_ctx, args) {
                Ok(Some(r)) => {
                    result = r;
                    break; // First result-returning callback wins
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Lua hook {event} error: {e}");
                }
            }
        }

        (result, pane_ctx.take_notifications())
    }

    /// Return the number of registered callbacks for a given event.
    pub fn handler_count(&self, event: &str) -> usize {
        self.callbacks.get(event).map(|v| v.len()).unwrap_or(0)
    }

    /// Return all event names that have registered callbacks.
    pub fn registered_events(&self) -> Vec<&str> {
        self.callbacks
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k.as_str())
            .collect()
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn call_handler(
        &self,
        func: &Function,
        pane_ctx: &LuaPaneContext,
        args: &[LuaEventArg],
    ) -> LuaResult<()> {
        let lua_args = self.build_lua_args(pane_ctx, args)?;
        func.call::<_, ()>(lua_args)?;
        Ok(())
    }

    fn call_handler_with_result(
        &self,
        func: &Function,
        pane_ctx: &LuaPaneContext,
        args: &[LuaEventArg],
    ) -> LuaResult<Option<DispatchResult>> {
        let lua_args = self.build_lua_args(pane_ctx, args)?;
        let ret: Value = func.call(lua_args)?;

        match ret {
            Value::Table(tbl) => {
                let approve: Option<bool> = tbl.get::<_, Option<bool>>("approve")?;
                Ok(Some(DispatchResult { approve }))
            }
            _ => Ok(None),
        }
    }

    fn build_lua_args(
        &self,
        pane_ctx: &LuaPaneContext,
        args: &[LuaEventArg],
    ) -> LuaResult<MultiValue> {
        let mut lua_args = vec![self.lua.create_userdata(pane_ctx.clone())?.into_lua(&self.lua)?];

        for arg in args {
            let val = match arg {
                LuaEventArg::Str(s) => Value::String(self.lua.create_string(s)?),
                LuaEventArg::Int(n) => Value::Integer(*n),
                LuaEventArg::Bool(b) => Value::Boolean(*b),
                LuaEventArg::OptInt(Some(n)) => Value::Integer(*n),
                LuaEventArg::OptInt(None) => Value::Nil,
            };
            lua_args.push(val);
        }

        Ok(MultiValue::from_vec(lua_args))
    }
}

/// Argument types for Lua event dispatch.
#[derive(Debug, Clone)]
pub enum LuaEventArg {
    /// A string value.
    Str(String),
    /// An integer value.
    Int(i64),
    /// A boolean value.
    Bool(bool),
    /// An optional integer (passed as nil if None).
    OptInt(Option<i64>),
}

/// Stub `elwood.on()` function installed in the Lua runtime.
///
/// Accumulates callbacks in the `_elwood_callbacks` global table, which
/// is harvested by `load_source()` after script execution.
fn elwood_on_stub(lua: &Lua, (name, func): (String, Function)) -> LuaResult<()> {
    let cb_table: Table = lua.globals().get::<_, Table>("_elwood_callbacks")?;
    let entry: Value = cb_table.get::<_, Value>(name.clone())?;
    match entry {
        Value::Table(tbl) => {
            let len = tbl.raw_len();
            tbl.set(len + 1, func)?;
        }
        _ => {
            let tbl = lua.create_table()?;
            tbl.set(1, func)?;
            cb_table.set(name, tbl)?;
        }
    }
    Ok(())
}

/// Return the default path for user hook scripts.
pub fn default_hooks_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".elwood")
        .join("hooks.lua")
}

/// Thread-safe wrapper around `ElwoodLuaEvents`.
///
/// With the `send` feature enabled, mlua's `Lua` type is `Send`.
/// The dispatcher is created on the main thread and all calls happen there.
pub struct LuaEventDispatcher {
    events: ElwoodLuaEvents,
}

// Safety: With mlua `send` feature, Lua is Send. ElwoodLuaEvents only
// contains Lua + HashMap<String, Vec<RegistryKey>>. RegistryKey is Send
// with the `send` feature. The pane accesses this from the GUI thread only.
// Sync is safe because the dispatcher is only accessed behind the pane's
// Mutex lock or through &self methods that don't mutate the Lua state.
unsafe impl Send for LuaEventDispatcher {}
unsafe impl Sync for LuaEventDispatcher {}

impl LuaEventDispatcher {
    /// Create a new dispatcher, loading the default hooks file if present.
    pub fn try_new() -> Option<Self> {
        match ElwoodLuaEvents::new() {
            Ok(mut events) => {
                match events.load_default() {
                    Ok(true) => {
                        let registered = events.registered_events();
                        log::info!(
                            "Elwood Lua hooks loaded: {} event(s) registered ({:?})",
                            registered.len(),
                            registered,
                        );
                    }
                    Ok(false) => {
                        log::debug!(
                            "No Elwood hooks file found at {}",
                            default_hooks_path().display()
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to load Elwood hooks: {e}");
                    }
                }
                Some(Self { events })
            }
            Err(e) => {
                log::warn!("Failed to initialize Lua runtime for Elwood hooks: {e}");
                None
            }
        }
    }

    /// Dispatch an event (fire-and-forget).
    pub fn dispatch(&self, event: &str, pane_id: u64, args: &[LuaEventArg]) -> Vec<String> {
        self.events.dispatch(event, pane_id, args)
    }

    /// Dispatch an event that can return a result.
    pub fn dispatch_with_result(
        &self,
        event: &str,
        pane_id: u64,
        args: &[LuaEventArg],
    ) -> (DispatchResult, Vec<String>) {
        self.events.dispatch_with_result(event, pane_id, args)
    }

    /// Check if any hooks are registered.
    pub fn has_hooks(&self) -> bool {
        !self.events.callbacks.is_empty()
    }
}

// ── Legacy API surface (kept for backward compatibility) ──────────────────

/// Register the Elwood Lua event handlers.
///
/// This should be called during WezTerm's Lua initialization.
/// It registers event handlers for Elwood-specific events like
/// agent cancellation and status updates.
///
/// Currently uses WezTerm's `EmitEvent` mechanism rather than
/// a custom Lua module, which avoids modifying WezTerm's Lua
/// initialization pipeline.
pub fn register_lua_api() {
    log::debug!(
        "Elwood Lua API events: cancel={}, send_selection={}",
        keybindings::CANCEL_EVENT,
        keybindings::SEND_SELECTION_EVENT,
    );
}

/// Get the default Lua configuration snippet for Elwood keybindings.
///
/// This can be written to a file or included in the user's wezterm.lua.
pub fn default_lua_config() -> &'static str {
    keybindings::default_keybindings_lua()
}

// ── Event name constants ────────────────────────────────────────────────────

/// Event fired when the agent produces content.
pub const EVENT_AGENT_MESSAGE: &str = "agent_message";
/// Event fired when a tool is about to execute.
pub const EVENT_TOOL_START: &str = "tool_start";
/// Event fired when a tool has finished executing.
pub const EVENT_TOOL_END: &str = "tool_end";
/// Event fired when a shell command completes.
pub const EVENT_COMMAND_COMPLETE: &str = "command_complete";
/// Event fired when an error pattern is detected in output.
pub const EVENT_ERROR_DETECTED: &str = "error_detected";
/// Event fired when the input mode switches (Agent <-> Terminal).
pub const EVENT_MODE_CHANGE: &str = "mode_change";
/// Event fired when a permission request is pending.
pub const EVENT_PERMISSION_REQUEST: &str = "permission_request";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_lua_events() {
        let events = ElwoodLuaEvents::new().expect("Failed to create Lua events");
        assert!(events.registered_events().is_empty());
    }

    #[test]
    fn test_register_and_dispatch() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    pane:notify("got: " .. text)
                end)
            "#,
                "test",
            )
            .unwrap();

        assert_eq!(events.handler_count("agent_message"), 1);

        let notifications = events.dispatch(
            "agent_message",
            42,
            &[LuaEventArg::Str("hello world".into())],
        );
        assert_eq!(notifications, vec!["got: hello world"]);
    }

    #[test]
    fn test_dispatch_with_result_approve() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("tool_start", function(pane, tool_name, args)
                    if tool_name == "ReadFileTool" then
                        return { approve = true }
                    end
                end)
            "#,
                "test",
            )
            .unwrap();

        let (result, _) = events.dispatch_with_result(
            "tool_start",
            1,
            &[
                LuaEventArg::Str("ReadFileTool".into()),
                LuaEventArg::Str("{}".into()),
            ],
        );
        assert_eq!(result.approve, Some(true));

        // Non-matching tool should not return approve
        let (result, _) = events.dispatch_with_result(
            "tool_start",
            1,
            &[
                LuaEventArg::Str("BashTool".into()),
                LuaEventArg::Str("{}".into()),
            ],
        );
        assert!(result.approve.is_none());
    }

    #[test]
    fn test_dispatch_no_handlers() {
        let events = ElwoodLuaEvents::new().unwrap();
        let notifications = events.dispatch("nonexistent", 1, &[]);
        assert!(notifications.is_empty());
    }

    #[test]
    fn test_multiple_handlers_same_event() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    pane:notify("handler1: " .. text)
                end)
                elwood.on("agent_message", function(pane, text)
                    pane:notify("handler2: " .. text)
                end)
            "#,
                "test",
            )
            .unwrap();

        assert_eq!(events.handler_count("agent_message"), 2);

        let notifications = events.dispatch(
            "agent_message",
            1,
            &[LuaEventArg::Str("test".into())],
        );
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0], "handler1: test");
        assert_eq!(notifications[1], "handler2: test");
    }

    #[test]
    fn test_multiple_events() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("tool_start", function(pane, name, args)
                    pane:notify("tool: " .. name)
                end)
                elwood.on("command_complete", function(pane, cmd, code)
                    pane:notify("cmd: " .. cmd .. " exit=" .. tostring(code))
                end)
            "#,
                "test",
            )
            .unwrap();

        let mut registered = events.registered_events();
        registered.sort();
        assert_eq!(registered, vec!["command_complete", "tool_start"]);
    }

    #[test]
    fn test_error_in_handler_continues() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    error("intentional error")
                end)
                elwood.on("agent_message", function(pane, text)
                    pane:notify("still runs")
                end)
            "#,
                "test",
            )
            .unwrap();

        let notifications = events.dispatch(
            "agent_message",
            1,
            &[LuaEventArg::Str("test".into())],
        );
        assert!(notifications.contains(&"still runs".to_string()));
    }

    #[test]
    fn test_permission_request_auto_approve() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("permission_request", function(pane, tool_name, desc)
                    if tool_name == "GlobTool" or tool_name == "GrepTool" then
                        return { approve = true }
                    end
                end)
            "#,
                "test",
            )
            .unwrap();

        let (result, _) = events.dispatch_with_result(
            "permission_request",
            1,
            &[
                LuaEventArg::Str("GlobTool".into()),
                LuaEventArg::Str("Search for files".into()),
            ],
        );
        assert_eq!(result.approve, Some(true));
    }

    #[test]
    fn test_mode_change_event() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("mode_change", function(pane, old_mode, new_mode)
                    pane:notify("switched from " .. old_mode .. " to " .. new_mode)
                end)
            "#,
                "test",
            )
            .unwrap();

        let notifications = events.dispatch(
            "mode_change",
            1,
            &[
                LuaEventArg::Str("Agent".into()),
                LuaEventArg::Str("Terminal".into()),
            ],
        );
        assert_eq!(notifications, vec!["switched from Agent to Terminal"]);
    }

    #[test]
    fn test_command_complete_with_exit_code() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("command_complete", function(pane, command, exit_code)
                    if exit_code ~= 0 then
                        pane:notify("FAILED: " .. command)
                    end
                end)
            "#,
                "test",
            )
            .unwrap();

        // Successful command -- no notification
        let notifications = events.dispatch(
            "command_complete",
            1,
            &[
                LuaEventArg::Str("ls".into()),
                LuaEventArg::Int(0),
            ],
        );
        assert!(notifications.is_empty());

        // Failed command -- notification
        let notifications = events.dispatch(
            "command_complete",
            1,
            &[
                LuaEventArg::Str("cargo build".into()),
                LuaEventArg::Int(1),
            ],
        );
        assert_eq!(notifications, vec!["FAILED: cargo build"]);
    }

    #[test]
    fn test_load_multiple_sources() {
        let mut events = ElwoodLuaEvents::new().unwrap();

        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    pane:notify("source1")
                end)
            "#,
                "source1",
            )
            .unwrap();

        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    pane:notify("source2")
                end)
            "#,
                "source2",
            )
            .unwrap();

        assert_eq!(events.handler_count("agent_message"), 2);
    }

    #[test]
    fn test_pane_context_id() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("agent_message", function(pane, text)
                    pane:notify("pane=" .. tostring(pane:pane_id()))
                end)
            "#,
                "test",
            )
            .unwrap();

        let notifications = events.dispatch(
            "agent_message",
            99,
            &[LuaEventArg::Str("hi".into())],
        );
        assert_eq!(notifications, vec!["pane=99"]);
    }

    #[test]
    fn test_opt_int_nil() {
        let mut events = ElwoodLuaEvents::new().unwrap();
        events
            .load_source(
                r#"
                local elwood = require("elwood")
                elwood.on("command_complete", function(pane, cmd, code)
                    if code == nil then
                        pane:notify("no exit code")
                    else
                        pane:notify("code=" .. tostring(code))
                    end
                end)
            "#,
                "test",
            )
            .unwrap();

        let notifications = events.dispatch(
            "command_complete",
            1,
            &[
                LuaEventArg::Str("killed".into()),
                LuaEventArg::OptInt(None),
            ],
        );
        assert_eq!(notifications, vec!["no exit code"]);

        let notifications = events.dispatch(
            "command_complete",
            1,
            &[
                LuaEventArg::Str("ls".into()),
                LuaEventArg::OptInt(Some(0)),
            ],
        );
        assert_eq!(notifications, vec!["code=0"]);
    }
}
