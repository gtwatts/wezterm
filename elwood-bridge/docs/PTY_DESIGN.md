# PTY Embedding Design for ElwoodPane

**Status**: Design / RFC
**Author**: Research Agent
**Date**: 2026-02-22

## Problem Statement

ElwoodPane currently executes shell commands via `tokio::process::Command` with
`$SHELL -c`, which captures stdout/stderr as strings. This works for
non-interactive commands but cannot handle interactive programs (vim, htop, gdb,
python REPLs, psql, etc.) because:

1. No PTY allocation — programs detect they are not connected to a terminal and
   either refuse to start or degrade to non-interactive mode.
2. No screen state — the agent cannot read what an interactive program is
   displaying (cursor position, screen layout, colors).
3. No keystroke injection — the agent cannot send Ctrl-sequences, arrow keys,
   or escape sequences required by TUI programs.

The goal is to embed a real PTY inside ElwoodPane so that:
- Users can run interactive programs from the Elwood terminal.
- The AI agent can read the PTY screen buffer and send keystrokes to control
  interactive programs (the "Full Terminal Use" pattern pioneered by Warp).

## Prior Art

### Warp Full Terminal Use

Warp's agent attaches to the running PTY session and can:
- See the live terminal buffer (screen state) in real time.
- Write to the PTY stdin to run commands or respond to prompts.
- Step through interactive programs (gdb, psql, vim).

Control model: Three permission levels govern agent writes — "ask on first
write", "always ask", "always allow". Users can take over control with CMD+I,
which stops the agent from issuing PTY writes until control is handed back.

Key insight: The agent sees the *same* terminal buffer the user sees, and
proposes actions that the user can approve or reject.

### interminai (PTY Proxy)

A PTY proxy that wraps interactive programs. Architecture:
- Launches programs inside a pseudo-terminal.
- Captures screen as ASCII text via a virtual terminal emulator.
- Sends keystrokes through a simple API: `start`, `input`, `output`, `kill`.
- Daemon-based with Unix socket communication.
- Two implementations: Rust (zero-dep) and Python.

### pilotty (Daemon-managed PTY Sessions)

Similar approach with richer features:
- Full VT100 terminal emulation for screen capture.
- Structured JSON snapshots: text content, cursor position, UI element detection.
- Content hash for change detection (avoids polling).
- `--await-change` flag blocks until screen changes (solves timing problem).
- Input injection: keyboard navigation, text typing, click simulation, scroll.

### WezTerm's Own PTY Architecture

WezTerm already has a complete PTY subsystem that we can leverage directly:

**portable-pty crate** (`pty/src/lib.rs`):
- `PtySystem::openpty(size) -> PtyPair { slave, master }`
- `MasterPty`: `resize()`, `try_clone_reader()`, `take_writer()`,
  `process_group_leader()`, `get_termios()`
- `SlavePty::spawn_command(cmd) -> Child`
- Cross-platform (Unix PTY / Windows ConPTY)

**LocalPane** (`mux/src/localpane.rs`):
- The reference implementation. Wraps `Terminal` + `MasterPty` + `Child`.
- `terminal: Mutex<Terminal>` — virtual terminal state.
- `pty: Mutex<Box<dyn MasterPty>>` — the PTY master end.
- `writer: Mutex<Box<dyn Write + Send>>` — writes to PTY stdin.
- `process: Mutex<ProcessState>` — child process lifecycle.
- Key routing: `key_down()` -> `terminal.key_down()` -> encodes keystrokes ->
  writes to PTY via the Terminal's internal writer.
- Screen reading: `get_lines()` -> `terminal_get_lines()` -> reads from Screen.
- Resize: resizes both PTY and Terminal.
- Password detection: reads termios flags (ECHO/ICANON) from PTY.
- Process info: `tcgetpgrp()` to identify foreground process.
- Semantic zones: `get_semantic_zones()` via Terminal (OSC 133 markup).

