//! Command auto-correction — suggests fixes when shell commands fail.
//!
//! When a command exits with a non-zero status, [`CommandCorrector`] analyzes the
//! command text and stderr output to suggest corrections. Strategies are tried in
//! order (common typos, Levenshtein distance, git/cargo-specific, permission errors)
//! and the first match is returned.
//!
//! ## Example
//!
//! ```
//! use elwood_bridge::autocorrect::{CommandCorrector, Correction};
//!
//! let corrector = CommandCorrector::new();
//! if let Some(fix) = corrector.suggest_correction("gti status", "command not found: gti", 127) {
//!     assert_eq!(fix.suggested, "git status");
//! }
//! ```

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

// ─── Correction ─────────────────────────────────────────────────────────

/// A suggested correction for a failed command.
#[derive(Debug, Clone)]
pub struct Correction {
    /// The original command that failed.
    pub original: String,
    /// The suggested corrected command.
    pub suggested: String,
    /// Confidence score from 0.0 (guess) to 1.0 (certain).
    pub confidence: f64,
    /// Human-readable explanation of the correction.
    pub explanation: String,
}

// ─── Levenshtein distance ───────────────────────────────────────────────

/// Compute the Levenshtein (edit) distance between two strings.
///
/// Uses the classic dynamic-programming algorithm with a single-row
/// optimization for O(min(m,n)) space.
///
/// ```
/// use elwood_bridge::autocorrect::levenshtein;
/// assert_eq!(levenshtein("kitten", "sitting"), 3);
/// assert_eq!(levenshtein("", "abc"), 3);
/// assert_eq!(levenshtein("same", "same"), 0);
/// ```
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Single-row DP: row[j] = distance for a[..i] vs b[..j]
    let mut row: Vec<usize> = (0..=n).collect();

    for i in 1..=m {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            let val = (prev + cost)
                .min(row[j] + 1) // deletion
                .min(row[j - 1] + 1); // insertion
            prev = row[j];
            row[j] = val;
        }
    }

    row[n]
}

// ─── Common typo map ────────────────────────────────────────────────────

/// Build the static map of common command typos.
fn common_typos() -> &'static HashMap<&'static str, &'static str> {
    static TYPOS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TYPOS.get_or_init(|| {
        let mut m = HashMap::new();
        // Git typos
        m.insert("gti", "git");
        m.insert("gIt", "git");
        m.insert("gi", "git");
        m.insert("got", "git");
        m.insert("giit", "git");
        m.insert("igt", "git");
        m.insert("tgi", "git");
        // Python typos
        m.insert("pytohn", "python");
        m.insert("pyhton", "python");
        m.insert("pyton", "python");
        m.insert("pytho", "python");
        m.insert("pythno", "python");
        m.insert("pthon", "python");
        m.insert("pythn", "python");
        m.insert("python3", "python3"); // identity — included for completeness
        m.insert("pyhton3", "python3");
        m.insert("pytohn3", "python3");
        // Cargo typos
        m.insert("carg", "cargo");
        m.insert("carog", "cargo");
        m.insert("crago", "cargo");
        m.insert("cargi", "cargo");
        m.insert("cagro", "cargo");
        // Docker typos
        m.insert("dcoker", "docker");
        m.insert("dokcer", "docker");
        m.insert("doker", "docker");
        m.insert("dockre", "docker");
        m.insert("docekr", "docker");
        // Node typos
        m.insert("ndoe", "node");
        m.insert("noed", "node");
        m.insert("onde", "node");
        m.insert("nde", "node");
        // npm typos
        m.insert("nmp", "npm");
        m.insert("nmp", "npm");
        m.insert("npn", "npm");
        // Misc command typos
        m.insert("cd..", "cd ..");
        m.insert("sl", "ls");
        m.insert("sls", "ls");
        m.insert("ls-la", "ls -la");
        m.insert("mkdr", "mkdir");
        m.insert("mkidr", "mkdir");
        m.insert("mdkir", "mkdir");
        m.insert("mkadir", "mkdir");
        m.insert("cta", "cat");
        m.insert("caat", "cat");
        m.insert("catt", "cat");
        m.insert("grpe", "grep");
        m.insert("gerp", "grep");
        m.insert("grrp", "grep");
        m.insert("grp", "grep");
        m.insert("claer", "clear");
        m.insert("cler", "clear");
        m.insert("clera", "clear");
        m.insert("suod", "sudo");
        m.insert("sduo", "sudo");
        m.insert("sudi", "sudo");
        m.insert("curlr", "curl");
        m.insert("crul", "curl");
        m.insert("ucrl", "curl");
        m.insert("wegt", "wget");
        m.insert("weget", "wget");
        m.insert("eixt", "exit");
        m.insert("exti", "exit");
        m.insert("eitx", "exit");
        m.insert("ehco", "echo");
        m.insert("ecoh", "echo");
        m.insert("ceho", "echo");
        // Editors / tools
        m.insert("vmi", "vim");
        m.insert("ivm", "vim");
        m.insert("nvi", "nvim");
        m.insert("nvmi", "nvim");
        m.insert("naon", "nano");
        m.insert("anon", "nano");
        m.insert("htop", "htop"); // identity
        m.insert("hotp", "htop");
        // Kubernetes
        m.insert("kubeclt", "kubectl");
        m.insert("kubetcl", "kubectl");
        m.insert("kuebctl", "kubectl");
        m
    })
}

