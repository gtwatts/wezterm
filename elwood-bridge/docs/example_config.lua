-- Elwood Terminal Lua Hooks Configuration
--
-- Place this file at ~/.elwood/hooks.lua to customize agent behavior.
-- All hooks are optional; only register the ones you need.
--
-- The `elwood` module is available via require:
--   local elwood = require("elwood")
--
-- Each callback receives a `pane` object as the first argument.
-- The pane object supports:
--   pane:notify(message)  -- Display a notification in the chat area
--   pane:pane_id()        -- Get the WezTerm pane ID (number)

local elwood = require("elwood")

-- ============================================================================
-- agent_message(pane, text)
-- ============================================================================
-- Fired when the agent produces content (streaming deltas).
-- `text` is the raw text chunk.
--
elwood.on("agent_message", function(pane, text)
    -- Example: log long responses
    -- if #text > 1000 then
    --     pane:notify("Long response detected: " .. #text .. " chars")
    -- end
end)

-- ============================================================================
-- tool_start(pane, tool_name, args)
-- ============================================================================
-- Fired when a tool is about to execute.
-- `tool_name` is the tool's registered name (e.g. "BashTool", "ReadFileTool").
-- `args` is a preview of the tool's input arguments (JSON string).
--
-- Return `{ approve = true }` to auto-approve the tool (skip permission prompt).
--
elwood.on("tool_start", function(pane, tool_name, args)
    -- Auto-approve read-only tools
    local safe_tools = {
        ReadFileTool = true,
        GlobTool = true,
        GrepTool = true,
        AstSearchTool = true,
    }
    if safe_tools[tool_name] then
        return { approve = true }
    end

    -- Notify on potentially destructive tools
    local risky_tools = {
        WriteFileTool = true,
        EditFileTool = true,
        BashTool = true,
    }
    if risky_tools[tool_name] then
        pane:notify("Tool starting: " .. tool_name)
    end
end)

-- ============================================================================
-- tool_end(pane, tool_name, success, output)
-- ============================================================================
-- Fired when a tool has finished executing.
-- `tool_name` is the tool name, `success` is a boolean,
-- `output` is a preview of the tool's output.
--
elwood.on("tool_end", function(pane, tool_name, success, output)
    if not success then
        pane:notify("Tool failed: " .. tool_name)
    end
end)

-- ============================================================================
-- command_complete(pane, command, exit_code)
-- ============================================================================
-- Fired when a shell command completes.
-- `command` is the shell command string.
-- `exit_code` is the exit code (integer) or nil if the process was killed.
--
elwood.on("command_complete", function(pane, command, exit_code)
    if exit_code ~= nil and exit_code ~= 0 then
        pane:notify("Command failed (exit " .. tostring(exit_code) .. "): " .. command)
    end
end)

-- ============================================================================
-- error_detected(pane, error_type, message)
-- ============================================================================
-- Fired when an error is detected.
-- `error_type` describes the category (e.g. "agent_error").
-- `message` is the error message.
--
elwood.on("error_detected", function(pane, error_type, message)
    pane:notify("Error [" .. error_type .. "]: " .. message:sub(1, 100))
end)

-- ============================================================================
-- mode_change(pane, old_mode, new_mode)
-- ============================================================================
-- Fired when the input mode switches between Agent and Terminal.
-- `old_mode` and `new_mode` are strings: "Agent" or "Terminal".
--
elwood.on("mode_change", function(pane, old_mode, new_mode)
    -- Example: show a notification when switching modes
    -- pane:notify("Switched from " .. old_mode .. " to " .. new_mode)
end)

-- ============================================================================
-- permission_request(pane, tool_name, description)
-- ============================================================================
-- Fired when the agent needs permission to perform an action.
-- `tool_name` is the tool requesting permission.
-- `description` is a human-readable description of what it wants to do.
--
-- Return `{ approve = true }` to auto-approve (skip the y/n prompt).
-- Return nothing (or nil) to show the normal permission prompt.
--
elwood.on("permission_request", function(pane, tool_name, description)
    -- Auto-approve read-only operations
    local safe_tools = {
        ReadFileTool = true,
        GlobTool = true,
        GrepTool = true,
        WebSearchTool = true,
    }
    if safe_tools[tool_name] then
        return { approve = true }
    end

    -- Everything else: show the normal permission prompt
end)