**Terminal** (`term/src/terminal.rs`):
- `advance_bytes()` — feeds PTY output through the escape sequence parser,
  updating the virtual terminal state (screen buffer, cursor, attributes).
- `perform_actions()` — applies parsed escape sequences.
- Wraps `TerminalState` which has `screen()`, `screen_mut()`, `cursor_pos()`,
  `get_semantic_zones()`, `is_alt_screen_active()`, `is_mouse_grabbed()`,
  `key_down()`, `key_up()`, `send_paste()`.

**renderable.rs** (`mux/src/renderable.rs`):
- Helper functions that bridge Pane trait to Terminal internals:
  `terminal_get_lines()`, `terminal_get_dimensions()`,
  `terminal_get_cursor_position()`, `terminal_get_dirty_lines()`.

## Architecture Options

### Option 1: ElwoodPane Manages Its Own PTY

ElwoodPane creates a `PtyPair` directly, spawns a shell, and reads/writes
the PTY master. The existing virtual Terminal is fed PTY output via
`advance_bytes()`.

**Pros:**
- Full control over PTY lifecycle, timing, and mode switching.
- Clean ownership — all state in one struct.
- Can reuse the existing Terminal for rendering (just feed it PTY bytes).

**Cons:**
- Must reimplement the PTY reader thread (background read loop) that LocalPane
  gets from the domain infrastructure.
- Must handle child process lifecycle (wait, kill, zombie reaping).

### Option 2: Delegate to a Real LocalPane

When entering terminal mode, ElwoodPane creates a real `LocalPane` (via the
`LocalDomain`) and delegates all Pane trait calls to it. ElwoodPane becomes
a thin wrapper that switches between "agent pane" and "local pane" modes.

**Pros:**
- Zero reimplementation — LocalPane already handles everything.
- Full compatibility with WezTerm features (tmux integration, process info, etc.).

**Cons:**
- Two separate Pane objects with two separate Terminals — complex state sharing.
- Mode switching requires swapping the active pane, which interacts poorly with
  WezTerm's tab/window model (pane IDs, focus tracking, etc.).
- Agent cannot easily read the LocalPane's screen from the ElwoodPane context.

### Option 3: Hybrid — ElwoodPane with an Inner PTY (RECOMMENDED)

ElwoodPane keeps its existing virtual Terminal for agent-mode rendering, and
*adds* an optional inner PTY that activates in terminal mode. Both modes share
the same Pane ID, the same virtual Terminal, and the same screen buffer.

In terminal mode:
- A PTY is opened and a shell is spawned.
- A background reader thread feeds PTY output into the existing Terminal via
  `advance_bytes()`.
- Key events are routed to the PTY writer instead of the InputEditor.
- The agent can read the Terminal screen via `get_lines()` / `get_semantic_zones()`.

In agent mode:
- The PTY is either idle (shell at prompt) or not spawned.
- Keys go to the InputEditor (current behavior).
- Agent output is written as ANSI to the Terminal (current behavior).

**Why this is the best approach:**

1. **Single Terminal, single screen buffer.** Both the user and the agent see
   the same content. The WezTerm renderer reads from one source of truth.

2. **Minimal disruption.** ElwoodPane keeps its existing Pane trait implementation.
   Only `key_down()` and the response loop change behavior based on mode.

3. **Agent screen reading is free.** The agent already has access to the Terminal
   via `self.terminal.lock()`. Adding `get_lines()` / `get_semantic_zones()`
   calls is trivial — no new infrastructure needed.

4. **Warp-like "Full Terminal Use" pattern.** The agent reads the same screen
   buffer the user sees, proposes keystrokes, and waits for approval. This
   matches the proven UX from Warp.

5. **Progressive complexity.** The PTY is optional. Agent-only sessions (no
   shell commands) work exactly as they do today.

## Detailed Design (Option 3)

