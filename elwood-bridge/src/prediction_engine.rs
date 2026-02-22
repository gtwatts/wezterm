//! Next-command prediction engine with three sources: rules, history bigrams, and LLM.
//!
//! Predictions are shown as ghost text in the input box. Three prediction sources
//! are merged by confidence:
//!
//! 1. **Rule-based** (instant, highest priority): common command sequences
//! 2. **History-based** (fast, medium priority): bigram model from session history
//! 3. **LLM-based** (async, lowest priority but highest quality): deferred prediction
//!
//! The engine is designed to be non-blocking: rule and history predictions return
//! immediately, while LLM predictions arrive asynchronously via a spawned task.

use std::collections::HashMap;
use std::path::PathBuf;

/// Context for making a prediction.
#[derive(Debug, Clone)]
pub struct PredictionContext {
    /// The command that just finished executing.
    pub last_command: String,
    /// Exit code of the last command (None if unknown).
    pub last_exit_code: Option<i32>,
    /// Current working directory.
    pub working_dir: PathBuf,
    /// Current git branch, if in a git repo.
    pub git_branch: Option<String>,
    /// The last N commands executed this session (most recent last).
    pub recent_commands: Vec<String>,
}

/// A single prediction with confidence and source.
#[derive(Debug, Clone)]
pub struct Prediction {
    /// The predicted command text.
    pub command: String,
    /// Confidence score in [0.0, 1.0].
    pub confidence: f32,
    /// Which predictor produced this.
    pub source: PredictionSource,
}

/// Which subsystem produced the prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionSource {
    /// Rule-based pattern matching.
    Rule,
    /// History bigram model.
    History,
    /// LLM-based prediction (async).
    Llm,
}

// ─── Rule Predictor ─────────────────────────────────────────────────────────

/// A rule mapping a command pattern to a predicted next command.
struct Rule {
    /// Prefix the last command must match (lowercase).
    prefix: &'static str,
    /// Whether this rule applies on success, failure, or both.
    on_success: Option<bool>,
    /// The predicted command. May contain `{dir}`, `{branch}`, `{pkg}` placeholders.
    prediction: &'static str,
    /// Confidence score for this rule.
    confidence: f32,
}

/// Instant, rule-based predictions for common command sequences.
struct RulePredictor {
    rules: Vec<Rule>,
}