/// Git subcommand typos — maps misspelled subcommands to correct ones.
fn git_subcommand_typos() -> &'static HashMap<&'static str, &'static str> {
    static TYPOS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TYPOS.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("psuh", "push");
        m.insert("puhs", "push");
        m.insert("phus", "push");
        m.insert("psh", "push");
        m.insert("comit", "commit");
        m.insert("commti", "commit");
        m.insert("commt", "commit");
        m.insert("committ", "commit");
        m.insert("commmit", "commit");
        m.insert("sttaus", "status");
        m.insert("stauts", "status");
        m.insert("staus", "status");
        m.insert("statsu", "status");
        m.insert("chekcout", "checkout");
        m.insert("chekout", "checkout");
        m.insert("checkotu", "checkout");
        m.insert("chekcot", "checkout");
        m.insert("pul", "pull");
        m.insert("plul", "pull");
        m.insert("pulll", "pull");
        m.insert("marge", "merge");
        m.insert("merg", "merge");
        m.insert("mrege", "merge");
        m.insert("branh", "branch");
        m.insert("brach", "branch");
        m.insert("brnach", "branch");
        m.insert("rbase", "rebase");
        m.insert("reabse", "rebase");
        m.insert("rebas", "rebase");
        m.insert("stsh", "stash");
        m.insert("stahs", "stash");
        m.insert("sahts", "stash");
        m.insert("dif", "diff");
        m.insert("idff", "diff");
        m.insert("dfif", "diff");
        m.insert("lgo", "log");
        m.insert("olg", "log");
        m.insert("resset", "reset");
        m.insert("reste", "reset");
        m.insert("ad", "add");
        m.insert("dad", "add");
        m.insert("addd", "add");
        m
    })
}

// ─── Known commands cache ───────────────────────────────────────────────

/// Shell builtins that won't appear in PATH but are valid commands.
const SHELL_BUILTINS: &[&str] = &[
    "alias", "bg", "break", "builtin", "case", "cd", "command", "continue", "declare", "dirs",
    "disown", "echo", "enable", "eval", "exec", "exit", "export", "false", "fc", "fg", "for",
    "getopts", "hash", "help", "history", "if", "jobs", "kill", "let", "local", "logout", "popd",
    "printf", "pushd", "pwd", "read", "readonly", "return", "select", "set", "shift", "shopt",
    "source", "suspend", "test", "time", "times", "trap", "true", "type", "typeset", "ulimit",
    "umask", "unalias", "unset", "until", "wait", "while",
];

/// Duration after which the known commands cache is refreshed.
const CACHE_TTL_SECS: u64 = 300; // 5 minutes

struct KnownCommandsCache {
    commands: Vec<String>,
    last_refresh: Instant,
}

