//! Toast notification system for proactive suggestions and status updates.
//!
//! Toasts are transient, auto-dismissing notifications that appear in the
//! top-right corner of the terminal. They stack vertically (max 3 visible)
//! and can carry an optional action (run command, send to agent, etc.).
//!
//! ## Rendering
//!
//! ```text
//!                     +-- i ----------------------+
//!                     | Command finished (12.3s)  |
//!                     | Exit code: 0              |
//!                     +---------------------------+
//!                     +-- ! ----------------------+
//!                     | Error in pane 2           |
//!                     | [Enter] Show fix          |
//!                     +---------------------------+
//! ```
//!
//! Color by level: Info=blue, Success=green, Warning=yellow, Error=red.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

// ─── Toast Level ────────────────────────────────────────────────────────

/// Severity/type of a toast notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    /// Informational (blue).
    Info,
    /// Success confirmation (green).
    Success,
    /// Warning that needs attention (yellow).
    Warning,
    /// Error requiring action (red).
    Error,
}

impl ToastLevel {
    /// Default auto-dismiss duration for this level.
    pub fn default_duration(self) -> Duration {
        match self {
            Self::Info => Duration::from_secs(5),
            Self::Success => Duration::from_secs(5),
            Self::Warning => Duration::from_secs(10),
            Self::Error => Duration::from_secs(30),
        }
    }

    /// Single-character icon for this level.
    fn icon(self) -> &'static str {
        match self {
            Self::Info => "i",
            Self::Success => "*",
            Self::Warning => "!",
            Self::Error => "x",
        }
    }

    /// RGB color tuple for this level (TokyoNight palette).
    fn color(self) -> (u8, u8, u8) {
        match self {
            Self::Info => (125, 207, 255),    // #7DCFFF — info/cyan
            Self::Success => (158, 206, 106), // #9ECE6A — green
            Self::Warning => (224, 175, 104), // #E0AF68 — amber
            Self::Error => (247, 118, 142),   // #F7768E — red
        }
    }
}

// ─── Toast Action ───────────────────────────────────────────────────────

/// An optional action attached to a toast, triggered by pressing Enter.
#[derive(Debug, Clone)]
pub enum ToastAction {
    /// Run a shell command in the terminal.
    RunCommand(String),
    /// Send a message to the agent.
    SendToAgent(String),
    /// Open a URL (future use).
    OpenUrl(String),
    /// Simply dismiss — no action.
    Dismiss,
}

// ─── Toast ──────────────────────────────────────────────────────────────

/// A single toast notification.
#[derive(Debug, Clone)]
pub struct Toast {
    /// Unique identifier.
    pub id: usize,
    /// Primary message text.
    pub message: String,
    /// Optional second line of detail.
    pub detail: Option<String>,
    /// Severity level (determines color and default duration).
    pub level: ToastLevel,
    /// Optional action triggered by Enter.
    pub action: Option<ToastAction>,
    /// When this toast was created.
    pub created_at: Instant,
    /// Auto-dismiss after this duration.
    pub duration: Duration,
}

impl Toast {
    /// Whether this toast has expired (past its duration).
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.duration
    }
}

// ─── Toast Manager ──────────────────────────────────────────────────────

/// Maximum number of toasts visible at once.
const MAX_VISIBLE: usize = 3;

/// Manages a queue of toast notifications.
///
/// Toasts are added to a queue and rendered from the front. Expired toasts
/// are automatically removed on each `tick()`. At most `MAX_VISIBLE`
/// toasts are shown at any time; the rest are queued.
pub struct ToastManager {
    toasts: VecDeque<Toast>,
    next_id: usize,
}

impl ToastManager {
    /// Create a new, empty toast manager.
    pub fn new() -> Self {
        Self {
            toasts: VecDeque::new(),
            next_id: 0,
        }
    }