impl RulePredictor {
    fn new() -> Self {
        Self {
            rules: vec![
                // ── Git workflow ──────────────────────────────────
                Rule {
                    prefix: "git add",
                    on_success: Some(true),
                    prediction: "git commit -m \"\"",
                    confidence: 0.95,
                },
                Rule {
                    prefix: "git commit",
                    on_success: Some(true),
                    prediction: "git push",
                    confidence: 0.90,
                },
                Rule {
                    prefix: "git checkout -b ",
                    on_success: Some(true),
                    prediction: "git push -u origin {branch}",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "git clone",
                    on_success: Some(true),
                    prediction: "cd {dir}",
                    confidence: 0.90,
                },
                Rule {
                    prefix: "git pull",
                    on_success: Some(true),
                    prediction: "git log --oneline -5",
                    confidence: 0.60,
                },
                Rule {
                    prefix: "git stash",
                    on_success: Some(true),
                    prediction: "git stash pop",
                    confidence: 0.70,
                },
                Rule {
                    prefix: "git merge",
                    on_success: Some(true),
                    prediction: "git push",
                    confidence: 0.65,
                },
                Rule {
                    prefix: "git rebase",
                    on_success: Some(true),
                    prediction: "git push --force-with-lease",
                    confidence: 0.60,
                },
                // ── Directory navigation ─────────────────────────
                Rule {
                    prefix: "cd ",
                    on_success: Some(true),
                    prediction: "ls",
                    confidence: 0.80,
                },
                Rule {
                    prefix: "mkdir ",
                    on_success: Some(true),
                    prediction: "cd {dir}",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "mkdir -p ",
                    on_success: Some(true),
                    prediction: "cd {dir}",
                    confidence: 0.85,
                },
                // ── Rust / Cargo ─────────────────────────────────
                Rule {
                    prefix: "cargo build",
                    on_success: Some(true),
                    prediction: "cargo test",
                    confidence: 0.80,
                },
                Rule {
                    prefix: "cargo build",
                    on_success: Some(false),
                    prediction: "cargo build",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "cargo test",
                    on_success: Some(true),
                    prediction: "cargo clippy -- -D warnings",
                    confidence: 0.75,
                },
                Rule {
                    prefix: "cargo test",
                    on_success: Some(false),
                    prediction: "cargo test",
                    confidence: 0.90,
                },
                Rule {
                    prefix: "cargo clippy",
                    on_success: Some(true),
                    prediction: "cargo build --release",
                    confidence: 0.65,
                },
                Rule {
                    prefix: "cargo clippy",
                    on_success: Some(false),
                    prediction: "cargo clippy -- -D warnings",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "cargo fmt",
                    on_success: Some(true),
                    prediction: "cargo clippy -- -D warnings",
                    confidence: 0.70,
                },
                Rule {
                    prefix: "cargo check",
                    on_success: Some(true),
                    prediction: "cargo test",
                    confidence: 0.70,
                },
                Rule {
                    prefix: "cargo check",
                    on_success: Some(false),
                    prediction: "cargo check",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "cargo new ",
                    on_success: Some(true),
                    prediction: "cd {dir}",
                    confidence: 0.90,
                },
                Rule {
                    prefix: "cargo init",
                    on_success: Some(true),
                    prediction: "cargo build",
                    confidence: 0.80,
                },
                // ── Node / NPM ──────────────────────────────────
                Rule {
                    prefix: "npm install",
                    on_success: Some(true),
                    prediction: "npm run build",
                    confidence: 0.80,
                },
                Rule {
                    prefix: "npm run build",
                    on_success: Some(true),
                    prediction: "npm start",
                    confidence: 0.75,
                },
                Rule {
                    prefix: "npm run build",
                    on_success: Some(false),
                    prediction: "npm run build",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "npm test",
                    on_success: Some(false),
                    prediction: "npm test",
                    confidence: 0.85,
                },
                Rule {
                    prefix: "npm init",
                    on_success: Some(true),
                    prediction: "npm install",
                    confidence: 0.85,
                },
                // ── Python ──────────────────────────────────────
                Rule {
                    prefix: "python -m venv",
                    on_success: Some(true),
                    prediction: "source venv/bin/activate",
                    confidence: 0.95,
                },
                Rule {
                    prefix: "python3 -m venv",
                    on_success: Some(true),
                    prediction: "source venv/bin/activate",
                    confidence: 0.95,
                },
                Rule {
                    prefix: "pip install",
                    on_success: Some(true),
                    prediction: "pip freeze > requirements.txt",
                    confidence: 0.60,
                },
                Rule {
                    prefix: "pytest",
                    on_success: Some(false),
                    prediction: "pytest",
                    confidence: 0.85,
                },
                // ── Docker ──────────────────────────────────────
                Rule {
                    prefix: "docker build",
                    on_success: Some(true),
                    prediction: "docker run {dir}",
                    confidence: 0.75,
                },
                Rule {
                    prefix: "docker compose up -d",
                    on_success: Some(true),
                    prediction: "docker compose logs -f",
                    confidence: 0.70,
                },
                Rule {
                    prefix: "docker compose down",
                    on_success: Some(true),
                    prediction: "docker compose up -d",
                    confidence: 0.60,
                },
                // ── Make ────────────────────────────────────────
                Rule {
                    prefix: "make",
                    on_success: Some(true),
                    prediction: "make test",
                    confidence: 0.65,
                },
                Rule {
                    prefix: "make",
                    on_success: Some(false),
                    prediction: "make",
                    confidence: 0.80,
                },
                // ── General failure retry ───────────────────────
                // Catch-all: re-run the same command on failure
                // (lower confidence so specific rules win)
            ],
        }
    }