impl KnownCommandsCache {
    fn new() -> Self {
        let mut cache = Self {
            commands: Vec::new(),
            last_refresh: Instant::now(),
        };
        cache.refresh();
        cache
    }

    fn refresh(&mut self) {
        let mut commands: Vec<String> = SHELL_BUILTINS.iter().map(|s| (*s).to_string()).collect();

        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(':') {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if let Ok(ft) = entry.file_type() {
                            if ft.is_file() || ft.is_symlink() {
                                if let Some(name) = entry.file_name().to_str() {
                                    commands.push(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        commands.sort_unstable();
        commands.dedup();
        self.commands = commands;
        self.last_refresh = Instant::now();
    }

    fn is_stale(&self) -> bool {
        self.last_refresh.elapsed().as_secs() >= CACHE_TTL_SECS
    }

    fn find_closest(&mut self, typo: &str, max_results: usize) -> Vec<(String, usize)> {
        if self.is_stale() {
            self.refresh();
        }

        let max_dist = if typo.len() < 6 { 2 } else { 3 };

        let mut candidates: Vec<(String, usize)> = self
            .commands
            .iter()
            .filter_map(|cmd| {
                let dist = levenshtein(typo, cmd);
                if dist > 0 && dist <= max_dist {
                    Some((cmd.clone(), dist))
                } else {
                    None
                }
            })
            .collect();

        candidates.sort_by_key(|(_, dist)| *dist);
        candidates.truncate(max_results);
        candidates
    }
}

// ─── CommandCorrector ───────────────────────────────────────────────────

/// Auto-correction engine for failed shell commands.
///
/// After a command fails (non-zero exit code), call
/// [`suggest_correction`](CommandCorrector::suggest_correction) with the command
/// text and stderr to get a suggested fix.
pub struct CommandCorrector {
    enabled: bool,
    cache: Mutex<KnownCommandsCache>,
    /// Track acceptance rates: (accepted, total) per strategy.
    acceptance: Mutex<HashMap<String, (u32, u32)>>,
}

impl CommandCorrector {
    /// Create a new corrector with a fresh known-commands cache.
    pub fn new() -> Self {
        Self {
            enabled: true,
            cache: Mutex::new(KnownCommandsCache::new()),
            acceptance: Mutex::new(HashMap::new()),
        }
    }

    /// Whether auto-correction is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable auto-correction.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Force a refresh of the known-commands cache (called by `/rehash`).
    pub fn rehash(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.refresh();
        }
    }

    /// Record that the user accepted or dismissed a correction.
    pub fn record_feedback(&self, strategy: &str, accepted: bool) {
        if let Ok(mut acc) = self.acceptance.lock() {
            let entry = acc.entry(strategy.to_string()).or_insert((0, 0));
            if accepted {
                entry.0 += 1;
            }
            entry.1 += 1;
        }
    }

    /// Suggest a correction for a failed command.
    ///
    /// Strategies are tried in order:
    /// 1. Common typo map (exact match)
    /// 2. Git subcommand corrections
    /// 3. Git "did you mean" stderr parsing
    /// 4. Permission error suggestions
    /// 5. Cargo "did you mean" parsing
    /// 6. Levenshtein distance against known commands
    ///
    /// Returns the first match, or `None` if no correction is found.
    pub fn suggest_correction(
        &self,
        command: &str,
        stderr: &str,
        exit_code: i32,
    ) -> Option<Correction> {
        if !self.enabled || command.is_empty() {
            return None;
        }

        // Only act on actual failures
        if exit_code == 0 {
            return None;
        }

        // Try strategies in priority order
        self.try_common_typo(command)
            .or_else(|| self.try_git_correction(command, stderr))
            .or_else(|| self.try_git_did_you_mean(command, stderr))
            .or_else(|| self.try_permission_error(command, stderr))
            .or_else(|| self.try_cargo_correction(command, stderr))
            .or_else(|| self.try_levenshtein_correction(command, stderr))
    }

    // ── Strategy 1: Common typo map ─────────────────────────────────────

    fn try_common_typo(&self, command: &str) -> Option<Correction> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let first_word = parts.first()?;

        let typos = common_typos();
        if let Some(&corrected) = typos.get(*first_word) {
            let rest: String = parts[1..].join(" ");
            let suggested = if rest.is_empty() {
                corrected.to_string()
            } else {
                format!("{corrected} {rest}")
            };
            return Some(Correction {
                original: command.to_string(),
                suggested,
                confidence: 0.95,
                explanation: format!("Common typo: `{first_word}` -> `{corrected}`"),
            });
        }

        None
    }

    // ── Strategy 2: Git subcommand corrections ──────────────────────────

    fn try_git_correction(&self, command: &str, _stderr: &str) -> Option<Correction> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.len() < 2 || parts[0] != "git" {
            return None;
        }

        let sub = parts[1];
        let typos = git_subcommand_typos();
        if let Some(&corrected) = typos.get(sub) {
            let rest: String = parts[2..].join(" ");
            let suggested = if rest.is_empty() {
                format!("git {corrected}")
            } else {
                format!("git {corrected} {rest}")
            };
            return Some(Correction {
                original: command.to_string(),
                suggested,
                confidence: 0.95,
                explanation: format!("Git typo: `{sub}` -> `{corrected}`"),
            });
        }

        None
    }

    // ── Strategy 3: Parse git's "did you mean" stderr ───────────────────

    fn try_git_did_you_mean(&self, command: &str, stderr: &str) -> Option<Correction> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() || parts[0] != "git" {
            return None;
        }

        // Git outputs: "The most similar command is\n\t<command>"
        // or: "Did you mean one of these?\n\t<command>\n\t<command>"
        if let Some(pos) = stderr.find("The most similar command is") {
            let after = &stderr[pos..];
            // Look for indented command name
            for line in after.lines().skip(1) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let rest: String = parts[2..].join(" ");
                    let suggested = if rest.is_empty() {
                        format!("git {trimmed}")
                    } else {
                        format!("git {trimmed} {rest}")
                    };
                    return Some(Correction {
                        original: command.to_string(),
                        suggested,
                        confidence: 0.9,
                        explanation: format!("Git suggested: `{trimmed}`"),
                    });
                }
            }
        }

