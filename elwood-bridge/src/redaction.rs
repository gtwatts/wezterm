//! Secret detection and redaction for agent context.
//!
//! Auto-detects API keys, tokens, passwords, JWTs, connection strings, and other
//! secrets before they reach the LLM. Secrets are replaced with `[REDACTED:type]`
//! placeholders.
//!
//! ## Usage
//!
//! ```
//! use elwood_bridge::redaction::Redactor;
//!
//! let redactor = Redactor::new();
//! let result = redactor.redact("my key is AKIAIOSFODNN7EXAMPLE");
//! assert_eq!(result.redacted, "my key is [REDACTED:aws_key]");
//! assert_eq!(result.secrets_found.len(), 1);
//! ```
//!
//! ## Configuration
//!
//! Additional patterns can be loaded from `~/.elwood/redaction.toml`:
//!
//! ```toml
//! [redaction]
//! enabled = true
//! notify = true
//!
//! [[patterns]]
//! name = "internal_token"
//! pattern = "itk_[A-Za-z0-9]{32}"
//! ```

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Global toggle for redaction (defaults to enabled).
static REDACTION_ENABLED: AtomicBool = AtomicBool::new(true);

/// A single secret pattern definition.
#[derive(Debug, Clone)]
struct PatternDef {
    /// Human-readable name (e.g., "aws_key").
    name: &'static str,
    /// Regex pattern string.
    pattern: &'static str,
    /// Which capture group contains the secret (0 = entire match).
    group: usize,
}

/// Built-in secret patterns.
const BUILTIN_PATTERNS: &[PatternDef] = &[
    PatternDef {
        name: "aws_key",
        pattern: r"AKIA[0-9A-Z]{16}",
        group: 0,
    },
    PatternDef {
        name: "github_token",
        pattern: r"gh[ps]_[A-Za-z0-9_]{36,}",
        group: 0,
    },
    PatternDef {
        name: "generic_api_key",
        pattern: r#"(?i)(api[_-]?key|apikey|secret[_-]?key|access[_-]?token)\s*[:=]\s*['"]?([A-Za-z0-9_\-]{20,})['"]?"#,
        group: 2,
    },
    PatternDef {
        name: "bearer_token",
        pattern: r"Bearer\s+[A-Za-z0-9_\-\.]{20,}",
        group: 0,
    },
    PatternDef {
        name: "private_key",
        pattern: r"-----BEGIN (RSA |EC |DSA )?PRIVATE KEY-----",
        group: 0,
    },
    PatternDef {
        name: "jwt",
        pattern: r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
        group: 0,
    },
    PatternDef {
        name: "connection_string",
        pattern: r"(?i)(postgres|mysql|mongodb|redis)://[^\s]+@[^\s]+",
        group: 0,
    },
    PatternDef {
        name: "env_secret",
        pattern: r#"(?i)(DB_PASSWORD|DATABASE_URL|SECRET_KEY|PRIVATE_KEY|AUTH_TOKEN|ENCRYPTION_KEY|AWS_SECRET_ACCESS_KEY)\s*=\s*['"]?([^\s'"]{8,})['"]?"#,
        group: 0,
    },
];

/// A compiled secret pattern ready for matching.
#[derive(Debug, Clone)]
struct CompiledPattern {
    name: String,
    regex: Regex,
    /// Which capture group holds the secret value.
    group: usize,
}

/// A detected secret match within text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    /// Name of the pattern that matched (e.g., "aws_key").
    pub pattern_name: String,
    /// Byte range of the secret in the original text.
    pub byte_range: Range<usize>,
    /// The placeholder that replaced the secret.
    pub placeholder: String,
}

/// Result of redacting text.
#[derive(Debug, Clone)]
pub struct RedactedText {
    /// The redacted (safe) version of the text.
    pub redacted: String,
    /// The original text (kept in memory only, never logged).
    pub original: String,
    /// All secrets found during redaction.
    pub secrets_found: Vec<SecretMatch>,
}