### A. PTY Embedding Strategy

#### New Data Structures

```rust
/// State of the embedded PTY (None = agent-only, no PTY spawned yet).
struct InnerPty {
    /// The PTY master end (for resize, reader clone, termios).
    master: Box<dyn MasterPty>,
    /// Write handle to PTY stdin.
    writer: Box<dyn Write + Send>,
    /// Child process state (waiter channel, signaller, pid).
    process: ProcessState,
    /// Handle to the background reader thread.
    reader_handle: Option<thread::JoinHandle<()>>,
    /// Cached foreground process leader info (for password detection).
    #[cfg(unix)]
    leader: Arc<Mutex<Option<CachedLeaderInfo>>>,
}

/// Extended ElwoodPane fields:
pub struct ElwoodPane {
    // ... existing fields ...

    /// The embedded PTY. None until terminal mode is first activated.
    inner_pty: Mutex<Option<InnerPty>>,
    /// Current interaction mode.
    mode: Mutex<InteractionMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    /// Agent mode: keys -> InputEditor, agent output -> Terminal.
    Agent,
    /// Terminal mode: keys -> PTY stdin, PTY output -> Terminal.
    Terminal,
    /// Agent-controlled terminal: agent reads screen + sends keys to PTY.
    /// User sees what the agent is doing and can approve/take over.
    AgentTerminal,
}
```

#### PTY Lifecycle

1. **Spawn on first terminal-mode entry:**
   When the user first presses Ctrl+T (or `!` prefix triggers terminal mode),
   ElwoodPane opens a PTY via `native_pty_system().openpty(size)`, spawns the
   user's `$SHELL`, and starts the reader thread.

2. **Reader thread:**
   A dedicated background thread reads from `master.try_clone_reader()` in a
   loop, feeding bytes into the Terminal via `advance_bytes()`. This runs on a
   std thread (not tokio) because it blocks on a file descriptor read.

   ```rust
   fn spawn_pty_reader(
       reader: Box<dyn Read + Send>,
       terminal: Arc<Mutex<Terminal>>,
       seqno: Arc<AtomicUsize>,
       dead_flag: Arc<AtomicBool>,
   ) -> thread::JoinHandle<()> {
       thread::Builder::new()
           .name("elwood-pty-reader".into())
           .spawn(move || {
               let mut buf = [0u8; 8192];
               loop {
                   match reader.read(&mut buf) {
                       Ok(0) | Err(_) => {
                           dead_flag.store(true, Ordering::Release);
                           break;
                       }
                       Ok(n) => {
                           terminal.lock().advance_bytes(&buf[..n]);
                           seqno.fetch_add(1, Ordering::Release);
                           // Notify WezTerm mux that pane content changed
                           // (via MuxNotification::PaneOutput)
                       }
                   }
               }
           })
           .expect("failed to spawn pty reader")
   }
   ```

3. **Shell exit handling:**
   When the child process exits, the reader thread detects EOF and sets the
   dead flag. ElwoodPane transitions back to agent mode and displays the exit
   status in the chat area.

4. **Cleanup on pane close:**
   `ElwoodPane::kill()` sends SIGHUP to the PTY child (matching LocalPane
   behavior), then joins the reader thread.

### B. Key Routing

#### Mode-Based Dispatch in `key_down()`

```
key_down(key, mods)
  |
  +-- Ctrl+T? -> toggle_mode() [always intercepted]
  |
  +-- mode == Agent?
  |     +-- route to InputEditor (current behavior)
  |
  +-- mode == Terminal?
  |     +-- encode keystroke via Terminal::key_down()
  |     +-- Terminal's writer sends to PTY stdin
  |
  +-- mode == AgentTerminal?
        +-- agent proposes actions, user approves
        +-- approved keys sent to PTY via writer
```

#### Terminal Key Encoding

