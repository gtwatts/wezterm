//! Session export in HTML, JSON, and encrypted shareable formats.
//!
//! Provides three export targets beyond the existing markdown export:
//!
//! - **HTML**: Self-contained single file with embedded Tokyo Night CSS
//! - **JSON**: Machine-readable structured format (version 1)
//! - **Encrypted share**: Compressed + encrypted `.elwood-session` file
//!
//! The encrypted format uses flate2 (zlib) compression and XOR encryption
//! with SHA-256 key derivation for lightweight but reasonable protection.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::session_log::{EntryType, SessionEntry, SessionLog};

/// Magic bytes identifying an Elwood session file.
const MAGIC: &[u8; 4] = b"ELWD";

/// Current file format version.
const FORMAT_VERSION: u8 = 1;

/// Number of SHA-256 iterations for key derivation.
const KDF_ITERATIONS: u32 = 100_000;

/// Salt length in bytes.
const SALT_LEN: usize = 16;

// ─── HTML Export ────────────────────────────────────────────────────────────

/// Generate a self-contained HTML document from a session log.
///
/// The output uses Tokyo Night dark theme colours, inline CSS, and requires
/// no external resources.
pub fn export_html(session: &SessionLog) -> String {
    let mut html = String::with_capacity(8192);

    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str(&format!(
        "<title>Elwood Session — {}</title>\n",
        escape_html(&session.started_at)
    ));
    html.push_str("<style>\n");
    html.push_str(TOKYO_NIGHT_CSS);
    html.push_str("</style>\n");
    html.push_str("</head>\n<body>\n");

    // Header
    html.push_str("<div class=\"session-header\">\n");
    html.push_str("<h1>Elwood Session</h1>\n");
    html.push_str(&format!(
        "<p class=\"meta\">Started: {}</p>\n",
        escape_html(&session.started_at)
    ));
    html.push_str(&format!(
        "<p class=\"meta\">Working directory: <code>{}</code></p>\n",
        escape_html(&session.working_dir.display().to_string())
    ));
    html.push_str("</div>\n<hr>\n");

    // Entries
    html.push_str("<div class=\"entries\">\n");
    for entry in &session.entries {
        render_entry_html(&mut html, entry);
    }
    html.push_str("</div>\n");

    html.push_str("</body>\n</html>\n");
    html
}