        // "Did you mean one of these?" — pick the first
        if let Some(pos) = stderr.find("Did you mean") {
            let after = &stderr[pos..];
            for line in after.lines().skip(1) {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let rest: String = parts[2..].join(" ");
                    let suggested = if rest.is_empty() {
                        format!("git {trimmed}")
                    } else {
                        format!("git {trimmed} {rest}")
                    };
                    return Some(Correction {
                        original: command.to_string(),
                        suggested,
                        confidence: 0.85,
                        explanation: format!("Git suggested: `{trimmed}`"),
                    });
                }
            }
        }

        None
    }

    // ── Strategy 4: Permission errors ───────────────────────────────────

    fn try_permission_error(&self, command: &str, stderr: &str) -> Option<Correction> {
        let lower = stderr.to_lowercase();
        let is_permission_err = lower.contains("permission denied")
            || lower.contains("eacces")
            || lower.contains("operation not permitted");

        if !is_permission_err {
            return None;
        }

        // Don't suggest sudo if already using sudo
        if command.starts_with("sudo ") {
            return None;
        }

        // npm-specific: suggest --user flag instead of sudo
        if command.starts_with("npm ") {
            return Some(Correction {
                original: command.to_string(),
                suggested: format!("{command} --user"),
                confidence: 0.7,
                explanation: "Permission denied — try installing for current user".to_string(),
            });
        }

        Some(Correction {
            original: command.to_string(),
            suggested: format!("sudo {command}"),
            confidence: 0.6,
            explanation: "Permission denied — may require elevated privileges (use with caution)"
                .to_string(),
        })
    }

    // ── Strategy 5: Cargo corrections ───────────────────────────────────

    fn try_cargo_correction(&self, command: &str, stderr: &str) -> Option<Correction> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() || parts[0] != "cargo" {
            return None;
        }

        // Cargo "did you mean" for test names
        // e.g., "no test functions matched pattern `foo`. did you mean `food`"
        let lower = stderr.to_lowercase();
        if lower.contains("did you mean") {
            // Extract the suggestion — cargo wraps it in backticks
            if let Some(pos) = lower.find("did you mean") {
                let after = &stderr[pos..];
                if let Some(start) = after.find('`') {
                    let rest = &after[start + 1..];
                    if let Some(end) = rest.find('`') {
                        let suggested_name = &rest[..end];
                        // Reconstruct command replacing the last argument
                        let mut new_parts: Vec<&str> = parts.clone();
                        if let Some(last) = new_parts.last_mut() {
                            *last = suggested_name;
                        }
                        let suggested = new_parts.join(" ");
                        return Some(Correction {
                            original: command.to_string(),
                            suggested,
                            confidence: 0.85,
                            explanation: format!("Cargo suggested: `{suggested_name}`"),
                        });
                    }
                }
            }
        }

        // "could not find `Cargo.toml`" → suggest checking directory
        if stderr.contains("could not find `Cargo.toml`") {
            return Some(Correction {
                original: command.to_string(),
                suggested: format!("{command} --manifest-path ./*/Cargo.toml"),
                confidence: 0.5,
                explanation: "No Cargo.toml found — check working directory or use --manifest-path"
                    .to_string(),
            });
        }

        None
    }

    // ── Strategy 6: Levenshtein against known commands ──────────────────

    fn try_levenshtein_correction(&self, command: &str, stderr: &str) -> Option<Correction> {
        // Only useful for "command not found" errors
        let lower = stderr.to_lowercase();
        let is_not_found = lower.contains("command not found")
            || lower.contains("not found")
            || lower.contains("no such file or directory");

        if !is_not_found {
            return None;
        }

        let parts: Vec<&str> = command.split_whitespace().collect();
        let first_word = parts.first()?;

        let mut cache = self.cache.lock().ok()?;
        let candidates = cache.find_closest(first_word, 3);

        if candidates.is_empty() {
            return None;
        }

        let (best_cmd, best_dist) = &candidates[0];
        let rest: String = parts[1..].join(" ");
        let suggested = if rest.is_empty() {
            best_cmd.clone()
        } else {
            format!("{best_cmd} {rest}")
        };

        // Confidence based on distance: dist 1 → 0.85, dist 2 → 0.7, dist 3 → 0.5
        let confidence = match best_dist {
            1 => 0.85,
            2 => 0.7,
            _ => 0.5,
        };

        Some(Correction {
            original: command.to_string(),
            suggested,
            confidence,
            explanation: format!("Similar command: `{best_cmd}` (edit distance {best_dist})"),
        })
    }
}