    /// Push a new toast notification.
    ///
    /// Returns the toast ID for later reference.
    pub fn push(
        &mut self,
        message: impl Into<String>,
        level: ToastLevel,
        duration: Option<Duration>,
        action: Option<ToastAction>,
    ) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.toasts.push_back(Toast {
            id,
            message: message.into(),
            detail: None,
            level,
            action,
            created_at: Instant::now(),
            duration: duration.unwrap_or_else(|| level.default_duration()),
        });
        id
    }

    /// Push a toast with a detail line.
    pub fn push_with_detail(
        &mut self,
        message: impl Into<String>,
        detail: impl Into<String>,
        level: ToastLevel,
        duration: Option<Duration>,
        action: Option<ToastAction>,
    ) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.toasts.push_back(Toast {
            id,
            message: message.into(),
            detail: Some(detail.into()),
            level,
            action,
            created_at: Instant::now(),
            duration: duration.unwrap_or_else(|| level.default_duration()),
        });
        id
    }

    /// Dismiss a specific toast by ID.
    pub fn dismiss(&mut self, id: usize) {
        self.toasts.retain(|t| t.id != id);
    }

    /// Dismiss the top (most recent visible) toast and return its action if any.
    pub fn dismiss_top(&mut self) -> Option<ToastAction> {
        let action = self.toasts.front().and_then(|t| t.action.clone());
        self.toasts.pop_front();
        action
    }

    /// Accept the action on the top toast (same as dismiss but returns the action).
    pub fn accept_top(&mut self) -> Option<ToastAction> {
        self.dismiss_top()
    }

    /// Remove expired toasts. Returns `true` if any were removed.
    pub fn tick(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| !t.is_expired());
        self.toasts.len() != before
    }

    /// The currently visible toasts (up to MAX_VISIBLE from the front).
    pub fn visible_toasts(&self) -> &[Toast] {
        let end = self.toasts.len().min(MAX_VISIBLE);
        // VecDeque::make_contiguous is not available on &self, but
        // we can use the (a, b) slices approach. Since we always push_back
        // and pop_front, the front elements should be contiguous in most cases.
        // Use as_slices and take from the first slice.
        let (front, _back) = self.toasts.as_slices();
        if front.len() >= end {
            &front[..end]
        } else {
            // Edge case: data spans both slices. We only return what fits
            // in the contiguous front slice (toasts that wrapped around).
            front
        }
    }

    /// Whether any toasts are currently visible.
    pub fn has_visible(&self) -> bool {
        !self.toasts.is_empty()
    }

    /// Number of visible toasts.
    pub fn visible_count(&self) -> usize {
        self.toasts.len().min(MAX_VISIBLE)
    }

    /// Total number of toasts (including queued).
    pub fn total_count(&self) -> usize {
        self.toasts.len()
    }

    /// Clear all toasts.
    pub fn clear(&mut self) {
        self.toasts.clear();
    }
}

impl Default for ToastManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── ANSI Rendering ─────────────────────────────────────────────────────

/// True-color foreground escape.
fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";

// Box drawing
const BOX_TL: char = '\u{256D}';
const BOX_TR: char = '\u{256E}';
const BOX_BL: char = '\u{2570}';
const BOX_BR: char = '\u{256F}';
const BOX_H: char = '\u{2500}';
const BOX_V: char = '\u{2502}';

// TokyoNight palette
const FG_PRIMARY: (u8, u8, u8) = (192, 202, 245);
const FG_MUTED: (u8, u8, u8) = (86, 95, 137);

/// Move cursor to (row, col) — 1-based.
fn goto(row: u16, col: u16) -> String {
    format!("\x1b[{row};{col}H")
}

