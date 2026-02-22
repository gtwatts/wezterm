# Warp Terminal TUI/UX Research

**Purpose**: Deep analysis of Warp terminal's visual design, UX innovations, and technical
implementation to inform Elwood Terminal's ANSI-based rendering approach.

**Context**: Elwood renders agent output as ANSI escape sequences through a virtual
`wezterm_term::Terminal`, which WezTerm's GPU renderer displays. We cannot use Warp's
native GPU primitives directly, but we can replicate much of the visual language using
Unicode box-drawing, 24-bit true color, and careful ANSI sequencing.

---

## Table of Contents

1. [Visual Hierarchy & Layout](#1-visual-hierarchy--layout)
2. [Block Model Chrome](#2-block-model-chrome)
3. [Input Area Design](#3-input-area-design)
4. [Animation & Transitions](#4-animation--transitions)
5. [Color System & Theming](#5-color-system--theming)
6. [GPU Rendering Architecture](#6-gpu-rendering-architecture)
7. [Agent UX](#7-agent-ux)
8. [Keyboard Navigation](#8-keyboard-navigation)
9. [Responsive Design](#9-responsive-design)
10. [Status Bar & Chrome](#10-status-bar--chrome)
11. [ANSI Implementation Techniques](#11-ansi-implementation-techniques)
12. [Elwood Implementation Recommendations](#12-elwood-implementation-recommendations)
13. [Proposed Layouts (ASCII Mockups)](#13-proposed-layouts-ascii-mockups)
14. [Color Palette Recommendations](#14-color-palette-recommendations)

---

## 1. Visual Hierarchy & Layout

### Warp's Approach

Warp's visual hierarchy is built around the **block model** -- each command and its output
form a discrete, selectable unit. This reduces context-switching by ~28% compared to raw
scrollback because blocks keep input and output grouped.

**Hierarchy (top to bottom):**
```
+--------------------------------------------------+
|  Tab Bar (tabs, pane indicators)                  |
+--------------------------------------------------+
|                                                    |
|  Block N-2: [prompt] [command]                     |
|             [output...]                            |
|                                                    |
|  Block N-1: [prompt] [command]                     |
|             [output...]                            |
|                                                    |
|  Block N:   [prompt] [command]  <- current         |
|             [output... streaming]                  |
|                                                    |
+--------------------------------------------------+
|  Input Editor (pinned bottom or top)               |
|  [contextual chips] [mode indicator] [toolbelt]    |
+--------------------------------------------------+
|  Status Bar (git branch, model, mode)              |
+--------------------------------------------------+
```

**Key layout decisions:**
- Blocks grow from bottom to top (newest at bottom)
- Input editor can be positioned at bottom (default), top, or "start at top"
- The "same line prompt" merges prompt and input on one line via a "left notch" --
  the truncated prompt grid occupies space on the input editor's first line
- Context chips (directory, git status, conversation) are inline with the input
- Panes support horizontal and vertical splits with drag-and-drop rearrangement

**Window > Tab > Pane > Block hierarchy:**
A window holds tabs. A tab holds panes. A pane holds blocks. A block holds a shell command
and its output. This four-level hierarchy is consistent throughout.

### Implications for Elwood

Since we render through ANSI into a virtual terminal, our "blocks" will be visual groupings
using box-drawing characters and color differentiation rather than native GPU primitives.
We should still follow the same hierarchy.

---

## 2. Block Model Chrome

### How Blocks Are Structured

Each Warp block consists of **three separate grids**:
1. **Prompt grid** -- the shell prompt (PS1)
2. **Input grid** -- the command typed by the user
3. **Output grid** -- command stdout/stderr

This grid isolation prevents VT100 cursor repositioning from causing one command's output to
overwrite another's. Each grid is backed by a **circular buffer** storing rows sequentially
in a vector, with `bottom_row` and `length` metadata for O(1) scroll operations.

### Block Creation Protocol

Blocks are demarcated via **shell hooks** (precmd/preexec in zsh/fish, bash-preexec for bash).
These hooks send a custom **DCS (Device Control String)** containing encoded JSON metadata:

```
DCS = "\x1bP" + encoded_json + "\x1b\\"
```

The JSON includes session metadata (working directory, exit code, timing, etc.). Warp parses
the DCS, deserializes the JSON, and creates a new block in its data model.

### Block Visual States

| State | Visual Treatment |
|-------|-----------------|
| Normal | Subtle border, standard colors |
| Selected | Accent-colored left border + highlight |
| Error (non-zero exit) | Red background tint + red left sidebar |
| Running | Animated spinner or progress indicator |
| Collapsed | Compact header-only view |
| Hover | Slight background highlight |

**Sticky Command Header**: When scrolling through a long block's output, the command that
produced it stays visible as a sticky header at the top of the viewport.

### Block Header Information

Warp block headers display:
- The command text
- Exit code (color-coded: green for 0, red for non-zero)
- Execution duration
- Working directory
- Timestamp (hover state)

### Block Selection

- Single block: Click or Cmd+Up/Cmd+Down
- Multiple blocks: Cmd+Click to toggle, Shift+Click for range
- Expand selection: Shift+Up/Shift+Down
- All blocks: Cmd+A
- Bookmark block: Cmd+B
- Navigate bookmarks: Alt+Up/Alt+Down
- Copy command only: Shift+Cmd+C
- Copy output only: Alt+Shift+Cmd+C
- Reinput command: Cmd+I

### ANSI Block Rendering Strategy

For Elwood, we render blocks using Unicode box-drawing and background colors:

```
Top border:    \u250C\u2500\u2500\u2500...  (light rounded: \u256D for rounded corners)
Left border:   \u2502                        (or colored \u2588 full block for accent)
Bottom border: \u2514\u2500\u2500\u2500...  (light rounded: \u2570 for rounded corners)
```

Error blocks use `\x1b[48;2;60;20;20m` (dark red background tint) plus a red left bar
rendered with `\x1b[31m\u2588` (red full block character).

---

## 3. Input Area Design

### Warp's IDE-Style Editor

Warp replaced the traditional readline-based input with a **full text editor** supporting:

- **Mouse-based cursor positioning** -- click anywhere to place cursor
- **Multiple cursors** -- Cmd+D for next occurrence, Alt+Click for additional cursors
- **Syntax highlighting** -- tokenizes commands into sub-commands, options/flags,
  arguments, and variables with distinct colors
- **Error highlighting** -- dashed red underline for invalid commands/unknown binaries
- **Smart bracket completion** -- auto-closes quotes, brackets, parentheses
- **Multi-line editing** -- Ctrl+J inserts newline
- **Undo/Redo** -- Cmd+Z / Shift+Cmd+Z
- **Vim keybindings** -- optional mode

### Syntax Highlighting Architecture

Warp built a custom command parser (loosely based on Nushell) that:
1. Identifies command validity for error underlining
2. Segments commands into distinct parts (command, subcommand, option, argument, variable)
3. Runs **asynchronously** to prevent performance regression during typing
4. Uses **debouncing** -- waits until token completion (spacebar, paste, cursor) before
   highlighting errors to avoid red-underlining partial words like "gi" before "git"

The styling system uses a **SumTree** data structure (essentially a Rope with generic types
indexable on multiple dimensions). It supports:
- O(log N) indexing operations
- Rapid insertion/deletion/style updates mid-chunk
- Distinction between **inheritable** styles (user-applied colors propagate) and
  **non-inheritable** styles (error indicators don't propagate to new characters)

### Auto-Detection (Universal Input)

Warp's Universal Input auto-detects whether the user is typing a shell command or a natural
language agent prompt using a **completely local model**. This enables seamless switching
between terminal mode and agent mode without explicit mode toggling.

Toggle with Cmd+I (macOS) / Ctrl+I (Linux/Windows).

### Completions

- Completions for 400+ CLI tools (cargo, docker, terraform, vim, etc.)
- Tab or arrow key acceptance (configurable)
- Fish-style autosuggestions (ghost text)
- Context-aware (git branch names, file paths, etc.)

### Contextual Chips (Input Toolbelt)

The input area shows inline chips for:
- Current directory
- Git branch + uncommitted file count
- Active conversation context
- Mode indicator (Terminal / Agent / sparkle icon)

### Implications for Elwood

Our input area renders as ANSI into the virtual terminal. We can achieve syntax highlighting
by emitting colored text. For autocompletion dropdowns, we render them as overlaid content
at specific cursor positions. Key techniques:

- **Ghost text**: Dim color (`\x1b[2m` or `\x1b[38;5;240m`) for autosuggestions
- **Error underlines**: `\x1b[4:3m` (curly underline, kitty extension) or `\x1b[4m\x1b[31m`
- **Syntax colors**: 24-bit RGB for each token type
- **Mode indicator**: Unicode symbols (e.g., `>_` for terminal, sparkle for agent)

---

## 4. Animation & Transitions

### Current Warp State

Warp's animation support is relatively limited compared to its other innovations:

- **No native smooth scrolling** -- this has been requested as a feature (GitHub issue #6169)
- **No tab loading spinner** -- also requested but not implemented
- **Block entry**: Blocks appear immediately without entrance animation
- **GPU rendering enables 144+ FPS** so transitions appear smooth even without explicit
  animation curves

### What They Do Well

- Streaming output updates in real-time at high frame rates (1.9ms average redraw)
- Selected block highlighting transitions are instantaneous
- Context menu and autocomplete dropdown appearance is immediate
- Agent output streams with markdown rendering updated in real-time

### ANSI Animation Techniques for Elwood

Since we render through ANSI, we can implement:

**Spinners:**
```
Frames: \u280B \u2819 \u2838 \u2830 \u2826 \u2807  (braille dots spinner)
        \u25DC \u25DD \u25DE \u25DF                  (arc spinner)
        \u2588\u2589\u258A\u258B\u258C\u258D\u258E\u258F  (block elements for progress)
```

**Progress bars:**
```
[\u2588\u2588\u2588\u2588\u2588\u2591\u2591\u2591\u2591\u2591] 50%
```

**Streaming text effect:**
- Write characters with cursor positioning, updating in-place via `\x1b[{row};{col}H`
- Use `\x1b[2K` (erase line) + rewrite for updating content

**Fade-in effect:**
- Render text first in dim (`\x1b[2m`), then rewrite in normal weight

**Typing indicator:**
- Three-dot animation: `.  ` -> `.. ` -> `...` -> `.  `
- Or braille spinner beside "thinking..." text

---

## 5. Color System & Theming

### Warp's Theme Architecture

Warp themes are YAML-based with this schema:

```yaml
name: "Theme Name"
accent: "#5C8FFF"        # UI highlight, tab indicator, block selection
cursor: "#5C8FFF"        # Defaults to accent if omitted
background: "#1E1E2E"    # Terminal background
foreground: "#CDD6F4"    # Default text color
details: "darker"        # "darker" or "lighter" -- determines overlay direction

terminal_colors:
  normal:
    black:   "#45475A"
    red:     "#F38BA8"
    green:   "#A6E3A1"
    yellow:  "#F9E2AF"
    blue:    "#89B4FA"
    magenta: "#F5C2E7"
    cyan:    "#94E2D5"
    white:   "#BAC2DE"
  bright:
    black:   "#585B70"
    red:     "#F38BA8"
    green:   "#A6E3A1"
    yellow:  "#F9E2AF"
    blue:    "#89B4FA"
    magenta: "#F5C2E7"
    cyan:    "#94E2D5"
    white:   "#A6ADC8"
```

**Gradient support**: Both `accent` and `background` can be gradient objects:
```yaml
background:
  top: "#1E1E2E"
  bottom: "#11111B"
```

### Semantic Color System

Warp distinguishes colors by semantic role:

| Role | Usage |
|------|-------|
| Accent | Selection highlights, tab indicators, active borders, UI elements |
| Background | Terminal background, base canvas |
| Foreground | Default text |
| Surface | Overlay backgrounds (menus, dropdowns, dialogs) |
| Error | Non-zero exit codes, error highlighting (`red`) |
| Success | Zero exit code, successful operations (`green`) |
| Warning | Caution states (`yellow`) |
| Info | Informational messages (`blue`) |

**UI Surface formula:**
- Dark themes: background + white overlay + outline stroke
- Light themes: background + black overlay + outline stroke

This ensures overlaid elements (command palette, autocomplete, context menus) have
consistent visual separation from the terminal content.

### Dark/Light Mode

Warp syncs with OS dark/light mode via Settings > Appearance > "Sync with OS".
The `details` field ("darker"/"lighter") determines the overlay direction for surfaces.

### ANSI 16-Color Foundation

Warp starts with the standard 16 ANSI colors for compatibility with existing themes
(Dracula, Solarized, One Dark, etc.), then layers semantic colors on top via its
accent/surface system.

---

## 6. GPU Rendering Architecture

### Warp's Metal Pipeline

Warp renders the entire UI on the GPU using **Apple Metal** (macOS) or **wgpu** (Linux/Windows).

**Three Primitives:**
1. **Rectangles** -- borders, backgrounds, shadows, rounded corners via SDF
2. **Glyphs** -- text rendering via texture atlas
3. **Images/Icons** -- icon rendering

Total shader code: ~300 lines across all three primitives.

**Vertex/Fragment Shader Pipeline:**
- Vertex shader: transforms viewport coordinates, applies size/origin from `PerRectUniforms`
- Fragment shader: determines pixel color using distance fields

**Rectangle Rendering (SDF-based):**
```
distance = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - corner_radius
where q = abs(p) - (rect_corner - corner_radius)
```
- Rounded corners via signed distance functions
- Anti-aliasing via `smoothstep()` across edge boundaries
- Linear gradients via dot product projection
- Borders rendered by comparing pixel position against adjusted corner coordinates

**Performance:**
- 144+ FPS with many UI elements on 4K displays
- Average screen redraw: **1.9ms**
- 400+ FPS in controlled scenarios
- Efficient GPU parallelization handles per-pixel calculations

**Cross-Platform Strategy:**
- macOS: Metal
- Linux/Windows: wgpu (Rust), winit, cosmic-text
- Web (future): WASM + WebGL

### Framework Origins

Built in collaboration with Nathan Sobo (Atom co-founder), inspired by Flutter's architecture.
The framework maintains element trees that render through platform-specific backends.

### Relevance to Elwood

We inherit WezTerm's GPU rendering pipeline, which already uses wgpu with Metal/Vulkan/DX12
backends. Our agent output goes through the ANSI -> virtual terminal -> GPU pipeline:

```
ElwoodBridge::render()
  -> ANSI escape sequences
  -> wezterm_term::Terminal (virtual)
  -> WezTerm GPU renderer (wgpu)
  -> Screen
```

We get GPU rendering for free, but our visual fidelity is limited to what ANSI can express.
This means: no rounded corners via SDF, no gradients, no shadows. We compensate with:
- Unicode box-drawing characters for borders
- Block elements (\u2580-\u259F) for partial fills
- 24-bit color for rich color expression
- Background/foreground color combinations for visual depth

---

## 7. Agent UX

### Warp 2.0 Agent Interface

Warp 2.0 redesigned the terminal around an **"Agentic Development Environment"** concept.
The primary workflow shifted from typing commands to **"prompt, steer agents, ship"**.

**Agent Modality:**
- **Terminal Mode**: Clean terminal for commands
- **Agent Mode**: Dedicated conversation view for multi-turn agent workflows
- **Auto-detect**: Local model determines if input is command or natural language
- Lock mode via Cmd+I toggle

**Agent Conversation Flow:**
```
1. User types natural language prompt in input
2. Agent streams response (markdown rendered)
3. Agent proposes commands -> shows command + reasoning
4. User approves/rejects each command
5. Command executes, output displayed
6. Agent analyzes output, proposes next step
7. Repeat until task complete or user stops
```

### Tool Call Visualization

When an agent wants to execute a tool:
1. Shows the agent's **reasoning** (brief explanation)
2. Shows the **proposed command**
3. User sees **Accept / Reject** buttons
4. Approved commands execute with output visible
5. `CMD+G` toggles showing/hiding agent intermediate output

### Permission System

- **Global permissions**: "Ask on first write" or "Always ask"
- **Per-session override**: Approve once, approve similar commands
- **Fast-forward control**: Auto-approve similar commands in current session
- **Command allowlist/denylist** for autonomous execution
- **MCP server permission scoping**

### Code Diff Display

When an agent generates code changes:
- Opens built-in text editor with **visual diff view**
- Changes grouped into **hunks**
- Navigate hunks: Up/Down arrow keys
- Navigate files: Left/Right arrow keys
- **Inline editing**: Edit diffs directly in the diff view
- **Inline comments**: Leave feedback, batch it, agent resolves all at once
- **Accept/Reject** per hunk or entire diff
- Diff view works against current branch or main
- Real-time updates as agent writes changes

### Planning Mode

- `/plan` command triggers specification-driven development
- Uses reasoning models (like o3) for alignment before execution
- Plans are shareable, versionable, stored for team reference
- Plans can be attached to PRs

### Notifications

- In-app + system notifications when agent completes or needs help
- Supports multithreaded agent workflows (multiple agents running in parallel)

### Streaming & Markdown

Agent output renders with full **markdown formatting**:
- Headers (H1-H6)
- Code blocks with syntax highlighting
- Lists (ordered, unordered, nested)
- Bold, italic, strikethrough
- Links
- Tables (recently improved)
- Custom font support for agent output (Settings > Appearance)

### Agent Performance

- Agents 3.0: Full terminal use (debuggers, REPLs, interactive tools)
- #1 on Terminal-Bench 2.0 (52%)
- Top 3 on SWE-Bench Verified (75.8%)
- 96%+ acceptance rate for agent-suggested code changes
- Generated 3.2 billion lines of code
- Indexed 120,000+ codebases

### Elwood Agent UX Strategy

Our agent output goes through ANSI, so we need to render:

**Agent message blocks:**
```
\x1b[38;2;130;170;255m\u256D\u2500 Agent \u2500\u2500\u2500\u2500\u2500\x1b[0m
\x1b[38;2;130;170;255m\u2502\x1b[0m  I'll help you fix that bug.
\x1b[38;2;130;170;255m\u2502\x1b[0m  Let me check the file first...
\x1b[38;2;130;170;255m\u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\x1b[0m
```

**Tool call blocks:**
```
\x1b[38;2;180;180;100m\u256D\u2500 Tool: read_file \u2500\u2500\u2500\x1b[0m
\x1b[38;2;180;180;100m\u2502\x1b[0m  path: src/main.rs
\x1b[38;2;180;180;100m\u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\x1b[0m
```

**Permission prompts:**
```
\x1b[38;2;255;200;50m\u256D\u2500 Permission Required \u2500\u2500\u2500\x1b[0m
\x1b[38;2;255;200;50m\u2502\x1b[0m  Execute: \x1b[1mcargo test\x1b[0m
\x1b[38;2;255;200;50m\u2502\x1b[0m  [y] Allow  [n] Deny  [a] Always
\x1b[38;2;255;200;50m\u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\x1b[0m
```

**Diff rendering:**
```
\x1b[38;2;100;100;100m\u256D\u2500 src/main.rs \u2500\u2500\u2500\x1b[0m
\x1b[31m- fn old_function() {\x1b[0m
\x1b[32m+ fn new_function() {\x1b[0m
\x1b[38;2;100;100;100m\u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\x1b[0m
```

---

## 8. Keyboard Navigation

### Warp's Complete Shortcut System

**Block Navigation:**
| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| Select next block | Cmd+Down | Ctrl+Down |
| Select previous block | Cmd+Up | Ctrl+Up |
| Select all blocks | Cmd+A | Ctrl+Shift+A |
| Bookmark block | Cmd+B | Ctrl+Shift+B |
| Next bookmark | Alt+Down | Alt+Down |
| Previous bookmark | Alt+Up | Alt+Up |
| Expand selection down | Shift+Down | Shift+Down |
| Expand selection up | Shift+Up | Shift+Up |
| Block context menu | Ctrl+M | -- |
| Share block | Shift+Cmd+S | Ctrl+Shift+S |

**Text Editing (Input):**
| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| Delete word left | Alt+Backspace | Ctrl+Backspace |
| Delete word right | Alt+Delete | Ctrl+Delete |
| Cursor left | Ctrl+B | Ctrl+B |
| Cursor right | Ctrl+F | Ctrl+F |
| Line start | Ctrl+A | Ctrl+A |
| Line end | Ctrl+E | Ctrl+E |
| Delete char | Ctrl+H | Ctrl+H |
| Insert newline | Ctrl+J | Ctrl+J |
| Undo | Cmd+Z | Ctrl+Z |
| Redo | Shift+Cmd+Z | Ctrl+Shift+Z |

**Window/Tab/Pane:**
| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| New tab | Cmd+T | Ctrl+Shift+T |
| Switch tab 1-9 | Cmd+1-9 | Ctrl+1-9 |
| Previous tab | Shift+Cmd+{ | Ctrl+PageUp |
| Next tab | Shift+Cmd+} | Ctrl+PageDown |
| Split pane right | Cmd+D | Ctrl+Shift+D |
| Split pane down | Shift+Cmd+D | Ctrl+Shift+E |
| Maximize pane | Shift+Cmd+Enter | Ctrl+Shift+Enter |
| Switch panes | Alt+Cmd+Arrows | Ctrl+Alt+Arrows |
| Resize pane | Ctrl+Cmd+Arrows | -- |

**Search & Navigation:**
| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| Command palette | Cmd+P | Ctrl+Shift+P |
| Command search (history) | Ctrl+R | Ctrl+R |
| Find in output | Cmd+F | Ctrl+Shift+F |
| Find next | Cmd+G | F3 |
| Find previous | Shift+Cmd+G | Shift+F3 |
| Navigation palette | Shift+Cmd+P | -- |
| Generate (AI) | Ctrl+` | Ctrl+` |
| Workflows | Ctrl+Shift+R | Ctrl+Shift+R |
| Warp Drive | Cmd+\ | Ctrl+Shift+\ |
| Settings | Cmd+, | Ctrl+, |
| Show all shortcuts | Cmd+/ | -- |

**Agent-specific:**
| Action | macOS | Linux/Windows |
|--------|-------|---------------|
| Toggle agent/terminal mode | Cmd+I | Ctrl+I |
| Toggle show/hide agent output | Cmd+G | Cmd+G |

### Implications for Elwood

Many of these shortcuts conflict with shell defaults. Elwood should:
1. Use the WezTerm key binding system (already has leader key + configurable bindings)
2. Reserve certain patterns for agent-specific actions
3. Follow Warp's Cmd+Up/Down for block navigation concept
4. Map agent mode toggle to a consistent key

---

## 9. Responsive Design

### Warp's Approach

Warp handles varying terminal sizes through:

- **Grid resize rewriting**: When display dimensions change, grids are completely rewritten
  (this is the trade-off for their circular buffer optimization)
- **Soft text wrapping**: Handles non-rectangular shapes (the "left notch" from same-line
  prompt required contributing indentation code to COSMIC Text library)
- **Block truncation**: Long output blocks show "Jump to bottom of this block" link
- **Sticky headers**: Command stays visible when scrolling through long output
- **Font size controls**: Cmd+=/Cmd+- with Cmd+0 reset
- **Full UI zoom**: Added in 2025 (previously limited by custom framework)

### Minimum viable content

Warp degrades gracefully:
- Very narrow terminals still show blocks but with heavy wrapping
- Context chips in input area collapse or hide at small widths
- Autocomplete dropdowns reposition to fit available space

### Elwood Strategy

Our ANSI renderer should:
- Query terminal dimensions and adapt layout accordingly
- Truncate long lines with ellipsis at terminal width
- Collapse block borders for very narrow terminals (< 40 cols)
- Always reserve at least 2 columns for the left border indicator
- Test at common sizes: 80x24, 120x40, 200x60

---

## 10. Status Bar & Chrome

### Warp's Status Information

Warp's status display includes:

**Input Area Chips (inline):**
- Current working directory
- Git branch + uncommitted changes count (staged + unstaged)
- Conversation context indicator
- Mode indicator: `>_` for terminal, sparkle icon for agent

**Agent Controls (input toolbelt):**
- Model selector dropdown
- Voice input button
- Image/file attachment (`@` key for file references)
- Slash command access
- AI feature controls

**Git Context Chip:**
- Branch name (or commit hash in detached HEAD)
- Count of new/modified/deleted files
- Visual indicator for staged vs unstaged

**Block-level Status:**
- Exit code (0 = green, non-zero = red)
- Duration
- Working directory at time of execution

### Elwood Status Bar Design

We should render a status bar at the bottom of our agent pane:

```
\x1b[48;2;30;30;50m\x1b[38;2;100;200;100m main \x1b[38;2;150;150;150m| \x1b[38;2;130;170;255mgemini-2.5-pro \x1b[38;2;150;150;150m| \x1b[38;2;200;200;100m1.2k tokens \x1b[38;2;150;150;150m| \x1b[38;2;180;180;180m$0.03 \x1b[38;2;150;150;150m| \x1b[38;2;100;200;100magent\x1b[0m
```

Rendering: `  main | gemini-2.5-pro | 1.2k tokens | $0.03 | agent`

---

## 11. ANSI Implementation Techniques

### Core Escape Sequences for Elwood

**Cursor Control:**
```
\x1b[H          -- move to home (0,0)
\x1b[{r};{c}H   -- move to row r, column c
\x1b[#A/B/C/D   -- move up/down/right/left by # cells
\x1b[s / \x1b[u -- save/restore cursor position
\x1b[?25h/l     -- show/hide cursor
```

**Text Formatting:**
```
\x1b[0m   -- reset all
\x1b[1m   -- bold
\x1b[2m   -- dim (for ghost text, secondary info)
\x1b[3m   -- italic (for agent thinking text)
\x1b[4m   -- underline
\x1b[7m   -- inverse (for selections, highlights)
\x1b[9m   -- strikethrough (for deleted code in diffs)
```

**24-bit True Color:**
```
\x1b[38;2;{r};{g};{b}m  -- foreground RGB
\x1b[48;2;{r};{g};{b}m  -- background RGB
```

**Screen Management:**
```
\x1b[2J   -- clear screen
\x1b[2K   -- erase entire line
\x1b[0K   -- erase from cursor to end of line
\x1b[?1049h/l  -- enter/leave alternate screen buffer
```

### Unicode Box Drawing Characters

**Light borders (preferred for blocks):**
```
\u250C \u2500 \u2510   -- top-left, horizontal, top-right
\u2502       \u2502   -- vertical
\u2514 \u2500 \u2518   -- bottom-left, horizontal, bottom-right
```

**Rounded corners (modern feel):**
```
\u256D \u2500 \u256E   -- rounded top-left, horizontal, rounded top-right
\u2502       \u2502   -- vertical
\u2570 \u2500 \u256F   -- rounded bottom-left, horizontal, rounded bottom-right
```

**Heavy borders (for emphasis/selection):**
```
\u250F \u2501 \u2513   -- heavy top-left, horizontal, top-right
\u2503       \u2503   -- heavy vertical
\u2517 \u2501 \u251B   -- heavy bottom-left, horizontal, bottom-right
```

**Block elements (for bars, progress, fills):**
```
\u2588  -- full block (for accent bars)
\u2589-\u258F  -- left 7/8 to 1/8 block (for progress)
\u2580  -- upper half block
\u2584  -- lower half block
\u2591  -- light shade
\u2592  -- medium shade
\u2593  -- dark shade
```

**Braille patterns (for spinners):**
```
\u280B \u2819 \u2838 \u2830 \u2826 \u2807  -- 6-frame dots spinner
```

### Rich Text Rendering Patterns

**Markdown-to-ANSI mapping:**
```
# Header    ->  \x1b[1m\x1b[38;2;130;170;255mHeader\x1b[0m  (bold + accent)
**bold**    ->  \x1b[1mbold\x1b[22m
*italic*    ->  \x1b[3mitalic\x1b[23m
`code`      ->  \x1b[48;2;40;40;60m\x1b[38;2;220;170;120m code \x1b[0m
~~strike~~  ->  \x1b[9mstrike\x1b[29m
```

**Code block rendering:**
```
\x1b[48;2;30;30;45m                         \x1b[0m  <- background fill line
\x1b[48;2;30;30;45m  fn main() {             \x1b[0m  <- code with bg
\x1b[48;2;30;30;45m      println!("hello");  \x1b[0m
\x1b[48;2;30;30;45m  }                       \x1b[0m
\x1b[48;2;30;30;45m                         \x1b[0m
```

---

## 12. Elwood Implementation Recommendations

### Priority 1: Block Model (Critical)

1. **Render agent interactions as visual blocks** using rounded Unicode borders
2. Each block type gets a distinct left-border color:
   - Agent response: blue (`#829AFF`)
   - Tool call: amber (`#B4B464`)
   - Tool result: dim gray (`#646464`)
   - Error: red (`#F38BA8`)
   - Permission prompt: yellow (`#F9E2AF`)
   - User message: green (`#A6E3A1`)
3. **Collapsible blocks**: Render header-only with `[+]` indicator for collapsed state
4. **Sticky command header**: When output exceeds viewport, keep the block header visible

### Priority 2: Agent Streaming (Critical)

1. Stream markdown-rendered text character by character
2. Use `\x1b[{row};{col}H` to update in-place during streaming
3. Render thinking indicators: `\x1b[2m\x1b[3mThinking...\x1b[0m` with spinner
4. Tool calls appear as collapsed blocks that expand when results arrive
5. Show tool execution duration after completion

### Priority 3: Diff Rendering (High)

1. Unified diff format with colored additions/deletions
2. File header in dim color with filename
3. `\x1b[32m+` for additions, `\x1b[31m-` for deletions
4. Context lines in default foreground
5. Hunk headers in dim cyan
6. Consider line numbers in gutter (dim)

### Priority 4: Permission Prompts (High)

1. Yellow-bordered blocks for permission requests
2. Show the command/action in bold
3. Show agent's reasoning in dim text
4. Keyboard shortcuts: `y` (allow), `n` (deny), `a` (always allow), `s` (session allow)
5. Auto-approve animation: brief green flash then continue

### Priority 5: Status Bar (Medium)

1. Full-width bar at bottom of agent pane
2. Segments: git branch, model name, token count, cost, mode
3. Background color slightly different from terminal background
4. Separator: dim `|` between segments
5. Right-align cost/tokens, left-align git/model

### Priority 6: Input Polish (Medium)

1. Mode indicator character before prompt
2. Ghost text in dim for autosuggestions
3. Syntax-colored input (command vs args vs flags)
4. Error underline for unknown commands

### Priority 7: Theming (Lower)

1. Define a Warp-compatible theme structure in TOML
2. Map theme colors to our semantic roles
3. Support at minimum: background, foreground, accent, 16 ANSI colors
4. Dark mode default with light mode support

---

## 13. Proposed Layouts (ASCII Mockups)

### Agent Conversation View

```
+==============================================================+
|  Elwood Terminal              [main]  gemini-2.5-pro  agent  |
+--------------------------------------------------------------+
|                                                              |
|  > Fix the failing test in src/auth.rs                       |
|                                                              |
|  \u256D\u2500 Agent \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502 I'll look at the failing test first.                       |
|  \u2502                                                            |
|  \u2502 \u256D\u2500 read_file \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502 \u2502  src/auth.rs (42 lines)                               |
|  \u2502 \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502                                                            |
|  \u2502 The issue is on line 37. The `verify_token` function       |
|  \u2502 returns `Result<Claims>` but the test expects `Option`.    |
|  \u2502                                                            |
|  \u2502 \u256D\u2500 src/auth.rs \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502 \u2502  - let result = verify_token(&token);                 |
|  \u2502 \u2502  + let result = verify_token(&token).ok();             |
|  \u2502 \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502                                                            |
|  \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|                                                              |
|  \u256D\u2500 Permission \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|  \u2502 Execute: cargo test auth::tests                            |
|  \u2502 [y] Allow  [n] Deny  [a] Always  [s] Session               |
|  \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500  |
|                                                              |
+--------------------------------------------------------------+
|  > _                                                         |
+--------------------------------------------------------------+
| \ue0a0 main | gemini-2.5-pro | 1.2k tok | $0.03 | agent        |
+--------------------------------------------------------------+
```

### Tool Execution with Spinner

```
  \u256D\u2500 Agent \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
  \u2502 Running the test suite now...
  \u2502
  \u2502 \u256D\u2500 bash \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
  \u2502 \u2502 \u280B cargo test auth::tests
  \u2502 \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
```

### Collapsed Tool Result

```
  \u2502 \u25B6 read_file src/auth.rs (42 lines, 1.2ms)
```

### Error Block

```
  \u256D\u2500 Error \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
  \u2502 Command failed with exit code 1
  \u2502
  \u2502  error[E0308]: mismatched types
  \u2502    --> src/auth.rs:37:20
  \u2502    |
  \u2502 37 |     let result: Option<Claims> = verify_token(&token);
  \u2502    |                 ^^^^^^^^^^^^^^   -------- expected due to this
  \u2502    |                 expected `Option<Claims>`, found `Result<Claims, Error>`
  \u2502
  \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
```

### Thinking/Streaming State

```
  \u256D\u2500 Agent \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
  \u2502 \u2591\u2591\u2592\u2593\u2588 Thinking...
  \u2570\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500
```

---

## 14. Color Palette Recommendations

### Elwood Dark Theme (Recommended Default)

Inspired by Warp's default + Catppuccin Mocha:

```toml
[theme]
name = "Elwood Dark"
background = "#1A1B26"    # Deep blue-black (Tokyo Night inspired)
foreground = "#C0CAF5"    # Soft lavender-white
accent     = "#7AA2F7"    # Bright blue (primary accent)
surface    = "#24283B"    # Slightly lighter than bg (overlays)
border     = "#3B4261"    # Dim blue-gray (inactive borders)

[theme.semantic]
error      = "#F7768E"    # Soft red
success    = "#9ECE6A"    # Soft green
warning    = "#E0AF68"    # Warm amber
info       = "#7DCFFF"    # Light cyan
dim        = "#565F89"    # Muted gray-blue

[theme.agent]
agent_text   = "#C0CAF5"    # Default foreground
agent_border = "#7AA2F7"    # Accent blue
tool_border  = "#BB9AF7"    # Purple for tool calls
tool_result  = "#565F89"    # Dim for results
diff_add     = "#9ECE6A"    # Green additions
diff_del     = "#F7768E"    # Red deletions
diff_context = "#565F89"    # Dim context
diff_header  = "#7DCFFF"    # Cyan file headers
permission   = "#E0AF68"    # Amber for permission prompts
thinking     = "#565F89"    # Dim for thinking state
user_border  = "#9ECE6A"    # Green for user messages
code_bg      = "#1F2335"    # Slightly different bg for code blocks

[theme.syntax]
keyword    = "#BB9AF7"    # Purple
function   = "#7AA2F7"    # Blue
string     = "#9ECE6A"    # Green
number     = "#FF9E64"    # Orange
comment    = "#565F89"    # Dim
type       = "#2AC3DE"    # Teal
operator   = "#89DDFF"    # Light cyan
variable   = "#C0CAF5"    # Foreground
flag       = "#E0AF68"    # Amber (for --flags)
path       = "#7DCFFF"    # Cyan (for file paths)

[theme.terminal_colors.normal]
black   = "#15161E"
red     = "#F7768E"
green   = "#9ECE6A"
yellow  = "#E0AF68"
blue    = "#7AA2F7"
magenta = "#BB9AF7"
cyan    = "#7DCFFF"
white   = "#A9B1D6"

[theme.terminal_colors.bright]
black   = "#414868"
red     = "#F7768E"
green   = "#9ECE6A"
yellow  = "#E0AF68"
blue    = "#7AA2F7"
magenta = "#BB9AF7"
cyan    = "#7DCFFF"
white   = "#C0CAF5"
```

### Elwood Light Theme

```toml
[theme]
name = "Elwood Light"
background = "#F5F5F5"
foreground = "#1A1B26"
accent     = "#2E59A8"
surface    = "#E8E8EC"
border     = "#C0C4D0"

[theme.semantic]
error   = "#C53B53"
success = "#587E2A"
warning = "#9A6E1A"
info    = "#2070A0"
dim     = "#8B8FA0"
```

### ANSI Color Map

For rendering in 24-bit color mode:

| Semantic Role | Hex | ANSI Sequence |
|--------------|-----|---------------|
| Agent border | #7AA2F7 | `\x1b[38;2;122;162;247m` |
| Tool border | #BB9AF7 | `\x1b[38;2;187;154;247m` |
| Error | #F7768E | `\x1b[38;2;247;118;142m` |
| Success | #9ECE6A | `\x1b[38;2;158;206;106m` |
| Warning | #E0AF68 | `\x1b[38;2;224;175;104m` |
| Info | #7DCFFF | `\x1b[38;2;125;207;255m` |
| Dim text | #565F89 | `\x1b[38;2;86;95;137m` |
| Code bg | #1F2335 | `\x1b[48;2;31;35;53m` |
| Surface bg | #24283B | `\x1b[48;2;36;40;59m` |

---

## Sources

- [How Warp Works](https://www.warp.dev/blog/how-warp-works)
- [How to Draw Styled Rectangles Using the GPU and Metal](https://www.warp.dev/blog/how-to-draw-styled-rectangles-using-the-gpu-and-metal)
- [The Data Structure Behind Terminals](https://www.warp.dev/blog/the-data-structure-behind-terminals)
- [How We Built Syntax Highlighting for the Terminal Input Editor](https://www.warp.dev/blog/how-we-built-syntax-highlighting-for-the-terminal-input-editor)
- [Warp's Product Principles](https://www.warp.dev/blog/how-we-design-warp-our-product-philosophy)
- [Introducing Warp 2.0: The Agentic Development Environment](https://www.warp.dev/blog/reimagining-coding-agentic-development-environment)
- [Agents 3.0: Full Terminal Use, Plan, Code Review, Integration](https://www.warp.dev/blog/agents-3-full-terminal-use-plan-code-review-integration)
- [Why It Took 11 Months to Move a Single Line of Text](https://www.warp.dev/blog/why-it-took-us-11-months-to-move-a-single-line-of-text)
- [Agent Mode: LLM in the Terminal](https://www.warp.dev/blog/agent-mode)
- [How We Designed Themes for the Terminal](https://www.warp.dev/blog/how-we-designed-themes-for-the-terminal-a-peek-into-our-process)
- [Custom Themes Documentation](https://docs.warp.dev/terminal/appearance/custom-themes)
- [Keyboard Shortcuts Documentation](https://docs.warp.dev/getting-started/keyboard-shortcuts)
- [Block Basics Documentation](https://docs.warp.dev/terminal/blocks/block-basics)
- [Modern Terminal Features](https://www.warp.dev/modern-terminal)
- [Warp 2025 Year in Review](https://www.warp.dev/blog/2025-in-review)
- [ANSI Escape Sequences Reference](https://gist.github.com/fnky/458719343aabd01cfb17a3a4f7296797)
- [Box-Drawing Characters (Wikipedia)](https://en.wikipedia.org/wiki/Box-drawing_characters)
- [HN Discussion: Show HN Warp](https://news.ycombinator.com/item?id=30921231)
