//! Diff computation engine for the Elwood code review system.
//!
//! Uses the `similar` crate for line-level and word-level diffing,
//! and provides a unified diff parser for `git diff` output.

use std::path::Path;
use std::process::Command;

/// A single diff for one file.
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// Path in the old tree (e.g. `a/src/main.rs`), `None` for new files.
    pub old_path: Option<String>,
    /// Path in the new tree (e.g. `b/src/main.rs`).
    pub new_path: String,
    /// The individual hunks of changes.
    pub hunks: Vec<DiffHunk>,
    /// Summary statistics.
    pub stats: DiffStats,
    /// What kind of change this is.
    pub kind: DiffKind,
}

/// Classification of a file-level change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Deleted,
    Modified,
    Renamed { old_path: String },
}

/// Summary statistics for a file diff.
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
}

/// A contiguous group of changes with surrounding context.
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// Human-readable hunk header (e.g. `@@ -10,7 +10,8 @@ fn main()`).
    pub header: String,
    /// Starting line number in the old file.
    pub old_start: usize,
    /// Starting line number in the new file.
    pub new_start: usize,
    /// The lines in this hunk.
    pub lines: Vec<DiffLine>,
    /// Whether this hunk is collapsed in the viewer.
    pub collapsed: bool,
}

/// A single line within a hunk.
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// What kind of line this is.
    pub kind: DiffLineKind,
    /// Line number in the old file (for context and deletions).
    pub old_lineno: Option<usize>,
    /// Line number in the new file (for context and additions).
    pub new_lineno: Option<usize>,
    /// Styled segments (for word-level emphasis).
    pub segments: Vec<DiffSegment>,
}

/// The type of a diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

/// A styled segment within a line (for word-level diff emphasis).
#[derive(Debug, Clone)]
pub struct DiffSegment {
    pub text: String,
    pub emphasized: bool,
}

// ─── Diff Computation (similar) ─────────────────────────────────────────

/// Compute a diff between two strings, returning hunks with the given context lines.
pub fn compute_diff(old: &str, new: &str, context: usize) -> Vec<DiffHunk> {
    use similar::{ChangeTag, TextDiff};

    let text_diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    for group in text_diff.grouped_ops(context) {
        let mut lines = Vec::new();
        let mut old_start = 0;
        let mut new_start = 0;
        let mut first = true;

        for op in &group {
            let tag = op.tag();
            let old_range = op.old_range();
            let new_range = op.new_range();

            if first {
                // +1 for 1-based line numbers
                old_start = old_range.start + 1;
                new_start = new_range.start + 1;
                first = false;
            }

            match tag {
                similar::DiffTag::Equal => {
                    for (i, value) in text_diff.iter_changes(op).enumerate() {
                        lines.push(DiffLine {
                            kind: DiffLineKind::Context,
                            old_lineno: Some(old_range.start + i + 1),
                            new_lineno: Some(new_range.start + i + 1),
                            segments: vec![DiffSegment {
                                text: value.to_string_lossy().to_string(),
                                emphasized: false,
                            }],
                        });
                    }
                }
                similar::DiffTag::Delete => {
                    for (i, value) in text_diff.iter_changes(op).enumerate() {
                        lines.push(DiffLine {
                            kind: DiffLineKind::Deletion,
                            old_lineno: Some(old_range.start + i + 1),
                            new_lineno: None,
                            segments: vec![DiffSegment {
                                text: value.to_string_lossy().to_string(),
                                emphasized: false,
                            }],
                        });
                    }
                }
                similar::DiffTag::Insert => {
                    for (i, value) in text_diff.iter_changes(op).enumerate() {
                        lines.push(DiffLine {
                            kind: DiffLineKind::Addition,
                            old_lineno: None,
                            new_lineno: Some(new_range.start + i + 1),
                            segments: vec![DiffSegment {
                                text: value.to_string_lossy().to_string(),
                                emphasized: false,
                            }],
                        });
                    }
                }
                similar::DiffTag::Replace => {
                    // For replace ops, compute word-level inline changes
                    let old_lines: Vec<_> = text_diff
                        .iter_changes(op)
                        .filter(|c| c.tag() == ChangeTag::Delete)
                        .collect();
                    let new_lines: Vec<_> = text_diff
                        .iter_changes(op)
                        .filter(|c| c.tag() == ChangeTag::Insert)
                        .collect();

                    // Pair old/new lines for word-level emphasis
                    let paired = old_lines.len().min(new_lines.len());

                    for (i, old_change) in old_lines.iter().enumerate() {
                        let old_text = old_change.to_string_lossy().to_string();
                        let segments = if i < paired {
                            let new_text = new_lines[i].to_string_lossy().to_string();
                            compute_word_segments(&old_text, &new_text, true)
                        } else {
                            vec![DiffSegment {
                                text: old_text,
                                emphasized: false,
                            }]
                        };
                        lines.push(DiffLine {
                            kind: DiffLineKind::Deletion,
                            old_lineno: Some(old_range.start + i + 1),
                            new_lineno: None,
                            segments,
                        });
                    }

                    for (i, new_change) in new_lines.iter().enumerate() {
                        let new_text = new_change.to_string_lossy().to_string();
                        let segments = if i < paired {
                            let old_text = old_lines[i].to_string_lossy().to_string();
                            compute_word_segments(&old_text, &new_text, false)
                        } else {
                            vec![DiffSegment {
                                text: new_text,
                                emphasized: false,
                            }]
                        };
                        lines.push(DiffLine {
                            kind: DiffLineKind::Addition,
                            old_lineno: None,
                            new_lineno: Some(new_range.start + i + 1),
                            segments,
                        });
                    }
                }
            }
        }

        // Build the hunk header
        let old_count = lines.iter().filter(|l| l.old_lineno.is_some()).count();
        let new_count = lines.iter().filter(|l| l.new_lineno.is_some()).count();
        let header = format!(
            "@@ -{},{} +{},{} @@",
            old_start, old_count, new_start, new_count
        );

        hunks.push(DiffHunk {
            header,
            old_start,
            new_start,
            lines,
            collapsed: false,
        });
    }

    hunks
}