/// Render toast notifications as an ANSI overlay positioned in the top-right corner.
///
/// The overlay is drawn at absolute positions using cursor movement.
/// Each toast is a bordered box 30 chars wide, stacked vertically
/// starting from row 2 (below the header).
///
/// `terminal_width` is used to position boxes at the right edge.
/// `chat_top` is the first available row (below header).
pub fn render_toast_overlay(
    toasts: &[Toast],
    terminal_width: u16,
    chat_top: u16,
) -> String {
    if toasts.is_empty() {
        return String::new();
    }

    let box_w: usize = 30;
    // Position the box so its right edge aligns with the terminal right edge minus 1
    let start_col = (terminal_width as usize).saturating_sub(box_w + 2);
    let col = (start_col as u16).max(1);

    let mut out = String::with_capacity(1024);
    out.push_str("\x1b[s"); // save cursor
    out.push_str(HIDE_CURSOR);

    let mut row = chat_top;

    for toast in toasts {
        let c = toast.level.color();
        let border = fg(c.0, c.1, c.2);
        let icon = toast.level.icon();
        let fgp = fg(FG_PRIMARY.0, FG_PRIMARY.1, FG_PRIMARY.2);
        let muted = fg(FG_MUTED.0, FG_MUTED.1, FG_MUTED.2);

        // ── Top border with icon ────────────────────────────
        let title = format!(" {icon} ");
        let fill_len = box_w.saturating_sub(title.len() + 2);
        let fill: String = std::iter::repeat(BOX_H).take(fill_len).collect();

        out.push_str(&goto(row, col));
        out.push_str(&format!(
            "{border}{BOX_TL}{BOX_H}{RESET}{border}{BOLD}{title}{RESET}{border}{fill}{BOX_TR}{RESET}",
        ));
        row += 1;

        // ── Message line ────────────────────────────────────
        let inner_w = box_w.saturating_sub(4); // "| " + content + " |"
        let msg = truncate_str(&toast.message, inner_w);
        let msg_pad = inner_w.saturating_sub(visible_len(msg));

        out.push_str(&goto(row, col));
        out.push_str(&format!(
            "{border}{BOX_V}{RESET} {fgp}{msg}{}{RESET} {border}{BOX_V}{RESET}",
            " ".repeat(msg_pad),
        ));
        row += 1;

        // ── Detail line (optional) ──────────────────────────
        if let Some(ref detail) = toast.detail {
            let d = truncate_str(detail, inner_w);
            let d_pad = inner_w.saturating_sub(visible_len(d));

            out.push_str(&goto(row, col));
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} {muted}{d}{}{RESET} {border}{BOX_V}{RESET}",
                " ".repeat(d_pad),
            ));
            row += 1;
        }

        // ── Action hint (if action exists) ──────────────────
        if toast.action.is_some() {
            let hint = "[Enter] Act  [Esc] Dismiss";
            let h = truncate_str(hint, inner_w);
            let h_pad = inner_w.saturating_sub(visible_len(h));

            out.push_str(&goto(row, col));
            out.push_str(&format!(
                "{border}{BOX_V}{RESET} {muted}{h}{}{RESET} {border}{BOX_V}{RESET}",
                " ".repeat(h_pad),
            ));
            row += 1;
        }

        // ── Bottom border ───────────────────────────────────
        let bot: String = std::iter::repeat(BOX_H).take(box_w.saturating_sub(2)).collect();
        out.push_str(&goto(row, col));
        out.push_str(&format!("{border}{BOX_BL}{bot}{BOX_BR}{RESET}"));
        row += 1;
    }

    out.push_str(SHOW_CURSOR);
    out.push_str("\x1b[u"); // restore cursor

    out
}

// ─── Helpers ────────────────────────────────────────────────────────────

/// Truncate a string to at most `max` visible characters (UTF-8 safe).
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