impl Default for CommandCorrector {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Levenshtein tests ───────────────────────────────────────────────

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_empty_strings() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "xyz"), 3);
    }

    #[test]
    fn test_levenshtein_single_char() {
        assert_eq!(levenshtein("a", "b"), 1);
        assert_eq!(levenshtein("a", "a"), 0);
        assert_eq!(levenshtein("a", ""), 1);
    }

    #[test]
    fn test_levenshtein_insertion() {
        assert_eq!(levenshtein("git", "grit"), 1);
    }

    #[test]
    fn test_levenshtein_deletion() {
        assert_eq!(levenshtein("grit", "git"), 1);
    }

    #[test]
    fn test_levenshtein_substitution() {
        assert_eq!(levenshtein("cat", "car"), 1);
    }

    #[test]
    fn test_levenshtein_classic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn test_levenshtein_symmetric() {
        assert_eq!(levenshtein("abc", "def"), levenshtein("def", "abc"));
    }

    #[test]
    fn test_levenshtein_prefix() {
        assert_eq!(levenshtein("car", "cargo"), 2);
    }

    // ── Common typo map tests ───────────────────────────────────────────

    #[test]
    fn test_common_typos_has_entries() {
        let typos = common_typos();
        assert!(
            typos.len() >= 50,
            "Expected 50+ typo entries, got {}",
            typos.len()
        );
    }

    #[test]
    fn test_common_typo_gti() {
        assert_eq!(common_typos().get("gti"), Some(&"git"));
    }

    #[test]
    fn test_common_typo_pytohn() {
        assert_eq!(common_typos().get("pytohn"), Some(&"python"));
    }

    #[test]
    fn test_common_typo_dcoker() {
        assert_eq!(common_typos().get("dcoker"), Some(&"docker"));
    }

    #[test]
    fn test_common_typo_ndoe() {
        assert_eq!(common_typos().get("ndoe"), Some(&"node"));
    }

    #[test]
    fn test_common_typo_carg() {
        assert_eq!(common_typos().get("carg"), Some(&"cargo"));
    }

    #[test]
    fn test_common_typo_sl() {
        assert_eq!(common_typos().get("sl"), Some(&"ls"));
    }

    #[test]
    fn test_common_typo_suod() {
        assert_eq!(common_typos().get("suod"), Some(&"sudo"));
    }

    #[test]
    fn test_common_typo_grpe() {
        assert_eq!(common_typos().get("grpe"), Some(&"grep"));
    }

    // ── Git subcommand typo tests ───────────────────────────────────────

    #[test]
    fn test_git_subcommand_typos_has_entries() {
        let typos = git_subcommand_typos();
        assert!(
            typos.len() >= 30,
            "Expected 30+ git subcommand typos, got {}",
            typos.len()
        );
    }

    #[test]
    fn test_git_sub_psuh() {
        assert_eq!(git_subcommand_typos().get("psuh"), Some(&"push"));
    }

    #[test]
    fn test_git_sub_comit() {
        assert_eq!(git_subcommand_typos().get("comit"), Some(&"commit"));
    }

    #[test]
    fn test_git_sub_sttaus() {
        assert_eq!(git_subcommand_typos().get("sttaus"), Some(&"status"));
    }

    // ── CommandCorrector strategy tests ─────────────────────────────────

    #[test]
    fn test_corrector_common_typo_simple() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("gti status", "command not found: gti", 127);
        assert!(fix.is_some());
        let fix = fix.unwrap();
        assert_eq!(fix.suggested, "git status");
        assert!(fix.confidence >= 0.9);
    }

    #[test]
    fn test_corrector_common_typo_with_args() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("pytohn -m pytest", "command not found: pytohn", 127);
        assert!(fix.is_some());
        assert_eq!(fix.unwrap().suggested, "python -m pytest");
    }

    #[test]
    fn test_corrector_git_subcommand() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("git psuh origin main", "", 1);
        assert!(fix.is_some());
        let fix = fix.unwrap();
        assert_eq!(fix.suggested, "git push origin main");
        assert!(fix.confidence >= 0.9);
    }

    #[test]
    fn test_corrector_git_comit() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("git comit -m 'fix'", "", 1);
        assert!(fix.is_some());
        assert_eq!(fix.unwrap().suggested, "git commit -m 'fix'");
    }

    #[test]
    fn test_corrector_git_sttaus() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("git sttaus", "", 1);
        assert!(fix.is_some());
        assert_eq!(fix.unwrap().suggested, "git status");
    }

    #[test]
    fn test_corrector_git_did_you_mean_similar_command() {
        let c = CommandCorrector::new();
        let stderr = "git: 'stahs' is not a git command. See 'git --help'.\n\n\
                       The most similar command is\n\tstash";
        let fix = c.suggest_correction("git stahs", stderr, 1);
        assert!(fix.is_some());
        assert_eq!(fix.unwrap().suggested, "git stash");
    }

    #[test]
    fn test_corrector_git_did_you_mean_one_of_these() {
        let c = CommandCorrector::new();
        let stderr = "git: 'foo' is not a git command. See 'git --help'.\n\n\
                       Did you mean one of these?\n\tfoo-bar\n\tfoo-baz";
        let fix = c.suggest_correction("git foo", stderr, 1);
        assert!(fix.is_some());
        assert_eq!(fix.unwrap().suggested, "git foo-bar");
    }

    #[test]
    fn test_corrector_git_did_you_mean_with_args() {
        let c = CommandCorrector::new();
        let stderr = "git: 'chekcout' is not a git command. See 'git --help'.\n\n\
                       The most similar command is\n\tcheckout";
        let fix = c.suggest_correction("git chekcout main", stderr, 1);
        assert!(fix.is_some());
        // Git subcommand typo takes priority since "chekcout" is in the map
        let fix = fix.unwrap();
        assert!(fix.suggested.contains("checkout"));
        assert!(fix.suggested.contains("main"));
    }

    // ── Permission error tests ──────────────────────────────────────────

    #[test]
    fn test_corrector_permission_denied() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("cat /etc/shadow", "cat: /etc/shadow: Permission denied", 1);
        assert!(fix.is_some());
        let fix = fix.unwrap();
        assert_eq!(fix.suggested, "sudo cat /etc/shadow");
        assert!(fix.confidence < 0.8); // Low confidence — sudo is risky
    }

    #[test]
    fn test_corrector_permission_already_sudo() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction("sudo cat /etc/shadow", "Permission denied", 1);
        // Should NOT re-wrap in sudo
        assert!(fix.is_none() || !fix.unwrap().suggested.starts_with("sudo sudo"));
    }

    #[test]
    fn test_corrector_npm_eacces() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction(
            "npm install -g typescript",
            "npm ERR! Error: EACCES: permission denied",
            1,
        );
        assert!(fix.is_some());
        let fix = fix.unwrap();
        assert!(fix.suggested.contains("--user"));
    }

    // ── Cargo correction tests ──────────────────────────────────────────

    #[test]
    fn test_corrector_cargo_did_you_mean() {
        let c = CommandCorrector::new();
        let stderr = "error[E0432]: no test functions matched pattern `test_helo`.\n\
                       Did you mean `test_hello`?";
        let fix = c.suggest_correction("cargo test test_helo", stderr, 101);
        assert!(fix.is_some());
        let fix = fix.unwrap();
        assert!(fix.suggested.contains("test_hello"));
    }

    #[test]
    fn test_corrector_cargo_no_toml() {
        let c = CommandCorrector::new();
        let fix = c.suggest_correction(
            "cargo build",
            "error: could not find `Cargo.toml` in `/home/user` or any parent directory",
            101,
        );
        assert!(fix.is_some());
        assert!(fix.unwrap().suggested.contains("--manifest-path"));
    }

    // ── Levenshtein-based correction tests ──────────────────────────────

    #[test]
    fn test_corrector_levenshtein_command_not_found() {
        let c = CommandCorrector::new();
        // "ggit" is not in the common typo map but is 1 edit from "git"
        let fix = c.suggest_correction("ggit status", "command not found: ggit", 127);
        // This may or may not match depending on PATH — but the corrector shouldn't panic
        if let Some(fix) = fix {
            assert!(!fix.suggested.is_empty());
            assert!(fix.confidence > 0.0);
        }
    }

    // ── Edge cases ──────────────────────────────────────────────────────

    #[test]
    fn test_corrector_empty_command() {
        let c = CommandCorrector::new();
        assert!(c.suggest_correction("", "", 1).is_none());
    }

    #[test]
    fn test_corrector_exit_code_zero() {
        let c = CommandCorrector::new();
        // Should not suggest correction for successful commands
        assert!(c.suggest_correction("gti status", "", 0).is_none());
    }

    #[test]
    fn test_corrector_disabled() {
        let mut c = CommandCorrector::new();
        c.set_enabled(false);
        assert!(!c.is_enabled());
        assert!(c
            .suggest_correction("gti status", "command not found", 127)
            .is_none());
    }

    #[test]
    fn test_corrector_toggle() {
        let mut c = CommandCorrector::new();
        assert!(c.is_enabled());
        c.set_enabled(false);
        assert!(!c.is_enabled());
        c.set_enabled(true);
        assert!(c.is_enabled());
    }

    #[test]
    fn test_corrector_rehash() {
        let c = CommandCorrector::new();
        // Should not panic
        c.rehash();
    }

    #[test]
    fn test_corrector_feedback() {
        let c = CommandCorrector::new();
        c.record_feedback("common_typo", true);
        c.record_feedback("common_typo", false);
        c.record_feedback("levenshtein", true);
        // No assertions — just ensure it doesn't panic
    }

    // ── Known commands cache tests ──────────────────────────────────────

    #[test]
    fn test_cache_contains_builtins() {
        let cache = KnownCommandsCache::new();
        assert!(cache.commands.contains(&"cd".to_string()));
        assert!(cache.commands.contains(&"echo".to_string()));
        assert!(cache.commands.contains(&"export".to_string()));
    }

    #[test]
    fn test_cache_refresh_does_not_panic() {
        let mut cache = KnownCommandsCache::new();
        cache.refresh();
        assert!(!cache.commands.is_empty());
    }

    #[test]
    fn test_cache_find_closest_returns_sorted() {
        let mut cache = KnownCommandsCache::new();
        let results = cache.find_closest("ech", 3);
        // "echo" is a builtin so should appear
        if !results.is_empty() {
            // Results should be sorted by distance (ascending)
            for w in results.windows(2) {
                assert!(w[0].1 <= w[1].1);
            }
        }
    }

    #[test]
    fn test_cache_find_closest_no_self_match() {
        let mut cache = KnownCommandsCache::new();
        // "echo" at distance 0 should not appear
        let results = cache.find_closest("echo", 3);
        for (cmd, dist) in &results {
            assert!(*dist > 0, "should not return exact match: {cmd}");
        }
    }

    #[test]
    fn test_cache_max_results_respected() {
        let mut cache = KnownCommandsCache::new();
        let results = cache.find_closest("a", 2);
        assert!(results.len() <= 2);
    }

    // ── Correction struct tests ─────────────────────────────────────────

    #[test]
    fn test_correction_confidence_range() {
        let c = CommandCorrector::new();
        // Test all strategies produce valid confidence
        let test_cases = vec![
            ("gti status", "command not found: gti", 127),
            ("git psuh", "", 1),
            ("cat /etc/shadow", "Permission denied", 1),
        ];
        for (cmd, stderr, code) in test_cases {
            if let Some(fix) = c.suggest_correction(cmd, stderr, code) {
                assert!(
                    (0.0..=1.0).contains(&fix.confidence),
                    "confidence {} out of range for {cmd}",
                    fix.confidence,
                );
            }
        }
    }

    // ── Levenshtein distance function thorough tests ────────────────────

    #[test]
    fn test_levenshtein_transposition() {
        // "ab" -> "ba" is 2 (Levenshtein, not Damerau-Levenshtein)
        assert_eq!(levenshtein("ab", "ba"), 2);
    }

    #[test]
    fn test_levenshtein_completely_different() {
        assert_eq!(levenshtein("abc", "xyz"), 3);
    }

    #[test]
    fn test_levenshtein_unicode() {
        assert_eq!(levenshtein("cafe", "cafe"), 0);
        assert_eq!(levenshtein("a", "b"), 1);
    }

    // ── Git correction edge cases ───────────────────────────────────────

    #[test]
    fn test_git_correction_non_git_command() {
        let c = CommandCorrector::new();
        // Should not apply git subcommand corrections to non-git commands
        let fix = c.try_git_correction("cargo psuh", "");
        assert!(fix.is_none());
    }

    #[test]
    fn test_git_did_you_mean_no_match() {
        let c = CommandCorrector::new();
        let fix = c.try_git_did_you_mean("git foo", "some random error");
        assert!(fix.is_none());
    }

    #[test]
    fn test_permission_error_eacces() {
        let c = CommandCorrector::new();
        let fix = c.try_permission_error("pip install flask", "EACCES: permission denied");
        assert!(fix.is_some());
        assert!(fix.unwrap().suggested.starts_with("sudo"));
    }

    #[test]
    fn test_permission_error_not_a_permission_issue() {
        let c = CommandCorrector::new();
        let fix = c.try_permission_error("ls foo", "No such file or directory");
        assert!(fix.is_none());
    }
}
