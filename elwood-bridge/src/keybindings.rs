//! Default keybinding configuration for the Elwood agent.
//!
//! Provides a Lua configuration snippet that users can include in their
//! `wezterm.lua` to get Elwood-specific keybindings. Also provides
//! programmatic access to the default bindings.
//!
//! ## Default Keybindings
//!
//! | Key | Action |
//! |-----|--------|
//! | `Ctrl+Shift+E` | Open/focus Elwood agent pane |
//! | `Ctrl+Shift+X` | Cancel current agent operation |
//! | `Escape` | Cancel operation (in Elwood pane) |
//! | `y` / `n` | Approve/deny permission (in Elwood pane) |

/// Generate a Lua snippet that registers default Elwood keybindings.
///
/// Users can include this in their `wezterm.lua`:
///
/// ```lua
/// local wezterm = require("wezterm")
/// local config = wezterm.config_builder()
/// -- ... your config ...
///
/// -- Add Elwood keybindings
/// local elwood_keys = require("elwood_keys")
/// elwood_keys.apply(config)
/// ```
pub fn default_keybindings_lua() -> &'static str {
    r#"
-- Elwood Agent keybindings for WezTerm
-- Include in your wezterm.lua: require("elwood_keys").apply(config)

local M = {}

function M.apply(config)
  local keys = config.keys or {}

  -- Ctrl+Shift+E: Open Elwood agent pane (split right)
  table.insert(keys, {
    key = "E",
    mods = "CTRL|SHIFT",
    action = wezterm.action.SplitPane {
      direction = "Right",
      size = { Percent = 40 },
      command = {
        domain = { DomainName = "elwood" },
      },
    },
  })

  -- Ctrl+Shift+X: Cancel current agent operation
  table.insert(keys, {
    key = "X",
    mods = "CTRL|SHIFT",
    action = wezterm.action.EmitEvent("elwood-cancel"),
  })

  config.keys = keys
end

return M
"#
}

/// The domain name used for Elwood agent panes.
pub const ELWOOD_DOMAIN_NAME: &str = "elwood";

/// Event name emitted for agent cancellation.
pub const CANCEL_EVENT: &str = "elwood-cancel";

/// Event name emitted for sending selected text to the agent.
pub const SEND_SELECTION_EVENT: &str = "elwood-send-selection";