WezTerm's `Terminal::key_down()` already handles encoding keystrokes into the
correct escape sequences for the PTY. In terminal mode, we configure the
Terminal's internal writer to point at the PTY stdin writer (instead of
`std::io::sink()`).

**Important:** The Terminal must be created with the PTY writer so that
`key_down()` / `send_paste()` / `mouse_event()` write to the correct
destination. We'll update the Terminal's writer when the PTY is spawned:

```rust
// When spawning PTY:
let pty_writer = WriterWrapper::new(pair.master.take_writer()?);
// Replace the Terminal's sink writer with the real PTY writer
// Terminal::new() takes a writer; we need to set it post-construction.
// Options:
//   a) Re-create the Terminal with the PTY writer
//   b) Use a SharedWriter that can be swapped (Arc<Mutex<Box<dyn Write>>>)
//   c) Intercept at the Pane level — don't use Terminal::key_down()

// Best approach: (b) SharedWriter
```

**SharedWriter pattern:**
The Terminal is created with a `SharedWriter` that wraps an
`Arc<Mutex<Box<dyn Write + Send>>>`. Initially it points to `io::sink()`.
When a PTY is spawned, we swap the inner writer to the PTY master writer.
When the PTY exits, we swap back to sink.

```rust
#[derive(Clone)]
struct SharedWriter {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.lock().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.lock().flush()
    }
}
```

#### Mode Toggle (Ctrl+T)

Ctrl+T cycles through modes: Agent -> Terminal -> Agent.

When switching to Terminal mode:
1. If no PTY exists, spawn one (shell at CWD).
2. Swap SharedWriter to PTY writer.
3. Switch key routing to Terminal path.
4. Update screen chrome (status bar shows "TERMINAL" indicator).
5. The Terminal scroll region keeps showing — the shell prompt appears naturally.

When switching back to Agent mode:
1. Swap SharedWriter back to sink.
2. Switch key routing to InputEditor path.
3. PTY stays alive in background (shell is still running).
4. Update screen chrome (status bar shows "AGENT" indicator).

#### Auto-Detection

In addition to manual Ctrl+T, we can auto-detect when to suggest terminal mode:
- If the agent's `BashTool` encounters a command that needs interaction
  (detected via the command name: `vim`, `htop`, `python`, `gdb`, etc.), the
  agent can suggest switching to terminal mode.
- If `is_alt_screen_active()` becomes true on the PTY terminal, we know a
  full-screen program has started.

### C. Screen Reading for Agent

The agent reads the PTY terminal's screen through the same Terminal object that
the renderer uses. This is the key advantage of Option 3.

#### Reading Methods

```rust
impl ElwoodPane {
    /// Read the visible screen content as text lines.
    /// Used by the agent to understand what an interactive program is showing.
    pub fn read_screen(&self) -> Vec<String> {
        let mut terminal = self.terminal.lock();
        let dims = terminal_get_dimensions(&mut terminal);
        let range = dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex;
        let (_, lines) = terminal_get_lines(&mut terminal, range);
        lines.iter().map(|l| l.as_str().to_string()).collect()
    }

    /// Read semantic zones (prompt/input/output regions).
    /// Only useful if the shell supports OSC 133.
    pub fn read_semantic_zones(&self) -> Vec<SemanticZone> {
        let mut terminal = self.terminal.lock();
        terminal.get_semantic_zones().unwrap_or_default()
    }

    /// Read the cursor position.
    pub fn read_cursor(&self) -> StableCursorPosition {
        let mut terminal = self.terminal.lock();
        terminal_get_cursor_position(&mut terminal)
    }

    /// Check if the terminal is in alt-screen mode (full-screen program).
    pub fn is_alt_screen(&self) -> bool {
        self.terminal.lock().is_alt_screen_active()
    }

    /// Check if the terminal is grabbing mouse input.
    pub fn is_mouse_grabbed_by_pty(&self) -> bool {
        self.terminal.lock().is_mouse_grabbed()
    }

    /// Detect if password input mode is active (echo disabled + canonical).
    #[cfg(unix)]
    pub fn is_password_input(&self) -> bool {
        let pty = self.inner_pty.lock();
        if let Some(ref inner) = *pty {
            if let Some(tio) = inner.master.get_termios() {
                use nix::sys::termios::LocalFlags;
                return !tio.local_flags.contains(LocalFlags::ECHO)
                    && tio.local_flags.contains(LocalFlags::ICANON);
            }
        }
        false
    }
}
```

