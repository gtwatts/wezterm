//! Natural language vs shell command classifier.
//!
//! A fast, local, zero-cloud heuristic classifier that determines whether user
//! input is a natural language message (routes to agent) or a shell command
//! (routes to `$SHELL -c`).
//!
//! ## Design
//!
//! Uses weighted feature scoring instead of ML. Features include:
//! - Known command prefix matching (~200 common binaries)
//! - Shell operator detection (`|`, `>`, `&&`, etc.)
//! - Flag pattern matching (`-f`, `--verbose`)
//! - Question word and prose marker detection
//! - English word ratio heuristics
//!
//! Performance target: <100 microseconds per classification.

use crate::runtime::InputMode;
use std::collections::HashSet;

/// Result of classifying an input string.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    /// Whether the input is natural language (Agent) or a shell command (Terminal).
    pub mode: InputMode,
    /// Confidence level (0.0 = uncertain, 1.0 = definite).
    pub confidence: f32,
}

/// Confidence threshold below which auto-detection is uncertain.
const AUTO_DETECT_THRESHOLD: f32 = 0.3;

/// Extracted features from an input string for classification.
#[derive(Debug)]
struct NlFeatures {
    starts_with_command: bool,
    has_shell_operators: bool,
    has_flags: bool,
    starts_with_question_word: bool,
    has_prose_markers: bool,
    english_word_ratio: f32,
    word_count: usize,
}

/// Heuristic natural language classifier.
///
/// Pre-populated with ~200 common command prefixes for fast lookup.
/// Supports user-maintained denylist (always Terminal) and allowlist (always Agent).
pub struct NlClassifier {
    command_prefixes: HashSet<&'static str>,
    denylist: HashSet<String>,
    allowlist: HashSet<String>,
}

/// Question words that typically start natural language queries.
const QUESTION_WORDS: &[&str] = &[
    "what", "how", "why", "where", "when", "who", "which",
    "can", "could", "should", "would", "will", "is", "are",
    "do", "does", "did", "has", "have", "had",
];

/// Prose markers that indicate natural language.
const PROSE_MARKERS: &[&str] = &[
    "please", "help", "explain", "show me", "tell me", "describe",
    "fix", "create", "write", "build", "implement", "refactor",
    "debug", "analyze", "i want", "i need", "can you", "help me",
    "what is", "how do", "how to", "why does", "why is",
];

/// Known command binaries (~200 common commands).
const COMMAND_PREFIXES: &[&str] = &[
    // Core utilities
    "ls", "cd", "pwd", "echo", "cat", "head", "tail", "grep", "find", "sed",
    "awk", "sort", "uniq", "wc", "cut", "tr", "tee", "xargs", "mkdir", "rmdir",
    "rm", "cp", "mv", "ln", "touch", "chmod", "chown", "chgrp", "stat", "file",
    "diff", "patch", "tar", "gzip", "gunzip", "zip", "unzip", "bzip2",
    // Network
    "curl", "wget", "ssh", "scp", "rsync", "ping", "traceroute", "nslookup",
    "dig", "netstat", "ss", "nc", "nmap",
    // Version control
    "git", "svn", "hg",
    // Containers & orchestration
    "docker", "kubectl", "podman", "helm", "skaffold",
    // Rust
    "cargo", "rustc", "rustup", "rustfmt", "clippy",
    // JavaScript/TypeScript
    "npm", "npx", "node", "bun", "deno", "yarn", "pnpm", "tsc",
    // Python
    "python", "python3", "pip", "pip3", "pipenv", "poetry", "uv", "pytest",
    // Go
    "go",
    // Build systems
    "make", "cmake", "gcc", "clang", "g++", "cc", "ld",
    // Java/JVM
    "javac", "java", "mvn", "gradle",
    // Ruby
    "ruby", "gem", "bundle", "rake",
    // Other languages
    "perl", "php", "composer", "swift", "kotlinc",
    // Package managers
    "brew", "apt", "apt-get", "yum", "dnf", "pacman", "snap", "flatpak",
    // System
    "systemctl", "journalctl", "sudo", "su", "env", "export", "alias",
    "unalias", "source", "eval", "exec", "nohup", "screen", "tmux",
    // Process management
    "htop", "top", "ps", "kill", "killall", "pkill", "nice", "renice",
    "jobs", "fg", "bg",
    // Disk & filesystem
    "df", "du", "mount", "umount", "fdisk", "lsblk", "lsof",
    // System info
    "free", "uname", "whoami", "id", "groups", "passwd", "date", "cal",
    "uptime", "hostname", "ifconfig", "ip",
    // Firewall
    "iptables", "ufw",
    // Help/info
    "man", "info", "which", "whereis", "type", "history",
    // Infrastructure
    "terraform", "ansible", "vagrant", "pulumi", "sam", "cdk",
    // Testing
    "jest", "vitest", "mocha", "rspec",
    // Elwood
    "elwood",
    // Misc
    "tree", "less", "more", "watch", "time", "xdg-open", "open", "pbcopy",
    "pbpaste", "clear", "reset", "true", "false", "test", "set", "unset",
    "read", "printf", "sleep", "wait",
];

