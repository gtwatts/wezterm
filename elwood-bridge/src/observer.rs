//! PaneObserver — reads content from other WezTerm panes.
//!
//! Subscribes to `MuxNotification::PaneOutput` events and caches pane content.
//! This allows the agent to see what's happening in shell/editor panes.
//!
//! ## Contextual content detection
//!
//! The observer can analyze pane content to extract actionable information
//! (compiler errors, test failures, stack traces) for agent prompt injection.
//! See [`ContentDetector`] for the pattern matching engine and
//! [`PaneObserver::get_contextual_content`] for the high-level API.

use mux::pane::PaneId;
use mux::Mux;
use parking_lot::RwLock;
use regex::Regex;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;
use wezterm_term::StableRowIndex;

/// A snapshot of a pane's visible content at a point in time.
#[derive(Debug, Clone)]
pub struct PaneSnapshot {
    /// The visible text lines.
    pub lines: Vec<String>,
    /// When this snapshot was taken.
    pub timestamp: Instant,
    /// The pane's title at snapshot time.
    pub title: String,
    /// Number of rows in the viewport.
    pub viewport_rows: usize,
}

/// The type of actionable content detected in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// Rust compiler error (e.g. `error[E0308]: mismatched types`).
    CompilerError,
    /// Test failure output (e.g. `test result: FAILED`).
    TestFailure,
    /// A stack trace / backtrace.
    StackTrace,
    /// Generic command output (non-zero exit, unknown pattern).
    CommandOutput,
    /// Content that didn't match any known pattern.
    Unknown,
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CompilerError => write!(f, "compiler_error"),
            Self::TestFailure => write!(f, "test_failure"),
            Self::StackTrace => write!(f, "stack_trace"),
            Self::CommandOutput => write!(f, "command_output"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Actionable content extracted from a pane snapshot.
///
/// Produced by [`ContentDetector::detect`] and returned by
/// [`PaneObserver::get_contextual_content`].
#[derive(Debug, Clone)]
pub struct ContextualContent {
    /// The pane this content came from.
    pub pane_id: PaneId,
    /// What kind of content was detected.
    pub content_type: ContentType,
    /// The extracted text (may be a subset of the full pane content).
    pub text: String,
    /// A source file path extracted from the content, if any.
    pub source_file: Option<String>,
    /// When the underlying snapshot was taken.
    pub timestamp: Instant,
}

/// Pattern-matching engine for detecting actionable content in terminal output.
///
/// Pre-compiles regexes on construction for efficient repeated use.
pub struct ContentDetector {
    // Rust compiler errors: `error[E0308]: ...` or `error: ...`
    rust_error: Regex,
    // Rust compiler warnings: `warning: ...` or `warning[...]:`
    rust_warning: Regex,
    // File location: `  --> src/main.rs:12:5`
    rust_location: Regex,
    // General file:line:col pattern (gcc, clang, TypeScript, etc.)
    file_line_col: Regex,
    // file:line:col followed by "error" (gcc, clang, TypeScript diagnostics)
    file_line_col_error: Regex,
    // Test failure: "FAILED" keyword
    test_failed: Regex,
    // "0 failed" — used to exclude false positives from passing test summaries
    zero_failed: Regex,
    // Rust test summary line — specifically the failing variant
    test_result_failed: Regex,
    // Python traceback
    python_traceback: Regex,
    // Node.js / JS stack trace line: `at Foo (/path:line:col)` or `at /path:line:col`
    node_stack: Regex,
    // Rust backtrace / panic
    rust_panic: Regex,
    // Non-zero exit code from shell prompt
    exit_code: Regex,
}

impl ContentDetector {
    /// Create a new detector with pre-compiled patterns.
    pub fn new() -> Self {
        Self {
            rust_error: Regex::new(r"^error(\[E\d+\])?:").expect("valid regex"),
            rust_warning: Regex::new(r"^warning(\[\w+\])?:").expect("valid regex"),
            rust_location: Regex::new(r"^\s*-->\s+(.+):(\d+):(\d+)").expect("valid regex"),
            file_line_col: Regex::new(r"^(.+?\.\w+):(\d+):(\d+)").expect("valid regex"),
            file_line_col_error: Regex::new(r"^.+?\.\w+:\d+:\d+.*\berror\b").expect("valid regex"),
            test_failed: Regex::new(r"(?i)\bFAILED\b").expect("valid regex"),
            zero_failed: Regex::new(r"(?i)\b0 failed\b").expect("valid regex"),
            test_result_failed: Regex::new(r"test result: FAILED").expect("valid regex"),
            python_traceback: Regex::new(r"^Traceback \(most recent call last\):")
                .expect("valid regex"),
            node_stack: Regex::new(r"^\s+at\s+").expect("valid regex"),
            rust_panic: Regex::new(r"^thread '.*' panicked at").expect("valid regex"),
            exit_code: Regex::new(r"exit (?:code|status)[:\s]+(\d+)").expect("valid regex"),
        }
    }

    /// Analyze lines of terminal output and return detected contextual content.
    ///
    /// Returns at most one [`ContextualContent`] per detected block. The
    /// `pane_id` and `timestamp` fields are filled in from the caller.
    pub fn detect(&self, pane_id: PaneId, lines: &[String], timestamp: Instant) -> Vec<ContextualContent> {
        let mut results = Vec::new();

        if let Some(cc) = self.detect_compiler_errors(pane_id, lines, timestamp) {
            results.push(cc);
        }
        if let Some(cc) = self.detect_test_failures(pane_id, lines, timestamp) {
            results.push(cc);
        }
        if let Some(cc) = self.detect_stack_traces(pane_id, lines, timestamp) {
            results.push(cc);
        }
        if let Some(cc) = self.detect_exit_codes(pane_id, lines, timestamp) {
            results.push(cc);
        }

        results
    }

    /// Detect Rust/C/C++/TypeScript compiler errors.
    fn detect_compiler_errors(
        &self,
        pane_id: PaneId,
        lines: &[String],
        timestamp: Instant,
    ) -> Option<ContextualContent> {
        let mut error_lines: Vec<usize> = Vec::new();
        let mut source_file: Option<String> = None;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if self.rust_error.is_match(trimmed)
                || self.rust_warning.is_match(trimmed)
                || self.file_line_col_error.is_match(trimmed)
            {
                error_lines.push(i);
            }
            if source_file.is_none() {
                if let Some(caps) = self.rust_location.captures(trimmed) {
                    source_file = Some(caps[1].to_string());
                } else if let Some(caps) = self.file_line_col.captures(trimmed) {
                    // Only accept if the path looks like a real file (has / or \)
                    let path = &caps[1];
                    if path.contains('/') || path.contains('\\') || path.contains('.') {
                        source_file = Some(path.to_string());
                    }
                }
            }
        }

        if error_lines.is_empty() {
            return None;
        }

        // Gather context: from first error line to the end (or up to 50 lines)
        let start = error_lines[0];
        let end = (start + 50).min(lines.len());
        let text = lines[start..end]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Some(ContextualContent {
            pane_id,
            content_type: ContentType::CompilerError,
            text,
            source_file,
            timestamp,
        })
    }

    /// Detect test failure output.
    fn detect_test_failures(
        &self,
        pane_id: PaneId,
        lines: &[String],
        timestamp: Instant,
    ) -> Option<ContextualContent> {
        let mut has_failure = false;
        let mut failure_start: Option<usize> = None;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // "test result: FAILED" is always a real failure
            if self.test_result_failed.is_match(trimmed) {
                has_failure = true;
                if failure_start.is_none() {
                    failure_start = Some(i);
                }
            } else if self.test_failed.is_match(trimmed) && !self.zero_failed.is_match(trimmed) {
                // "FAILED" keyword but not in "0 failed" context
                has_failure = true;
                if failure_start.is_none() {
                    failure_start = Some(i);
                }
            }
        }

        if !has_failure {
            return None;
        }

        // Try to extract a source file from assertion/panic context nearby
        let source_file = lines.iter().find_map(|line| {
            let trimmed = line.trim();
            if let Some(caps) = self.rust_location.captures(trimmed) {
                return Some(caps[1].to_string());
            }
            if let Some(caps) = self.file_line_col.captures(trimmed) {
                let path = &caps[1];
                if path.contains('/') || path.contains('\\') || path.contains('.') {
                    return Some(path.to_string());
                }
            }
            None
        });

        let start = failure_start.unwrap_or(0);
        let end = (start + 50).min(lines.len());
        let text = lines[start..end]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Some(ContextualContent {
            pane_id,
            content_type: ContentType::TestFailure,
            text,
            source_file,
            timestamp,
        })
    }

    /// Detect stack traces (Rust panics, Python tracebacks, Node.js stacks).
    fn detect_stack_traces(
        &self,
        pane_id: PaneId,
        lines: &[String],
        timestamp: Instant,
    ) -> Option<ContextualContent> {
        let mut trace_start: Option<usize> = None;

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // node_stack needs the raw line to detect leading whitespace before `at`
            if self.rust_panic.is_match(trimmed)
                || self.python_traceback.is_match(trimmed)
                || self.node_stack.is_match(line)
            {
                if trace_start.is_none() {
                    trace_start = Some(i);
                }
            }
        }

        let start = trace_start?;
        let end = (start + 50).min(lines.len());

        // Try to extract a source file from the stack trace
        let source_file = lines[start..end].iter().find_map(|line| {
            let trimmed = line.trim();
            if let Some(caps) = self.file_line_col.captures(trimmed) {
                let path = &caps[1];
                if path.contains('/') || path.contains('\\') || path.contains('.') {
                    return Some(path.to_string());
                }
            }
            None
        });

        let text = lines[start..end]
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Some(ContextualContent {
            pane_id,
            content_type: ContentType::StackTrace,
            text,
            source_file,
            timestamp,
        })
    }

    /// Detect non-zero exit codes in shell output.
    fn detect_exit_codes(
        &self,
        pane_id: PaneId,
        lines: &[String],
        timestamp: Instant,
    ) -> Option<ContextualContent> {
        for (i, line) in lines.iter().enumerate() {
            if let Some(caps) = self.exit_code.captures(line) {
                let code: u32 = caps[1].parse().unwrap_or(0);
                if code != 0 {
                    let start = i.saturating_sub(5);
                    let end = (i + 5).min(lines.len());
                    let text = lines[start..end]
                        .iter()
                        .map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");

                    return Some(ContextualContent {
                        pane_id,
                        content_type: ContentType::CommandOutput,
                        text,
                        source_file: None,
                        timestamp,
                    });
                }
            }
        }
        None
    }
}