/// Compute word-level segments between two paired lines.
///
/// When `is_old` is true, returns segments for the deletion side;
/// when false, returns segments for the addition side.
fn compute_word_segments(old_line: &str, new_line: &str, is_old: bool) -> Vec<DiffSegment> {
    use similar::{ChangeTag, TextDiff};

    let word_diff = TextDiff::from_words(old_line, new_line);
    let mut segments = Vec::new();

    for change in word_diff.iter_all_changes() {
        let text = change.to_string_lossy().to_string();
        match change.tag() {
            ChangeTag::Equal => {
                segments.push(DiffSegment {
                    text,
                    emphasized: false,
                });
            }
            ChangeTag::Delete if is_old => {
                segments.push(DiffSegment {
                    text,
                    emphasized: true,
                });
            }
            ChangeTag::Insert if !is_old => {
                segments.push(DiffSegment {
                    text,
                    emphasized: true,
                });
            }
            _ => {
                // Skip insert changes when building the old side, and vice versa
            }
        }
    }

    if segments.is_empty() {
        let text = if is_old { old_line } else { new_line };
        segments.push(DiffSegment {
            text: text.to_string(),
            emphasized: false,
        });
    }

    segments
}

/// Build a complete `FileDiff` from old and new content strings.
pub fn compute_file_diff(
    old_path: Option<&str>,
    new_path: &str,
    old_content: &str,
    new_content: &str,
    context: usize,
) -> FileDiff {
    let hunks = compute_diff(old_content, new_content, context);

    let mut stats = DiffStats::default();
    for hunk in &hunks {
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Addition => stats.additions += 1,
                DiffLineKind::Deletion => stats.deletions += 1,
                DiffLineKind::Context => {}
            }
        }
    }

    let kind = if old_content.is_empty() && !new_content.is_empty() {
        DiffKind::Added
    } else if !old_content.is_empty() && new_content.is_empty() {
        DiffKind::Deleted
    } else {
        DiffKind::Modified
    };

    FileDiff {
        old_path: old_path.map(String::from),
        new_path: new_path.to_string(),
        hunks,
        stats,
        kind,
    }
}

// ─── Git Diff Parser ────────────────────────────────────────────────────