#### Screen Diffing for Agent

The agent should not re-read the entire screen every tick. Instead:

1. **Sequence number tracking:** WezTerm's Terminal increments a seqno on every
   state change. The agent stores the last-seen seqno and only reads when it
   changes.

2. **Dirty line tracking:** `get_changed_since(range, seqno)` returns only the
   lines that changed, allowing efficient incremental reads.

3. **Content hashing (pilotty pattern):** For the agent's LLM context, we hash
   the screen content and only send a new snapshot when the hash changes.

#### Agent Terminal Context Format

When the agent needs to see the terminal screen, we format it as a structured
text block for the LLM:

```
[Terminal Screen - 80x24 - Alt Screen: No - Cursor: (5, 12)]
postgres=# SELECT * FROM users WHERE id = 42;
 id |  name   |       email        | created_at
----+---------+--------------------+----------------------------
 42 | Alice   | alice@example.com  | 2026-01-15 10:30:00.000000
(1 row)

postgres=#
[End Terminal Screen]
```

### D. Agent Terminal Interaction

#### New Protocol Messages

```rust
// In runtime.rs — new AgentRequest variants:

pub enum AgentRequest {
    // ... existing variants ...

    /// Agent wants to write keystrokes to the PTY.
    /// Requires user approval unless auto-approved.
    PtyWrite {
        /// The keystrokes to send (raw bytes or encoded keys).
        input: String,
        /// Human-readable description of what the agent is doing.
        description: String,
    },

    /// Agent requests a screen snapshot from the PTY.
    PtyReadScreen,

    /// Agent wants to spawn an interactive program in the PTY.
    PtySpawnInteractive {
        command: String,
        working_dir: Option<String>,
    },
}

pub enum AgentResponse {
    // ... existing variants ...

    /// Screen snapshot from the PTY.
    PtyScreenSnapshot {
        lines: Vec<String>,
        cursor_x: usize,
        cursor_y: i64,
        cols: usize,
        rows: usize,
        alt_screen: bool,
    },

    /// Agent is requesting permission to write to the PTY.
    PtyWriteRequest {
        request_id: String,
        input: String,
        description: String,
    },
}
```

#### Agent-Controlled Terminal Flow

1. Agent determines it needs to interact with an interactive program (e.g.,
   "debug this segfault in gdb").
2. Agent sends `PtySpawnInteractive { command: "gdb ./myprogram" }`.
3. ElwoodPane spawns the PTY if not already running, sends the command.
4. Agent sends `PtyReadScreen` to see the gdb prompt.
5. ElwoodPane responds with `PtyScreenSnapshot`.
6. Agent analyzes the screen and decides to send `run` command.
7. Agent sends `PtyWrite { input: "run\r", description: "Start program in gdb" }`.
8. ElwoodPane shows the proposed action to the user for approval.
9. User approves (Enter) or rejects (Ctrl+C).
10. If approved, ElwoodPane writes to PTY stdin.
11. Agent polls screen again to see the result.

#### Safety Considerations

**Permission System:**
- PTY writes from the agent ALWAYS require user approval by default.
- Three permission levels (matching Warp):
  - `AskFirst` — first write requires approval, subsequent auto-approve.
  - `AlwaysAsk` — every write requires explicit approval.
  - `AlwaysAllow` — agent writes freely (dangerous, power-user only).