/// Visible character length (no ANSI stripping needed for plain text).
fn visible_len(s: &str) -> usize {
    s.chars().count()
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_visible() {
        let mut mgr = ToastManager::new();
        assert!(!mgr.has_visible());
        assert_eq!(mgr.visible_count(), 0);

        let id = mgr.push("Hello", ToastLevel::Info, None, None);
        assert_eq!(id, 0);
        assert!(mgr.has_visible());
        assert_eq!(mgr.visible_count(), 1);
        assert_eq!(mgr.visible_toasts().len(), 1);
        assert_eq!(mgr.visible_toasts()[0].message, "Hello");
    }

    #[test]
    fn test_max_visible_capped() {
        let mut mgr = ToastManager::new();
        for i in 0..5 {
            mgr.push(format!("Toast {i}"), ToastLevel::Info, None, None);
        }
        assert_eq!(mgr.total_count(), 5);
        assert_eq!(mgr.visible_count(), 3); // MAX_VISIBLE
        assert_eq!(mgr.visible_toasts().len(), 3);
    }

    #[test]
    fn test_dismiss_by_id() {
        let mut mgr = ToastManager::new();
        let id0 = mgr.push("A", ToastLevel::Info, None, None);
        let _id1 = mgr.push("B", ToastLevel::Success, None, None);

        mgr.dismiss(id0);
        assert_eq!(mgr.total_count(), 1);
        assert_eq!(mgr.visible_toasts()[0].message, "B");
    }

    #[test]
    fn test_dismiss_top() {
        let mut mgr = ToastManager::new();
        mgr.push("First", ToastLevel::Info, None, Some(ToastAction::Dismiss));
        mgr.push("Second", ToastLevel::Warning, None, None);

        let action = mgr.dismiss_top();
        assert!(matches!(action, Some(ToastAction::Dismiss)));
        assert_eq!(mgr.total_count(), 1);
        assert_eq!(mgr.visible_toasts()[0].message, "Second");
    }

    #[test]
    fn test_accept_top_returns_action() {
        let mut mgr = ToastManager::new();
        mgr.push(
            "Fix it",
            ToastLevel::Error,
            None,
            Some(ToastAction::RunCommand("cargo fix".into())),
        );

        let action = mgr.accept_top();
        assert!(matches!(action, Some(ToastAction::RunCommand(cmd)) if cmd == "cargo fix"));
        assert_eq!(mgr.total_count(), 0);
    }

    #[test]
    fn test_tick_removes_expired() {
        let mut mgr = ToastManager::new();
        // Push a toast with zero duration (immediately expired)
        mgr.push("Gone", ToastLevel::Info, Some(Duration::ZERO), None);
        mgr.push("Stays", ToastLevel::Info, Some(Duration::from_secs(999)), None);

        // Allow a tiny bit of time for the zero-duration toast to expire
        std::thread::sleep(Duration::from_millis(1));

        let changed = mgr.tick();
        assert!(changed);
        assert_eq!(mgr.total_count(), 1);
        assert_eq!(mgr.visible_toasts()[0].message, "Stays");
    }

    #[test]
    fn test_tick_no_change_when_none_expired() {
        let mut mgr = ToastManager::new();
        mgr.push("Fresh", ToastLevel::Info, Some(Duration::from_secs(999)), None);
        let changed = mgr.tick();
        assert!(!changed);
    }

    #[test]
    fn test_clear() {
        let mut mgr = ToastManager::new();
        mgr.push("A", ToastLevel::Info, None, None);
        mgr.push("B", ToastLevel::Warning, None, None);
        mgr.clear();
        assert!(!mgr.has_visible());
        assert_eq!(mgr.total_count(), 0);
    }

    #[test]
    fn test_default() {
        let mgr = ToastManager::default();
        assert_eq!(mgr.total_count(), 0);
    }

    #[test]
    fn test_push_with_detail() {
        let mut mgr = ToastManager::new();
        mgr.push_with_detail("Title", "Detail line", ToastLevel::Success, None, None);
        assert_eq!(mgr.visible_toasts()[0].detail.as_deref(), Some("Detail line"));
    }

    #[test]
    fn test_toast_level_defaults() {
        assert_eq!(ToastLevel::Info.default_duration(), Duration::from_secs(5));
        assert_eq!(ToastLevel::Success.default_duration(), Duration::from_secs(5));
        assert_eq!(ToastLevel::Warning.default_duration(), Duration::from_secs(10));
        assert_eq!(ToastLevel::Error.default_duration(), Duration::from_secs(30));
    }

    #[test]
    fn test_toast_level_icon() {
        assert_eq!(ToastLevel::Info.icon(), "i");
        assert_eq!(ToastLevel::Success.icon(), "*");
        assert_eq!(ToastLevel::Warning.icon(), "!");
        assert_eq!(ToastLevel::Error.icon(), "x");
    }

    #[test]
    fn test_toast_is_expired() {
        let toast = Toast {
            id: 0,
            message: "Test".into(),
            detail: None,
            level: ToastLevel::Info,
            action: None,
            created_at: Instant::now() - Duration::from_secs(10),
            duration: Duration::from_secs(5),
        };
        assert!(toast.is_expired());

        let fresh = Toast {
            id: 1,
            message: "Fresh".into(),
            detail: None,
            level: ToastLevel::Info,
            action: None,
            created_at: Instant::now(),
            duration: Duration::from_secs(999),
        };
        assert!(!fresh.is_expired());
    }

    #[test]
    fn test_render_toast_overlay_empty() {
        let result = render_toast_overlay(&[], 80, 2);
        assert!(result.is_empty());
    }

    #[test]
    fn test_render_toast_overlay_single() {
        let toasts = vec![Toast {
            id: 0,
            message: "Command finished".into(),
            detail: Some("Exit code: 0".into()),
            level: ToastLevel::Success,
            action: None,
            created_at: Instant::now(),
            duration: Duration::from_secs(5),
        }];

        let result = render_toast_overlay(&toasts, 80, 2);
        assert!(!result.is_empty());
        assert!(result.contains("Command finished"));
        assert!(result.contains("Exit code: 0"));
        // Has box drawing chars
        assert!(result.contains('\u{256D}')); // TL
        assert!(result.contains('\u{256F}')); // BR
    }

    #[test]
    fn test_render_toast_overlay_with_action() {
        let toasts = vec![Toast {
            id: 0,
            message: "Error detected".into(),
            detail: None,
            level: ToastLevel::Error,
            action: Some(ToastAction::RunCommand("cargo fix".into())),
            created_at: Instant::now(),
            duration: Duration::from_secs(30),
        }];

        let result = render_toast_overlay(&toasts, 80, 2);
        assert!(result.contains("[Enter] Act"));
        assert!(result.contains("[Esc] Dismiss"));
    }

    #[test]
    fn test_render_toast_overlay_multiple() {
        let toasts = vec![
            Toast {
                id: 0,
                message: "First".into(),
                detail: None,
                level: ToastLevel::Info,
                action: None,
                created_at: Instant::now(),
                duration: Duration::from_secs(5),
            },
            Toast {
                id: 1,
                message: "Second".into(),
                detail: None,
                level: ToastLevel::Warning,
                action: None,
                created_at: Instant::now(),
                duration: Duration::from_secs(10),
            },
        ];

        let result = render_toast_overlay(&toasts, 100, 2);
        assert!(result.contains("First"));
        assert!(result.contains("Second"));
    }

    #[test]
    fn test_truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("short", 10), "short");
    }

    #[test]
    fn test_truncate_str_multibyte() {
        let s = "hello\u{4E16}\u{754C}"; // "hello世界"
        let t = truncate_str(s, 6);
        assert!(t.len() <= 6);
        assert!(t.is_char_boundary(t.len()));
    }

    #[test]
    fn test_id_increments() {
        let mut mgr = ToastManager::new();
        let id0 = mgr.push("A", ToastLevel::Info, None, None);
        let id1 = mgr.push("B", ToastLevel::Info, None, None);
        let id2 = mgr.push("C", ToastLevel::Info, None, None);
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }
}