/// Parse unified diff output from `git diff` into structured `FileDiff`s.
pub fn parse_git_diff(diff_output: &str) -> Vec<FileDiff> {
    let mut diffs = Vec::new();
    let mut lines = diff_output.lines().peekable();

    while let Some(line) = lines.next() {
        // Look for "diff --git a/... b/..."
        if !line.starts_with("diff --git ") {
            continue;
        }

        let mut old_path: Option<String> = None;
        let mut new_path: Option<String> = None;
        let mut is_new_file = false;
        let mut is_deleted = false;
        let mut is_binary = false;
        let mut rename_from: Option<String> = None;
        let mut hunks = Vec::new();

        // Parse the a/b paths from the diff line
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts: Vec<&str> = rest.splitn(2, " b/").collect();
            if parts.len() == 2 {
                old_path = Some(parts[0].strip_prefix("a/").unwrap_or(parts[0]).to_string());
                new_path = Some(parts[1].to_string());
            }
        }

        // Parse header lines until we hit a hunk or next diff
        while let Some(&next) = lines.peek() {
            if next.starts_with("diff --git ") || next.starts_with("@@") {
                break;
            }
            let hdr = lines.next().unwrap();

            if hdr.starts_with("new file") {
                is_new_file = true;
            } else if hdr.starts_with("deleted file") {
                is_deleted = true;
            } else if let Some(from) = hdr.strip_prefix("rename from ") {
                rename_from = Some(from.to_string());
            } else if let Some(to) = hdr.strip_prefix("rename to ") {
                new_path = Some(to.to_string());
            } else if hdr.starts_with("--- a/") {
                old_path = Some(hdr[6..].to_string());
            } else if hdr.starts_with("--- /dev/null") {
                old_path = None;
            } else if hdr.starts_with("+++ b/") {
                new_path = Some(hdr[6..].to_string());
            } else if hdr.starts_with("+++ /dev/null") {
                new_path = None;
            } else if hdr.contains("Binary files") {
                is_binary = true;
            }
        }

        // Skip binary files
        if is_binary {
            let np = new_path
                .clone()
                .or_else(|| old_path.clone())
                .unwrap_or_else(|| "(binary)".to_string());
            diffs.push(FileDiff {
                old_path: old_path.clone(),
                new_path: np,
                hunks: Vec::new(),
                stats: DiffStats::default(),
                kind: DiffKind::Modified,
            });
            continue;
        }

        // Parse hunks
        while let Some(&next) = lines.peek() {
            if next.starts_with("diff --git ") {
                break;
            }
            if !next.starts_with("@@") {
                lines.next();
                continue;
            }

            let hunk_header = lines.next().unwrap().to_string();
            let (old_start, new_start) = parse_hunk_header(&hunk_header);

            let mut hunk_lines = Vec::new();
            let mut old_lineno = old_start;
            let mut new_lineno = new_start;

            while let Some(&content) = lines.peek() {
                if content.starts_with("diff --git ") || content.starts_with("@@") {
                    break;
                }
                let content = lines.next().unwrap();

                if content.starts_with('+') {
                    let text = &content[1..];
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Addition,
                        old_lineno: None,
                        new_lineno: Some(new_lineno),
                        segments: vec![DiffSegment {
                            text: text.to_string(),
                            emphasized: false,
                        }],
                    });
                    new_lineno += 1;
                } else if content.starts_with('-') {
                    let text = &content[1..];
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Deletion,
                        old_lineno: Some(old_lineno),
                        new_lineno: None,
                        segments: vec![DiffSegment {
                            text: text.to_string(),
                            emphasized: false,
                        }],
                    });
                    old_lineno += 1;
                } else if content.starts_with(' ') || content.is_empty() {
                    let text = if content.is_empty() {
                        ""
                    } else {
                        &content[1..]
                    };
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        old_lineno: Some(old_lineno),
                        new_lineno: Some(new_lineno),
                        segments: vec![DiffSegment {
                            text: text.to_string(),
                            emphasized: false,
                        }],
                    });
                    old_lineno += 1;
                    new_lineno += 1;
                } else if content.starts_with('\\') {
                    // "\ No newline at end of file" — skip
                } else {
                    // Treat as context if no prefix
                    hunk_lines.push(DiffLine {
                        kind: DiffLineKind::Context,
                        old_lineno: Some(old_lineno),
                        new_lineno: Some(new_lineno),
                        segments: vec![DiffSegment {
                            text: content.to_string(),
                            emphasized: false,
                        }],
                    });
                    old_lineno += 1;
                    new_lineno += 1;
                }
            }

            hunks.push(DiffHunk {
                header: hunk_header,
                old_start,
                new_start,
                lines: hunk_lines,
                collapsed: false,
            });
        }

        // Compute stats
        let mut stats = DiffStats::default();
        for hunk in &hunks {
            for hline in &hunk.lines {
                match hline.kind {
                    DiffLineKind::Addition => stats.additions += 1,
                    DiffLineKind::Deletion => stats.deletions += 1,
                    DiffLineKind::Context => {}
                }
            }
        }

        let kind = if is_new_file {
            DiffKind::Added
        } else if is_deleted {
            DiffKind::Deleted
        } else if let Some(ref from) = rename_from {
            DiffKind::Renamed {
                old_path: from.clone(),
            }
        } else {
            DiffKind::Modified
        };

        let np = new_path
            .or_else(|| old_path.clone())
            .unwrap_or_else(|| "(unknown)".to_string());

        diffs.push(FileDiff {
            old_path,
            new_path: np,
            hunks,
            stats,
            kind,
        });
    }

    diffs
}

