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

// ─── Error Detection Types ──────────────────────────────────────────────────

/// The broad category of an error detected in terminal output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorType {
    /// Compile-time error (Rust, C/C++, TypeScript, Go).
    Compile,
    /// Runtime error (Python traceback, Node.js TypeError, panics).
    Runtime,
    /// Test failure (cargo test, pytest, jest, go test).
    Test,
    /// Permission denied, EACCES, sudo required.
    Permission,
    /// File or command not found, ENOENT.
    NotFound,
    /// Git conflict or git fatal error.
    Git,
    /// General error that doesn't fit other categories.
    General,
}

impl fmt::Display for ErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile => write!(f, "compile"),
            Self::Runtime => write!(f, "runtime"),
            Self::Test => write!(f, "test"),
            Self::Permission => write!(f, "permission"),
            Self::NotFound => write!(f, "not_found"),
            Self::Git => write!(f, "git"),
            Self::General => write!(f, "general"),
        }
    }
}

/// Severity level for a detected error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Informational (warnings, hints).
    Info,
    /// A real error that likely needs fixing.
    Error,
    /// A critical/fatal error that blocks progress.
    Fatal,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Error => write!(f, "error"),
            Self::Fatal => write!(f, "fatal"),
        }
    }
}

/// A structured error detection extracted from terminal output.
///
/// Produced by [`ContentDetector::detect_errors`] with richer metadata
/// than the legacy [`ContextualContent`] type.
#[derive(Debug, Clone)]
pub struct ErrorDetection {
    /// Broad category of the error.
    pub error_type: ErrorType,
    /// Severity level.
    pub severity: Severity,
    /// The raw error message line(s) extracted from output.
    pub message: String,
    /// A human-readable suggested fix command or action.
    pub suggested_fix: String,
    /// Source file extracted from the error, if any.
    pub source_file: Option<String>,
    /// Whether this fix can be applied automatically (without LLM).
    pub auto_fixable: bool,
}

/// A snapshot of a pane's visible content at a point in time.
#[derive(Debug, Clone)]
pub struct PaneSnapshot {
    /// The pane ID this snapshot belongs to.
    pub pane_id: PaneId,
    /// The visible text lines.
    pub lines: Vec<String>,
    /// When this snapshot was taken.
    pub timestamp: Instant,
    /// The pane's title at snapshot time.
    pub title: String,
    /// Cursor row position (stable index).
    pub cursor_row: i64,
    /// Terminal dimensions (cols, rows).
    pub dimensions: (usize, usize),
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
    // ── Rust ────────────────────────────────────────────────────────────
    // Rust compiler errors: `error[E0308]: ...` or `error: ...`
    rust_error: Regex,
    // Rust compiler warnings: `warning: ...` or `warning[...]:`
    rust_warning: Regex,
    // File location: `  --> src/main.rs:12:5`
    rust_location: Regex,
    // Rust "cannot find" errors
    rust_cannot_find: Regex,
    // Rust "expected ... found" type mismatch
    rust_expected_found: Regex,
    // Rust backtrace / panic
    rust_panic: Regex,

    // ── General file location ─────────────────────────────────────────
    // General file:line:col pattern (gcc, clang, TypeScript, etc.)
    file_line_col: Regex,
    // file:line:col followed by "error" (gcc, clang, TypeScript diagnostics)
    file_line_col_error: Regex,

    // ── Python ─────────────────────────────────────────────────────────
    // Python traceback
    python_traceback: Regex,
    // Python SyntaxError
    python_syntax_error: Regex,
    // Python ImportError / ModuleNotFoundError
    python_import_error: Regex,
    // Python NameError / AttributeError / TypeError / ValueError
    python_runtime_error: Regex,

    // ── JavaScript / TypeScript ────────────────────────────────────────
    // JS SyntaxError / TypeError / ReferenceError
    js_error: Regex,
    // "Cannot find module" (Node.js)
    js_cannot_find_module: Regex,
    // Node.js / JS stack trace line: `at Foo (/path:line:col)` or `at /path:line:col`
    node_stack: Regex,