impl Default for ContentDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Observes content from other WezTerm panes.
///
/// The observer subscribes to Mux notifications and maintains a cache of
/// pane content that the agent can query.
pub struct PaneObserver {
    /// Cached snapshots keyed by PaneId.
    cache: Arc<RwLock<HashMap<PaneId, PaneSnapshot>>>,
    /// Set of pane IDs we're actively watching.
    subscriptions: Arc<RwLock<Vec<PaneId>>>,
    /// The pane ID of the Elwood agent pane (excluded from observation).
    own_pane_id: PaneId,
    /// Pre-compiled pattern detector for contextual content extraction.
    detector: ContentDetector,
}

impl PaneObserver {
    /// Create a new PaneObserver.
    pub fn new(own_pane_id: PaneId) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(Vec::new())),
            own_pane_id,
            detector: ContentDetector::new(),
        }
    }

    /// Start observing Mux notifications.
    ///
    /// Subscribes to the Mux's notification system. The callback runs on
    /// WezTerm's notification thread (smol executor).
    pub fn start_observing(&self) {
        let cache = Arc::clone(&self.cache);
        let subscriptions = Arc::clone(&self.subscriptions);
        let own_pane_id = self.own_pane_id;

        let mux = Mux::get();
        mux.subscribe(move |notification| {
            match notification {
                mux::MuxNotification::PaneOutput(pane_id) => {
                    // Skip our own pane
                    if pane_id == own_pane_id {
                        return true;
                    }

                    // Only cache if we're subscribed to this pane
                    let subs = subscriptions.read();
                    if !subs.contains(&pane_id) && !subs.is_empty() {
                        return true;
                    }

                    // Read the pane content
                    let mux = Mux::try_get();
                    if let Some(mux) = mux {
                        if let Some(pane) = mux.get_pane(pane_id) {
                            let dims = pane.get_dimensions();
                            let range = dims.physical_top
                                ..dims.physical_top + dims.viewport_rows as StableRowIndex;
                            let (_first, lines) = pane.get_lines(range);

                            let text_lines: Vec<String> =
                                lines.iter().map(|l| l.as_str().to_string()).collect();

                            let snapshot = PaneSnapshot {
                                lines: text_lines,
                                timestamp: Instant::now(),
                                title: pane.get_title(),
                                viewport_rows: dims.viewport_rows,
                            };

                            cache.write().insert(pane_id, snapshot);
                        }
                    }
                }
                mux::MuxNotification::PaneRemoved(pane_id) => {
                    cache.write().remove(&pane_id);
                    subscriptions.write().retain(|id| *id != pane_id);
                }
                _ => {}
            }
            true // Keep subscription alive
        });
    }

    /// Subscribe to output from a specific pane.
    pub fn subscribe(&self, pane_id: PaneId) {
        let mut subs = self.subscriptions.write();
        if !subs.contains(&pane_id) {
            subs.push(pane_id);
        }
    }

    /// Unsubscribe from a pane.
    pub fn unsubscribe(&self, pane_id: PaneId) {
        self.subscriptions.write().retain(|id| *id != pane_id);
        self.cache.write().remove(&pane_id);
    }

    /// Subscribe to all panes (empty subscription list means "watch all").
    pub fn subscribe_all(&self) {
        self.subscriptions.write().clear();
    }

    /// Get the cached snapshot for a pane.
    pub fn get_snapshot(&self, pane_id: PaneId) -> Option<PaneSnapshot> {
        self.cache.read().get(&pane_id).cloned()
    }

    /// Get snapshots for all cached panes.
    pub fn get_all_snapshots(&self) -> HashMap<PaneId, PaneSnapshot> {
        self.cache.read().clone()
    }

    /// Extract actionable contextual content from all cached pane snapshots.
    ///
    /// Scans each cached pane for compiler errors, test failures, stack traces,
    /// and non-zero exit codes. Returns all detected items, ready for injection
    /// into an agent prompt.
    pub fn get_contextual_content(&self) -> Vec<ContextualContent> {
        let cache = self.cache.read();
        let mut results = Vec::new();
        for (pane_id, snapshot) in cache.iter() {
            results.extend(self.detector.detect(*pane_id, &snapshot.lines, snapshot.timestamp));
        }
        results
    }

    /// Read a pane's content directly (not from cache).
    ///
    /// This reads the pane's current visible content on demand, bypassing
    /// the notification-based cache.
    pub fn read_pane_now(pane_id: PaneId) -> Option<PaneSnapshot> {
        let mux = Mux::try_get()?;
        let pane = mux.get_pane(pane_id)?;
        let dims = pane.get_dimensions();
        let range = dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex;
        let (_first, lines) = pane.get_lines(range);

        let text_lines: Vec<String> = lines.iter().map(|l| l.as_str().to_string()).collect();

        Some(PaneSnapshot {
            lines: text_lines,
            timestamp: Instant::now(),
            title: pane.get_title(),
            viewport_rows: dims.viewport_rows,
        })
    }

    /// List all panes with their IDs, titles, and process info.
    pub fn list_panes() -> Vec<PaneInfo> {
        let mux = match Mux::try_get() {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut panes = Vec::new();
        // Iterate through all panes via the mux
        for pane in mux.iter_panes() {
            let dims = pane.get_dimensions();
            panes.push(PaneInfo {
                pane_id: pane.pane_id(),
                domain_id: pane.domain_id(),
                title: pane.get_title(),
                cols: dims.cols,
                rows: dims.viewport_rows,
                is_dead: pane.is_dead(),
                foreground_process: pane
                    .get_foreground_process_name(mux::pane::CachePolicy::AllowStale),
                cwd: pane
                    .get_current_working_dir(mux::pane::CachePolicy::AllowStale)
                    .map(|u| u.to_string()),
            });
        }
        panes
    }
}