impl RedactedText {
    /// Returns true if any secrets were found and redacted.
    #[must_use]
    pub fn has_secrets(&self) -> bool {
        !self.secrets_found.is_empty()
    }

    /// Returns the count of redacted secrets.
    #[must_use]
    pub fn secret_count(&self) -> usize {
        self.secrets_found.len()
    }
}

/// Custom pattern loaded from configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPattern {
    /// Pattern name for the placeholder.
    pub name: String,
    /// Regex pattern string.
    pub pattern: String,
}

/// Redaction configuration from `~/.elwood/redaction.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedactionConfig {
    /// Top-level redaction settings.
    #[serde(default)]
    pub redaction: RedactionSettings,
    /// Custom patterns.
    #[serde(default)]
    pub patterns: Vec<CustomPattern>,
}

/// Settings within the `[redaction]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionSettings {
    /// Whether redaction is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether to show toast notifications when secrets are redacted.
    #[serde(default = "default_true")]
    pub notify: bool,
}

impl Default for RedactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            notify: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Secret detector and redactor.
///
/// Holds compiled regex patterns (both built-in and custom) and provides
/// methods to scan text for secrets and replace them with placeholders.
#[derive(Debug)]
pub struct Redactor {
    patterns: Vec<CompiledPattern>,
    notify: bool,
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor {
    /// Create a new redactor with built-in patterns only.
    #[must_use]
    pub fn new() -> Self {
        let patterns = BUILTIN_PATTERNS
            .iter()
            .filter_map(|def| {
                Regex::new(def.pattern).ok().map(|regex| CompiledPattern {
                    name: def.name.to_string(),
                    regex,
                    group: def.group,
                })
            })
            .collect();
        Self {
            patterns,
            notify: true,
        }
    }

    /// Create a redactor with both built-in and custom patterns from config.
    #[must_use]
    pub fn with_config(config: &RedactionConfig) -> Self {
        let mut redactor = Self::new();
        redactor.notify = config.redaction.notify;

        for custom in &config.patterns {
            if let Ok(regex) = Regex::new(&custom.pattern) {
                redactor.patterns.push(CompiledPattern {
                    name: custom.name.clone(),
                    regex,
                    group: 0,
                });
            }
        }

        redactor
    }

    /// Load a redactor from the default config path (`~/.elwood/redaction.toml`).
    #[must_use]
    pub fn from_default_config() -> Self {
        let config_path = dirs_next::home_dir()
            .unwrap_or_default()
            .join(".elwood")
            .join("redaction.toml");
        Self::from_config_path(&config_path)
    }

    /// Load a redactor from a specific config file path.
    #[must_use]
    pub fn from_config_path(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let config: RedactionConfig =
                    toml::from_str(&content).unwrap_or_default();
                Self::with_config(&config)
            }
            Err(_) => Self::new(),
        }
    }

    /// Scan text and redact all detected secrets.
    ///
    /// Returns a [`RedactedText`] with the safe version and metadata about
    /// what was redacted. If redaction is globally disabled, returns the
    /// original text unchanged.
    #[must_use]
    pub fn redact(&self, text: &str) -> RedactedText {
        if !is_enabled() {
            return RedactedText {
                redacted: text.to_string(),
                original: text.to_string(),
                secrets_found: Vec::new(),
            };
        }

        // Collect all matches with their byte ranges.
        let mut matches: Vec<(Range<usize>, String)> = Vec::new();

        for pattern in &self.patterns {
            for caps in pattern.regex.captures_iter(text) {
                let m = if pattern.group > 0 {
                    // Use specific capture group if defined.
                    match caps.get(pattern.group) {
                        Some(m) => m,
                        None => continue,
                    }
                } else {
                    caps.get(0).expect("group 0 always exists")
                };

                let range = m.start()..m.end();
                let placeholder = format!("[REDACTED:{}]", pattern.name);

                // Skip if this range overlaps with an already-found match.
                let overlaps = matches
                    .iter()
                    .any(|(r, _)| r.start < range.end && range.start < r.end);
                if !overlaps {
                    matches.push((range, placeholder));
                }
            }
        }

        // Sort matches by start position (descending) for safe replacement.
        matches.sort_by(|a, b| b.0.start.cmp(&a.0.start));

        let mut redacted = text.to_string();
        let mut secrets_found = Vec::new();

        for (range, placeholder) in matches {
            secrets_found.push(SecretMatch {
                pattern_name: placeholder
                    .trim_start_matches("[REDACTED:")
                    .trim_end_matches(']')
                    .to_string(),
                byte_range: range.clone(),
                placeholder: placeholder.clone(),
            });
            redacted.replace_range(range, &placeholder);
        }

        // Reverse so they're in document order.
        secrets_found.reverse();

        RedactedText {
            redacted,
            original: text.to_string(),
            secrets_found,
        }
    }

    /// Restore original text from a redacted version using stored secret matches.
    ///
    /// This is used for display purposes only -- never send unredacted text to LLM.
    #[must_use]
    pub fn unredact(redacted: &str, original: &str, secrets: &[SecretMatch]) -> String {
        if secrets.is_empty() {
            return redacted.to_string();
        }
        let mut result = redacted.to_string();
        for secret in secrets.iter().rev() {
            let original_value =
                &original[secret.byte_range.clone()];
            result = result.replacen(&secret.placeholder, original_value, 1);
        }
        result
    }

    /// Returns a list of all active pattern names (for `/redact patterns`).
    #[must_use]
    pub fn pattern_names(&self) -> Vec<String> {
        self.patterns.iter().map(|p| p.name.clone()).collect()
    }

    /// Returns true if toast notifications are enabled.
    #[must_use]
    pub fn should_notify(&self) -> bool {
        self.notify
    }
}