    // ── Go ──────────────────────────────────────────────────────────────
    // Go "cannot find package" or "undefined:"
    go_error: Regex,
    // Go "syntax error"
    go_syntax_error: Regex,

    // ── Git ──────────────────────────────────────────────────────────────
    // Git merge conflicts
    git_conflict: Regex,
    // Git fatal errors
    git_fatal: Regex,
    // Git generic errors
    _git_error: Regex,

    // ── General / OS ───────────────────────────────────────────────────
    // Permission denied
    permission_denied: Regex,
    // No such file or directory
    no_such_file: Regex,
    // command not found
    command_not_found: Regex,
    // EACCES / ENOENT (Node.js / system)
    errno_pattern: Regex,

    // ── Test failures ──────────────────────────────────────────────────
    // Test failure: "FAILED" keyword
    test_failed: Regex,
    // "0 failed" — used to exclude false positives from passing test summaries
    zero_failed: Regex,
    // Rust test summary line — specifically the failing variant
    test_result_failed: Regex,
    // "assertion failed" / "AssertionError"
    assertion_failed: Regex,
    // pytest / jest "FAIL:" prefix
    test_fail_prefix: Regex,

    // ── Exit codes ─────────────────────────────────────────────────────
    // Non-zero exit code from shell prompt
    exit_code: Regex,
}