/// Summary information about a pane.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: PaneId,
    pub domain_id: DomainId,
    pub title: String,
    pub cols: usize,
    pub rows: usize,
    pub is_dead: bool,
    pub foreground_process: Option<String>,
    pub cwd: Option<String>,
}

use mux::domain::DomainId;

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    fn detect(input: &str) -> Vec<ContextualContent> {
        let d = ContentDetector::new();
        d.detect(0, &lines(input), Instant::now())
    }

    // ---- Compiler error detection ----

    #[test]
    fn detects_rust_error_with_code() {
        let input = r#"error[E0308]: mismatched types
  --> src/main.rs:12:5
   |
12 |     let x: u32 = "hello";
   |                  ^^^^^^^ expected `u32`, found `&str`"#;
        let results = detect(input);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, ContentType::CompilerError);
        assert_eq!(results[0].source_file.as_deref(), Some("src/main.rs"));
        assert!(results[0].text.contains("error[E0308]"));
    }

    #[test]
    fn detects_rust_error_without_code() {
        let input = "error: could not compile `my-crate`";
        let results = detect(input);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, ContentType::CompilerError);
    }

    #[test]
    fn detects_rust_warning() {
        let input = r#"warning: unused variable: `x`
  --> src/lib.rs:5:9"#;
        let results = detect(input);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content_type, ContentType::CompilerError);
        assert_eq!(results[0].source_file.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn detects_gcc_style_error() {
        let input = "src/main.c:42:10: error: expected ';' after expression";
        let results = detect(input);
        // Detected as compiler error due to file:line:col pattern
        assert!(!results.is_empty());
    }

    #[test]
    fn no_compiler_error_in_clean_output() {
        let input = r#"   Compiling my-crate v0.1.0
    Finished dev [unoptimized + debuginfo] target(s) in 0.52s"#;
        let results = detect(input);
        // No compiler errors — there may be other detections but not CompilerError
        let compiler_errors: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::CompilerError)
            .collect();
        assert!(compiler_errors.is_empty());
    }

    // ---- Test failure detection ----

    #[test]
    fn detects_rust_test_failure() {
        let input = r#"running 3 tests
test tests::test_add ... ok
test tests::test_sub ... FAILED
test tests::test_mul ... ok

failures:

---- tests::test_sub stdout ----
thread 'tests::test_sub' panicked at 'assertion failed: `(left == right)`
  left: `3`,
 right: `4`', src/lib.rs:15:9

test result: FAILED. 1 passed; 1 failed; 0 ignored"#;
        let results = detect(input);
        let failures: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::TestFailure)
            .collect();
        assert!(!failures.is_empty());
        assert!(failures[0].text.contains("FAILED"));
    }

    #[test]
    fn detects_generic_failed_keyword() {
        let input = "Tests: 5 passed, 2 FAILED, 7 total";
        let results = detect(input);
        let failures: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::TestFailure)
            .collect();
        assert!(!failures.is_empty());
    }

    #[test]
    fn no_test_failure_on_passing() {
        let input = r#"running 3 tests
test tests::test_add ... ok
test tests::test_sub ... ok
test tests::test_mul ... ok

test result: ok. 3 passed; 0 failed; 0 ignored"#;
        let results = detect(input);
        let failures: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::TestFailure)
            .collect();
        assert!(failures.is_empty());
    }

    // ---- Stack trace detection ----

    #[test]
    fn detects_rust_panic() {
        let input = r#"thread 'main' panicked at 'index out of bounds: the len is 3 but the index is 5', src/main.rs:10:5
note: run with `RUST_BACKTRACE=1` for a backtrace"#;
        let results = detect(input);
        let traces: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::StackTrace)
            .collect();
        assert!(!traces.is_empty());
        assert!(traces[0].text.contains("panicked"));
    }

    #[test]
    fn detects_python_traceback() {
        let input = r#"Traceback (most recent call last):
  File "main.py", line 10, in <module>
    result = divide(1, 0)
  File "main.py", line 5, in divide
    return a / b
ZeroDivisionError: division by zero"#;
        let results = detect(input);
        let traces: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::StackTrace)
            .collect();
        assert!(!traces.is_empty());
        assert!(traces[0].text.contains("Traceback"));
    }

    #[test]
    fn detects_node_stack_trace() {
        let input = r#"TypeError: Cannot read property 'foo' of undefined
    at Object.<anonymous> (/app/src/index.js:15:10)
    at Module._compile (node:internal/modules/cjs/loader:1105:14)"#;
        let results = detect(input);
        let traces: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::StackTrace)
            .collect();
        assert!(!traces.is_empty());
    }

    // ---- Exit code detection ----

    #[test]
    fn detects_nonzero_exit_code() {
        let input = "make: *** [all] Error 2\nexit code: 2";
        let results = detect(input);
        let exits: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::CommandOutput)
            .collect();
        assert!(!exits.is_empty());
    }

    #[test]
    fn detects_exit_status() {
        let input = "Process finished with exit status: 1";
        let results = detect(input);
        let exits: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::CommandOutput)
            .collect();
        assert!(!exits.is_empty());
    }

    #[test]
    fn ignores_zero_exit_code() {
        let input = "exit code: 0";
        let results = detect(input);
        let exits: Vec<_> = results
            .iter()
            .filter(|c| c.content_type == ContentType::CommandOutput)
            .collect();
        assert!(exits.is_empty());
    }

    // ---- Edge cases ----

    #[test]
    fn empty_input_produces_no_results() {
        let results = detect("");
        assert!(results.is_empty());
    }

    #[test]
    fn blank_lines_produce_no_results() {
        let results = detect("\n\n\n   \n\n");
        assert!(results.is_empty());
    }

    #[test]
    fn multiple_content_types_detected() {
        let input = r#"error[E0308]: mismatched types
  --> src/main.rs:12:5
thread 'main' panicked at 'assertion failed', src/lib.rs:20:1
exit code: 1"#;
        let results = detect(input);
        let types: Vec<ContentType> = results.iter().map(|c| c.content_type).collect();
        assert!(types.contains(&ContentType::CompilerError));
        assert!(types.contains(&ContentType::StackTrace));
        assert!(types.contains(&ContentType::CommandOutput));
    }

    #[test]
    fn content_type_display() {
        assert_eq!(ContentType::CompilerError.to_string(), "compiler_error");
        assert_eq!(ContentType::TestFailure.to_string(), "test_failure");
        assert_eq!(ContentType::StackTrace.to_string(), "stack_trace");
        assert_eq!(ContentType::CommandOutput.to_string(), "command_output");
        assert_eq!(ContentType::Unknown.to_string(), "unknown");
    }

    #[test]
    fn detector_default_works() {
        let d = ContentDetector::default();
        let results = d.detect(0, &lines("error: something broke"), Instant::now());
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn source_file_extracted_from_typescript_error() {
        let input = "src/app/page.tsx:25:3 - error TS2322: Type 'string' is not assignable";
        let results = detect(input);
        // Should extract a file path
        let with_file: Vec<_> = results.iter().filter(|c| c.source_file.is_some()).collect();
        assert!(!with_file.is_empty());
        assert!(with_file[0]
            .source_file
            .as_deref()
            .expect("has file")
            .contains("page.tsx"));
    }
}