/// Parse a hunk header like `@@ -10,7 +10,8 @@ fn main()` into (old_start, new_start).
fn parse_hunk_header(header: &str) -> (usize, usize) {
    // Format: @@ -old_start[,old_count] +new_start[,new_count] @@
    let mut old_start = 1;
    let mut new_start = 1;

    if let Some(rest) = header.strip_prefix("@@ ") {
        let parts: Vec<&str> = rest.splitn(3, ' ').collect();
        if parts.len() >= 2 {
            // Parse -old_start,count
            if let Some(old_part) = parts[0].strip_prefix('-') {
                if let Some((start, _)) = old_part.split_once(',') {
                    old_start = start.parse().unwrap_or(1);
                } else {
                    old_start = old_part.parse().unwrap_or(1);
                }
            }
            // Parse +new_start,count
            if let Some(new_part) = parts[1].strip_prefix('+') {
                let new_part = new_part.trim_end_matches(" @@");
                if let Some((start, _)) = new_part.split_once(',') {
                    new_start = start.parse().unwrap_or(1);
                } else {
                    new_start = new_part.parse().unwrap_or(1);
                }
            }
        }
    }

    (old_start, new_start)
}

// ─── Git Integration ────────────────────────────────────────────────────

/// Run `git diff` and parse the output into structured diffs.
///
/// If `staged` is true, runs `git diff --staged`.
pub fn git_diff(cwd: &Path, staged: bool) -> Result<Vec<FileDiff>, String> {
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--staged");
    }

    let output = Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_diff(&stdout))
}