impl NlClassifier {
    /// Create a new classifier with default command prefixes and empty deny/allow lists.
    pub fn new() -> Self {
        Self {
            command_prefixes: COMMAND_PREFIXES.iter().copied().collect(),
            denylist: HashSet::new(),
            allowlist: HashSet::new(),
        }
    }

    /// Create a classifier with custom deny/allow lists.
    pub fn with_lists(denylist: HashSet<String>, allowlist: HashSet<String>) -> Self {
        Self {
            command_prefixes: COMMAND_PREFIXES.iter().copied().collect(),
            denylist,
            allowlist,
        }
    }

    /// Add an entry to the denylist (always classified as Terminal).
    pub fn add_to_denylist(&mut self, entry: String) {
        self.denylist.insert(entry);
    }

    /// Add an entry to the allowlist (always classified as Agent).
    pub fn add_to_allowlist(&mut self, entry: String) {
        self.allowlist.insert(entry);
    }

    /// Classify an input string as natural language or shell command.
    ///
    /// Returns a [`Classification`] with the detected mode and confidence.
    /// When confidence is below [`AUTO_DETECT_THRESHOLD`], the result is uncertain.
    pub fn classify(&self, input: &str) -> Classification {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Classification {
                mode: InputMode::Agent,
                confidence: 0.0,
            };
        }

        // 1. Check denylist/allowlist overrides (exact match)
        if self.denylist.contains(trimmed) {
            return Classification {
                mode: InputMode::Terminal,
                confidence: 1.0,
            };
        }
        if self.allowlist.contains(trimmed) {
            return Classification {
                mode: InputMode::Agent,
                confidence: 1.0,
            };
        }

        // 2. Prefix overrides
        if trimmed.starts_with('!') {
            return Classification {
                mode: InputMode::Terminal,
                confidence: 1.0,
            };
        }

        // 3. Extract features
        let features = extract_features(trimmed, &self.command_prefixes);

        // 4. Weighted scoring (positive = terminal, negative = agent)
        let mut score: f32 = 0.0;

        if features.starts_with_command {
            score += 3.0;
        }
        if features.has_shell_operators {
            score += 4.0;
        }
        if features.has_flags {
            score += 3.0;
        }

        if features.starts_with_question_word {
            score -= 4.0;
        }
        // Only apply prose markers when input does NOT start with a known command,
        // since commands like "cargo build" contain the word "build" which is a
        // false-positive prose marker.
        if features.has_prose_markers && !features.starts_with_command {
            score -= 3.0;
        }
        if features.english_word_ratio > 0.7 {
            score -= 2.0;
        }
        if features.word_count > 6 && !features.has_shell_operators {
            score -= 1.5;
        }

        // 5. Convert score to confidence and mode
        let confidence = (score.abs() / 8.0).min(1.0);
        let mode = if score >= 0.0 {
            InputMode::Terminal
        } else {
            InputMode::Agent
        };

        Classification { mode, confidence }
    }

    /// Returns the auto-detect threshold.
    pub fn threshold(&self) -> f32 {
        AUTO_DETECT_THRESHOLD
    }
}