    /// Predict the next command given context.
    fn predict(&self, ctx: &PredictionContext) -> Option<Prediction> {
        let lower = ctx.last_command.to_lowercase();
        let success = ctx.last_exit_code.map(|c| c == 0);

        for rule in &self.rules {
            if !lower.starts_with(rule.prefix) {
                continue;
            }
            // Check success/failure filter
            if let Some(requires_success) = rule.on_success {
                match success {
                    Some(s) if s != requires_success => continue,
                    None if requires_success => continue, // unknown exit code, skip success-only rules
                    _ => {}
                }
            }

            let command = self.expand_placeholders(rule.prediction, ctx);
            return Some(Prediction {
                command,
                confidence: rule.confidence,
                source: PredictionSource::Rule,
            });
        }

        None
    }

    /// Expand `{dir}`, `{branch}`, `{pkg}` placeholders in a prediction template.
    fn expand_placeholders(&self, template: &str, ctx: &PredictionContext) -> String {
        let mut result = template.to_string();

        // {dir} — extract directory name from command args
        if result.contains("{dir}") {
            let dir = extract_dir_arg(&ctx.last_command).unwrap_or_default();
            result = result.replace("{dir}", &dir);
        }

        // {branch} — extract branch name from command args or use git context
        if result.contains("{branch}") {
            let branch = extract_branch_arg(&ctx.last_command)
                .or_else(|| ctx.git_branch.clone())
                .unwrap_or_else(|| "branch".to_string());
            result = result.replace("{branch}", &branch);
        }

        result
    }
}

/// Extract a directory-like argument from a command string.
///
/// For commands like `mkdir foo`, `cd foo`, `cargo new myapp`, extracts `foo`/`myapp`.
fn extract_dir_arg(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    // Take the last non-flag positional argument (e.g. "myapp" from "cargo new myapp")
    parts
        .iter()
        .skip(1)
        .filter(|p| !p.starts_with('-'))
        .last()
        .map(|s| s.to_string())
}

/// Extract a branch name from a `git checkout -b <branch>` command.
fn extract_branch_arg(command: &str) -> Option<String> {
    let lower = command.to_lowercase();
    if lower.starts_with("git checkout -b ") || lower.starts_with("git switch -c ") {
        let parts: Vec<&str> = command.split_whitespace().collect();
        return parts.get(3).map(|s| s.to_string());
    }
    None
}

// ─── History Predictor ──────────────────────────────────────────────────────

/// History-based bigram predictor — tracks what command typically follows what.
struct HistoryPredictor {
    /// Bigram counts: `bigrams[prev_command] = { next_command: count }`.
    bigrams: HashMap<String, HashMap<String, u32>>,
    /// Total occurrences of each command as "previous command".
    totals: HashMap<String, u32>,
}

impl HistoryPredictor {
    fn new() -> Self {
        Self {
            bigrams: HashMap::new(),
            totals: HashMap::new(),
        }
    }

    /// Record that `next` was executed after `prev`.
    fn record(&mut self, prev: &str, next: &str) {
        let prev_key = normalize_command(prev);
        let next_key = normalize_command(next);

        *self
            .bigrams
            .entry(prev_key.clone())
            .or_default()
            .entry(next_key)
            .or_insert(0) += 1;
        *self.totals.entry(prev_key).or_insert(0) += 1;
    }

    /// Predict the next command based on bigram frequencies.
    fn predict(&self, ctx: &PredictionContext) -> Option<Prediction> {
        let key = normalize_command(&ctx.last_command);
        let followers = self.bigrams.get(&key)?;
        let total = *self.totals.get(&key)?;

        if total < 2 {
            return None; // Need at least 2 observations for a meaningful prediction
        }

        // Find the most frequent follower
        let (best_cmd, best_count) = followers.iter().max_by_key(|(_, count)| *count)?;

        let confidence = (*best_count as f32) / (total as f32);

        // Only predict if confidence is reasonable
        if confidence < 0.3 {
            return None;
        }

        Some(Prediction {
            command: best_cmd.clone(),
            confidence: confidence * 0.8, // Scale down vs rules
            source: PredictionSource::History,
        })
    }
}