/// Run `git diff` for a specific file and parse the output.
pub fn git_diff_file(cwd: &Path, file: &str, staged: bool) -> Result<Vec<FileDiff>, String> {
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--staged");
    }
    args.push("--");
    args.push(file);

    let output = Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_diff(&stdout))
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_diff_empty_to_content() {
        let hunks = compute_diff("", "hello\nworld\n", 3);
        assert_eq!(hunks.len(), 1);
        let additions: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Addition)
            .collect();
        assert_eq!(additions.len(), 2);
    }

    #[test]
    fn test_compute_diff_content_to_empty() {
        let hunks = compute_diff("hello\nworld\n", "", 3);
        assert_eq!(hunks.len(), 1);
        let deletions: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Deletion)
            .collect();
        assert_eq!(deletions.len(), 2);
    }

    #[test]
    fn test_compute_diff_identical() {
        let hunks = compute_diff("hello\nworld\n", "hello\nworld\n", 3);
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_compute_diff_single_line_change() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\n";
        let hunks = compute_diff(old, new, 3);
        assert_eq!(hunks.len(), 1);

        let deletions: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Deletion)
            .collect();
        let additions: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Addition)
            .collect();
        assert_eq!(deletions.len(), 1);
        assert_eq!(additions.len(), 1);
    }

    #[test]
    fn test_compute_diff_multi_hunk() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nK\nl\nm\nn\n";
        // With context=1, these should produce 2 separate hunks
        let hunks = compute_diff(old, new, 1);
        assert!(hunks.len() >= 2, "expected 2+ hunks, got {}", hunks.len());
    }

    #[test]
    fn test_compute_diff_word_emphasis() {
        let old = "let raw = parse(input);\n";
        let new = "let raw = parse(input)?;\n";
        let hunks = compute_diff(old, new, 3);
        assert_eq!(hunks.len(), 1);

        // Should have both deletion and addition lines
        let has_del = hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Deletion);
        let has_add = hunks[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffLineKind::Addition);
        assert!(has_del);
        assert!(has_add);
    }

    #[test]
    fn test_compute_diff_line_numbers() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let hunks = compute_diff(old, new, 3);
        assert_eq!(hunks.len(), 1);

        // Context lines should have both line numbers
        for line in &hunks[0].lines {
            match line.kind {
                DiffLineKind::Context => {
                    assert!(line.old_lineno.is_some());
                    assert!(line.new_lineno.is_some());
                }
                DiffLineKind::Deletion => {
                    assert!(line.old_lineno.is_some());
                    assert!(line.new_lineno.is_none());
                }
                DiffLineKind::Addition => {
                    assert!(line.old_lineno.is_none());
                    assert!(line.new_lineno.is_some());
                }
            }
        }
    }

    #[test]
    fn test_compute_file_diff_stats() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nC\nd\n";
        let diff = compute_file_diff(Some("old.rs"), "new.rs", old, new, 3);
        assert_eq!(diff.stats.deletions, 2); // b, c removed
        assert_eq!(diff.stats.additions, 3); // B, C, d added
        assert_eq!(diff.kind, DiffKind::Modified);
        assert_eq!(diff.old_path.as_deref(), Some("old.rs"));
        assert_eq!(diff.new_path, "new.rs");
    }

    #[test]
    fn test_compute_file_diff_new_file() {
        let diff = compute_file_diff(None, "new.rs", "", "hello\n", 3);
        assert_eq!(diff.kind, DiffKind::Added);
        assert_eq!(diff.stats.additions, 1);
    }

    #[test]
    fn test_compute_file_diff_deleted() {
        let diff = compute_file_diff(Some("old.rs"), "old.rs", "hello\n", "", 3);
        assert_eq!(diff.kind, DiffKind::Deleted);
        assert_eq!(diff.stats.deletions, 1);
    }

    #[test]
    fn test_parse_hunk_header() {
        assert_eq!(parse_hunk_header("@@ -10,7 +10,8 @@"), (10, 10));
        assert_eq!(
            parse_hunk_header("@@ -1,3 +1,5 @@ fn main()"),
            (1, 1) // Both old_start and new_start are 1; 3 and 5 are counts
        );
        assert_eq!(parse_hunk_header("@@ -0,0 +1,10 @@"), (0, 1));
        assert_eq!(parse_hunk_header("@@ -5 +5 @@"), (5, 5));
    }

    #[test]
    fn test_parse_git_diff_simple() {
        let diff_text = r#"diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!("hello");
+    println!("hello world");
+    println!("goodbye");
 }
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 1);

        let d = &diffs[0];
        assert_eq!(d.old_path.as_deref(), Some("src/main.rs"));
        assert_eq!(d.new_path, "src/main.rs");
        assert_eq!(d.kind, DiffKind::Modified);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.stats.additions, 2);
        assert_eq!(d.stats.deletions, 1);
    }

    #[test]
    fn test_parse_git_diff_new_file() {
        let diff_text = r#"diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..abc1234
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::Added);
        assert!(diffs[0].old_path.is_none());
        assert_eq!(diffs[0].new_path, "new.txt");
    }

    #[test]
    fn test_parse_git_diff_deleted_file() {
        let diff_text = r#"diff --git a/old.txt b/old.txt
deleted file mode 100644
index abc1234..0000000
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-hello
-world
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::Deleted);
    }

    #[test]
    fn test_parse_git_diff_multiple_files() {
        let diff_text = r#"diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
 fn a() {
-    old();
+    new();
 }
diff --git a/b.rs b/b.rs
index 333..444 100644
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1,2 @@
 fn b() {
-    old();
+    new();
 }
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].new_path, "a.rs");
        assert_eq!(diffs[1].new_path, "b.rs");
    }

    #[test]
    fn test_parse_git_diff_rename() {
        let diff_text = r#"diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
index abc..def 100644
--- a/old_name.rs
+++ b/new_name.rs
@@ -1,3 +1,3 @@
 fn hello() {
-    old();
+    new();
 }
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 1);
        assert_eq!(
            diffs[0].kind,
            DiffKind::Renamed {
                old_path: "old_name.rs".to_string()
            }
        );
        assert_eq!(diffs[0].new_path, "new_name.rs");
    }

    #[test]
    fn test_parse_git_diff_empty() {
        let diffs = parse_git_diff("");
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_parse_git_diff_no_newline_at_end() {
        let diff_text = r#"diff --git a/test.txt b/test.txt
index abc..def 100644
--- a/test.txt
+++ b/test.txt
@@ -1 +1 @@
-hello
\ No newline at end of file
+world
\ No newline at end of file
"#;
        let diffs = parse_git_diff(diff_text);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].stats.deletions, 1);
        assert_eq!(diffs[0].stats.additions, 1);
    }
}
