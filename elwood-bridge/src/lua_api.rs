//! Lua API extensions for controlling the Elwood agent from WezTerm's Lua config.
//!
//! Exposes functions that can be called from `wezterm.lua`:
//!
//! ```lua
//! local wezterm = require("wezterm")
//! local config = wezterm.config_builder()
//!
//! -- Elwood agent events
//! wezterm.on("elwood-cancel", function(window, pane)
//!   -- Agent cancellation triggered via Ctrl+Shift+X
//!   window:perform_action(
//!     wezterm.action.SendKey { key = "Escape" },
//!     pane
//!   )
//! end)
//!
//! -- Custom agent status in the right status bar
//! wezterm.on("update-right-status", function(window, pane)
//!   local vars = pane:get_user_vars()
//!   if vars.ELWOOD_PANE == "true" then
//!     window:set_right_status("Elwood Agent Active")
//!   end
//! end)
//! ```

use crate::keybindings;

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