- Password input detection: If `is_password_input()` is true, NEVER auto-approve
  agent PTY writes. Always require explicit approval.

**Sandboxing:**
- The PTY runs within the same Seatbelt sandbox as other Elwood tools.
- The agent cannot spawn a PTY outside the CWD sandbox boundaries.
- Network-accessing programs are subject to the existing network allowlist.

**Kill switch:**
- Ctrl+C in terminal mode sends SIGINT to the PTY foreground process group.
- Escape in AgentTerminal mode immediately stops the agent from issuing further
  PTY writes and returns control to the user.
- The user can always take over with Ctrl+T.

### E. Implementation Plan

#### Phase 1: PTY Embedding (Core Infrastructure)

**Files to create:**
- `elwood-bridge/src/pty_inner.rs` — `InnerPty` struct, spawn/reader/cleanup.
- `elwood-bridge/src/shared_writer.rs` — `SharedWriter` for swappable Terminal writer.

**Files to modify:**
- `elwood-bridge/src/pane.rs`:
  - Add `inner_pty: Mutex<Option<InnerPty>>` field.
  - Add `mode: Mutex<InteractionMode>` field.
  - Add `shared_writer: SharedWriter` field (replaces `writer: Mutex<Box<dyn Write + Send>>`).
  - Modify `ElwoodPane::new()` to create Terminal with SharedWriter.
  - Modify `key_down()` to dispatch based on mode.
  - Add `spawn_pty()`, `kill_pty()` methods.
  - Add screen reading methods (`read_screen()`, etc.).
  - Modify `resize()` to also resize the inner PTY.
  - Modify `is_dead()` to check both agent and PTY state.
  - Modify `is_alt_screen_active()` to reflect PTY state.
  - Modify `is_mouse_grabbed()` to reflect PTY state.
- `elwood-bridge/src/runtime.rs`:
  - Add `PtyWrite`, `PtyReadScreen`, `PtySpawnInteractive` to `AgentRequest`.
  - Add `PtyScreenSnapshot`, `PtyWriteRequest` to `AgentResponse`.

**Files to modify (WezTerm):**
- None in Phase 1. The PTY infrastructure is entirely within elwood-bridge.

#### Phase 2: Agent Screen Reading

**Files to modify:**
- `elwood-bridge/src/domain.rs` (`agent_runtime_loop`):
  - Handle `PtyReadScreen` requests by reading ElwoodPane's screen.
  - Handle `PtySpawnInteractive` requests.
- `elwood-bridge/src/pane.rs`:
  - Wire `poll_responses()` to handle `PtyWrite` approval flow.

**New tool in elwood-core:**
- `TerminalInteractTool` — an LLM tool that lets the agent:
  - Read the terminal screen (`read_screen` action).
  - Send keystrokes (`send_keys` action).
  - Check terminal state (`get_state` action — alt screen, cursor, password mode).

#### Phase 3: Agent-Controlled Terminal Mode

**Files to modify:**
- `elwood-bridge/src/pane.rs`:
  - Implement `AgentTerminal` mode with approval UI.
  - Show agent's proposed keystrokes in the status bar.
  - Implement auto-approval tracking.
- `elwood-bridge/src/screen.rs`:
  - Add rendering for agent terminal proposals (similar to permission requests).

#### Phase 4: Polish

- Password detection integration (suppress auto-approval).
- Process info display (foreground process name in status bar).
- Terminal mode indicator in header chrome.
- History: agent can scroll back through PTY scrollback.
- Semantic zone support for shell-specific features.

### Test Strategy

#### Unit Tests

1. **SharedWriter swap test:** Verify writes go to the correct destination after
   swap.
2. **InteractionMode transitions:** Verify mode cycling and state consistency.
3. **Screen reading:** Create a Terminal, feed it ANSI, verify `read_screen()`
   returns expected text.
4. **PTY spawn/kill lifecycle:** Verify clean startup and shutdown without leaks.