/// Render a single entry as HTML.
fn render_entry_html(html: &mut String, entry: &SessionEntry) {
    match entry.entry_type {
        EntryType::User => {
            html.push_str("<div class=\"entry user\">\n");
            html.push_str(&format!(
                "<div class=\"entry-header\">You <span class=\"ts\">{}</span></div>\n",
                escape_html(&entry.timestamp)
            ));
            html.push_str(&format!(
                "<div class=\"entry-body\">{}</div>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
        EntryType::Agent => {
            html.push_str("<div class=\"entry agent\">\n");
            html.push_str(&format!(
                "<div class=\"entry-header\">Elwood <span class=\"ts\">{}</span></div>\n",
                escape_html(&entry.timestamp)
            ));
            html.push_str(&format!(
                "<div class=\"entry-body\">{}</div>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
        EntryType::Command => {
            html.push_str("<div class=\"entry command\">\n");
            html.push_str(&format!(
                "<div class=\"entry-header\">Command <span class=\"ts\">{}</span></div>\n",
                escape_html(&entry.timestamp)
            ));
            html.push_str(&format!(
                "<pre class=\"code-block\"><code>$ {}</code></pre>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
        EntryType::CommandOutput => {
            html.push_str("<div class=\"entry output\">\n");
            html.push_str(&format!(
                "<pre class=\"code-block output-block\"><code>{}</code></pre>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
        EntryType::Tool => {
            html.push_str("<div class=\"entry tool\">\n");
            html.push_str(&format!(
                "<div class=\"entry-header\">Tool <span class=\"ts\">{}</span></div>\n",
                escape_html(&entry.timestamp)
            ));
            html.push_str(&format!(
                "<div class=\"entry-body tool-body\">{}</div>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
        EntryType::System => {
            html.push_str("<div class=\"entry system\">\n");
            html.push_str(&format!(
                "<div class=\"entry-body system-body\">{}</div>\n",
                escape_html(&entry.content)
            ));
            html.push_str("</div>\n");
        }
    }
}

/// Escape HTML special characters.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Inline CSS using Tokyo Night colours.
const TOKYO_NIGHT_CSS: &str = r#"
:root {
  --bg: #1a1b26;
  --bg-dark: #16161e;
  --bg-highlight: #292e42;
  --fg: #c0caf5;
  --fg-dark: #a9b1d6;
  --comment: #565f89;
  --blue: #7aa2f7;
  --cyan: #7dcfff;
  --green: #9ece6a;
  --magenta: #bb9af7;
  --red: #f7768e;
  --yellow: #e0af68;
  --orange: #ff9e64;
  --border: #3b4261;
}

* { margin: 0; padding: 0; box-sizing: border-box; }

body {
  background: var(--bg);
  color: var(--fg);
  font-family: 'SF Mono', 'Fira Code', 'JetBrains Mono', 'Cascadia Code', monospace;
  font-size: 14px;
  line-height: 1.6;
  padding: 2rem;
  max-width: 900px;
  margin: 0 auto;
}

.session-header h1 {
  color: var(--blue);
  font-size: 1.5rem;
  margin-bottom: 0.5rem;
}

.meta { color: var(--comment); font-size: 0.85rem; }
.meta code { color: var(--cyan); background: var(--bg-highlight); padding: 2px 6px; border-radius: 3px; }

hr { border: none; border-top: 1px solid var(--border); margin: 1.5rem 0; }

.entries { display: flex; flex-direction: column; gap: 1rem; }

.entry {
  border-left: 3px solid var(--border);
  padding: 0.75rem 1rem;
  border-radius: 0 6px 6px 0;
  background: var(--bg-dark);
}

.entry.user { border-left-color: var(--green); }
.entry.agent { border-left-color: var(--blue); }
.entry.command { border-left-color: var(--yellow); }
.entry.output { border-left-color: var(--comment); padding: 0; }
.entry.tool { border-left-color: var(--magenta); }
.entry.system { border-left-color: var(--orange); }

.entry-header {
  font-weight: bold;
  margin-bottom: 0.4rem;
  color: var(--fg);
}

.entry.user .entry-header { color: var(--green); }
.entry.agent .entry-header { color: var(--blue); }
.entry.command .entry-header { color: var(--yellow); }
.entry.tool .entry-header { color: var(--magenta); }

.ts { font-weight: normal; color: var(--comment); font-size: 0.8rem; margin-left: 0.5rem; }

.entry-body { white-space: pre-wrap; word-break: break-word; }

.tool-body { color: var(--fg-dark); font-size: 0.9rem; }
.system-body { color: var(--orange); font-style: italic; font-size: 0.9rem; }

.code-block {
  background: var(--bg-highlight);
  padding: 0.75rem 1rem;
  border-radius: 4px;
  overflow-x: auto;
  font-size: 0.9rem;
}

.output-block { color: var(--fg-dark); border-radius: 0 0 6px 0; }

@media (max-width: 600px) {
  body { padding: 1rem; font-size: 13px; }
  .session-header h1 { font-size: 1.2rem; }
}
"#;

// ─── JSON Export ────────────────────────────────────────────────────────────

/// JSON representation of a session (version 1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionJson {
    /// Schema version.
    pub version: u32,
    /// Session start time (ISO 8601).
    pub started_at: String,
    /// All log entries.
    pub entries: Vec<EntryJson>,
    /// Session metadata.
    pub metadata: SessionMetadata,
}

/// JSON representation of a single log entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EntryJson {
    /// Entry type: "user", "agent", "command", "command_output", "tool", "system".
    #[serde(rename = "type")]
    pub entry_type: String,
    /// Entry content.
    pub content: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Session metadata included in JSON export.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMetadata {
    /// Working directory at session start.
    pub working_dir: String,
}

impl SessionLog {
    /// Export the session as a JSON string.
    pub fn to_json(&self) -> serde_json::Result<String> {
        let json = SessionJson {
            version: 1,
            started_at: self.started_at.clone(),
            entries: self
                .entries
                .iter()
                .map(|e| EntryJson {
                    entry_type: entry_type_to_str(&e.entry_type).to_string(),
                    content: e.content.clone(),
                    timestamp: e.timestamp.clone(),
                })
                .collect(),
            metadata: SessionMetadata {
                working_dir: self.working_dir.display().to_string(),
            },
        };
        serde_json::to_string_pretty(&json)
    }

    /// Import a session from a JSON string.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        let parsed: SessionJson = serde_json::from_str(json)?;
        Ok(SessionLog {
            started_at: parsed.started_at,
            working_dir: PathBuf::from(&parsed.metadata.working_dir),
            entries: parsed
                .entries
                .into_iter()
                .map(|e| SessionEntry {
                    timestamp: e.timestamp,
                    entry_type: str_to_entry_type(&e.entry_type),
                    content: e.content,
                })
                .collect(),
        })
    }
}

/// Convert an `EntryType` to its JSON string representation.
fn entry_type_to_str(t: &EntryType) -> &'static str {
    match t {
        EntryType::User => "user",
        EntryType::Agent => "agent",
        EntryType::Command => "command",
        EntryType::CommandOutput => "command_output",
        EntryType::Tool => "tool",
        EntryType::System => "system",
    }
}

/// Parse a JSON type string back to an `EntryType`.
fn str_to_entry_type(s: &str) -> EntryType {
    match s {
        "user" => EntryType::User,
        "agent" => EntryType::Agent,
        "command" => EntryType::Command,
        "command_output" => EntryType::CommandOutput,
        "tool" => EntryType::Tool,
        _ => EntryType::System,
    }
}

// ─── Encrypted Sharing ─────────────────────────────────────────────────────

/// Errors that can occur during session sharing operations.
#[derive(Debug)]
pub enum ShareError {
    /// I/O error reading or writing files.
    Io(std::io::Error),
    /// JSON serialization/deserialization error.
    Json(serde_json::Error),
    /// Compression/decompression error.
    Compression(String),
    /// Invalid file format (bad magic, version, etc.).
    InvalidFormat(String),
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::Io(e) => write!(f, "I/O error: {e}"),
            ShareError::Json(e) => write!(f, "JSON error: {e}"),
            ShareError::Compression(e) => write!(f, "compression error: {e}"),
            ShareError::InvalidFormat(e) => write!(f, "invalid format: {e}"),
        }
    }
}

impl std::error::Error for ShareError {}

impl From<std::io::Error> for ShareError {
    fn from(e: std::io::Error) -> Self {
        ShareError::Io(e)
    }
}

impl From<serde_json::Error> for ShareError {
    fn from(e: serde_json::Error) -> Self {
        ShareError::Json(e)
    }
}

/// Export a session as an encrypted `.elwood-session` file.
///
/// File layout:
/// ```text
/// [4 bytes] magic "ELWD"
/// [1 byte ] version
/// [16 bytes] random salt
/// [N bytes ] XOR-encrypted zlib-compressed JSON payload
/// ```
pub fn export_shared(session: &SessionLog, passphrase: &str) -> Result<Vec<u8>, ShareError> {
    let json = session.to_json()?;
    let compressed = compress(json.as_bytes())?;
    let salt = generate_salt();
    let key = derive_key(passphrase, &salt);
    let encrypted = xor_encrypt(&compressed, &key);

    let mut out = Vec::with_capacity(MAGIC.len() + 1 + SALT_LEN + encrypted.len());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&encrypted);

    Ok(out)
}

/// Import a session from an encrypted `.elwood-session` file.
pub fn import_shared(data: &[u8], passphrase: &str) -> Result<SessionLog, ShareError> {
    let header_len = MAGIC.len() + 1 + SALT_LEN;
    if data.len() < header_len {
        return Err(ShareError::InvalidFormat("file too small".into()));
    }
    if &data[..4] != MAGIC {
        return Err(ShareError::InvalidFormat(
            "not an Elwood session file".into(),
        ));
    }
    let version = data[4];
    if version != FORMAT_VERSION {
        return Err(ShareError::InvalidFormat(format!(
            "unsupported version {version}"
        )));
    }
    let salt = &data[5..5 + SALT_LEN];
    let encrypted = &data[header_len..];

    let key = derive_key(passphrase, salt);
    let compressed = xor_encrypt(encrypted, &key); // XOR is symmetric
    let json_bytes = decompress(&compressed)?;
    let json = String::from_utf8(json_bytes).map_err(|e| {
        ShareError::InvalidFormat(format!("invalid UTF-8 (wrong passphrase?): {e}"))
    })?;
    let session = SessionLog::from_json(&json)?;
    Ok(session)
}

/// Import a session from a file path, detecting format.
///
/// - `.elwood-session` files are treated as encrypted (passphrase required)
/// - `.json` files are imported as JSON
/// - `.md` files are not supported for import
pub fn import_from_file(path: &Path, passphrase: Option<&str>) -> Result<SessionLog, ShareError> {
    let data = std::fs::read(path)?;

    // Check for encrypted format by magic bytes
    if data.len() >= 4 && &data[..4] == MAGIC {
        let pass = passphrase.unwrap_or("");
        return import_shared(&data, pass);
    }

    // Try JSON
    let text = String::from_utf8(data)
        .map_err(|e| ShareError::InvalidFormat(format!("not UTF-8: {e}")))?;
    let session = SessionLog::from_json(&text)?;
    Ok(session)
}

/// Export a session to a file, choosing format by extension.
pub fn export_to_file(
    session: &SessionLog,
    path: &Path,
    passphrase: Option<&str>,
) -> Result<PathBuf, ShareError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let contents: Vec<u8> = match ext {
        "html" => export_html(session).into_bytes(),
        "json" => session.to_json()?.into_bytes(),
        "elwood-session" => {
            let pass = passphrase.unwrap_or("");
            export_shared(session, pass)?
        }
        _ => {
            // Default to markdown
            session.export_markdown().into_bytes()
        }
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &contents)?;
    Ok(path.to_path_buf())
}

// ─── Crypto helpers ─────────────────────────────────────────────────────────

/// Derive a 32-byte key from a passphrase and salt using iterated SHA-256.
fn derive_key(passphrase: &str, salt: &[u8]) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(passphrase.as_bytes());
    hash.update(salt);
    let mut result = hash.finalize().to_vec();

    for _ in 1..KDF_ITERATIONS {
        let mut h = Sha256::new();
        h.update(&result);
        h.update(salt);
        result = h.finalize().to_vec();
    }

    result
}

/// XOR-encrypt (or decrypt, since XOR is symmetric) data with a repeating key.
fn xor_encrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

/// Generate a random 16-byte salt using basic entropy sources.
fn generate_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    // Use std::time for entropy — not cryptographically strong but adequate for v1
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u128;
    let entropy = nanos.wrapping_mul(pid.wrapping_add(0x517cc1b727220a95));

    let mut hasher = Sha256::new();
    hasher.update(entropy.to_le_bytes());
    // Use thread ID hash for extra entropy (format is stable across editions)
    let thread_id = format!("{:?}", std::thread::current().id());
    hasher.update(thread_id.as_bytes());
    let hash = hasher.finalize();
    salt.copy_from_slice(&hash[..SALT_LEN]);
    salt
}

/// Compress data using flate2 zlib.
fn compress(data: &[u8]) -> Result<Vec<u8>, ShareError> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| ShareError::Compression(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| ShareError::Compression(e.to_string()))
}

/// Decompress zlib data.
fn decompress(data: &[u8]) -> Result<Vec<u8>, ShareError> {
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| ShareError::Compression(e.to_string()))?;
    Ok(buf)
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> SessionLog {
        let mut log = SessionLog::new(PathBuf::from("/home/user/project"));
        log.log_user("Fix the bug in main.rs");
        log.log_agent("I'll look at the file and fix the issue.");
        log.log_command("cargo test");
        log.log_command_output("test result: ok. 5 passed\n", "", Some(0));
        log.log_tool("ReadFile", "src/main.rs (200 lines)");
        log.log_system("Session started");
        log
    }

    // ── HTML tests ──

    #[test]
    fn test_html_export_is_self_contained() {
        let session = sample_session();
        let html = export_html(&session);

        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("</style>"));
        assert!(html.contains("</html>"));
        // No external links
        assert!(!html.contains("<link"));
        assert!(!html.contains("<script src"));
    }

    #[test]
    fn test_html_export_contains_entries() {
        let session = sample_session();
        let html = export_html(&session);

        assert!(html.contains("Fix the bug in main.rs"));
        assert!(html.contains("look at the file"));
        assert!(html.contains("$ cargo test"));
        assert!(html.contains("test result: ok"));
        assert!(html.contains("ReadFile"));
        assert!(html.contains("Session started"));
    }

    #[test]
    fn test_html_escapes_special_chars() {
        let mut log = SessionLog::new(PathBuf::from("/tmp"));
        log.log_user("Use <script>alert('xss')</script> safely");
        let html = export_html(&log);

        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_html_export_empty_session() {
        let log = SessionLog::new(PathBuf::from("/tmp"));
        let html = export_html(&log);
        assert!(html.contains("Elwood Session"));
        assert!(html.contains("</html>"));
    }

    // ── JSON tests ──

    #[test]
    fn test_json_roundtrip() {
        let session = sample_session();
        let json = session.to_json().unwrap();
        let restored = SessionLog::from_json(&json).unwrap();

        assert_eq!(restored.entries.len(), session.entries.len());
        assert_eq!(
            restored.working_dir.display().to_string(),
            session.working_dir.display().to_string()
        );
        for (orig, rest) in session.entries.iter().zip(restored.entries.iter()) {
            assert_eq!(orig.entry_type, rest.entry_type);
            assert_eq!(orig.content, rest.content);
            assert_eq!(orig.timestamp, rest.timestamp);
        }
    }

    #[test]
    fn test_json_structure() {
        let session = sample_session();
        let json = session.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["version"], 1);
        assert!(parsed["started_at"].is_string());
        assert!(parsed["entries"].is_array());
        assert!(parsed["metadata"]["working_dir"].is_string());

        let first = &parsed["entries"][0];
        assert_eq!(first["type"], "user");
        assert!(first["content"].is_string());
        assert!(first["timestamp"].is_string());
    }

    #[test]
    fn test_json_entry_types() {
        let session = sample_session();
        let json = session.to_json().unwrap();
        let parsed: SessionJson = serde_json::from_str(&json).unwrap();

        let types: Vec<&str> = parsed
            .entries
            .iter()
            .map(|e| e.entry_type.as_str())
            .collect();
        assert_eq!(
            types,
            vec![
                "user",
                "agent",
                "command",
                "command_output",
                "tool",
                "system"
            ]
        );
    }

    #[test]
    fn test_json_empty_session() {
        let log = SessionLog::new(PathBuf::from("/tmp"));
        let json = log.to_json().unwrap();
        let restored = SessionLog::from_json(&json).unwrap();
        assert!(restored.entries.is_empty());
    }

    // ── Encrypted sharing tests ──

    #[test]
    fn test_encrypted_roundtrip() {
        let session = sample_session();
        let passphrase = "test-password-123";

        let encrypted = export_shared(&session, passphrase).unwrap();
        let restored = import_shared(&encrypted, passphrase).unwrap();

        assert_eq!(restored.entries.len(), session.entries.len());
        for (orig, rest) in session.entries.iter().zip(restored.entries.iter()) {
            assert_eq!(orig.entry_type, rest.entry_type);
            assert_eq!(orig.content, rest.content);
        }
    }

    #[test]
    fn test_encrypted_file_header() {
        let session = sample_session();
        let encrypted = export_shared(&session, "pass").unwrap();

        assert_eq!(&encrypted[..4], b"ELWD");
        assert_eq!(encrypted[4], FORMAT_VERSION);
        assert!(encrypted.len() > 4 + 1 + SALT_LEN);
    }

    #[test]
    fn test_encrypted_wrong_passphrase() {
        let session = sample_session();
        let encrypted = export_shared(&session, "correct").unwrap();

        // Wrong passphrase should fail (either decompression or JSON parse)
        let result = import_shared(&encrypted, "wrong");
        assert!(result.is_err());
    }

    #[test]
    fn test_encrypted_invalid_magic() {
        // Data must be at least header_len (4 + 1 + 16 = 21) to reach magic check
        let mut bad_data = vec![0u8; 24];
        bad_data[..4].copy_from_slice(b"NOPE");
        let result = import_shared(&bad_data, "pass");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not an Elwood session file"));
    }

    #[test]
    fn test_encrypted_too_small() {
        let result = import_shared(b"ELW", "pass");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("file too small"));
    }

    #[test]
    fn test_encrypted_empty_passphrase() {
        let session = sample_session();
        let encrypted = export_shared(&session, "").unwrap();
        let restored = import_shared(&encrypted, "").unwrap();
        assert_eq!(restored.entries.len(), session.entries.len());
    }

    // ── Key derivation tests ──

    #[test]
    fn test_derive_key_deterministic() {
        let salt = [1u8; SALT_LEN];
        let k1 = derive_key("pass", &salt);
        let k2 = derive_key("pass", &salt);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_derive_key_different_passphrases() {
        let salt = [1u8; SALT_LEN];
        let k1 = derive_key("pass1", &salt);
        let k2 = derive_key("pass2", &salt);
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_derive_key_different_salts() {
        let s1 = [1u8; SALT_LEN];
        let s2 = [2u8; SALT_LEN];
        let k1 = derive_key("pass", &s1);
        let k2 = derive_key("pass", &s2);
        assert_ne!(k1, k2);
    }

    // ── XOR tests ──

    #[test]
    fn test_xor_symmetric() {
        let data = b"hello world";
        let key = b"secret";
        let encrypted = xor_encrypt(data, key);
        let decrypted = xor_encrypt(&encrypted, key);
        assert_eq!(decrypted, data);
    }

    // ── Compression tests ──

    #[test]
    fn test_compress_decompress_roundtrip() {
        let data = b"The quick brown fox jumps over the lazy dog. Repeated text for compression.";
        let compressed = compress(data).unwrap();
        let decompressed = decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    // ── File-based tests ──

    #[test]
    fn test_export_to_file_json() {
        let session = sample_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");

        let result = export_to_file(&session, &path, None).unwrap();
        assert_eq!(result, path);

        let contents = std::fs::read_to_string(&path).unwrap();
        let restored = SessionLog::from_json(&contents).unwrap();
        assert_eq!(restored.entries.len(), session.entries.len());
    }

    #[test]
    fn test_export_to_file_html() {
        let session = sample_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.html");

        export_to_file(&session, &path, None).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn test_import_from_file_json() {
        let session = sample_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");

        std::fs::write(&path, session.to_json().unwrap()).unwrap();
        let restored = import_from_file(&path, None).unwrap();
        assert_eq!(restored.entries.len(), session.entries.len());
    }

    #[test]
    fn test_import_from_file_encrypted() {
        let session = sample_session();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.elwood-session");

        let encrypted = export_shared(&session, "mypass").unwrap();
        std::fs::write(&path, &encrypted).unwrap();

        let restored = import_from_file(&path, Some("mypass")).unwrap();
        assert_eq!(restored.entries.len(), session.entries.len());
    }
}
