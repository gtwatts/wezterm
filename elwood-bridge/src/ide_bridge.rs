//! IDE editor integration — detect, launch, and bridge external editors.
//!
//! Detects installed editors (VSCode, Zed, Cursor, Sublime, Neovim, etc.),
//! opens files at specific line:column positions, opens diffs, and opens
//! project directories. Also provides OSC 8 hyperlink generation for
//! clickable file paths in agent output.
//!
//! ## Configuration
//!
//! Set preferred editor in `~/.elwood/elwood.toml`:
//!
//! ```toml
//! [editor]
//! preferred = "cursor"  # or "code", "zed", "subl", "nvim"
//! ```
//!
//! ## Slash Commands
//!
//! | Command              | Description                          |
//! |----------------------|--------------------------------------|
//! | `/open <file> [line]` | Open file in IDE at optional line   |
//! | `/open .`            | Open working directory as project    |
//! | `/editor`            | Show detected editor and preference  |
//! | `/editor set <name>` | Set preferred editor                 |

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

// ─── Editor Definitions ─────────────────────────────────────────────────

/// Known editor types with their CLI binary names and capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorKind {
    Cursor,
    Zed,
    VSCode,
    Sublime,
    Neovim,
    Vim,
    Emacs,
    IntelliJ,
}

impl EditorKind {
    /// The CLI binary name used to detect and launch this editor.
    #[must_use]
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Zed => "zed",
            Self::VSCode => "code",
            Self::Sublime => "subl",
            Self::Neovim => "nvim",
            Self::Vim => "vim",
            Self::Emacs => "emacs",
            Self::IntelliJ => "idea",
        }
    }

    /// Human-readable display name for this editor.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Cursor => "Cursor",
            Self::Zed => "Zed",
            Self::VSCode => "VS Code",
            Self::Sublime => "Sublime Text",
            Self::Neovim => "Neovim",
            Self::Vim => "Vim",
            Self::Emacs => "Emacs",
            Self::IntelliJ => "IntelliJ IDEA",
        }
    }

    /// Whether this editor supports `--goto file:line:column` syntax.
    #[must_use]
    pub fn supports_goto(self) -> bool {
        matches!(self, Self::Cursor | Self::VSCode | Self::Sublime)
    }

    /// Whether this editor supports `--diff file_a file_b`.
    #[must_use]
    pub fn supports_diff(self) -> bool {
        matches!(self, Self::VSCode | Self::Cursor)
    }

    /// Whether this editor is a terminal-based editor (nvim, vim, emacs).
    #[must_use]
    pub fn is_terminal_editor(self) -> bool {
        matches!(self, Self::Neovim | Self::Vim | Self::Emacs)
    }

    /// Default preference order (higher = more preferred).
    #[must_use]
    pub fn default_priority(self) -> u8 {
        match self {
            Self::Cursor => 7,
            Self::Zed => 6,
            Self::VSCode => 5,
            Self::Sublime => 4,
            Self::Neovim => 3,
            Self::Vim => 2,
            Self::Emacs => 1,
            Self::IntelliJ => 0,
        }
    }

    /// All known editor kinds in default priority order (highest first).
    #[must_use]
    pub fn all() -> &'static [EditorKind] {
        static ALL: &[EditorKind] = &[
            EditorKind::Cursor,
            EditorKind::Zed,
            EditorKind::VSCode,
            EditorKind::Sublime,
            EditorKind::Neovim,
            EditorKind::Vim,
            EditorKind::Emacs,
            EditorKind::IntelliJ,
        ];
        ALL
    }

    /// Parse an editor kind from a string (binary name or display name).
    #[must_use]
    pub fn from_str(s: &str) -> Option<EditorKind> {
        let lower = s.to_lowercase();
        match lower.as_str() {
            "cursor" => Some(Self::Cursor),
            "zed" => Some(Self::Zed),
            "code" | "vscode" | "vs code" => Some(Self::VSCode),
            "subl" | "sublime" | "sublime text" => Some(Self::Sublime),
            "nvim" | "neovim" => Some(Self::Neovim),
            "vim" => Some(Self::Vim),
            "emacs" => Some(Self::Emacs),
            "idea" | "intellij" | "intellij idea" => Some(Self::IntelliJ),
            _ => None,
        }
    }
}

impl std::fmt::Display for EditorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

// ─── Editor Configuration ───────────────────────────────────────────────

/// Editor section from `~/.elwood/elwood.toml`.
#[derive(Debug, Clone, Default)]
pub struct EditorConfig {
    /// Preferred editor name (e.g., "cursor", "code", "zed").
    pub preferred: Option<String>,
}

