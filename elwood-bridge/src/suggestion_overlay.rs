//! Suggestion overlay — renders actionable error-fix suggestions.
//!
//! When the [`ContentDetector`](crate::observer::ContentDetector) finds errors in
//! terminal output, the [`SuggestionManager`] collects them and presents an ANSI
//! overlay at the bottom of the chat area with a suggested fix and keybindings.
//!
//! ## Keybindings
//!
//! - **Enter** — accept the active suggestion (sends fix command to agent or shell)
//! - **Esc** — dismiss the active suggestion
//! - **Tab** — cycle to the next suggestion

use crate::observer::{ErrorDetection, ErrorType, Severity};
use std::collections::HashSet;

// ─── Suggestion ─────────────────────────────────────────────────────────

/// A single actionable suggestion derived from an [`ErrorDetection`].
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// Unique monotonic ID for this suggestion.
    pub id: usize,
    /// The broad error category.
    pub error_type: ErrorType,
    /// The severity of the underlying error.
    pub severity: Severity,
    /// The raw error message extracted from terminal output.
    pub message: String,
    /// Human-readable suggested fix (command or instruction).
    pub suggested_fix: String,
    /// Whether the fix can be applied automatically without LLM.
    pub auto_fixable: bool,
}

// ─── SuggestionManager ──────────────────────────────────────────────────

/// Manages a set of suggestions and tracks user interaction state.
///
/// At most one suggestion is *active* (highlighted) at a time. The user
/// can cycle through suggestions with Tab, accept with Enter, or dismiss
/// with Esc.
pub struct SuggestionManager {
    /// All suggestions in insertion order.
    suggestions: Vec<Suggestion>,
    /// Index of the currently highlighted suggestion, if any.
    active_index: Option<usize>,
    /// IDs that have been dismissed by the user.
    dismissed: HashSet<usize>,
    /// Monotonic counter for generating unique suggestion IDs.
    next_id: usize,
}

impl SuggestionManager {
    /// Create an empty suggestion manager.
    pub fn new() -> Self {
        Self {
            suggestions: Vec::new(),
            active_index: None,
            dismissed: HashSet::new(),
            next_id: 0,
        }
    }

    /// Add a suggestion derived from an [`ErrorDetection`].
    ///
    /// The first suggestion added automatically becomes active.
    pub fn add_from_detection(&mut self, detection: &ErrorDetection) {
        let id = self.next_id;
        self.next_id += 1;

        self.suggestions.push(Suggestion {
            id,
            error_type: detection.error_type,
            severity: detection.severity,
            message: detection.message.clone(),
            suggested_fix: detection.suggested_fix.clone(),
            auto_fixable: detection.auto_fixable,
        });

        // Auto-activate the first one
        if self.active_index.is_none() {
            self.active_index = Some(self.suggestions.len() - 1);
        }
    }

    /// Add suggestions from a batch of detections.
    pub fn add_batch(&mut self, detections: &[ErrorDetection]) {
        for d in detections {
            self.add_from_detection(d);
        }
    }

    /// Dismiss the currently active suggestion. Advances to the next
    /// visible suggestion, or clears the overlay if none remain.
    pub fn dismiss(&mut self) {
        if let Some(idx) = self.active_index {
            if let Some(s) = self.suggestions.get(idx) {
                self.dismissed.insert(s.id);
            }
            self.advance_to_next_visible();
        }
    }

    /// Accept the currently active suggestion and return its suggested fix.
    ///
    /// The suggestion is removed from the list after acceptance.
    pub fn accept(&mut self) -> Option<String> {
        let idx = self.active_index?;
        let fix = self.suggestions.get(idx).map(|s| s.suggested_fix.clone());
        if let Some(s) = self.suggestions.get(idx) {
            self.dismissed.insert(s.id);
        }
        self.advance_to_next_visible();
        fix
    }

    /// Cycle to the next visible (non-dismissed) suggestion.
    pub fn next(&mut self) {
        self.advance_to_next_visible();
    }

    /// Return the currently active suggestion, if any.
    pub fn active(&self) -> Option<&Suggestion> {
        let idx = self.active_index?;
        self.suggestions.get(idx)
    }

    /// True if there is at least one visible (non-dismissed) suggestion.
    pub fn has_visible(&self) -> bool {
        self.suggestions
            .iter()
            .any(|s| !self.dismissed.contains(&s.id))
    }