/// Enable or disable redaction globally.
pub fn set_enabled(enabled: bool) {
    REDACTION_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Check if redaction is currently enabled.
#[must_use]
pub fn is_enabled() -> bool {
    REDACTION_ENABLED.load(Ordering::Relaxed)
}

/// Execute a `/redact` slash command.
///
/// Returns a formatted message string for display.
pub fn execute_redact_command(args: &str, redactor: &Redactor) -> String {
    let (subcmd, sub_args) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd.trim(), rest.trim()),
        None => (args.trim(), ""),
    };

    match subcmd {
        "on" => {
            set_enabled(true);
            "Secret redaction enabled.".to_string()
        }
        "off" => {
            set_enabled(false);
            "Secret redaction disabled. Secrets will be sent to the LLM.".to_string()
        }
        "test" => {
            if sub_args.is_empty() {
                return "Usage: /redact test <text>\n\nTest what secrets would be redacted."
                    .to_string();
            }
            // Temporarily force-enable for the test.
            let was_enabled = is_enabled();
            set_enabled(true);
            let result = redactor.redact(sub_args);
            set_enabled(was_enabled);

            if result.secrets_found.is_empty() {
                "No secrets detected in the provided text.".to_string()
            } else {
                let mut msg = format!(
                    "Found {} secret(s):\n\n",
                    result.secrets_found.len()
                );
                for s in &result.secrets_found {
                    msg.push_str(&format!(
                        "  [{:>20}] bytes {}..{}\n",
                        s.pattern_name, s.byte_range.start, s.byte_range.end
                    ));
                }
                msg.push_str(&format!("\nRedacted output:\n  {}", result.redacted));
                msg
            }
        }
        "patterns" => {
            let names = redactor.pattern_names();
            let enabled_str = if is_enabled() {
                "enabled"
            } else {
                "disabled"
            };
            let mut msg = format!(
                "Redaction is {}. {} active pattern(s):\n\n",
                enabled_str,
                names.len()
            );
            for name in &names {
                msg.push_str(&format!("  - {name}\n"));
            }
            msg
        }
        "" => {
            let status = if is_enabled() { "ON" } else { "OFF" };
            format!(
                "Secret redaction is {status}.\n\n\
                 Usage:\n\
                 \x20 /redact on        Enable redaction\n\
                 \x20 /redact off       Disable redaction\n\
                 \x20 /redact test <t>  Test redaction on text\n\
                 \x20 /redact patterns  List active patterns"
            )
        }
        other => {
            format!(
                "Unknown subcommand: {other}\nUsage: /redact [on|off|test|patterns]"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_redactor() -> Redactor {
        // Reset state for each test.
        set_enabled(true);
        Redactor::new()
    }

    // ── AWS Keys ────────────────────────────────────────────────────────

    #[test]
    fn test_aws_key_detection() {
        let r = test_redactor();
        let result = r.redact("my key is AKIAIOSFODNN7EXAMPLE");
        assert_eq!(result.redacted, "my key is [REDACTED:aws_key]");
        assert_eq!(result.secrets_found.len(), 1);
        assert_eq!(result.secrets_found[0].pattern_name, "aws_key");
    }

    #[test]
    fn test_aws_key_in_config_file() {
        let r = test_redactor();
        let input = "aws_access_key_id = AKIAI44QH8DHBEXAMPLE";
        let result = r.redact(input);
        assert!(result.redacted.contains("[REDACTED:aws_key]"));
        assert!(!result.redacted.contains("AKIAI44QH8DHBEXAMPLE"));
    }

    // ── GitHub Tokens ───────────────────────────────────────────────────

    #[test]
    fn test_github_pat_detection() {
        let r = test_redactor();
        let token = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkl";
        let result = r.redact(&format!("GITHUB_TOKEN={token}"));
        assert!(result.redacted.contains("[REDACTED:github_token]"));
        assert!(!result.redacted.contains(token));
    }

    #[test]
    fn test_github_secret_token() {
        let r = test_redactor();
        let token = "ghs_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkl";
        let result = r.redact(token);
        assert!(result.redacted.contains("[REDACTED:github_token]"));
    }

    // ── Generic API Keys ────────────────────────────────────────────────

    #[test]
    fn test_generic_api_key_equals() {
        let r = test_redactor();
        let result = r.redact("api_key=sk_live_ABCDEFGHIJ1234567890");
        assert!(result.redacted.contains("[REDACTED:generic_api_key]"));
        assert!(!result.redacted.contains("sk_live_ABCDEFGHIJ1234567890"));
    }

    #[test]
    fn test_generic_api_key_colon() {
        let r = test_redactor();
        let result = r.redact("apikey: abcdef1234567890ABCDEFGH");
        assert!(result.redacted.contains("[REDACTED:generic_api_key]"));
    }

    #[test]
    fn test_generic_secret_key_quoted() {
        let r = test_redactor();
        let result = r.redact(r#"secret_key = "very_secret_key_value_1234""#);
        assert!(result.redacted.contains("[REDACTED:generic_api_key]"));
    }

    #[test]
    fn test_access_token_detection() {
        let r = test_redactor();
        let result = r.redact("access_token=mytoken12345678901234567890");
        assert!(result.redacted.contains("[REDACTED:generic_api_key]"));
    }

    // ── Bearer Tokens ───────────────────────────────────────────────────

    #[test]
    fn test_bearer_token_detection() {
        let r = test_redactor();
        let result = r.redact("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc.def");
        // This could match bearer_token or jwt depending on ordering; both are valid.
        assert!(
            result.redacted.contains("[REDACTED:bearer_token]")
                || result.redacted.contains("[REDACTED:jwt]")
        );
        assert!(!result.redacted.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
    }

    #[test]
    fn test_bearer_non_jwt() {
        let r = test_redactor();
        let result = r.redact("Bearer abcdefghij1234567890xyzw");
        assert!(result.redacted.contains("[REDACTED:bearer_token]"));
    }

    // ── Private Keys ────────────────────────────────────────────────────

    #[test]
    fn test_rsa_private_key() {
        let r = test_redactor();
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAK...";
        let result = r.redact(input);
        assert!(result.redacted.contains("[REDACTED:private_key]"));
    }

    #[test]
    fn test_ec_private_key() {
        let r = test_redactor();
        let input = "-----BEGIN EC PRIVATE KEY-----";
        let result = r.redact(input);
        assert!(result.redacted.contains("[REDACTED:private_key]"));
    }

    #[test]
    fn test_generic_private_key() {
        let r = test_redactor();
        let input = "-----BEGIN PRIVATE KEY-----";
        let result = r.redact(input);
        assert!(result.redacted.contains("[REDACTED:private_key]"));
    }

    // ── JWT Tokens ──────────────────────────────────────────────────────

    #[test]
    fn test_jwt_detection() {
        let r = test_redactor();
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = r.redact(&format!("token: {jwt}"));
        assert!(result.redacted.contains("[REDACTED:jwt]"));
        assert!(!result.redacted.contains(jwt));
    }

    // ── Connection Strings ──────────────────────────────────────────────

    #[test]
    fn test_postgres_connection_string() {
        let r = test_redactor();
        let result = r.redact("postgres://user:password@localhost:5432/mydb");
        assert!(result.redacted.contains("[REDACTED:connection_string]"));
        assert!(!result.redacted.contains("password"));
    }

    #[test]
    fn test_mysql_connection_string() {
        let r = test_redactor();
        let result = r.redact("mysql://admin:secret@db.host.com/production");
        assert!(result.redacted.contains("[REDACTED:connection_string]"));
    }

    #[test]
    fn test_mongodb_connection_string() {
        let r = test_redactor();
        let result = r.redact("mongodb://root:pass123@cluster.mongodb.net/app");
        assert!(result.redacted.contains("[REDACTED:connection_string]"));
    }

    #[test]
    fn test_redis_connection_string() {
        let r = test_redactor();
        let result = r.redact("redis://default:mypassword@redis.example.com:6379");
        assert!(result.redacted.contains("[REDACTED:connection_string]"));
    }

    // ── .env Secret Values ──────────────────────────────────────────────

    #[test]
    fn test_env_db_password() {
        let r = test_redactor();
        let result = r.redact("DB_PASSWORD=super_secret_123");
        assert!(result.redacted.contains("[REDACTED:env_secret]"));
        assert!(!result.redacted.contains("super_secret_123"));
    }

    #[test]
    fn test_env_secret_key() {
        let r = test_redactor();
        let result = r.redact(r#"SECRET_KEY="my-app-secret-key-value""#);
        // May match generic_api_key (since "secret_key" triggers it) or env_secret.
        assert!(
            result.redacted.contains("[REDACTED:env_secret]")
                || result.redacted.contains("[REDACTED:generic_api_key]"),
            "expected secret to be redacted, got: {}",
            result.redacted
        );
        assert!(!result.redacted.contains("my-app-secret-key-value"));
    }

    #[test]
    fn test_env_aws_secret() {
        let r = test_redactor();
        let result = r.redact("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
        assert!(result.redacted.contains("[REDACTED:env_secret]"));
    }

    // ── False Positive Avoidance ────────────────────────────────────────

    #[test]
    fn test_git_sha_not_redacted() {
        let r = test_redactor();
        // 40-char hex git SHA should NOT be redacted.
        let sha = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let result = r.redact(&format!("commit {sha}"));
        assert_eq!(result.secrets_found.len(), 0);
        assert!(result.redacted.contains(sha));
    }

    #[test]
    fn test_short_hex_not_redacted() {
        let r = test_redactor();
        // Short hex strings (like abbreviated SHAs) should not match.
        let result = r.redact("commit d7a412c");
        assert_eq!(result.secrets_found.len(), 0);
    }

    #[test]
    fn test_normal_text_not_redacted() {
        let r = test_redactor();
        let result = r.redact("Hello, this is a normal message with no secrets.");
        assert_eq!(result.secrets_found.len(), 0);
        assert_eq!(result.redacted, "Hello, this is a normal message with no secrets.");
    }

    #[test]
    fn test_uuid_not_redacted() {
        let r = test_redactor();
        let result = r.redact("id: 550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(result.secrets_found.len(), 0);
    }

    #[test]
    fn test_base64_image_not_redacted() {
        let r = test_redactor();
        // A short base64 chunk should not match.
        let result = r.redact("data:image/png;base64,iVBOR");
        assert_eq!(result.secrets_found.len(), 0);
    }

    // ── Multiple Secrets ────────────────────────────────────────────────

    #[test]
    fn test_multiple_secrets_in_one_text() {
        let r = test_redactor();
        let input = "key=AKIAIOSFODNN7EXAMPLE\nDB_PASSWORD=hunter2_secret";
        let result = r.redact(input);
        assert!(result.secrets_found.len() >= 2);
        assert!(!result.redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!result.redacted.contains("hunter2_secret"));
    }

    // ── Redact / Unredact Round-Trip ────────────────────────────────────

    #[test]
    fn test_redact_unredact_roundtrip() {
        let r = test_redactor();
        let original = "deploy with AKIAIOSFODNN7EXAMPLE to prod";
        let result = r.redact(original);
        assert!(result.has_secrets());
        let restored = Redactor::unredact(&result.redacted, &result.original, &result.secrets_found);
        assert_eq!(restored, original);
    }

    #[test]
    fn test_unredact_no_secrets() {
        let result = Redactor::unredact("no secrets here", "no secrets here", &[]);
        assert_eq!(result, "no secrets here");
    }

    // ── Custom Patterns (Config) ────────────────────────────────────────

    #[test]
    fn test_custom_pattern_from_config() {
        let config = RedactionConfig {
            redaction: RedactionSettings::default(),
            patterns: vec![CustomPattern {
                name: "internal_token".to_string(),
                pattern: r"itk_[A-Za-z0-9]{32}".to_string(),
            }],
        };
        let r = Redactor::with_config(&config);
        let result = r.redact("token: itk_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef");
        assert!(result.redacted.contains("[REDACTED:internal_token]"));
    }

    #[test]
    fn test_custom_pattern_invalid_regex_skipped() {
        let config = RedactionConfig {
            redaction: RedactionSettings::default(),
            patterns: vec![CustomPattern {
                name: "bad_pattern".to_string(),
                pattern: r"[invalid".to_string(), // invalid regex
            }],
        };
        let r = Redactor::with_config(&config);
        // Should still work with built-in patterns.
        assert!(!r.pattern_names().contains(&"bad_pattern".to_string()));
    }

    // ── Config Loading ──────────────────────────────────────────────────

    #[test]
    fn test_config_deserialize() {
        let toml_str = r#"
            [redaction]
            enabled = true
            notify = false

            [[patterns]]
            name = "stripe_key"
            pattern = "sk_live_[A-Za-z0-9]{24}"
        "#;
        let config: RedactionConfig = toml::from_str(toml_str).unwrap();
        assert!(config.redaction.enabled);
        assert!(!config.redaction.notify);
        assert_eq!(config.patterns.len(), 1);
        assert_eq!(config.patterns[0].name, "stripe_key");
    }

    #[test]
    fn test_config_deserialize_empty() {
        let config: RedactionConfig = toml::from_str("").unwrap();
        assert!(config.redaction.enabled);
        assert!(config.patterns.is_empty());
    }

    #[test]
    fn test_from_nonexistent_config_path() {
        let r = Redactor::from_config_path(Path::new("/nonexistent/redaction.toml"));
        // Should fall back to defaults.
        assert!(!r.pattern_names().is_empty());
    }

    // ── Global Toggle ───────────────────────────────────────────────────

    #[test]
    fn test_redaction_disabled() {
        let r = test_redactor();
        set_enabled(false);
        let result = r.redact("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(result.secrets_found.len(), 0);
        assert!(result.redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        // Reset.
        set_enabled(true);
    }

    #[test]
    fn test_toggle_on_off() {
        set_enabled(true);
        assert!(is_enabled());
        set_enabled(false);
        assert!(!is_enabled());
        set_enabled(true);
        assert!(is_enabled());
    }

    // ── Slash Command Execution ─────────────────────────────────────────

    #[test]
    fn test_redact_command_on() {
        set_enabled(false);
        let r = test_redactor();
        let result = execute_redact_command("on", &r);
        assert!(result.contains("enabled"));
        assert!(is_enabled());
    }

    #[test]
    fn test_redact_command_off() {
        set_enabled(true);
        let r = test_redactor();
        let result = execute_redact_command("off", &r);
        assert!(result.contains("disabled"));
        assert!(!is_enabled());
        set_enabled(true);
    }

    #[test]
    fn test_redact_command_test_with_secret() {
        let r = test_redactor();
        let result = execute_redact_command("test AKIAIOSFODNN7EXAMPLE", &r);
        assert!(result.contains("1 secret(s)"));
        assert!(result.contains("[REDACTED:aws_key]"));
    }

    #[test]
    fn test_redact_command_test_no_secret() {
        let r = test_redactor();
        let result = execute_redact_command("test hello world", &r);
        assert!(result.contains("No secrets detected"));
    }

    #[test]
    fn test_redact_command_test_no_args() {
        let r = test_redactor();
        let result = execute_redact_command("test", &r);
        assert!(result.contains("Usage"));
    }

    #[test]
    fn test_redact_command_patterns() {
        let r = test_redactor();
        let result = execute_redact_command("patterns", &r);
        assert!(result.contains("aws_key"));
        assert!(result.contains("github_token"));
        assert!(result.contains("jwt"));
    }

    #[test]
    fn test_redact_command_no_args() {
        let r = test_redactor();
        let result = execute_redact_command("", &r);
        assert!(result.contains("/redact on"));
        assert!(result.contains("/redact off"));
    }

    #[test]
    fn test_redact_command_unknown() {
        let r = test_redactor();
        let result = execute_redact_command("foobar", &r);
        assert!(result.contains("Unknown subcommand"));
    }

    // ── Helper Methods ──────────────────────────────────────────────────

    #[test]
    fn test_has_secrets() {
        let r = test_redactor();
        let result = r.redact("no secrets");
        assert!(!result.has_secrets());

        let result = r.redact("AKIAIOSFODNN7EXAMPLE");
        assert!(result.has_secrets());
    }

    #[test]
    fn test_secret_count() {
        let r = test_redactor();
        let result = r.redact("no secrets");
        assert_eq!(result.secret_count(), 0);

        let result = r.redact("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(result.secret_count(), 1);
    }

    #[test]
    fn test_pattern_names_includes_builtins() {
        let r = test_redactor();
        let names = r.pattern_names();
        assert!(names.contains(&"aws_key".to_string()));
        assert!(names.contains(&"github_token".to_string()));
        assert!(names.contains(&"generic_api_key".to_string()));
        assert!(names.contains(&"bearer_token".to_string()));
        assert!(names.contains(&"private_key".to_string()));
        assert!(names.contains(&"jwt".to_string()));
        assert!(names.contains(&"connection_string".to_string()));
        assert!(names.contains(&"env_secret".to_string()));
    }

    #[test]
    fn test_should_notify_default() {
        let r = test_redactor();
        assert!(r.should_notify());
    }

    #[test]
    fn test_should_notify_config_override() {
        let config = RedactionConfig {
            redaction: RedactionSettings {
                enabled: true,
                notify: false,
            },
            patterns: Vec::new(),
        };
        let r = Redactor::with_config(&config);
        assert!(!r.should_notify());
    }
}