impl ContentDetector {
    /// Create a new detector with pre-compiled patterns.
    pub fn new() -> Self {
        Self {
            // ── Rust ────────────────────────────────────────────────
            rust_error: Regex::new(r"^error(\[E\d+\])?:").expect("valid regex"),
            rust_warning: Regex::new(r"^warning(\[\w+\])?:").expect("valid regex"),
            rust_location: Regex::new(r"^\s*-->\s+(.+):(\d+):(\d+)").expect("valid regex"),
            rust_cannot_find: Regex::new(r"cannot find (?:crate|value|type|trait|module|macro) `(\w+)`").expect("valid regex"),
            rust_expected_found: Regex::new(r"expected .+, found .+").expect("valid regex"),
            rust_panic: Regex::new(r"^thread '.*' panicked at").expect("valid regex"),

            // ── General file location ─────────────────────────────
            file_line_col: Regex::new(r"^(.+?\.\w+):(\d+):(\d+)").expect("valid regex"),
            file_line_col_error: Regex::new(r"^.+?\.\w+:\d+:\d+.*\berror\b").expect("valid regex"),

            // ── Python ─────────────────────────────────────────────
            python_traceback: Regex::new(r"^Traceback \(most recent call last\):")
                .expect("valid regex"),
            python_syntax_error: Regex::new(r"^\s*SyntaxError:").expect("valid regex"),
            python_import_error: Regex::new(r"^(?:ImportError|ModuleNotFoundError):\s*(.+)")
                .expect("valid regex"),
            python_runtime_error: Regex::new(
                r"^(?:NameError|AttributeError|TypeError|ValueError|KeyError|IndexError|ZeroDivisionError):\s*(.+)"
            ).expect("valid regex"),

            // ── JavaScript / TypeScript ────────────────────────────
            js_error: Regex::new(
                r"^(?:SyntaxError|TypeError|ReferenceError|RangeError|URIError|EvalError):\s*(.+)"
            ).expect("valid regex"),
            js_cannot_find_module: Regex::new(r"Cannot find module '([^']+)'").expect("valid regex"),
            node_stack: Regex::new(r"^\s+at\s+").expect("valid regex"),

            // ── Go ──────────────────────────────────────────────────
            go_error: Regex::new(r"(?:cannot find package|undefined:)\s*(.+)").expect("valid regex"),
            go_syntax_error: Regex::new(r"syntax error").expect("valid regex"),

            // ── Git ──────────────────────────────────────────────────
            git_conflict: Regex::new(r"(?i)(?:CONFLICT|merge conflict|Merge conflict)").expect("valid regex"),
            git_fatal: Regex::new(r"^fatal:\s*(.+)").expect("valid regex"),
            _git_error: Regex::new(r"^error:\s*(.+)").expect("valid regex"),

            // ── General / OS ────────────────────────────────────────
            permission_denied: Regex::new(r"(?i)permission denied").expect("valid regex"),
            no_such_file: Regex::new(r"(?i)no such file or directory").expect("valid regex"),
            command_not_found: Regex::new(r"(?:command not found|not found)$").expect("valid regex"),
            errno_pattern: Regex::new(r"\bE(?:ACCES|NOENT|PERM)\b").expect("valid regex"),

            // ── Test failures ───────────────────────────────────────
            test_failed: Regex::new(r"(?i)\bFAILED\b").expect("valid regex"),
            zero_failed: Regex::new(r"(?i)\b0 failed\b").expect("valid regex"),
            test_result_failed: Regex::new(r"test result: FAILED").expect("valid regex"),
            assertion_failed: Regex::new(r"(?i)(?:assertion failed|AssertionError|assert\.fail)").expect("valid regex"),
            test_fail_prefix: Regex::new(r"^(?:FAIL:|FAIL\s|failures:)").expect("valid regex"),

            // ── Exit codes ──────────────────────────────────────────
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

    // ── Enhanced error detection with structured results ─────────────────

    /// Analyze lines of terminal output and return structured error detections.
    ///
    /// Unlike [`detect`], this method returns [`ErrorDetection`] structs with
    /// error type classification, severity, and suggested fixes.
    pub fn detect_errors(&self, lines: &[String]) -> Vec<ErrorDetection> {
        let mut results = Vec::new();

        // Helper to extract source file from nearby lines
        let extract_source_file = |lines: &[String]| -> Option<String> {
            lines.iter().find_map(|line| {
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
            })
        };

        for line in lines {
            let trimmed = line.trim();

            // ── Rust compiler errors ────────────────────────────────
            if self.rust_error.is_match(trimmed) {
                let is_cannot_find = self.rust_cannot_find.is_match(trimmed);
                let fix = if is_cannot_find {
                    if let Some(caps) = self.rust_cannot_find.captures(trimmed) {
                        format!("cargo add {}", &caps[1])
                    } else {
                        "Check spelling and imports".to_string()
                    }
                } else if self.rust_expected_found.is_match(trimmed) {
                    "Fix the type mismatch in the highlighted expression".to_string()
                } else {
                    "Press Ctrl+F to ask Elwood to fix this error".to_string()
                };

                results.push(ErrorDetection {
                    error_type: ErrorType::Compile,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: fix,
                    source_file: extract_source_file(lines),
                    auto_fixable: is_cannot_find,
                });
                continue;
            }

            // ── Rust warnings ───────────────────────────────────────
            if self.rust_warning.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Compile,
                    severity: Severity::Info,
                    message: trimmed.to_string(),
                    suggested_fix: "cargo clippy --fix".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: true,
                });
                continue;
            }

            // ── Python errors ──────────────────────────────────────
            if self.python_syntax_error.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Compile,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Fix the syntax error at the indicated line".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }
            if let Some(caps) = self.python_import_error.captures(trimmed) {
                let module = caps.get(1).map(|m| m.as_str()).unwrap_or("unknown");
                results.push(ErrorDetection {
                    error_type: ErrorType::NotFound,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: format!("pip install {module}"),
                    source_file: extract_source_file(lines),
                    auto_fixable: true,
                });
                continue;
            }
            if self.python_runtime_error.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Runtime,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix this error".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }
            if self.python_traceback.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Runtime,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix this error".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }

            // ── JavaScript/TypeScript errors ───────────────────────
            if self.js_error.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Runtime,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix this error".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }
            if let Some(caps) = self.js_cannot_find_module.captures(trimmed) {
                let module = caps.get(1).map(|m| m.as_str()).unwrap_or("unknown");
                results.push(ErrorDetection {
                    error_type: ErrorType::NotFound,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: format!("npm install {module}"),
                    source_file: extract_source_file(lines),
                    auto_fixable: true,
                });
                continue;
            }

            // ── Go errors ──────────────────────────────────────────
            if self.go_error.is_match(trimmed) || self.go_syntax_error.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Compile,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix this error".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }

            // ── Git errors ─────────────────────────────────────────
            if self.git_conflict.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Git,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Resolve merge conflicts, then git add and git commit".to_string(),
                    source_file: None,
                    auto_fixable: false,
                });
                continue;
            }
            if self.git_fatal.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Git,
                    severity: Severity::Fatal,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to diagnose the git error".to_string(),
                    source_file: None,
                    auto_fixable: false,
                });
                continue;
            }

            // ── Permission denied ──────────────────────────────────
            if self.permission_denied.is_match(trimmed) || self.errno_pattern.is_match(trimmed) {
                if trimmed.contains("EACCES") || trimmed.contains("EPERM") || self.permission_denied.is_match(trimmed) {
                    results.push(ErrorDetection {
                        error_type: ErrorType::Permission,
                        severity: Severity::Error,
                        message: trimmed.to_string(),
                        suggested_fix: "Check file permissions or run with appropriate privileges".to_string(),
                        source_file: None,
                        auto_fixable: false,
                    });
                    continue;
                }
            }

            // ── No such file / command not found ───────────────────
            if self.no_such_file.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::NotFound,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Check the file path exists".to_string(),
                    source_file: None,
                    auto_fixable: false,
                });
                continue;
            }
            if self.command_not_found.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::NotFound,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Install the missing command or check your PATH".to_string(),
                    source_file: None,
                    auto_fixable: false,
                });
                continue;
            }
            if trimmed.contains("ENOENT") {
                results.push(ErrorDetection {
                    error_type: ErrorType::NotFound,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Check the file path exists".to_string(),
                    source_file: None,
                    auto_fixable: false,
                });
                continue;
            }

            // ── Test failures ──────────────────────────────────────
            if self.test_result_failed.is_match(trimmed)
                || self.test_fail_prefix.is_match(trimmed)
                || self.assertion_failed.is_match(trimmed)
            {
                results.push(ErrorDetection {
                    error_type: ErrorType::Test,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix the failing tests".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }
            // "FAILED" keyword (but not "0 failed")
            if self.test_failed.is_match(trimmed) && !self.zero_failed.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Test,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix the failing tests".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }

            // ── Rust panic ─────────────────────────────────────────
            if self.rust_panic.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Runtime,
                    severity: Severity::Fatal,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix the panic".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
                continue;
            }

            // ── file:line:col error (gcc, clang, TypeScript) ───────
            if self.file_line_col_error.is_match(trimmed) {
                results.push(ErrorDetection {
                    error_type: ErrorType::Compile,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    suggested_fix: "Press Ctrl+F to ask Elwood to fix this error".to_string(),
                    source_file: extract_source_file(lines),
                    auto_fixable: false,
                });
            }
        }

        results
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
                            let cursor = pane.get_cursor_position();
                            let range = dims.physical_top
                                ..dims.physical_top + dims.viewport_rows as StableRowIndex;
                            let (_first, lines) = pane.get_lines(range);

                            let text_lines: Vec<String> =
                                lines.iter().map(|l| l.as_str().to_string()).collect();

                            let snapshot = PaneSnapshot {
                                pane_id,
                                lines: text_lines,
                                timestamp: Instant::now(),
                                title: pane.get_title(),
                                cursor_row: cursor.y as i64,
                                dimensions: (dims.cols, dims.viewport_rows),
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
        let cursor = pane.get_cursor_position();
        let range = dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex;
        let (_first, lines) = pane.get_lines(range);

        let text_lines: Vec<String> = lines.iter().map(|l| l.as_str().to_string()).collect();

        Some(PaneSnapshot {
            pane_id,
            lines: text_lines,
            timestamp: Instant::now(),
            title: pane.get_title(),
            cursor_row: cursor.y as i64,
            dimensions: (dims.cols, dims.viewport_rows),
        })
    }

    /// Scan all sibling panes and return fresh snapshots.
    ///
    /// Reads content from every pane in the mux except the agent's own pane.
    /// This is a point-in-time read that bypasses the notification cache.
    pub fn scan_sibling_panes(&self) -> Vec<PaneSnapshot> {
        let mux = match Mux::try_get() {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut snapshots = Vec::new();
        for pane in mux.iter_panes() {
            let id = pane.pane_id();
            if id == self.own_pane_id || pane.is_dead() {
                continue;
            }

            let dims = pane.get_dimensions();
            let cursor = pane.get_cursor_position();
            // Read the last N lines of visible content (viewport)
            let range = dims.physical_top
                ..dims.physical_top + dims.viewport_rows as StableRowIndex;
            let (_first, lines) = pane.get_lines(range);

            let text_lines: Vec<String> =
                lines.iter().map(|l| l.as_str().to_string()).collect();

            snapshots.push(PaneSnapshot {
                pane_id: id,
                lines: text_lines,
                timestamp: Instant::now(),
                title: pane.get_title(),
                cursor_row: cursor.y as i64,
                dimensions: (dims.cols, dims.viewport_rows),
            });
        }

        // Update the cache with fresh data
        {
            let mut cache = self.cache.write();
            for snap in &snapshots {
                cache.insert(snap.pane_id, snap.clone());
            }
        }

        snapshots
    }

    /// Get content from a specific sibling pane (on demand, not from cache).
    pub fn get_pane_content(&self, pane_id: PaneId) -> Option<PaneSnapshot> {
        if pane_id == self.own_pane_id {
            return None;
        }
        Self::read_pane_now(pane_id)
    }

    /// Detect errors in all sibling panes.
    ///
    /// Returns `(pane_id, error_summary)` pairs for each pane that has errors.
    pub fn detect_errors_in_siblings(&self) -> Vec<(PaneId, String)> {
        let snapshots = self.scan_sibling_panes();
        let mut errors = Vec::new();

        for snap in &snapshots {
            let detections = self.detector.detect(snap.pane_id, &snap.lines, snap.timestamp);
            for det in detections {
                let summary = match det.content_type {
                    ContentType::CompilerError => {
                        format!("Compiler error in '{}': {}", snap.title,
                            det.text.lines().next().unwrap_or("(unknown)"))
                    }
                    ContentType::TestFailure => {
                        format!("Test failure in '{}': {}", snap.title,
                            det.text.lines().next().unwrap_or("(unknown)"))
                    }
                    ContentType::StackTrace => {
                        format!("Stack trace in '{}': {}", snap.title,
                            det.text.lines().next().unwrap_or("(unknown)"))
                    }
                    ContentType::CommandOutput => {
                        format!("Command error in '{}': {}", snap.title,
                            det.text.lines().next().unwrap_or("(unknown)"))
                    }
                    ContentType::Unknown => {
                        format!("Error in '{}': {}", snap.title,
                            det.text.lines().next().unwrap_or("(unknown)"))
                    }
                };
                errors.push((snap.pane_id, summary));
            }
        }

        errors
    }

    /// Format sibling pane content for injection into the agent's LLM context.
    ///
    /// Returns a string with the last `max_lines` lines from each sibling pane,
    /// formatted for the LLM to understand. Empty if no siblings or no content.
    pub fn format_context_for_agent(&self, max_lines_per_pane: usize) -> String {
        let snapshots = self.scan_sibling_panes();
        if snapshots.is_empty() {
            return String::new();
        }

        let mut ctx = String::from("[Sibling Panes]\n");

        for snap in &snapshots {
            // Take the last N lines (most recent output)
            let start = snap.lines.len().saturating_sub(max_lines_per_pane);
            let recent: Vec<&str> = snap.lines[start..].iter().map(|s| s.as_str()).collect();

            // Skip panes with only blank content
            if recent.iter().all(|l| l.trim().is_empty()) {
                continue;
            }

            ctx.push_str(&format!(
                "<pane id=\"{}\" title=\"{}\" dims=\"{}x{}\">\n",
                snap.pane_id, snap.title, snap.dimensions.0, snap.dimensions.1,
            ));
            for line in &recent {
                ctx.push_str(line);
                ctx.push('\n');
            }
            ctx.push_str("</pane>\n");
        }

        // Check for errors and append a summary
        let errors = self.detect_errors_in_siblings();
        if !errors.is_empty() {
            ctx.push_str("\n[Detected Errors in Sibling Panes]\n");
            for (pane_id, summary) in &errors {
                ctx.push_str(&format!("- Pane {pane_id}: {summary}\n"));
            }
        }

        ctx
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

// ─── Next-command Suggester ─────────────────────────────────────────────────

/// A rule mapping a command prefix pattern to a suggestion string.
struct NextCommandRule {
    /// The prefix the last command must start with (case-insensitive).
    prefix: &'static str,
    /// The human-readable suggestion shown to the user.
    suggestion: &'static str,
}

/// Suggests likely follow-up commands based on the previous command and exit code.
///
/// This is a simple, zero-allocation rule table: the first matching rule wins.
/// All matching is case-insensitive on the command prefix so both `git add` and
/// `GIT ADD` trigger the same suggestion.
///
/// # Examples
///
/// ```rust
/// let s = elwood_bridge::observer::NextCommandSuggester::new();
/// // git add -> git commit suggestion
/// assert!(s.suggest("git add .", true).is_some());
/// // cargo build error -> apply fix suggestion
/// assert!(s.suggest("cargo build", false).is_some());
/// // Unknown command — no suggestion
/// assert!(s.suggest("ls -la", true).is_none());
/// ```
pub struct NextCommandSuggester {
    success_rules: Vec<NextCommandRule>,
    failure_rules: Vec<NextCommandRule>,
}

impl NextCommandSuggester {
    /// Create a new suggester with the built-in rule table.
    pub fn new() -> Self {
        Self {
            success_rules: vec![
                NextCommandRule {
                    prefix: "git add",
                    suggestion: "git commit -m \"<message>\"",
                },
                NextCommandRule {
                    prefix: "git commit",
                    suggestion: "git push",
                },
                NextCommandRule {
                    prefix: "git clone",
                    suggestion: "cd <repo> && ls",
                },
                NextCommandRule {
                    prefix: "cargo build",
                    suggestion: "cargo run",
                },
                NextCommandRule {
                    prefix: "cargo test",
                    suggestion: "cargo clippy -- -D warnings",
                },
                NextCommandRule {
                    prefix: "npm install",
                    suggestion: "npm run build",
                },
                NextCommandRule {
                    prefix: "npm run build",
                    suggestion: "npm run test",
                },
                NextCommandRule {
                    prefix: "make",
                    suggestion: "make test",
                },
                NextCommandRule {
                    prefix: "docker build",
                    suggestion: "docker run <image>",
                },
                NextCommandRule {
                    prefix: "cd ",
                    suggestion: "ls  (or git status to check repo state)",
                },
            ],
            failure_rules: vec![
                NextCommandRule {
                    prefix: "cargo build",
                    suggestion: "Press Ctrl+F to ask Elwood to fix the compiler errors",
                },
                NextCommandRule {
                    prefix: "cargo test",
                    suggestion: "Press Ctrl+F to ask Elwood to fix the failing tests",
                },
                NextCommandRule {
                    prefix: "cargo clippy",
                    suggestion: "Press Ctrl+F to ask Elwood to resolve the lint warnings",
                },
                NextCommandRule {
                    prefix: "make",
                    suggestion: "Press Ctrl+F to ask Elwood to diagnose the build failure",
                },
                NextCommandRule {
                    prefix: "npm run",
                    suggestion: "Press Ctrl+F to ask Elwood to fix the npm script error",
                },
                NextCommandRule {
                    prefix: "python",
                    suggestion: "Press Ctrl+F to ask Elwood to fix the Python error",
                },
                NextCommandRule {
                    prefix: "pytest",
                    suggestion: "Press Ctrl+F to ask Elwood to fix the failing tests",
                },
            ],
        }
    }

    /// Suggest a follow-up action given the command that just ran and whether it succeeded.
    ///
    /// Returns `Some(&str)` with the suggestion text if a rule matched, or `None`.
    pub fn suggest(&self, command: &str, success: bool) -> Option<&str> {
        let lower = command.to_lowercase();
        let rules = if success {
            &self.success_rules
        } else {
            &self.failure_rules
        };
        rules
            .iter()
            .find(|rule| lower.starts_with(rule.prefix))
            .map(|rule| rule.suggestion)
    }
}

impl Default for NextCommandSuggester {
    fn default() -> Self {
        Self::new()
    }
}

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

    // ---- NextCommandSuggester ----

    #[test]
    fn suggests_git_commit_after_git_add() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("git add .", true);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("git commit"));
    }

    #[test]
    fn suggests_git_push_after_git_commit() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("git commit -m \"fix bug\"", true);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("git push"));
    }

    #[test]
    fn suggests_cargo_run_after_successful_cargo_build() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("cargo build --release", true);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("cargo run"));
    }

    #[test]
    fn suggests_fix_after_failed_cargo_build() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("cargo build", false);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("Ctrl+F"));
    }

    #[test]
    fn suggests_fix_after_failed_cargo_test() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("cargo test --workspace", false);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("Ctrl+F"));
    }

    #[test]
    fn no_suggestion_for_unknown_success_command() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("ls -la", true);
        assert!(suggestion.is_none());
    }

    #[test]
    fn no_suggestion_for_unknown_failed_command() {
        let s = NextCommandSuggester::new();
        let suggestion = s.suggest("echo hello", false);
        assert!(suggestion.is_none());
    }

    #[test]
    fn suggest_is_case_insensitive() {
        let s = NextCommandSuggester::new();
        // Upper-case prefix should still match
        let suggestion = s.suggest("GIT ADD .", true);
        assert!(suggestion.is_some());
        assert!(suggestion.unwrap().contains("git commit"));
    }

    #[test]
    fn suggester_default_works() {
        let s = NextCommandSuggester::default();
        assert!(s.suggest("cargo build", false).is_some());
    }

    // ---- Enhanced detect_errors tests ----

    fn detect_errors(input: &str) -> Vec<ErrorDetection> {
        let d = ContentDetector::new();
        d.detect_errors(&lines(input))
    }

    #[test]
    fn detect_errors_rust_compile_error() {
        let results = detect_errors("error[E0308]: mismatched types");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].error_type, ErrorType::Compile);
        assert_eq!(results[0].severity, Severity::Error);
        assert!(results[0].message.contains("E0308"));
    }

    #[test]
    fn detect_errors_rust_cannot_find_suggests_cargo_add() {
        let results = detect_errors("error[E0432]: cannot find crate `serde`");
        assert!(!results.is_empty());
        assert!(results[0].suggested_fix.contains("cargo add"));
        assert!(results[0].auto_fixable);
    }

    #[test]
    fn detect_errors_rust_warning() {
        let results = detect_errors("warning: unused variable: `x`");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].error_type, ErrorType::Compile);
        assert_eq!(results[0].severity, Severity::Info);
        assert!(results[0].suggested_fix.contains("clippy"));
        assert!(results[0].auto_fixable);
    }

    #[test]
    fn detect_errors_python_syntax_error() {
        let results = detect_errors("  SyntaxError: invalid syntax");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].error_type, ErrorType::Compile);
        assert_eq!(results[0].severity, Severity::Error);
    }

    #[test]
    fn detect_errors_python_import_error_suggests_pip() {
        let results = detect_errors("ModuleNotFoundError: No module named 'requests'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::NotFound);
        assert!(results[0].suggested_fix.contains("pip install"));
        assert!(results[0].auto_fixable);
    }

    #[test]
    fn detect_errors_python_runtime_errors() {
        for err in &[
            "NameError: name 'foo' is not defined",
            "TypeError: unsupported operand type(s)",
            "ValueError: invalid literal",
            "KeyError: 'missing_key'",
            "AttributeError: 'NoneType' object has no attribute 'foo'",
        ] {
            let results = detect_errors(err);
            assert!(!results.is_empty(), "Should detect: {err}");
            assert_eq!(results[0].error_type, ErrorType::Runtime);
        }
    }

    #[test]
    fn detect_errors_python_traceback() {
        let results = detect_errors("Traceback (most recent call last):");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Runtime);
    }

    #[test]
    fn detect_errors_js_error() {
        let results = detect_errors("TypeError: Cannot read properties of undefined");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Runtime);
    }

    #[test]
    fn detect_errors_js_cannot_find_module_suggests_npm() {
        let results = detect_errors("Cannot find module 'express'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::NotFound);
        assert!(results[0].suggested_fix.contains("npm install"));
        assert!(results[0].auto_fixable);
    }

    #[test]
    fn detect_errors_js_syntax_error() {
        let results = detect_errors("SyntaxError: Unexpected token '}'");
        assert!(!results.is_empty());
        // SyntaxError is a parse/compile-time error
        assert_eq!(results[0].error_type, ErrorType::Compile);
    }

    #[test]
    fn detect_errors_go_error() {
        let results = detect_errors("undefined: myFunction");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Compile);
    }

    #[test]
    fn detect_errors_git_conflict() {
        let results = detect_errors("CONFLICT (content): Merge conflict in src/main.rs");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Git);
        assert!(results[0].suggested_fix.contains("Resolve merge conflicts"));
    }

    #[test]
    fn detect_errors_git_fatal() {
        let results = detect_errors("fatal: not a git repository");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Git);
        assert_eq!(results[0].severity, Severity::Fatal);
    }

    #[test]
    fn detect_errors_permission_denied() {
        let results = detect_errors("bash: /etc/shadow: Permission denied");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Permission);
    }

    #[test]
    fn detect_errors_eacces() {
        let results = detect_errors("Error: EACCES: permission denied, open '/root/.config'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Permission);
    }

    #[test]
    fn detect_errors_no_such_file() {
        let results = detect_errors("ls: cannot access 'foo': No such file or directory");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::NotFound);
    }

    #[test]
    fn detect_errors_command_not_found() {
        let results = detect_errors("bash: foobar: command not found");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::NotFound);
        assert!(results[0].suggested_fix.contains("PATH"));
    }

    #[test]
    fn detect_errors_enoent() {
        let results = detect_errors("Error: ENOENT: no such file or directory, open 'foo.txt'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::NotFound);
    }

    #[test]
    fn detect_errors_test_result_failed() {
        let results = detect_errors("test result: FAILED. 1 passed; 2 failed; 0 ignored");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Test);
    }

    #[test]
    fn detect_errors_assertion_failed() {
        let results = detect_errors("assertion failed: `(left == right)`");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Test);
    }

    #[test]
    fn detect_errors_jest_fail() {
        let results = detect_errors("FAIL: src/app.test.ts");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Test);
    }

    #[test]
    fn detect_errors_rust_panic() {
        let results = detect_errors("thread 'main' panicked at 'index out of bounds'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Runtime);
        assert_eq!(results[0].severity, Severity::Fatal);
    }

    #[test]
    fn detect_errors_file_line_col_error() {
        let results = detect_errors("src/main.c:42:10: error: expected ';'");
        assert!(!results.is_empty());
        assert_eq!(results[0].error_type, ErrorType::Compile);
    }

    #[test]
    fn detect_errors_clean_output_empty() {
        let results = detect_errors("   Compiling my-crate v0.1.0\n    Finished dev in 0.52s");
        assert!(results.is_empty());
    }

    #[test]
    fn detect_errors_multiple_errors() {
        let input = "error[E0308]: mismatched types\nwarning: unused variable";
        let results = detect_errors(input);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].error_type, ErrorType::Compile);
        assert_eq!(results[0].severity, Severity::Error);
        assert_eq!(results[1].error_type, ErrorType::Compile);
        assert_eq!(results[1].severity, Severity::Info);
    }

    #[test]
    fn detect_errors_error_type_display() {
        assert_eq!(ErrorType::Compile.to_string(), "compile");
        assert_eq!(ErrorType::Runtime.to_string(), "runtime");
        assert_eq!(ErrorType::Test.to_string(), "test");
        assert_eq!(ErrorType::Permission.to_string(), "permission");
        assert_eq!(ErrorType::NotFound.to_string(), "not_found");
        assert_eq!(ErrorType::Git.to_string(), "git");
        assert_eq!(ErrorType::General.to_string(), "general");
    }

    #[test]
    fn detect_errors_severity_display() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Error.to_string(), "error");
        assert_eq!(Severity::Fatal.to_string(), "fatal");
    }

    #[test]
    fn detect_errors_severity_ordering() {
        assert!(Severity::Info < Severity::Error);
        assert!(Severity::Error < Severity::Fatal);
    }
}