    /// Count of visible (non-dismissed) suggestions.
    pub fn visible_count(&self) -> usize {
        self.suggestions
            .iter()
            .filter(|s| !self.dismissed.contains(&s.id))
            .count()
    }

    /// Clear all suggestions and reset state.
    pub fn clear(&mut self) {
        self.suggestions.clear();
        self.active_index = None;
        self.dismissed.clear();
    }

    /// Advance `active_index` to the next non-dismissed suggestion after
    /// the current one, wrapping around. Clears the index if none remain.
    fn advance_to_next_visible(&mut self) {
        let len = self.suggestions.len();
        if len == 0 {
            self.active_index = None;
            return;
        }

        let start = self.active_index.map(|i| i + 1).unwrap_or(0);
        for offset in 0..len {
            let idx = (start + offset) % len;
            if let Some(s) = self.suggestions.get(idx) {
                if !self.dismissed.contains(&s.id) {
                    self.active_index = Some(idx);
                    return;
                }
            }
        }

        // All dismissed
        self.active_index = None;
    }
}

impl Default for SuggestionManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::{ErrorDetection, ErrorType, Severity};

    fn make_detection(msg: &str, fix: &str, auto: bool) -> ErrorDetection {
        ErrorDetection {
            error_type: ErrorType::Compile,
            severity: Severity::Error,
            message: msg.to_string(),
            suggested_fix: fix.to_string(),
            source_file: None,
            auto_fixable: auto,
        }
    }

    #[test]
    fn test_add_and_active() {
        let mut mgr = SuggestionManager::new();
        assert!(!mgr.has_visible());

        let det = make_detection("error[E0308]", "fix types", false);
        mgr.add_from_detection(&det);

        assert!(mgr.has_visible());
        assert_eq!(mgr.visible_count(), 1);
        let active = mgr.active().unwrap();
        assert_eq!(active.message, "error[E0308]");
        assert_eq!(active.suggested_fix, "fix types");
    }

    #[test]
    fn test_dismiss_clears_when_single() {
        let mut mgr = SuggestionManager::new();
        mgr.add_from_detection(&make_detection("err", "fix", false));

        mgr.dismiss();
        assert!(!mgr.has_visible());
        assert!(mgr.active().is_none());
    }

    #[test]
    fn test_cycle_through_suggestions() {
        let mut mgr = SuggestionManager::new();
        mgr.add_from_detection(&make_detection("err1", "fix1", false));
        mgr.add_from_detection(&make_detection("err2", "fix2", true));
        mgr.add_from_detection(&make_detection("err3", "fix3", false));

        // First added becomes active
        assert_eq!(mgr.active().unwrap().message, "err1");

        mgr.next();
        assert_eq!(mgr.active().unwrap().message, "err2");

        mgr.next();
        assert_eq!(mgr.active().unwrap().message, "err3");

        // Wraps around
        mgr.next();
        assert_eq!(mgr.active().unwrap().message, "err1");
    }

    #[test]
    fn test_dismiss_advances_to_next() {
        let mut mgr = SuggestionManager::new();
        mgr.add_from_detection(&make_detection("err1", "fix1", false));
        mgr.add_from_detection(&make_detection("err2", "fix2", false));

        // Active is err1, dismiss it
        mgr.dismiss();
        assert_eq!(mgr.visible_count(), 1);
        assert_eq!(mgr.active().unwrap().message, "err2");
    }

    #[test]
    fn test_accept_returns_fix() {
        let mut mgr = SuggestionManager::new();
        mgr.add_from_detection(&make_detection("err1", "cargo add serde", true));

        let fix = mgr.accept();
        assert_eq!(fix.as_deref(), Some("cargo add serde"));
        assert!(!mgr.has_visible());
    }

    #[test]
    fn test_batch_add() {
        let mut mgr = SuggestionManager::new();
        let detections = vec![
            make_detection("err1", "fix1", false),
            make_detection("err2", "fix2", true),
        ];
        mgr.add_batch(&detections);
        assert_eq!(mgr.visible_count(), 2);
        assert_eq!(mgr.active().unwrap().message, "err1");
    }

    #[test]
    fn test_clear_resets_everything() {
        let mut mgr = SuggestionManager::new();
        mgr.add_from_detection(&make_detection("err1", "fix1", false));
        mgr.dismiss();
        mgr.add_from_detection(&make_detection("err2", "fix2", false));

        mgr.clear();
        assert!(!mgr.has_visible());
        assert!(mgr.active().is_none());
        assert_eq!(mgr.visible_count(), 0);
    }
}