impl Default for NlClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract classification features from input text.
fn extract_features(input: &str, commands: &HashSet<&str>) -> NlFeatures {
    let words: Vec<&str> = input.split_whitespace().collect();
    let word_count = words.len();
    let first_word = words.first().copied().unwrap_or("");
    let first_word_lower = first_word.to_ascii_lowercase();

    // Check if first word is a known command
    let starts_with_command = commands.contains(first_word_lower.as_str())
        || first_word.starts_with("./")
        || first_word.starts_with('/')
        || first_word.starts_with("~/");

    // Shell operators
    let has_shell_operators = input.contains(" | ")
        || input.contains(" > ")
        || input.contains(" >> ")
        || input.contains(" < ")
        || input.contains(" && ")
        || input.contains(" || ")
        || input.contains(" ; ")
        || input.contains("$(")
        || input.contains('`');

    // Flags (-x, --flag)
    let has_flags = words.iter().any(|w| {
        (w.starts_with('-') && w.len() >= 2 && w.as_bytes()[1] != b' ')
            && !w.starts_with("---")
    });

    // Question words
    let starts_with_question_word = QUESTION_WORDS
        .iter()
        .any(|q| first_word_lower == *q);

    // Prose markers (check in lowercase input)
    let input_lower = input.to_ascii_lowercase();
    let has_prose_markers = PROSE_MARKERS
        .iter()
        .any(|m| input_lower.contains(m));

    // English word ratio heuristic: words with >3 chars that are all-alpha
    let long_alpha_words = words
        .iter()
        .filter(|w| w.len() > 3 && w.chars().all(|c| c.is_alphabetic()))
        .count();
    let english_word_ratio = if word_count > 0 {
        long_alpha_words as f32 / word_count as f32
    } else {
        0.0
    };

    NlFeatures {
        starts_with_command,
        has_shell_operators,
        has_flags,
        starts_with_question_word,
        has_prose_markers,
        english_word_ratio,
        word_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> NlClassifier {
        NlClassifier::new()
    }

    // ── Known commands ───────────────────────────────────────────────

    #[test]
    fn classify_ls() {
        let c = classifier();
        let r = c.classify("ls -la");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_git_status() {
        let c = classifier();
        let r = c.classify("git status");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.3);
    }

    #[test]
    fn classify_cargo_build_release() {
        let c = classifier();
        let r = c.classify("cargo build --release");
        assert_eq!(r.mode, InputMode::Terminal);
        // starts_with_command(+3) + has_flags(+3) = 6, confidence = 6/8 = 0.75
        assert!(r.confidence > 0.7, "confidence was {}", r.confidence);
    }

    #[test]
    fn classify_find_with_flags() {
        let c = classifier();
        let r = c.classify("find . -name \"*.rs\"");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.7);
    }

    #[test]
    fn classify_docker_compose() {
        let c = classifier();
        let r = c.classify("docker compose up -d");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_pipe_command() {
        let c = classifier();
        let r = c.classify("cat file.txt | grep error | wc -l");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.7);
    }

    #[test]
    fn classify_path_prefix() {
        let c = classifier();
        let r = c.classify("./run.sh");
        assert_eq!(r.mode, InputMode::Terminal);
    }

    #[test]
    fn classify_absolute_path() {
        let c = classifier();
        let r = c.classify("/usr/bin/python3 script.py");
        assert_eq!(r.mode, InputMode::Terminal);
    }

    // ── Natural language ─────────────────────────────────────────────

    #[test]
    fn classify_what_files() {
        let c = classifier();
        let r = c.classify("what files are in this directory?");
        assert_eq!(r.mode, InputMode::Agent);
        // starts_with_question_word(-4), confidence = 4/8 = 0.5
        assert!(r.confidence >= 0.5, "confidence was {}", r.confidence);
    }

    #[test]
    fn classify_help_me_fix() {
        let c = classifier();
        let r = c.classify("help me fix this error");
        assert_eq!(r.mode, InputMode::Agent);
        // has_prose_markers(-3), confidence = 3/8 = 0.375
        assert!(r.confidence > 0.3, "confidence was {}", r.confidence);
    }

    #[test]
    fn classify_explain() {
        let c = classifier();
        let r = c.classify("explain the diff");
        assert_eq!(r.mode, InputMode::Agent);
        assert!(r.confidence > 0.3);
    }

    #[test]
    fn classify_can_you_question() {
        let c = classifier();
        let r = c.classify("can you run the tests?");
        assert_eq!(r.mode, InputMode::Agent);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_please_create() {
        let c = classifier();
        let r = c.classify("please create a new rust module for authentication");
        assert_eq!(r.mode, InputMode::Agent);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_how_do_i() {
        let c = classifier();
        let r = c.classify("how do I add a dependency in Cargo.toml?");
        assert_eq!(r.mode, InputMode::Agent);
        assert!(r.confidence > 0.5);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn classify_empty() {
        let c = classifier();
        let r = c.classify("");
        assert_eq!(r.mode, InputMode::Agent);
        assert_eq!(r.confidence, 0.0);
    }

    #[test]
    fn classify_whitespace() {
        let c = classifier();
        let r = c.classify("   ");
        assert_eq!(r.mode, InputMode::Agent);
        assert_eq!(r.confidence, 0.0);
    }

    #[test]
    fn classify_single_word_command() {
        let c = classifier();
        let r = c.classify("ls");
        assert_eq!(r.mode, InputMode::Terminal);
    }

    #[test]
    fn classify_single_word_noncommand() {
        let c = classifier();
        let r = c.classify("hello");
        // Should default to Terminal with low confidence (no strong NL signals)
        assert!(r.confidence < 0.5);
    }

    // ── Denylist / Allowlist ─────────────────────────────────────────

    #[test]
    fn classify_denylist_override() {
        let mut c = classifier();
        c.add_to_denylist("terraform plan".to_string());
        let r = c.classify("terraform plan");
        assert_eq!(r.mode, InputMode::Terminal);
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn classify_allowlist_override() {
        let mut c = classifier();
        c.add_to_allowlist("list all files".to_string());
        let r = c.classify("list all files");
        assert_eq!(r.mode, InputMode::Agent);
        assert_eq!(r.confidence, 1.0);
    }

    // ── Bang prefix ──────────────────────────────────────────────────

    #[test]
    fn classify_bang_prefix() {
        let c = classifier();
        let r = c.classify("!ls -la");
        assert_eq!(r.mode, InputMode::Terminal);
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn classify_bang_prefix_nl() {
        let c = classifier();
        let r = c.classify("!what files are here");
        assert_eq!(r.mode, InputMode::Terminal);
        assert_eq!(r.confidence, 1.0);
    }

    // ── Threshold check ──────────────────────────────────────────────

    #[test]
    fn threshold_value() {
        let c = classifier();
        assert!((c.threshold() - 0.3).abs() < f32::EPSILON);
    }

    // ── Default trait ────────────────────────────────────────────────

    #[test]
    fn default_creates_valid_classifier() {
        let c = NlClassifier::default();
        let r = c.classify("git status");
        assert_eq!(r.mode, InputMode::Terminal);
    }

    // ── With lists constructor ───────────────────────────────────────

    #[test]
    fn with_lists_constructor() {
        let denylist: HashSet<String> = ["run tests".to_string()].into();
        let allowlist: HashSet<String> = HashSet::new();
        let c = NlClassifier::with_lists(denylist, allowlist);
        let r = c.classify("run tests");
        assert_eq!(r.mode, InputMode::Terminal);
        assert_eq!(r.confidence, 1.0);
    }

    // ── Redirect operators ───────────────────────────────────────────

    #[test]
    fn classify_redirect() {
        let c = classifier();
        let r = c.classify("echo hello > output.txt");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_and_operator() {
        let c = classifier();
        let r = c.classify("mkdir foo && cd foo");
        assert_eq!(r.mode, InputMode::Terminal);
        assert!(r.confidence > 0.5);
    }

    // ── Mixed signals ────────────────────────────────────────────────

    #[test]
    fn classify_terraform_plan() {
        let c = classifier();
        let r = c.classify("terraform plan");
        // terraform is a known command
        assert_eq!(r.mode, InputMode::Terminal);
    }

    #[test]
    fn classify_long_natural_sentence() {
        let c = classifier();
        let r = c.classify(
            "I need you to refactor the authentication module to use JWT tokens instead of sessions",
        );
        assert_eq!(r.mode, InputMode::Agent);
        assert!(r.confidence > 0.3);
    }
}