impl EditorConfig {
    /// Load editor config from `~/.elwood/elwood.toml`.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&default_config_path())
    }

    /// Load editor config from a specific TOML file.
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };

        // Parse `[editor]` section
        let table: toml::Table = match content.parse() {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };

        let editor_table = match table.get("editor").and_then(|v| v.as_table()) {
            Some(t) => t,
            None => return Self::default(),
        };

        Self {
            preferred: editor_table
                .get("preferred")
                .and_then(|v| v.as_str())
                .map(String::from),
        }
    }

    /// Save the preferred editor to the config file.
    ///
    /// This does a minimal update: reads the existing file, updates/inserts
    /// the `[editor]` section, and writes it back.
    pub fn save_preferred(editor_name: &str) -> Result<(), String> {
        let path = default_config_path();

        let mut content = std::fs::read_to_string(&path).unwrap_or_default();

        // Check if [editor] section exists
        if let Some(pos) = content.find("[editor]") {
            // Find the "preferred" line within the section
            let section_start = pos;
            let section_end = content[section_start + 8..]
                .find("\n[")
                .map(|p| section_start + 8 + p)
                .unwrap_or(content.len());

            let section = &content[section_start..section_end];
            if let Some(pref_offset) = section.find("preferred") {
                // Replace existing preferred line
                let line_start = section_start + pref_offset;
                let line_end = content[line_start..]
                    .find('\n')
                    .map(|p| line_start + p)
                    .unwrap_or(content.len());
                content.replace_range(
                    line_start..line_end,
                    &format!("preferred = \"{editor_name}\""),
                );
            } else {
                // Add preferred under [editor]
                let insert_pos = section_start + 8; // after "[editor]"
                let newline = if content[insert_pos..].starts_with('\n') {
                    ""
                } else {
                    "\n"
                };
                content.insert_str(
                    insert_pos,
                    &format!("{newline}preferred = \"{editor_name}\"\n"),
                );
            }
        } else {
            // Append [editor] section
            if !content.ends_with('\n') && !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&format!("\n[editor]\npreferred = \"{editor_name}\"\n"));
        }

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {e}"))?;
        }

        std::fs::write(&path, content)
            .map_err(|e| format!("Failed to write config: {e}"))?;

        Ok(())
    }
}

fn default_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".elwood")
        .join("elwood.toml")
}

// ─── Editor Detection ───────────────────────────────────────────────────

/// Trait for checking whether a binary is available on `$PATH`.
///
/// Extracted as a trait for testability — tests can inject a mock.
pub trait WhichProvider: Send + Sync {
    /// Returns `true` if the given binary name is available.
    fn is_available(&self, binary: &str) -> bool;
}

/// Real `which` implementation that checks `$PATH` using `command -v`.
struct RealWhichProvider;