#### Integration Tests

1. **PTY echo test:** Spawn `cat`, write text, verify it appears in Terminal
   screen buffer.
2. **Interactive program test:** Spawn `sh`, send `echo hello`, verify output
   appears in screen.
3. **Resize propagation:** Verify resizing ElwoodPane also resizes the inner PTY.
4. **Mode switch during active PTY:** Switch Agent -> Terminal -> Agent while
   shell is running.

#### Manual Testing

1. Run `vim`, verify full-screen rendering, verify Ctrl+T returns to agent mode.
2. Run `python3` REPL, type expressions, verify output.
3. Run `htop`, verify alt-screen detection.
4. Agent-controlled: ask agent to "run the tests and fix any failures" —
   verify it spawns PTY, reads screen, proposes commands.

## Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| PTY embedding approach | Option 3: Hybrid with inner PTY | Single Terminal, minimal disruption, agent gets free screen reading |
| Writer sharing | SharedWriter (Arc<Mutex<Box<Write>>>) | Allows swapping writer destination without recreating Terminal |
| Reader thread | std::thread (not tokio) | PTY fd read is blocking I/O, doesn't belong on async executor |
| Key routing | Mode-based dispatch in key_down() | Clean separation, single intercept point |
| Agent screen format | Structured text with metadata header | LLM-friendly, includes cursor/alt-screen state |
| Permission model | Three levels (AskFirst/AlwaysAsk/AlwaysAllow) | Matches Warp's proven UX, safe defaults |
| PTY spawn timing | Lazy (on first terminal-mode entry) | No overhead for agent-only sessions |

## Open Questions

1. **Should the PTY share the Terminal or have its own?**
   This design uses a shared Terminal. An alternative is two Terminals — one for
   agent chrome (header/input/status) and one for the PTY. The shared approach is
   simpler but means the agent's chrome disappears in terminal mode (replaced by
   the shell). This may actually be desirable — when you are in terminal mode,
   you want to see the terminal, not the agent chrome.

2. **How to handle the scroll region in terminal mode?**
   In agent mode, the Terminal uses a scroll region for the chat area with fixed
   chrome. In terminal mode, the shell owns the full screen. We should reset the
   scroll region when entering terminal mode and restore it when returning to
   agent mode. Alternatively, we could use a second "sub-terminal" for the PTY
   embedded within the scroll region, but this adds complexity.

   **Recommendation:** In terminal mode, the PTY owns the full Terminal screen.
   Agent chrome is hidden. On return to agent mode, the full-screen layout is
   re-rendered with the chat history preserved in memory (not in the Terminal
   scrollback — that was overwritten by the shell). This means we need a
   separate chat history buffer that can be replayed.

3. **Should we support split view (agent + terminal side by side)?**
   Not in Phase 1. WezTerm already has split pane support — the user could open
   a regular LocalPane next to the ElwoodPane. A future enhancement could add
   a built-in split within ElwoodPane.

4. **How to handle the agent's `BashTool` once PTY exists?**
   The agent's BashTool currently uses `tokio::process::Command`. Once the PTY
   exists, the agent could optionally route commands through the PTY instead.
   This is a Phase 3+ consideration. For now, BashTool and PTY are independent.

## References

- [Warp Full Terminal Use Docs](https://docs.warp.dev/agent-platform/capabilities/full-terminal-use)
- [Warp Agents 3.0 Blog Post](https://www.warp.dev/blog/agents-3-full-terminal-use-plan-code-review-integration)
- [interminai — PTY proxy for AI agents](https://github.com/mstsirkin/interminai)
- [pilotty — Daemon-managed PTY sessions](https://github.com/msmps/pilotty)
- [term-cli — Interactive terminals for AI agents](https://github.com/EliasOenal/term-cli)
- WezTerm source: `pty/src/lib.rs`, `mux/src/localpane.rs`, `mux/src/domain.rs`
