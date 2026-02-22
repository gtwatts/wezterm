//! Git repository status detection for the Elwood status bar and agent context.
//!
//! Uses `git` CLI commands rather than the `git2` crate — simpler dependency chain
//! and the git binary is already on the user's PATH.

use std::path::Path;
use std::process::Command;

/// Summary of the current git repository state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    /// Current branch name (e.g. "main", "feat/my-branch").
    pub branch: String,
    /// Whether there are uncommitted changes (staged or unstaged).
    pub is_dirty: bool,
    /// Number of commits ahead of the upstream tracking branch.
    pub ahead: u32,
    /// Number of commits behind the upstream tracking branch.
    pub behind: u32,
}

impl GitInfo {
    /// Format for the status bar: `main*` or `feat/branch ^2 v1`.
    pub fn status_display(&self) -> String {
        let mut out = self.branch.clone();
        if self.is_dirty {
            out.push('*');
        }
        if self.ahead > 0 {
            out.push_str(&format!(" \u{2191}{}", self.ahead));
        }
        if self.behind > 0 {
            out.push_str(&format!(" \u{2193}{}", self.behind));
        }
        out
    }
}

/// Git context passed to the agent with each message for repository awareness.
#[derive(Debug, Clone, Default)]
pub struct GitContext {
    /// Current branch name.
    pub branch: String,
    /// Recent commit summaries (one-line format, newest first).
    pub recent_commits: Vec<String>,
    /// Files currently staged for commit.
    pub staged_files: Vec<String>,
}

impl GitContext {
    /// Format as a context block suitable for prepending to agent messages.
    pub fn format_context(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Git branch: {}\n", self.branch));

        if !self.recent_commits.is_empty() {
            out.push_str("Recent commits:\n");
            for c in &self.recent_commits {
                out.push_str(&format!("  {c}\n"));
            }
        }

        if !self.staged_files.is_empty() {
            out.push_str("Staged files:\n");
            for f in &self.staged_files {
                out.push_str(&format!("  {f}\n"));
            }
        }

        out
    }
}