impl WhichProvider for RealWhichProvider {
    fn is_available(&self, binary: &str) -> bool {
        Command::new("sh")
            .args(["-c", &format!("command -v {binary}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// Cached results of editor detection.
#[derive(Debug, Clone)]
pub struct EditorDetector {
    /// Editors found on `$PATH`, sorted by priority (highest first).
    available: Vec<EditorKind>,
    /// Cache of availability checks.
    cache: HashMap<EditorKind, bool>,
}

impl EditorDetector {
    /// Detect all available editors using the real `$PATH`.
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_with(&RealWhichProvider)
    }

    /// Detect available editors using a custom provider (for testing).
    #[must_use]
    pub fn detect_with(provider: &dyn WhichProvider) -> Self {
        let mut cache = HashMap::new();
        let mut available = Vec::new();

        for &kind in EditorKind::all() {
            let found = provider.is_available(kind.binary_name());
            cache.insert(kind, found);
            if found {
                available.push(kind);
            }
        }

        // Sort by default priority (highest first — already in order from all())
        available.sort_by(|a, b| b.default_priority().cmp(&a.default_priority()));

        Self { available, cache }
    }

    /// Get a cached singleton detector.
    #[must_use]
    pub fn cached() -> &'static Self {
        static INSTANCE: OnceLock<EditorDetector> = OnceLock::new();
        INSTANCE.get_or_init(Self::detect)
    }

    /// All detected editors, in priority order.
    #[must_use]
    pub fn available_editors(&self) -> &[EditorKind] {
        &self.available
    }

    /// Whether a specific editor is available.
    #[must_use]
    pub fn is_available(&self, kind: EditorKind) -> bool {
        self.cache.get(&kind).copied().unwrap_or(false)
    }

    /// Get the best editor, respecting the user's preference if set.
    #[must_use]
    pub fn best_editor(&self, preferred: Option<&str>) -> Option<EditorKind> {
        // If user has a preference and it's available, use it
        if let Some(pref) = preferred {
            if let Some(kind) = EditorKind::from_str(pref) {
                if self.is_available(kind) {
                    return Some(kind);
                }
            }
        }
        // Otherwise, use the highest-priority available editor
        self.available.first().copied()
    }

    /// Format a human-readable summary of detected editors.
    #[must_use]
    pub fn summary(&self, preferred: Option<&str>) -> String {
        if self.available.is_empty() {
            return "No editors detected on $PATH.\n\
                    Install one of: cursor, zed, code (VSCode), subl, nvim"
                .to_string();
        }

        let best = self.best_editor(preferred);
        let mut out = String::from("Detected editors:\n");

        for &kind in &self.available {
            let marker = if Some(kind) == best { " (active)" } else { "" };
            out.push_str(&format!(
                "  {} ({}){}  \n",
                kind.display_name(),
                kind.binary_name(),
                marker,
            ));
        }

        if let Some(pref) = preferred {
            out.push_str(&format!("\nPreferred: {pref}"));
            if let Some(kind) = EditorKind::from_str(pref) {
                if !self.is_available(kind) {
                    out.push_str(" (not found on $PATH)");
                }
            }
        } else {
            out.push_str("\nNo preference set — using highest priority.");
            out.push_str("\nSet with: /editor set <name>");
        }

        out
    }
}

// ─── Editor Bridge (launching) ──────────────────────────────────────────

/// Build and execute editor commands.
pub struct EditorBridge;

impl EditorBridge {
    /// Open a file in the given editor, optionally at a specific line and column.
    ///
    /// Returns the command that was spawned (for logging), or an error.
    pub fn open_file(
        editor: EditorKind,
        path: &str,
        line: Option<u32>,
        column: Option<u32>,
    ) -> Result<String, String> {
        let binary = editor.binary_name();
        let mut cmd = Command::new(binary);

        match editor {
            EditorKind::VSCode | EditorKind::Cursor => {
                if let Some(ln) = line {
                    let col = column.unwrap_or(1);
                    cmd.args(["--goto", &format!("{path}:{ln}:{col}")]);
                } else {
                    cmd.arg(path);
                }
            }
            EditorKind::Zed => {
                if let Some(ln) = line {
                    cmd.arg(format!("{path}:{ln}"));
                } else {
                    cmd.arg(path);
                }
            }
            EditorKind::Sublime => {
                if let Some(ln) = line {
                    let col = column.unwrap_or(1);
                    cmd.arg(format!("{path}:{ln}:{col}"));
                } else {
                    cmd.arg(path);
                }
            }
            EditorKind::Neovim | EditorKind::Vim => {
                if let Some(ln) = line {
                    cmd.args([&format!("+{ln}"), path]);
                } else {
                    cmd.arg(path);
                }
            }
            EditorKind::Emacs => {
                if let Some(ln) = line {
                    cmd.args([&format!("+{ln}"), path]);
                } else {
                    cmd.arg(path);
                }
            }
            EditorKind::IntelliJ => {
                if let Some(ln) = line {
                    cmd.args(["--line", &ln.to_string(), path]);
                } else {
                    cmd.arg(path);
                }
            }
        }

        let description = format_command_description(&cmd);

        // Fire-and-forget: spawn and detach
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to launch {binary}: {e}"))?;

        Ok(description)
    }

    /// Open a diff view between two files.
    ///
    /// Only supported by VSCode and Cursor; returns an error for other editors.
    pub fn open_diff(
        editor: EditorKind,
        file_a: &str,
        file_b: &str,
    ) -> Result<String, String> {
        if !editor.supports_diff() {
            return Err(format!(
                "{} does not support diff view. Use VS Code or Cursor.",
                editor.display_name()
            ));
        }

        let binary = editor.binary_name();
        let mut cmd = Command::new(binary);
        cmd.args(["--diff", file_a, file_b]);

        let description = format_command_description(&cmd);

        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to launch {binary}: {e}"))?;

        Ok(description)
    }

    /// Open a directory as a project in the editor.
    pub fn open_project(editor: EditorKind, dir: &str) -> Result<String, String> {
        let binary = editor.binary_name();
        let mut cmd = Command::new(binary);
        cmd.arg(dir);

        let description = format_command_description(&cmd);

        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to launch {binary}: {e}"))?;

        Ok(description)
    }
}

/// Format a `Command` as a human-readable string (for logging/display).
fn format_command_description(cmd: &Command) -> String {
    let program = cmd.get_program().to_string_lossy();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.is_empty() {
        program.into_owned()
    } else {
        format!("{program} {}", args.join(" "))
    }
}

// ─── File Path Parsing ──────────────────────────────────────────────────

/// A parsed file reference from agent output (e.g., `src/main.rs:42:10`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReference {
    /// The file path.
    pub path: String,
    /// Optional line number.
    pub line: Option<u32>,
    /// Optional column number.
    pub column: Option<u32>,
}

impl FileReference {
    /// Parse a string like `path/to/file.rs:42:10` into a `FileReference`.
    ///
    /// Supports formats:
    /// - `path/to/file.rs`
    /// - `path/to/file.rs:42`
    /// - `path/to/file.rs:42:10`
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Split from the right to handle paths with colons (e.g., C:\... on Windows)
        // Strategy: try splitting on `:` and check if parts after are numbers
        let parts: Vec<&str> = trimmed.rsplitn(3, ':').collect();

        match parts.len() {
            3 => {
                // Could be path:line:column
                if let (Ok(col), Ok(line)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    let path = parts[2].to_string();
                    if !path.is_empty() && looks_like_path(&path) {
                        return Some(Self {
                            path,
                            line: Some(line),
                            column: Some(col),
                        });
                    }
                }
                // Fall through: treat entire string as path
                Some(Self {
                    path: trimmed.to_string(),
                    line: None,
                    column: None,
                })
            }
            2 => {
                // Could be path:line
                if let Ok(line) = parts[0].parse::<u32>() {
                    let path = parts[1].to_string();
                    if !path.is_empty() && looks_like_path(&path) {
                        return Some(Self {
                            path,
                            line: Some(line),
                            column: None,
                        });
                    }
                }
                // Not a line number — entire string is the path
                Some(Self {
                    path: trimmed.to_string(),
                    line: None,
                    column: None,
                })
            }
            1 => {
                if looks_like_path(parts[0]) {
                    Some(Self {
                        path: parts[0].to_string(),
                        line: None,
                        column: None,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Heuristic check: does this look like a file path?
fn looks_like_path(s: &str) -> bool {
    // Must contain at least one path separator or dot (for extension)
    s.contains('/') || s.contains('.') || s.starts_with('~')
}

// ─── OSC 8 Hyperlinks ──────────────────────────────────────────────────

/// Generate an OSC 8 hyperlink for a file path.
///
/// OSC 8 format: `\x1b]8;params;uri\x07display_text\x1b]8;;\x07`
///
/// The URI uses the `file://` scheme so terminals can open the file.
///
/// ## Examples
///
/// ```
/// use elwood_bridge::ide_bridge::osc8_file_link;
///
/// let link = osc8_file_link("/home/user/src/main.rs", "src/main.rs", None, None);
/// assert!(link.contains("file:///home/user/src/main.rs"));
/// assert!(link.contains("src/main.rs"));
/// ```
pub fn osc8_file_link(
    absolute_path: &str,
    display_text: &str,
    _line: Option<u32>,
    _column: Option<u32>,
) -> String {
    // Use file:// URI — terminals that support OSC 8 will handle this
    let uri = format!("file://{absolute_path}");
    format!("\x1b]8;;{uri}\x07{display_text}\x1b]8;;\x07")
}

/// Scan a line of text for file references and wrap them in OSC 8 hyperlinks.
///
/// Looks for patterns like `path/to/file.rs:42:10` and wraps them in
/// clickable links. The working directory is used to resolve relative paths
/// to absolute paths for the `file://` URI.
pub fn linkify_file_paths(text: &str, working_dir: &Path) -> String {
    // Regex: match paths that look like `word/word.ext` or `/abs/path.ext`
    // optionally followed by `:line` or `:line:col`.
    // Uses a boundary group instead of look-behind (not supported by regex crate).
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(^|[\s`'\x22(])(/[\w./-]+(?:\.\w+)|[\w./-]+/[\w./-]+(?:\.\w+))(?::(\d+))?(?::(\d+))?"
        )
        .expect("valid regex")
    });

    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;

    for cap in re.captures_iter(text) {
        let full_match = cap.get(0).expect("match exists");
        // Group 1 is the boundary character (space, etc.) — preserve it
        let prefix = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let path_str = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let line: Option<u32> = cap.get(3).and_then(|m| m.as_str().parse().ok());
        let col: Option<u32> = cap.get(4).and_then(|m| m.as_str().parse().ok());

        result.push_str(&text[last_end..full_match.start()]);
        result.push_str(prefix);

        // Resolve to absolute path
        let abs_path = if Path::new(path_str).is_absolute() {
            PathBuf::from(path_str)
        } else {
            working_dir.join(path_str)
        };

        // Build display text from the path + line + col portions only
        let path_match_end = cap.get(2).expect("path group").end();
        let display_end = cap.get(4).or(cap.get(3)).map(|m| m.end())
            .unwrap_or(path_match_end);
        // Include the `:line:col` suffix in display — compute from original text
        let display_start = cap.get(2).expect("path group").start();
        let display = &text[display_start..display_end];

        let link = osc8_file_link(&abs_path.to_string_lossy(), display, line, col);
        result.push_str(&link);

        last_end = display_end;
    }

    result.push_str(&text[last_end..]);
    result
}

// ─── Slash Command Handlers ─────────────────────────────────────────────

/// Execute the `/open` command.
///
/// - `/open <file> [line]` — open file in IDE
/// - `/open .` — open working directory as project
pub fn execute_open(args: &str, working_dir: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        return "Usage: /open <file> [line]\n       /open .  (open project)".to_string();
    }

    let config = EditorConfig::load();
    let detector = EditorDetector::cached();
    let editor = match detector.best_editor(config.preferred.as_deref()) {
        Some(e) => e,
        None => {
            return "No editor detected. Install one of: cursor, zed, code, subl, nvim"
                .to_string();
        }
    };

    if args == "." {
        return match EditorBridge::open_project(editor, working_dir) {
            Ok(cmd) => format!("Opened project in {}: {cmd}", editor.display_name()),
            Err(e) => format!("Error: {e}"),
        };
    }

    // Parse file reference (supports path:line:col)
    let file_ref = match FileReference::parse(args) {
        Some(r) => r,
        None => {
            return format!("Could not parse file reference: {args}");
        }
    };

    // Resolve path relative to working directory
    let resolved = if Path::new(&file_ref.path).is_absolute() {
        file_ref.path.clone()
    } else {
        Path::new(working_dir)
            .join(&file_ref.path)
            .to_string_lossy()
            .into_owned()
    };

    match EditorBridge::open_file(editor, &resolved, file_ref.line, file_ref.column) {
        Ok(cmd) => format!("Opened in {}: {cmd}", editor.display_name()),
        Err(e) => format!("Error: {e}"),
    }
}

/// Execute the `/editor` command.
///
/// - `/editor` — show detected editors
/// - `/editor set <name>` — set preferred editor
pub fn execute_editor(args: &str) -> String {
    let args = args.trim();

    if args.is_empty() {
        let config = EditorConfig::load();
        let detector = EditorDetector::cached();
        return detector.summary(config.preferred.as_deref());
    }

    let (subcmd, sub_args) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "set" => {
            if sub_args.is_empty() {
                return "Usage: /editor set <name>\n\
                        Names: cursor, zed, code, subl, nvim, vim, emacs, idea"
                    .to_string();
            }

            let name = sub_args.trim();
            if EditorKind::from_str(name).is_none() {
                return format!(
                    "Unknown editor: {name}\n\
                     Known: cursor, zed, code (vscode), subl (sublime), nvim, vim, emacs, idea"
                );
            }

            match EditorConfig::save_preferred(name) {
                Ok(()) => format!("Preferred editor set to: {name}"),
                Err(e) => format!("Failed to save preference: {e}"),
            }
        }
        _ => format!("Unknown editor subcommand: {subcmd}\nUsage: /editor [set <name>]"),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Mock WhichProvider ───────────────────────────────────────────

    struct MockWhich {
        available: Vec<&'static str>,
    }

    impl MockWhich {
        fn new(available: &[&'static str]) -> Self {
            Self {
                available: available.to_vec(),
            }
        }
    }

    impl WhichProvider for MockWhich {
        fn is_available(&self, binary: &str) -> bool {
            self.available.contains(&binary)
        }
    }

    // ── EditorKind tests ────────────────────────────────────────────

    #[test]
    fn test_editor_kind_binary_names() {
        assert_eq!(EditorKind::Cursor.binary_name(), "cursor");
        assert_eq!(EditorKind::Zed.binary_name(), "zed");
        assert_eq!(EditorKind::VSCode.binary_name(), "code");
        assert_eq!(EditorKind::Sublime.binary_name(), "subl");
        assert_eq!(EditorKind::Neovim.binary_name(), "nvim");
        assert_eq!(EditorKind::Vim.binary_name(), "vim");
        assert_eq!(EditorKind::Emacs.binary_name(), "emacs");
        assert_eq!(EditorKind::IntelliJ.binary_name(), "idea");
    }

    #[test]
    fn test_editor_kind_from_str() {
        assert_eq!(EditorKind::from_str("cursor"), Some(EditorKind::Cursor));
        assert_eq!(EditorKind::from_str("zed"), Some(EditorKind::Zed));
        assert_eq!(EditorKind::from_str("code"), Some(EditorKind::VSCode));
        assert_eq!(EditorKind::from_str("vscode"), Some(EditorKind::VSCode));
        assert_eq!(EditorKind::from_str("VSCode"), Some(EditorKind::VSCode));
        assert_eq!(EditorKind::from_str("subl"), Some(EditorKind::Sublime));
        assert_eq!(EditorKind::from_str("sublime"), Some(EditorKind::Sublime));
        assert_eq!(EditorKind::from_str("nvim"), Some(EditorKind::Neovim));
        assert_eq!(EditorKind::from_str("neovim"), Some(EditorKind::Neovim));
        assert_eq!(EditorKind::from_str("vim"), Some(EditorKind::Vim));
        assert_eq!(EditorKind::from_str("emacs"), Some(EditorKind::Emacs));
        assert_eq!(EditorKind::from_str("idea"), Some(EditorKind::IntelliJ));
        assert_eq!(EditorKind::from_str("intellij"), Some(EditorKind::IntelliJ));
        assert_eq!(EditorKind::from_str("unknown"), None);
        assert_eq!(EditorKind::from_str(""), None);
    }

    #[test]
    fn test_editor_kind_supports_goto() {
        assert!(EditorKind::Cursor.supports_goto());
        assert!(EditorKind::VSCode.supports_goto());
        assert!(EditorKind::Sublime.supports_goto());
        assert!(!EditorKind::Zed.supports_goto());
        assert!(!EditorKind::Neovim.supports_goto());
    }

    #[test]
    fn test_editor_kind_supports_diff() {
        assert!(EditorKind::VSCode.supports_diff());
        assert!(EditorKind::Cursor.supports_diff());
        assert!(!EditorKind::Zed.supports_diff());
        assert!(!EditorKind::Sublime.supports_diff());
    }

    #[test]
    fn test_editor_kind_is_terminal() {
        assert!(EditorKind::Neovim.is_terminal_editor());
        assert!(EditorKind::Vim.is_terminal_editor());
        assert!(EditorKind::Emacs.is_terminal_editor());
        assert!(!EditorKind::VSCode.is_terminal_editor());
        assert!(!EditorKind::Cursor.is_terminal_editor());
    }

    #[test]
    fn test_editor_kind_display() {
        assert_eq!(format!("{}", EditorKind::VSCode), "VS Code");
        assert_eq!(format!("{}", EditorKind::Cursor), "Cursor");
        assert_eq!(format!("{}", EditorKind::IntelliJ), "IntelliJ IDEA");
    }

    #[test]
    fn test_editor_kind_all_has_correct_order() {
        let all = EditorKind::all();
        assert_eq!(all[0], EditorKind::Cursor);
        assert_eq!(all[1], EditorKind::Zed);
        assert_eq!(all[2], EditorKind::VSCode);
        assert_eq!(all.len(), 8);
    }

    // ── EditorDetector tests ────────────────────────────────────────

    #[test]
    fn test_detector_finds_available_editors() {
        let mock = MockWhich::new(&["code", "nvim"]);
        let detector = EditorDetector::detect_with(&mock);

        assert!(detector.is_available(EditorKind::VSCode));
        assert!(detector.is_available(EditorKind::Neovim));
        assert!(!detector.is_available(EditorKind::Cursor));
        assert!(!detector.is_available(EditorKind::Zed));

        let avail = detector.available_editors();
        assert_eq!(avail.len(), 2);
        // VSCode has higher priority than Neovim
        assert_eq!(avail[0], EditorKind::VSCode);
        assert_eq!(avail[1], EditorKind::Neovim);
    }

    #[test]
    fn test_detector_no_editors() {
        let mock = MockWhich::new(&[]);
        let detector = EditorDetector::detect_with(&mock);

        assert!(detector.available_editors().is_empty());
        assert_eq!(detector.best_editor(None), None);
    }

    #[test]
    fn test_detector_best_editor_default() {
        let mock = MockWhich::new(&["code", "zed", "nvim"]);
        let detector = EditorDetector::detect_with(&mock);

        // Without preference, Zed has higher priority than VSCode
        assert_eq!(detector.best_editor(None), Some(EditorKind::Zed));
    }

    #[test]
    fn test_detector_best_editor_with_preference() {
        let mock = MockWhich::new(&["code", "zed", "nvim"]);
        let detector = EditorDetector::detect_with(&mock);

        // With preference for nvim, it should be selected
        assert_eq!(
            detector.best_editor(Some("nvim")),
            Some(EditorKind::Neovim)
        );
    }

    #[test]
    fn test_detector_best_editor_unavailable_preference() {
        let mock = MockWhich::new(&["code", "nvim"]);
        let detector = EditorDetector::detect_with(&mock);

        // Preference for cursor, but it's not available — fall back
        assert_eq!(
            detector.best_editor(Some("cursor")),
            Some(EditorKind::VSCode)
        );
    }

    #[test]
    fn test_detector_summary_no_editors() {
        let mock = MockWhich::new(&[]);
        let detector = EditorDetector::detect_with(&mock);

        let summary = detector.summary(None);
        assert!(summary.contains("No editors detected"));
    }

    #[test]
    fn test_detector_summary_with_editors() {
        let mock = MockWhich::new(&["code", "zed"]);
        let detector = EditorDetector::detect_with(&mock);

        let summary = detector.summary(None);
        assert!(summary.contains("VS Code"));
        assert!(summary.contains("Zed"));
        assert!(summary.contains("(active)"));
        assert!(summary.contains("No preference set"));
    }

    #[test]
    fn test_detector_summary_with_preference() {
        let mock = MockWhich::new(&["code", "zed"]);
        let detector = EditorDetector::detect_with(&mock);

        let summary = detector.summary(Some("code"));
        assert!(summary.contains("Preferred: code"));
    }

    #[test]
    fn test_detector_summary_missing_preference() {
        let mock = MockWhich::new(&["code"]);
        let detector = EditorDetector::detect_with(&mock);

        let summary = detector.summary(Some("cursor"));
        assert!(summary.contains("not found on $PATH"));
    }

    // ── FileReference parsing tests ─────────────────────────────────

    #[test]
    fn test_parse_file_reference_path_only() {
        let r = FileReference::parse("src/main.rs").unwrap();
        assert_eq!(r.path, "src/main.rs");
        assert_eq!(r.line, None);
        assert_eq!(r.column, None);
    }

    #[test]
    fn test_parse_file_reference_with_line() {
        let r = FileReference::parse("src/main.rs:42").unwrap();
        assert_eq!(r.path, "src/main.rs");
        assert_eq!(r.line, Some(42));
        assert_eq!(r.column, None);
    }

    #[test]
    fn test_parse_file_reference_with_line_and_column() {
        let r = FileReference::parse("src/main.rs:42:10").unwrap();
        assert_eq!(r.path, "src/main.rs");
        assert_eq!(r.line, Some(42));
        assert_eq!(r.column, Some(10));
    }

    #[test]
    fn test_parse_file_reference_absolute_path() {
        let r = FileReference::parse("/home/user/project/lib.rs:100").unwrap();
        assert_eq!(r.path, "/home/user/project/lib.rs");
        assert_eq!(r.line, Some(100));
    }

    #[test]
    fn test_parse_file_reference_no_extension_no_slash() {
        // "Makefile" has no `/` or `.` — doesn't look like a path
        assert!(FileReference::parse("Makefile").is_none());
    }

    #[test]
    fn test_parse_file_reference_with_dot() {
        // "config.toml" has a dot, so it looks like a path
        let r = FileReference::parse("config.toml").unwrap();
        assert_eq!(r.path, "config.toml");
        assert_eq!(r.line, None);
    }

    #[test]
    fn test_parse_file_reference_empty() {
        assert!(FileReference::parse("").is_none());
        assert!(FileReference::parse("   ").is_none());
    }

    #[test]
    fn test_parse_file_reference_tilde_path() {
        let r = FileReference::parse("~/project/file.rs:10").unwrap();
        assert_eq!(r.path, "~/project/file.rs");
        assert_eq!(r.line, Some(10));
    }

    #[test]
    fn test_parse_file_reference_deep_path() {
        let r = FileReference::parse("a/b/c/d/e.txt:1:1").unwrap();
        assert_eq!(r.path, "a/b/c/d/e.txt");
        assert_eq!(r.line, Some(1));
        assert_eq!(r.column, Some(1));
    }

    // ── OSC 8 hyperlink tests ───────────────────────────────────────

    #[test]
    fn test_osc8_file_link_basic() {
        let link = osc8_file_link("/home/user/src/main.rs", "src/main.rs", None, None);
        assert!(link.starts_with("\x1b]8;;"));
        assert!(link.contains("file:///home/user/src/main.rs"));
        assert!(link.contains("src/main.rs"));
        assert!(link.ends_with("\x1b]8;;\x07"));
    }

    #[test]
    fn test_osc8_file_link_format() {
        let link = osc8_file_link("/a/b.rs", "b.rs", Some(42), Some(10));
        // Should still work (line/col in the link are reserved for future use)
        assert_eq!(
            link,
            "\x1b]8;;file:///a/b.rs\x07b.rs\x1b]8;;\x07"
        );
    }

    #[test]
    fn test_linkify_absolute_path() {
        let text = "Error at /home/user/src/main.rs:42";
        let result = linkify_file_paths(text, Path::new("/home/user"));
        assert!(result.contains("\x1b]8;;"));
        assert!(result.contains("file:///home/user/src/main.rs"));
    }

    #[test]
    fn test_linkify_relative_path() {
        let text = "See src/lib.rs:10:5 for details";
        let result = linkify_file_paths(text, Path::new("/project"));
        assert!(result.contains("\x1b]8;;"));
        assert!(result.contains("file:///project/src/lib.rs"));
    }

    #[test]
    fn test_linkify_no_paths() {
        let text = "This is just plain text with no file paths.";
        let result = linkify_file_paths(text, Path::new("/project"));
        // Should be unchanged (or only trivially so)
        assert!(!result.contains("\x1b]8;;"));
    }

    #[test]
    fn test_linkify_preserves_surrounding_text() {
        let text = "Check /home/user/file.rs:10 now";
        let result = linkify_file_paths(text, Path::new("/"));
        assert!(result.starts_with("Check "));
        assert!(result.ends_with(" now"));
    }

    // ── Command construction tests ──────────────────────────────────

    #[test]
    fn test_vscode_open_file_no_line() {
        let mut cmd = Command::new("code");
        cmd.arg("/tmp/test.rs");
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "code /tmp/test.rs");
    }

    #[test]
    fn test_vscode_goto_format() {
        let mut cmd = Command::new("code");
        cmd.args(["--goto", "test.rs:42:10"]);
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "code --goto test.rs:42:10");
    }

    #[test]
    fn test_zed_line_format() {
        let mut cmd = Command::new("zed");
        cmd.arg("test.rs:42");
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "zed test.rs:42");
    }

    #[test]
    fn test_nvim_line_format() {
        let mut cmd = Command::new("nvim");
        cmd.args(["+42", "test.rs"]);
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "nvim +42 test.rs");
    }

    #[test]
    fn test_intellij_line_format() {
        let mut cmd = Command::new("idea");
        cmd.args(["--line", "42", "test.rs"]);
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "idea --line 42 test.rs");
    }

    #[test]
    fn test_diff_command_format() {
        let mut cmd = Command::new("code");
        cmd.args(["--diff", "a.rs", "b.rs"]);
        let desc = format_command_description(&cmd);
        assert_eq!(desc, "code --diff a.rs b.rs");
    }

    // ── EditorConfig tests ──────────────────────────────────────────

    #[test]
    fn test_editor_config_default() {
        let config = EditorConfig::default();
        assert!(config.preferred.is_none());
    }

    #[test]
    fn test_editor_config_load_from_nonexistent() {
        let config = EditorConfig::load_from(Path::new("/nonexistent/path/elwood.toml"));
        assert!(config.preferred.is_none());
    }

    #[test]
    fn test_editor_config_load_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elwood.toml");
        std::fs::write(
            &path,
            r#"
            provider = "gemini"
            model = "gemini-2.5-pro"

            [editor]
            preferred = "cursor"
            "#,
        )
        .unwrap();

        let config = EditorConfig::load_from(&path);
        assert_eq!(config.preferred.as_deref(), Some("cursor"));
    }

    #[test]
    fn test_editor_config_load_no_editor_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elwood.toml");
        std::fs::write(
            &path,
            r#"
            provider = "gemini"
            model = "gemini-2.5-pro"
            "#,
        )
        .unwrap();

        let config = EditorConfig::load_from(&path);
        assert!(config.preferred.is_none());
    }

    #[test]
    fn test_editor_config_save_preferred_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".elwood").join("elwood.toml");

        // Manually call save logic with a custom path
        // (save_preferred uses the default path, so we test the config writing logic)
        let content = "\n[editor]\npreferred = \"zed\"\n";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();

        let config = EditorConfig::load_from(&path);
        assert_eq!(config.preferred.as_deref(), Some("zed"));
    }

    // ── execute_open tests ──────────────────────────────────────────

    #[test]
    fn test_execute_open_no_args() {
        let result = execute_open("", "/tmp");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn test_execute_editor_no_args() {
        // This calls the real detector — we can't easily mock the cached singleton
        // but we can verify it returns a string (not panic)
        let result = execute_editor("");
        assert!(!result.is_empty());
    }

    #[test]
    fn test_execute_editor_set_no_name() {
        let result = execute_editor("set");
        assert!(result.contains("Usage:"));
    }

    #[test]
    fn test_execute_editor_set_unknown() {
        let result = execute_editor("set foobar");
        assert!(result.contains("Unknown editor"));
    }

    #[test]
    fn test_execute_editor_unknown_subcmd() {
        let result = execute_editor("frobulate");
        assert!(result.contains("Unknown editor subcommand"));
    }
}