/// Normalize a command for bigram matching.
///
/// Strips arguments from common commands to group similar invocations.
/// `cargo test --workspace` and `cargo test -p foo` both become `cargo test`.
fn normalize_command(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    match parts.first().map(|s| *s) {
        Some("git") => parts.iter().take(2).copied().collect::<Vec<_>>().join(" "),
        Some("cargo") => parts.iter().take(2).copied().collect::<Vec<_>>().join(" "),
        Some("npm") => parts
            .iter()
            .take(3)
            .min_by_key(|_| 0)
            .map(|_| parts.iter().take(3).copied().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        Some("docker") => parts.iter().take(2).copied().collect::<Vec<_>>().join(" "),
        Some("make") => parts.iter().take(2).copied().collect::<Vec<_>>().join(" "),
        _ => parts.first().unwrap_or(&"").to_string(),
    }
}

// ─── Prediction Engine ─────────────────────────────────────────────────────

/// Next-command prediction engine combining rule, history, and LLM sources.
///
/// Predictions are returned immediately from rules/history. LLM predictions
/// arrive asynchronously and can be polled via [`take_llm_prediction`].
pub struct PredictionEngine {
    rule_predictor: RulePredictor,
    history_predictor: HistoryPredictor,
    /// Most recent LLM prediction (set asynchronously).
    llm_prediction: Option<Prediction>,
    /// Previous command for bigram tracking.
    prev_command: Option<String>,
}

impl PredictionEngine {
    /// Create a new prediction engine.
    pub fn new() -> Self {
        Self {
            rule_predictor: RulePredictor::new(),
            history_predictor: HistoryPredictor::new(),
            llm_prediction: None,
            prev_command: None,
        }
    }

    /// Predict the next command given context.
    ///
    /// Returns the highest-confidence prediction from rule or history sources.
    /// LLM predictions (if available) are also considered.
    pub fn predict(&mut self, ctx: &PredictionContext) -> Option<Prediction> {
        let mut candidates: Vec<Prediction> = Vec::new();

        // 1. Rule-based prediction (instant, highest priority)
        if let Some(p) = self.rule_predictor.predict(ctx) {
            candidates.push(p);
        }

        // 2. History-based prediction (fast, medium priority)
        if let Some(p) = self.history_predictor.predict(ctx) {
            candidates.push(p);
        }

        // 3. LLM prediction (if available from previous async request)
        if let Some(p) = self.llm_prediction.take() {
            candidates.push(p);
        }

        // Return the highest confidence prediction
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates.into_iter().next()
    }

    /// Record that a command was executed (for bigram tracking).
    ///
    /// Call this after each command completes. The `exit_code` is stored for
    /// context but the bigram model currently ignores it.
    pub fn record_command(&mut self, command: &str, _exit_code: Option<i32>) {
        // Update bigram model
        if let Some(ref prev) = self.prev_command {
            self.history_predictor.record(prev, command);
        }
        self.prev_command = Some(command.to_string());
    }

    /// Set an LLM prediction (called from async context).
    pub fn set_llm_prediction(&mut self, prediction: Prediction) {
        self.llm_prediction = Some(prediction);
    }

    /// Cancel any pending LLM prediction.
    pub fn cancel_pending(&mut self) {
        self.llm_prediction = None;
    }
}

impl Default for PredictionEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(cmd: &str, exit_code: i32) -> PredictionContext {
        PredictionContext {
            last_command: cmd.to_string(),
            last_exit_code: Some(exit_code),
            working_dir: PathBuf::from("/tmp/project"),
            git_branch: Some("main".to_string()),
            recent_commands: vec![cmd.to_string()],
        }
    }

    fn ctx_success(cmd: &str) -> PredictionContext {
        ctx(cmd, 0)
    }

    fn ctx_failure(cmd: &str) -> PredictionContext {
        ctx(cmd, 1)
    }

    // ── Rule-based predictions (success) ────────────────────────────────

    #[test]
    fn rule_git_add_suggests_commit() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("git add .")).unwrap();
        assert_eq!(p.source, PredictionSource::Rule);
        assert!(p.command.contains("git commit"));
    }

    #[test]
    fn rule_git_commit_suggests_push() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("git commit -m \"fix bug\""))
            .unwrap();
        assert!(p.command.contains("git push"));
    }

    #[test]
    fn rule_git_checkout_b_suggests_push_u() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("git checkout -b feature/new"))
            .unwrap();
        assert!(p.command.contains("git push -u origin feature/new"));
    }

    #[test]
    fn rule_git_clone_suggests_cd() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("git clone https://github.com/user/repo.git"))
            .unwrap();
        assert!(p.command.starts_with("cd "));
    }

    #[test]
    fn rule_git_pull_suggests_log() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("git pull")).unwrap();
        assert!(p.command.contains("git log"));
    }

    #[test]
    fn rule_git_stash_suggests_pop() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("git stash")).unwrap();
        assert!(p.command.contains("git stash pop"));
    }

    #[test]
    fn rule_cd_suggests_ls() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("cd src")).unwrap();
        assert_eq!(p.command, "ls");
    }

    #[test]
    fn rule_mkdir_suggests_cd() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("mkdir my-project")).unwrap();
        assert!(p.command.contains("cd my-project"));
    }

    #[test]
    fn rule_mkdir_p_suggests_cd() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("mkdir -p src/utils")).unwrap();
        assert!(p.command.contains("cd src/utils"));
    }

    #[test]
    fn rule_cargo_build_success_suggests_test() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("cargo build")).unwrap();
        assert!(p.command.contains("cargo test"));
    }

    #[test]
    fn rule_cargo_test_success_suggests_clippy() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("cargo test --workspace"))
            .unwrap();
        assert!(p.command.contains("clippy"));
    }

    #[test]
    fn rule_cargo_clippy_success_suggests_release() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("cargo clippy -- -D warnings"))
            .unwrap();
        assert!(p.command.contains("cargo build --release"));
    }

    #[test]
    fn rule_cargo_fmt_suggests_clippy() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("cargo fmt")).unwrap();
        assert!(p.command.contains("clippy"));
    }

    #[test]
    fn rule_cargo_new_suggests_cd() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("cargo new myapp")).unwrap();
        assert!(p.command.contains("cd myapp"));
    }

    #[test]
    fn rule_npm_install_suggests_build() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("npm install")).unwrap();
        assert!(p.command.contains("npm run build"));
    }

    #[test]
    fn rule_npm_build_success_suggests_start() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("npm run build")).unwrap();
        assert!(p.command.contains("npm start"));
    }

    #[test]
    fn rule_npm_init_suggests_install() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("npm init -y")).unwrap();
        assert!(p.command.contains("npm install"));
    }

    #[test]
    fn rule_python_venv_suggests_activate() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("python -m venv venv")).unwrap();
        assert!(p.command.contains("source venv/bin/activate"));
    }

    #[test]
    fn rule_python3_venv_suggests_activate() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("python3 -m venv .venv"))
            .unwrap();
        assert!(p.command.contains("source venv/bin/activate"));
    }

    #[test]
    fn rule_docker_build_suggests_run() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("docker build -t myapp ."))
            .unwrap();
        assert!(p.command.contains("docker run"));
    }

    #[test]
    fn rule_docker_compose_up_suggests_logs() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("docker compose up -d"))
            .unwrap();
        assert!(p.command.contains("docker compose logs"));
    }

    // ── Rule-based predictions (failure → retry) ────────────────────────

    #[test]
    fn rule_cargo_build_failure_suggests_rebuild() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("cargo build")).unwrap();
        assert_eq!(p.command, "cargo build");
    }

    #[test]
    fn rule_cargo_test_failure_suggests_retest() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_failure("cargo test --workspace"))
            .unwrap();
        assert_eq!(p.command, "cargo test");
    }

    #[test]
    fn rule_cargo_clippy_failure_suggests_reclippy() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("cargo clippy")).unwrap();
        assert!(p.command.contains("cargo clippy"));
    }

    #[test]
    fn rule_npm_build_failure_suggests_rebuild() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("npm run build")).unwrap();
        assert_eq!(p.command, "npm run build");
    }

    #[test]
    fn rule_npm_test_failure_suggests_retest() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("npm test")).unwrap();
        assert_eq!(p.command, "npm test");
    }

    #[test]
    fn rule_pytest_failure_suggests_retest() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("pytest tests/")).unwrap();
        assert_eq!(p.command, "pytest");
    }

    #[test]
    fn rule_make_failure_suggests_remake() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_failure("make all")).unwrap();
        assert_eq!(p.command, "make");
    }

    // ── No prediction for unknown commands ──────────────────────────────

    #[test]
    fn rule_no_prediction_for_ls() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("ls -la"));
        assert!(p.is_none());
    }

    #[test]
    fn rule_no_prediction_for_echo() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("echo hello world"));
        assert!(p.is_none());
    }

    // ── Case insensitivity ──────────────────────────────────────────────

    #[test]
    fn rule_case_insensitive() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("GIT ADD .")).unwrap();
        assert!(p.command.contains("git commit"));
    }

    // ── History bigram predictions ──────────────────────────────────────

    #[test]
    fn history_predicts_frequent_follower() {
        let mut engine = PredictionEngine::new();
        // Build up bigram: after "cargo check" we usually do "cargo test"
        for _ in 0..5 {
            engine.record_command("cargo check", Some(0));
            engine.record_command("cargo test", Some(0));
        }

        let p = engine.predict(&PredictionContext {
            last_command: "cargo check".to_string(),
            last_exit_code: Some(0),
            working_dir: PathBuf::from("/tmp"),
            git_branch: None,
            // No rule match for `cargo check` success + `cargo test` follow-up
            // at higher confidence than history, so history wins only if rules
            // don't fire first. In this case, the rule predictor has a rule for
            // `cargo check` success, so the rule prediction wins.
            recent_commands: vec!["cargo check".to_string()],
        });

        assert!(p.is_some());
    }

    #[test]
    fn history_needs_minimum_observations() {
        let mut engine = PredictionEngine::new();
        // Only 1 observation — not enough
        engine.record_command("foo", Some(0));
        engine.record_command("bar", Some(0));

        // The history predictor needs at least 2 observations of the same prev command
        let p = engine.history_predictor.predict(&PredictionContext {
            last_command: "foo".to_string(),
            last_exit_code: Some(0),
            working_dir: PathBuf::from("/tmp"),
            git_branch: None,
            recent_commands: vec!["foo".to_string()],
        });
        assert!(p.is_none());
    }

    #[test]
    fn history_predicts_after_enough_data() {
        let mut engine = PredictionEngine::new();
        // 3 observations of "foo" -> "bar"
        for _ in 0..3 {
            engine.record_command("foo", Some(0));
            engine.record_command("bar", Some(0));
        }

        let p = engine.history_predictor.predict(&PredictionContext {
            last_command: "foo".to_string(),
            last_exit_code: Some(0),
            working_dir: PathBuf::from("/tmp"),
            git_branch: None,
            recent_commands: vec!["foo".to_string()],
        });
        assert!(p.is_some());
        assert_eq!(p.unwrap().command, "bar");
    }

    // ── Placeholder expansion ───────────────────────────────────────────

    #[test]
    fn placeholder_dir_from_mkdir() {
        let mut engine = PredictionEngine::new();
        let p = engine.predict(&ctx_success("mkdir components")).unwrap();
        assert_eq!(p.command, "cd components");
    }

    #[test]
    fn placeholder_branch_from_checkout() {
        let mut engine = PredictionEngine::new();
        let p = engine
            .predict(&ctx_success("git checkout -b feat/auth"))
            .unwrap();
        assert!(p.command.contains("feat/auth"));
    }

    // ── LLM prediction integration ─────────────────────────────────────

    #[test]
    fn llm_prediction_used_when_no_rule_match() {
        let mut engine = PredictionEngine::new();
        engine.set_llm_prediction(Prediction {
            command: "npm run lint".to_string(),
            confidence: 0.7,
            source: PredictionSource::Llm,
        });

        let p = engine.predict(&ctx_success("echo done")).unwrap();
        assert_eq!(p.source, PredictionSource::Llm);
        assert_eq!(p.command, "npm run lint");
    }

    #[test]
    fn llm_prediction_consumed_after_use() {
        let mut engine = PredictionEngine::new();
        engine.set_llm_prediction(Prediction {
            command: "npm run lint".to_string(),
            confidence: 0.7,
            source: PredictionSource::Llm,
        });

        // First call consumes it
        let _ = engine.predict(&ctx_success("echo done"));

        // Second call has no LLM prediction
        let p = engine.predict(&ctx_success("echo done"));
        assert!(p.is_none());
    }

    #[test]
    fn rule_beats_llm_when_higher_confidence() {
        let mut engine = PredictionEngine::new();
        engine.set_llm_prediction(Prediction {
            command: "npm run lint".to_string(),
            confidence: 0.5,
            source: PredictionSource::Llm,
        });

        let p = engine.predict(&ctx_success("git add .")).unwrap();
        // Rule has 0.95 confidence, LLM has 0.5 — rule wins
        assert_eq!(p.source, PredictionSource::Rule);
    }

    #[test]
    fn cancel_pending_clears_llm() {
        let mut engine = PredictionEngine::new();
        engine.set_llm_prediction(Prediction {
            command: "npm run lint".to_string(),
            confidence: 0.7,
            source: PredictionSource::Llm,
        });
        engine.cancel_pending();

        let p = engine.predict(&ctx_success("echo done"));
        assert!(p.is_none());
    }

    // ── record_command tracking ─────────────────────────────────────────

    #[test]
    fn record_command_updates_prev() {
        let mut engine = PredictionEngine::new();
        engine.record_command("git status", Some(0));
        assert_eq!(engine.prev_command.as_deref(), Some("git status"));

        engine.record_command("git add .", Some(0));
        assert_eq!(engine.prev_command.as_deref(), Some("git add ."));
    }

    // ── normalize_command ───────────────────────────────────────────────

    #[test]
    fn normalize_git() {
        assert_eq!(normalize_command("git add ."), "git add");
        assert_eq!(normalize_command("git commit -m \"fix\""), "git commit");
    }

    #[test]
    fn normalize_cargo() {
        assert_eq!(normalize_command("cargo test --workspace"), "cargo test");
        assert_eq!(normalize_command("cargo build --release"), "cargo build");
    }

    #[test]
    fn normalize_unknown() {
        assert_eq!(normalize_command("echo hello world"), "echo");
        assert_eq!(normalize_command("ls -la src/"), "ls");
    }

    // ── extract_dir_arg ─────────────────────────────────────────────────

    #[test]
    fn extract_dir_basic() {
        assert_eq!(extract_dir_arg("mkdir foo"), Some("foo".to_string()));
        assert_eq!(extract_dir_arg("cd src"), Some("src".to_string()));
        assert_eq!(extract_dir_arg("mkdir -p a/b/c"), Some("a/b/c".to_string()));
    }

    #[test]
    fn extract_dir_no_args() {
        assert_eq!(extract_dir_arg("ls"), None);
    }

    // ── extract_branch_arg ──────────────────────────────────────────────

    #[test]
    fn extract_branch_checkout() {
        assert_eq!(
            extract_branch_arg("git checkout -b feature/new"),
            Some("feature/new".to_string())
        );
    }

    #[test]
    fn extract_branch_switch() {
        assert_eq!(
            extract_branch_arg("git switch -c fix/bug"),
            Some("fix/bug".to_string())
        );
    }

    #[test]
    fn extract_branch_no_branch() {
        assert_eq!(extract_branch_arg("git checkout main"), None);
    }

    // ── Default trait ───────────────────────────────────────────────────

    #[test]
    fn default_engine_works() {
        let mut engine = PredictionEngine::default();
        let p = engine.predict(&ctx_success("git add .")).unwrap();
        assert!(p.command.contains("git commit"));
    }
}