/// Detect the git repository state for the given working directory.
///
/// Returns `None` if the directory is not inside a git repository or if
/// `git` is not available on PATH.
pub fn get_git_info(cwd: &Path) -> Option<GitInfo> {
    // Check if we're in a git repo
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() {
        return None;
    }

    let is_dirty = run_git(cwd, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    let (ahead, behind) = get_ahead_behind(cwd);

    Some(GitInfo {
        branch,
        is_dirty,
        ahead,
        behind,
    })
}

/// Collect full git context for the agent (branch, recent commits, staged files).
///
/// Returns a default (empty) context if not in a git repo.
pub fn get_git_context(cwd: &Path) -> GitContext {
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_default();

    if branch.is_empty() {
        return GitContext::default();
    }

    let recent_commits = run_git(cwd, &["log", "--oneline", "-5"])
        .map(|s| {
            s.lines()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let staged_files = run_git(cwd, &["diff", "--cached", "--name-only"])
        .map(|s| {
            s.lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    GitContext {
        branch,
        recent_commits,
        staged_files,
    }
}

/// Parse ahead/behind counts from `git rev-list --left-right --count HEAD...@{u}`.
fn get_ahead_behind(cwd: &Path) -> (u32, u32) {
    let output = run_git(cwd, &["rev-list", "--left-right", "--count", "HEAD...@{u}"]);
    match output {
        Some(s) => parse_ahead_behind(&s),
        None => (0, 0),
    }
}

/// Parse the output of `git rev-list --left-right --count`.
///
/// Expected format: `<ahead>\t<behind>` (tab-separated).
fn parse_ahead_behind(output: &str) -> (u32, u32) {
    let parts: Vec<&str> = output.split_whitespace().collect();
    if parts.len() == 2 {
        let ahead = parts[0].parse().unwrap_or(0);
        let behind = parts[1].parse().unwrap_or(0);
        (ahead, behind)
    } else {
        (0, 0)
    }
}

/// Run a git command and return its stdout, trimmed.
///
/// Returns `None` if the command fails (not a git repo, git not found, etc.).
fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_ahead_behind_normal() {
        assert_eq!(parse_ahead_behind("3\t1"), (3, 1));
    }

    #[test]
    fn test_parse_ahead_behind_zeros() {
        assert_eq!(parse_ahead_behind("0\t0"), (0, 0));
    }

    #[test]
    fn test_parse_ahead_behind_empty() {
        assert_eq!(parse_ahead_behind(""), (0, 0));
    }

    #[test]
    fn test_parse_ahead_behind_malformed() {
        assert_eq!(parse_ahead_behind("abc\tdef"), (0, 0));
    }

    #[test]
    fn test_parse_ahead_behind_single() {
        assert_eq!(parse_ahead_behind("5"), (0, 0));
    }

    #[test]
    fn test_status_display_clean() {
        let info = GitInfo {
            branch: "main".to_string(),
            is_dirty: false,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(info.status_display(), "main");
    }

    #[test]
    fn test_status_display_dirty() {
        let info = GitInfo {
            branch: "main".to_string(),
            is_dirty: true,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(info.status_display(), "main*");
    }

    #[test]
    fn test_status_display_ahead_behind() {
        let info = GitInfo {
            branch: "feat/branch".to_string(),
            is_dirty: true,
            ahead: 2,
            behind: 1,
        };
        assert_eq!(info.status_display(), "feat/branch* \u{2191}2 \u{2193}1");
    }

    #[test]
    fn test_status_display_ahead_only() {
        let info = GitInfo {
            branch: "dev".to_string(),
            is_dirty: false,
            ahead: 3,
            behind: 0,
        };
        assert_eq!(info.status_display(), "dev \u{2191}3");
    }

    #[test]
    fn test_git_info_in_real_repo() {
        // This test runs in the actual elwood-pro repo
        let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let info = get_git_info(&cwd);
        // We should be in a git repo (the project itself)
        assert!(info.is_some(), "Expected to be in a git repo");
        let info = info.unwrap();
        assert!(!info.branch.is_empty());
    }

    #[test]
    fn test_git_info_not_a_repo() {
        let info = get_git_info(Path::new("/tmp"));
        // /tmp is (usually) not a git repo; if it happens to be, just skip
        // This primarily checks that we don't panic
        let _ = info;
    }

    #[test]
    fn test_git_context_in_real_repo() {
        let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let ctx = get_git_context(&cwd);
        assert!(!ctx.branch.is_empty());
        // recent_commits may or may not be empty depending on repo state
    }

    #[test]
    fn test_git_context_format() {
        let ctx = GitContext {
            branch: "main".to_string(),
            recent_commits: vec![
                "abc1234 First commit".to_string(),
                "def5678 Second commit".to_string(),
            ],
            staged_files: vec!["src/main.rs".to_string()],
        };
        let formatted = ctx.format_context();
        assert!(formatted.contains("Git branch: main"));
        assert!(formatted.contains("Recent commits:"));
        assert!(formatted.contains("abc1234 First commit"));
        assert!(formatted.contains("Staged files:"));
        assert!(formatted.contains("src/main.rs"));
    }

    #[test]
    fn test_git_context_empty_format() {
        let ctx = GitContext::default();
        let formatted = ctx.format_context();
        assert!(formatted.contains("Git branch:"));
        assert!(!formatted.contains("Recent commits:"));
        assert!(!formatted.contains("Staged files:"));
    }

    #[test]
    fn test_detached_head() {
        // Detached HEAD returns a hash or "HEAD" — either way it should not be empty
        // We can't easily test this without detaching, so just verify parsing works
        let info = GitInfo {
            branch: "HEAD".to_string(),
            is_dirty: false,
            ahead: 0,
            behind: 0,
        };
        assert_eq!(info.status_display(), "HEAD");
    }
}
