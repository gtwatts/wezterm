//! Interactive notebook system for runbooks with embedded commands.
//!
//! Notebooks are markdown documents with executable code cells that run directly
//! in the terminal. They are stored as `.elwood-nb` files (TOML format) in
//! `~/.elwood/notebooks/`.
//!
//! ## Storage Format
//!
//! ```toml
//! [notebook]
//! title = "Deploy Checklist"
//! description = "Production deployment steps"
//! author = "gordon"
//! created_at = "2026-02-22T17:00:00Z"
//! tags = ["deploy", "production"]
//!
//! [[cells]]
//! type = "markdown"
//! content = "# Deploy to Production\nRun these steps in order."
//!
//! [[cells]]
//! type = "code"
//! language = "bash"
//! source = "cargo test --workspace"
//! ```
//!
//! ## Viewer Navigation
//!
//! - `j`/`k` — scroll cells
//! - `Enter` or `r` — run current code cell
//! - `R` — run all cells from current downward
//! - `q` — close notebook viewer

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Data Structures ─────────────────────────────────────────────────────

/// Execution state of a code cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    /// Cell has not been executed.
    Idle,
    /// Cell is currently executing.
    Running,
    /// Cell completed successfully.
    Completed(i32),
    /// Cell execution failed.
    Failed(i32),
}

impl Default for CellState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Output captured from a code cell execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellOutput {
    /// Standard output text.
    pub stdout: String,
    /// Standard error text.
    pub stderr: String,
    /// Process exit code.
    pub exit_code: i32,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
}

impl CellOutput {
    /// Execution duration as a [`Duration`].
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }
}

/// A single cell in a notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotebookCell {
    /// Rich text cell rendered as markdown.
    Markdown {
        /// Markdown content.
        content: String,
    },
    /// Executable code cell.
    Code {
        /// Language identifier (e.g. "bash", "python").
        language: String,
        /// Source code to execute.
        source: String,
        /// Last execution output (if any).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<CellOutput>,
        /// Current execution state.
        #[serde(default)]
        state: CellState,
    },
}

impl NotebookCell {
    /// Create a new markdown cell.
    #[must_use]
    pub fn markdown(content: impl Into<String>) -> Self {
        Self::Markdown {
            content: content.into(),
        }
    }

    /// Create a new code cell.
    #[must_use]
    pub fn code(language: impl Into<String>, source: impl Into<String>) -> Self {
        Self::Code {
            language: language.into(),
            source: source.into(),
            output: None,
            state: CellState::Idle,
        }
    }

    /// Whether this is a code cell.
    #[must_use]
    pub fn is_code(&self) -> bool {
        matches!(self, Self::Code { .. })
    }

    /// Whether this is a markdown cell.
    #[must_use]
    pub fn is_markdown(&self) -> bool {
        matches!(self, Self::Markdown { .. })
    }

    /// Render this cell to ANSI-styled terminal output.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Markdown { content } => crate::markdown::render_markdown(content),
            Self::Code {
                language,
                source,
                output,
                state,
            } => render_code_cell(language, source, output.as_ref(), state),
        }
    }
}

/// Notebook metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotebookMetadata {
    /// When the notebook was created.
    pub created_at: DateTime<Utc>,
    /// When the notebook was last modified.
    pub updated_at: DateTime<Utc>,
    /// Author name.
    #[serde(default)]
    pub author: String,
    /// Tags for organization and search.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Default for NotebookMetadata {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            created_at: now,
            updated_at: now,
            author: String::new(),
            tags: Vec::new(),
        }
    }
}

/// An interactive notebook with markdown and code cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notebook {
    /// Notebook header fields (nested under `[notebook]` in TOML).
    pub notebook: NotebookHeader,
    /// Ordered list of cells.
    pub cells: Vec<NotebookCell>,
}

/// Header section of a notebook (maps to `[notebook]` in TOML).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotebookHeader {
    /// Display title.
    pub title: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Author name.
    #[serde(default)]
    pub author: String,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-modified timestamp.
    pub updated_at: DateTime<Utc>,
    /// Tags for search and organization.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Notebook {
    /// Create a new empty notebook.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            notebook: NotebookHeader {
                title: title.into(),
                description: String::new(),
                author: String::new(),
                created_at: now,
                updated_at: now,
                tags: Vec::new(),
            },
            cells: Vec::new(),
        }
    }

    /// Add a cell to the notebook.
    pub fn add_cell(&mut self, cell: NotebookCell) {
        self.cells.push(cell);
        self.notebook.updated_at = Utc::now();
    }

    /// Insert a cell at the given index.
    pub fn insert_cell(&mut self, index: usize, cell: NotebookCell) {
        let idx = index.min(self.cells.len());
        self.cells.insert(idx, cell);
        self.notebook.updated_at = Utc::now();
    }

    /// Remove a cell at the given index.
    pub fn remove_cell(&mut self, index: usize) -> Option<NotebookCell> {
        if index < self.cells.len() {
            self.notebook.updated_at = Utc::now();
            Some(self.cells.remove(index))
        } else {
            None
        }
    }

    /// Number of cells.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Number of code cells.
    #[must_use]
    pub fn code_cell_count(&self) -> usize {
        self.cells.iter().filter(|c| c.is_code()).count()
    }

    /// Serialize to TOML string.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Deserialize from TOML string.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Import a notebook from a plain markdown file.
    ///
    /// Fenced code blocks become code cells; everything else becomes markdown cells.
    #[must_use]
    pub fn from_markdown(title: &str, markdown: &str) -> Self {
        let mut nb = Self::new(title);
        let mut md_buffer = String::new();
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buffer = String::new();

        for line in markdown.lines() {
            let trimmed = line.trim();
            if !in_code_block && trimmed.starts_with("```") {
                // Flush accumulated markdown
                let content = md_buffer.trim().to_string();
                if !content.is_empty() {
                    nb.cells.push(NotebookCell::markdown(content));
                }
                md_buffer.clear();

                // Start code block
                in_code_block = true;
                code_lang = trimmed.trim_start_matches('`').trim().to_string();
                code_buffer.clear();
            } else if in_code_block && trimmed.starts_with("```") {
                // End code block
                in_code_block = false;
                let lang = if code_lang.is_empty() {
                    "bash".to_string()
                } else {
                    code_lang.clone()
                };
                nb.cells.push(NotebookCell::code(lang, code_buffer.trim_end()));
                code_buffer.clear();
                code_lang.clear();
            } else if in_code_block {
                if !code_buffer.is_empty() {
                    code_buffer.push('\n');
                }
                code_buffer.push_str(line);
            } else {
                if !md_buffer.is_empty() {
                    md_buffer.push('\n');
                }
                md_buffer.push_str(line);
            }
        }

        // Flush remaining markdown
        let content = md_buffer.trim().to_string();
        if !content.is_empty() {
            nb.cells.push(NotebookCell::markdown(content));
        }

        // Flush unclosed code block (malformed markdown)
        if in_code_block && !code_buffer.is_empty() {
            let lang = if code_lang.is_empty() {
                "bash".to_string()
            } else {
                code_lang
            };
            nb.cells.push(NotebookCell::code(lang, code_buffer.trim_end()));
        }

        nb
    }

    /// Export the notebook as plain markdown.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", self.notebook.title));
        if !self.notebook.description.is_empty() {
            out.push_str(&self.notebook.description);
            out.push_str("\n\n");
        }

        for cell in &self.cells {
            match cell {
                NotebookCell::Markdown { content } => {
                    out.push_str(content);
                    out.push_str("\n\n");
                }
                NotebookCell::Code {
                    language, source, ..
                } => {
                    out.push_str(&format!("```{language}\n"));
                    out.push_str(source);
                    out.push_str("\n```\n\n");
                }
            }
        }

        out.trim_end().to_string() + "\n"
    }

    /// Derive a filesystem-safe slug from the title.
    #[must_use]
    pub fn slug(&self) -> String {
        sanitize_name(&self.notebook.title)
    }
}

// ─── NotebookManager ─────────────────────────────────────────────────────

/// Manages notebook storage on disk.
///
/// Notebooks are stored as `.elwood-nb` TOML files in `~/.elwood/notebooks/`.
pub struct NotebookManager {
    /// Directory where notebooks are stored.
    dir: PathBuf,
}

impl NotebookManager {
    /// Create a new manager using the default notebooks directory.
    ///
    /// Returns `None` if the home directory cannot be determined.
    pub fn new() -> Option<Self> {
        let home = dirs_next::home_dir()?;
        let dir = home.join(".elwood").join("notebooks");
        Some(Self { dir })
    }

    /// Create a manager with a custom storage directory.
    #[must_use]
    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Ensure the storage directory exists.
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Path for a notebook file given its name/slug.
    #[must_use]
    pub fn notebook_path(&self, name: &str) -> PathBuf {
        let safe = sanitize_name(name);
        self.dir.join(format!("{safe}.elwood-nb"))
    }

    /// Save a notebook to disk.
    pub fn save(&self, notebook: &Notebook) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let path = self.notebook_path(&notebook.notebook.title);
        let toml_str = notebook.to_toml()?;
        std::fs::write(&path, toml_str)?;
        Ok(())
    }

    /// Load a notebook by name.
    pub fn load(&self, name: &str) -> anyhow::Result<Notebook> {
        let path = self.notebook_path(name);
        if !path.exists() {
            anyhow::bail!("Notebook not found: {name}");
        }
        let content = std::fs::read_to_string(&path)?;
        let nb = Notebook::from_toml(&content)?;
        Ok(nb)
    }

    /// Delete a notebook by name.
    pub fn delete(&self, name: &str) -> anyhow::Result<()> {
        let path = self.notebook_path(name);
        if !path.exists() {
            anyhow::bail!("Notebook not found: {name}");
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// List all saved notebooks (name, title, cell count, tags).
    pub fn list(&self) -> anyhow::Result<Vec<NotebookSummary>> {
        let mut summaries = Vec::new();
        let dir = &self.dir;
        if !dir.exists() {
            return Ok(summaries);
        }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("elwood-nb") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(nb) = Notebook::from_toml(&content) {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let code_cell_count = nb.code_cell_count();
                        let cell_count = nb.cells.len();
                        summaries.push(NotebookSummary {
                            name,
                            title: nb.notebook.title,
                            description: nb.notebook.description,
                            cell_count,
                            code_cell_count,
                            tags: nb.notebook.tags,
                            updated_at: nb.notebook.updated_at,
                        });
                    }
                }
            }
        }
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Search notebooks by title or tags containing the query.
    pub fn search(&self, query: &str) -> anyhow::Result<Vec<NotebookSummary>> {
        let query_lower = query.to_lowercase();
        let all = self.list()?;
        Ok(all
            .into_iter()
            .filter(|s| {
                s.title.to_lowercase().contains(&query_lower)
                    || s.description.to_lowercase().contains(&query_lower)
                    || s.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect())
    }

    /// Import a markdown file as a notebook.
    pub fn import(&self, md_path: &Path) -> anyhow::Result<Notebook> {
        let content = std::fs::read_to_string(md_path)?;
        let title = md_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Imported Notebook")
            .to_string();
        let nb = Notebook::from_markdown(&title, &content);
        self.save(&nb)?;
        Ok(nb)
    }

    /// Export a notebook as plain markdown.
    pub fn export(&self, name: &str, output_path: &Path) -> anyhow::Result<()> {
        let nb = self.load(name)?;
        let md = nb.to_markdown();
        std::fs::write(output_path, md)?;
        Ok(())
    }
}

/// Summary of a notebook for listing.
#[derive(Debug, Clone)]
pub struct NotebookSummary {
    /// Filesystem slug (without extension).
    pub name: String,
    /// Display title.
    pub title: String,
    /// Short description.
    pub description: String,
    /// Total number of cells.
    pub cell_count: usize,
    /// Number of executable code cells.
    pub code_cell_count: usize,
    /// Tags.
    pub tags: Vec<String>,
    /// Last modified timestamp.
    pub updated_at: DateTime<Utc>,
}

// ─── Notebook Slash Command ──────────────────────────────────────────────

/// Result of parsing a `/notebook` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotebookCommand {
    /// List all saved notebooks.
    List,
    /// Open a notebook by name.
    Open { name: String },
    /// Create a new notebook.
    Create { name: String },
    /// Import a markdown file as a notebook.
    Import { path: String },
    /// Export a notebook as plain markdown.
    Export { name: String },
    /// Show help for the notebook command.
    Help,
}

/// Parse `/notebook` subcommand arguments.
#[must_use]
pub fn parse_notebook_command(args: &str) -> NotebookCommand {
    let args = args.trim();
    let (subcmd, rest) = match args.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd.trim(), rest.trim()),
        None => (args, ""),
    };

    match subcmd {
        "list" | "ls" => NotebookCommand::List,
        "open" | "o" => {
            if rest.is_empty() {
                NotebookCommand::Help
            } else {
                NotebookCommand::Open {
                    name: rest.to_string(),
                }
            }
        }
        "create" | "new" => {
            if rest.is_empty() {
                NotebookCommand::Help
            } else {
                NotebookCommand::Create {
                    name: rest.to_string(),
                }
            }
        }
        "import" => {
            if rest.is_empty() {
                NotebookCommand::Help
            } else {
                NotebookCommand::Import {
                    path: rest.to_string(),
                }
            }
        }
        "export" => {
            if rest.is_empty() {
                NotebookCommand::Help
            } else {
                NotebookCommand::Export {
                    name: rest.to_string(),
                }
            }
        }
        "" => NotebookCommand::Help,
        // Single arg treated as "open <name>"
        name => NotebookCommand::Open {
            name: name.to_string(),
        },
    }
}

/// Format the help text for the `/notebook` command.
#[must_use]
pub fn notebook_help() -> String {
    "\
/notebook list              List saved notebooks\n\
/notebook open <name>       Open a notebook\n\
/notebook create <name>     Create a new notebook\n\
/notebook import <file.md>  Import markdown as notebook\n\
/notebook export <name>     Export notebook as markdown\n\
\n\
Aliases: /nb, open=o, list=ls, create=new\n\
\n\
Viewer keys:\n\
  j/k          Navigate cells\n\
  Enter / r    Run current code cell\n\
  R            Run all from current cell\n\
  q            Close viewer"
        .to_string()
}

// ─── Rendering Helpers ───────────────────────────────────────────────────

// ANSI constants (TokyoNight palette — consistent with crate::markdown)
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

const ACCENT: (u8, u8, u8) = (122, 162, 247); // #7aa2f7
const MUTED: (u8, u8, u8) = (86, 95, 137);    // #565f89
const CYAN: (u8, u8, u8) = (125, 207, 255);    // #7dcfff
const GREEN: (u8, u8, u8) = (158, 206, 106);   // #9ece6a
const RED: (u8, u8, u8) = (247, 118, 142);     // #f7768e
const YELLOW: (u8, u8, u8) = (224, 175, 104);  // #e0af68
const CODE_BG: (u8, u8, u8) = (36, 40, 59);    // #24283b
const BORDER: (u8, u8, u8) = (59, 66, 97);     // #3b4261

fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn bg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

fn fgc(c: (u8, u8, u8)) -> String {
    fg(c.0, c.1, c.2)
}

fn bgc(c: (u8, u8, u8)) -> String {
    bg(c.0, c.1, c.2)
}

/// Render a code cell with language label, state indicator, and optional output.
fn render_code_cell(
    language: &str,
    source: &str,
    output: Option<&CellOutput>,
    state: &CellState,
) -> String {
    let mut out = String::new();
    let border = fgc(BORDER);
    let muted = fgc(MUTED);
    let code_bg = bgc(CODE_BG);
    let code_fg = fgc(CYAN);

    // State indicator
    let indicator = match state {
        CellState::Idle => format!("{}  {BOLD}{}  Run{RESET}", fgc(MUTED), fgc(ACCENT)),
        CellState::Running => format!("{}  Running...{RESET}", fgc(YELLOW)),
        CellState::Completed(code) => {
            format!("{}  {BOLD}  Done (exit {code}){RESET}", fgc(GREEN))
        }
        CellState::Failed(code) => {
            format!("{}  {BOLD}  Failed (exit {code}){RESET}", fgc(RED))
        }
    };

    // Language label + state on same line
    out.push_str(&format!("  {DIM}{muted}{language}{RESET}  {indicator}\r\n"));

    // Source lines with line numbers
    for (i, line) in source.lines().enumerate() {
        let num = i + 1;
        out.push_str(&format!(
            "  {border}\u{2502}{RESET} {DIM}{muted}{num:>3}{RESET} {code_bg}{code_fg}{line}{RESET}\r\n"
        ));
    }

    // Output area (if present)
    if let Some(output) = output {
        let duration = output.duration();
        let secs = duration.as_secs_f64();
        out.push_str(&format!(
            "  {border}\u{2500}\u{2500}\u{2500}{RESET} {DIM}{muted}output ({secs:.1}s){RESET}\r\n"
        ));
        if !output.stdout.is_empty() {
            for line in output.stdout.lines() {
                out.push_str(&format!("  {border}\u{2502}{RESET}   {line}\r\n"));
            }
        }
        if !output.stderr.is_empty() {
            for line in output.stderr.lines() {
                out.push_str(&format!(
                    "  {border}\u{2502}{RESET}   {}{line}{RESET}\r\n",
                    fgc(RED)
                ));
            }
        }
    }

    out
}

/// Sanitize a name for use as a filename (lowercase, alphanumeric + hyphens).
fn sanitize_name(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Collapse multiple hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    // Trim trailing hyphen
    result.trim_end_matches('-').to_string()
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_state_default() {
        assert_eq!(CellState::default(), CellState::Idle);
    }

    #[test]
    fn test_cell_output_duration() {
        let output = CellOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 1500,
        };
        assert_eq!(output.duration(), Duration::from_millis(1500));
    }

    #[test]
    fn test_notebook_cell_constructors() {
        let md = NotebookCell::markdown("# Hello");
        assert!(md.is_markdown());
        assert!(!md.is_code());

        let code = NotebookCell::code("bash", "echo hello");
        assert!(code.is_code());
        assert!(!code.is_markdown());
    }

    #[test]
    fn test_notebook_new() {
        let nb = Notebook::new("Test Notebook");
        assert_eq!(nb.notebook.title, "Test Notebook");
        assert!(nb.cells.is_empty());
        assert_eq!(nb.cell_count(), 0);
        assert_eq!(nb.code_cell_count(), 0);
    }

    #[test]
    fn test_notebook_add_cells() {
        let mut nb = Notebook::new("Test");
        nb.add_cell(NotebookCell::markdown("# Intro"));
        nb.add_cell(NotebookCell::code("bash", "echo hi"));
        nb.add_cell(NotebookCell::markdown("Done."));
        nb.add_cell(NotebookCell::code("python", "print(42)"));

        assert_eq!(nb.cell_count(), 4);
        assert_eq!(nb.code_cell_count(), 2);
    }

    #[test]
    fn test_notebook_insert_cell() {
        let mut nb = Notebook::new("Test");
        nb.add_cell(NotebookCell::markdown("First"));
        nb.add_cell(NotebookCell::markdown("Third"));
        nb.insert_cell(1, NotebookCell::markdown("Second"));

        assert_eq!(nb.cell_count(), 3);
        match &nb.cells[1] {
            NotebookCell::Markdown { content } => assert_eq!(content, "Second"),
            _ => panic!("expected markdown cell"),
        }
    }

    #[test]
    fn test_notebook_insert_cell_out_of_bounds() {
        let mut nb = Notebook::new("Test");
        nb.add_cell(NotebookCell::markdown("First"));
        nb.insert_cell(999, NotebookCell::markdown("Last"));
        assert_eq!(nb.cell_count(), 2);
        // Should be appended at end
        match &nb.cells[1] {
            NotebookCell::Markdown { content } => assert_eq!(content, "Last"),
            _ => panic!("expected markdown cell"),
        }
    }

    #[test]
    fn test_notebook_remove_cell() {
        let mut nb = Notebook::new("Test");
        nb.add_cell(NotebookCell::markdown("A"));
        nb.add_cell(NotebookCell::markdown("B"));
        nb.add_cell(NotebookCell::markdown("C"));

        let removed = nb.remove_cell(1);
        assert!(removed.is_some());
        assert_eq!(nb.cell_count(), 2);

        let removed = nb.remove_cell(99);
        assert!(removed.is_none());
    }

    #[test]
    fn test_toml_round_trip() {
        let mut nb = Notebook::new("Deploy Checklist");
        nb.notebook.description = "Production deployment steps".to_string();
        nb.notebook.author = "gordon".to_string();
        nb.notebook.tags = vec!["deploy".to_string(), "production".to_string()];

        nb.add_cell(NotebookCell::markdown(
            "# Deploy to Production\nRun these steps in order.",
        ));
        nb.add_cell(NotebookCell::code("bash", "cargo test --workspace"));
        nb.add_cell(NotebookCell::markdown("If tests pass, deploy:"));
        nb.add_cell(NotebookCell::code("bash", "git push origin main"));

        // Serialize
        let toml_str = nb.to_toml().expect("serialization should succeed");
        assert!(toml_str.contains("[notebook]"));
        assert!(toml_str.contains("Deploy Checklist"));
        assert!(toml_str.contains("[[cells]]"));

        // Deserialize
        let restored = Notebook::from_toml(&toml_str).expect("deserialization should succeed");
        assert_eq!(restored.notebook.title, nb.notebook.title);
        assert_eq!(restored.notebook.description, nb.notebook.description);
        assert_eq!(restored.notebook.author, nb.notebook.author);
        assert_eq!(restored.notebook.tags, nb.notebook.tags);
        assert_eq!(restored.cells.len(), nb.cells.len());

        // Check cell types
        assert!(restored.cells[0].is_markdown());
        assert!(restored.cells[1].is_code());
        assert!(restored.cells[2].is_markdown());
        assert!(restored.cells[3].is_code());
    }

    #[test]
    fn test_toml_round_trip_with_output() {
        let mut nb = Notebook::new("Test");
        nb.cells.push(NotebookCell::Code {
            language: "bash".to_string(),
            source: "echo hello".to_string(),
            output: Some(CellOutput {
                stdout: "hello\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 42,
            }),
            state: CellState::Completed(0),
        });

        let toml_str = nb.to_toml().expect("serialize");
        let restored = Notebook::from_toml(&toml_str).expect("deserialize");

        match &restored.cells[0] {
            NotebookCell::Code {
                output, state, ..
            } => {
                let out = output.as_ref().expect("should have output");
                assert_eq!(out.stdout, "hello\n");
                assert_eq!(out.exit_code, 0);
                assert_eq!(out.duration_ms, 42);
                assert_eq!(*state, CellState::Completed(0));
            }
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_toml_round_trip_failed_state() {
        let mut nb = Notebook::new("Test");
        nb.cells.push(NotebookCell::Code {
            language: "bash".to_string(),
            source: "exit 1".to_string(),
            output: Some(CellOutput {
                stdout: String::new(),
                stderr: "error\n".to_string(),
                exit_code: 1,
                duration_ms: 10,
            }),
            state: CellState::Failed(1),
        });

        let toml_str = nb.to_toml().expect("serialize");
        let restored = Notebook::from_toml(&toml_str).expect("deserialize");

        match &restored.cells[0] {
            NotebookCell::Code {
                output, state, ..
            } => {
                assert_eq!(*state, CellState::Failed(1));
                assert_eq!(output.as_ref().unwrap().stderr, "error\n");
            }
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_markdown_import() {
        let md = r#"# Deploy Guide

First, run the tests.

```bash
cargo test --workspace
```

If tests pass, deploy:

```bash
git push origin main
```

All done!
"#;

        let nb = Notebook::from_markdown("Deploy Guide", md);
        assert_eq!(nb.notebook.title, "Deploy Guide");

        // Should have: md, code, md, code, md
        assert_eq!(nb.cell_count(), 5);
        assert!(nb.cells[0].is_markdown());
        assert!(nb.cells[1].is_code());
        assert!(nb.cells[2].is_markdown());
        assert!(nb.cells[3].is_code());
        assert!(nb.cells[4].is_markdown());

        // Check code cell content
        match &nb.cells[1] {
            NotebookCell::Code {
                language, source, ..
            } => {
                assert_eq!(language, "bash");
                assert_eq!(source, "cargo test --workspace");
            }
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_markdown_import_no_code() {
        let md = "# Just text\n\nNo code blocks here.";
        let nb = Notebook::from_markdown("TextOnly", md);
        assert_eq!(nb.cell_count(), 1);
        assert!(nb.cells[0].is_markdown());
    }

    #[test]
    fn test_markdown_import_only_code() {
        let md = "```python\nprint(42)\n```";
        let nb = Notebook::from_markdown("CodeOnly", md);
        assert_eq!(nb.cell_count(), 1);
        assert!(nb.cells[0].is_code());
    }

    #[test]
    fn test_markdown_import_no_language() {
        let md = "```\necho hello\n```";
        let nb = Notebook::from_markdown("NoLang", md);
        match &nb.cells[0] {
            NotebookCell::Code { language, .. } => assert_eq!(language, "bash"),
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_markdown_export() {
        let mut nb = Notebook::new("Export Test");
        nb.notebook.description = "A test notebook".to_string();
        nb.add_cell(NotebookCell::markdown("Some intro text."));
        nb.add_cell(NotebookCell::code("bash", "echo hello"));
        nb.add_cell(NotebookCell::markdown("Conclusion."));

        let md = nb.to_markdown();
        assert!(md.contains("# Export Test"));
        assert!(md.contains("A test notebook"));
        assert!(md.contains("Some intro text."));
        assert!(md.contains("```bash"));
        assert!(md.contains("echo hello"));
        assert!(md.contains("Conclusion."));
    }

    #[test]
    fn test_markdown_round_trip() {
        let original_md = "\
# Deploy

Run tests:

```bash
cargo test
```

Deploy:

```bash
git push
```

Done.";

        let nb = Notebook::from_markdown("Deploy", original_md);
        let exported = nb.to_markdown();

        // Re-import should produce same structure
        let nb2 = Notebook::from_markdown("Deploy", &exported);
        assert_eq!(nb.cells.len(), nb2.cells.len());
        for (a, b) in nb.cells.iter().zip(nb2.cells.iter()) {
            assert_eq!(a.is_code(), b.is_code());
        }
    }

    #[test]
    fn test_slug_generation() {
        let nb = Notebook::new("Deploy Checklist");
        assert_eq!(nb.slug(), "deploy-checklist");

        let nb = Notebook::new("My Cool Notebook!!!");
        assert_eq!(nb.slug(), "my-cool-notebook");

        let nb = Notebook::new("test");
        assert_eq!(nb.slug(), "test");
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("Hello World"), "hello-world");
        assert_eq!(sanitize_name("a---b"), "a-b");
        assert_eq!(sanitize_name("  spaces  "), "spaces");
        assert_eq!(sanitize_name("CamelCase"), "camelcase");
        assert_eq!(sanitize_name("with/slashes"), "with-slashes");
        assert_eq!(sanitize_name("dots.and.stuff"), "dots-and-stuff");
    }

    #[test]
    fn test_cell_state_transitions() {
        // Simulate a cell lifecycle: Idle -> Running -> Completed
        let mut cell = NotebookCell::code("bash", "echo test");
        match &cell {
            NotebookCell::Code { state, .. } => assert_eq!(*state, CellState::Idle),
            _ => panic!("expected code cell"),
        }

        // Transition to Running
        if let NotebookCell::Code { ref mut state, .. } = cell {
            *state = CellState::Running;
        }
        match &cell {
            NotebookCell::Code { state, .. } => assert_eq!(*state, CellState::Running),
            _ => panic!("expected code cell"),
        }

        // Transition to Completed
        if let NotebookCell::Code {
            ref mut state,
            ref mut output,
            ..
        } = cell
        {
            *state = CellState::Completed(0);
            *output = Some(CellOutput {
                stdout: "test\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 100,
            });
        }
        match &cell {
            NotebookCell::Code { state, output, .. } => {
                assert_eq!(*state, CellState::Completed(0));
                assert!(output.is_some());
            }
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_cell_state_failure_transition() {
        let mut cell = NotebookCell::code("bash", "exit 1");
        if let NotebookCell::Code {
            ref mut state,
            ref mut output,
            ..
        } = cell
        {
            *state = CellState::Failed(1);
            *output = Some(CellOutput {
                stdout: String::new(),
                stderr: "command failed\n".to_string(),
                exit_code: 1,
                duration_ms: 50,
            });
        }
        match &cell {
            NotebookCell::Code { state, output, .. } => {
                assert_eq!(*state, CellState::Failed(1));
                assert_eq!(output.as_ref().unwrap().exit_code, 1);
            }
            _ => panic!("expected code cell"),
        }
    }

    #[test]
    fn test_render_markdown_cell() {
        let cell = NotebookCell::markdown("# Hello World");
        let rendered = cell.render();
        // Should contain the markdown rendering (delegates to crate::markdown)
        assert!(rendered.contains("Hello World"));
    }

    #[test]
    fn test_render_code_cell_idle() {
        let cell = NotebookCell::code("bash", "echo hello");
        let rendered = cell.render();
        assert!(rendered.contains("bash"));
        assert!(rendered.contains("echo hello"));
        assert!(rendered.contains("Run"));
    }

    #[test]
    fn test_render_code_cell_with_output() {
        let cell = NotebookCell::Code {
            language: "bash".to_string(),
            source: "echo hello".to_string(),
            output: Some(CellOutput {
                stdout: "hello\n".to_string(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 150,
            }),
            state: CellState::Completed(0),
        };
        let rendered = cell.render();
        assert!(rendered.contains("Done"));
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("output"));
    }

    #[test]
    fn test_render_code_cell_failed() {
        let cell = NotebookCell::Code {
            language: "bash".to_string(),
            source: "exit 1".to_string(),
            output: Some(CellOutput {
                stdout: String::new(),
                stderr: "error occurred\n".to_string(),
                exit_code: 1,
                duration_ms: 10,
            }),
            state: CellState::Failed(1),
        };
        let rendered = cell.render();
        assert!(rendered.contains("Failed"));
        assert!(rendered.contains("error occurred"));
    }

    #[test]
    fn test_notebook_manager_crud() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());

        // Save
        let mut nb = Notebook::new("Test Notebook");
        nb.notebook.description = "A test".to_string();
        nb.add_cell(NotebookCell::markdown("# Hello"));
        nb.add_cell(NotebookCell::code("bash", "echo test"));

        mgr.save(&nb).expect("save should succeed");

        // List
        let summaries = mgr.list().expect("list should succeed");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title, "Test Notebook");
        assert_eq!(summaries[0].cell_count, 2);
        assert_eq!(summaries[0].code_cell_count, 1);

        // Load
        let loaded = mgr.load("Test Notebook").expect("load should succeed");
        assert_eq!(loaded.notebook.title, "Test Notebook");
        assert_eq!(loaded.cells.len(), 2);

        // Delete
        mgr.delete("Test Notebook").expect("delete should succeed");
        let summaries = mgr.list().expect("list after delete");
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_notebook_manager_load_not_found() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());

        let result = mgr.load("nonexistent");
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("not found"),
            "error should mention 'not found'"
        );
    }

    #[test]
    fn test_notebook_manager_delete_not_found() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());

        let result = mgr.delete("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_notebook_manager_search() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());

        let mut nb1 = Notebook::new("Deploy Checklist");
        nb1.notebook.tags = vec!["deploy".to_string(), "production".to_string()];
        nb1.add_cell(NotebookCell::markdown("Deploy steps"));

        let mut nb2 = Notebook::new("Debug Guide");
        nb2.notebook.tags = vec!["debug".to_string()];
        nb2.add_cell(NotebookCell::markdown("Debug steps"));

        mgr.save(&nb1).expect("save 1");
        mgr.save(&nb2).expect("save 2");

        // Search by title
        let results = mgr.search("deploy").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Deploy Checklist");

        // Search by tag
        let results = mgr.search("debug").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Debug Guide");

        // Search with no match
        let results = mgr.search("zzz").expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn test_notebook_manager_import_export() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());

        // Create a markdown file
        let md_path = dir.path().join("input.md");
        std::fs::write(
            &md_path,
            "# Import Test\n\nSome text.\n\n```bash\necho hello\n```\n",
        )
        .expect("write md");

        // Import
        let nb = mgr.import(&md_path).expect("import should succeed");
        assert_eq!(nb.notebook.title, "input");
        assert!(nb.cell_count() > 0);

        // Verify it was saved
        let summaries = mgr.list().expect("list");
        assert_eq!(summaries.len(), 1);

        // Export
        let export_path = dir.path().join("output.md");
        mgr.export("input", &export_path)
            .expect("export should succeed");

        let exported = std::fs::read_to_string(&export_path).expect("read export");
        assert!(exported.contains("# input"));
        assert!(exported.contains("echo hello"));
    }

    #[test]
    fn test_notebook_manager_list_empty_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mgr = NotebookManager::with_dir(dir.path().to_path_buf());
        let summaries = mgr.list().expect("list empty");
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_notebook_manager_list_nonexistent_dir() {
        let mgr = NotebookManager::with_dir(PathBuf::from("/tmp/nonexistent_elwood_test_dir"));
        let summaries = mgr.list().expect("list nonexistent");
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_parse_notebook_command_list() {
        assert_eq!(
            parse_notebook_command("list"),
            NotebookCommand::List
        );
        assert_eq!(
            parse_notebook_command("ls"),
            NotebookCommand::List
        );
    }

    #[test]
    fn test_parse_notebook_command_open() {
        assert_eq!(
            parse_notebook_command("open deploy"),
            NotebookCommand::Open { name: "deploy".to_string() }
        );
        assert_eq!(
            parse_notebook_command("o deploy"),
            NotebookCommand::Open { name: "deploy".to_string() }
        );
    }

    #[test]
    fn test_parse_notebook_command_create() {
        assert_eq!(
            parse_notebook_command("create my notebook"),
            NotebookCommand::Create { name: "my notebook".to_string() }
        );
        assert_eq!(
            parse_notebook_command("new test"),
            NotebookCommand::Create { name: "test".to_string() }
        );
    }

    #[test]
    fn test_parse_notebook_command_import() {
        assert_eq!(
            parse_notebook_command("import /tmp/file.md"),
            NotebookCommand::Import { path: "/tmp/file.md".to_string() }
        );
    }

    #[test]
    fn test_parse_notebook_command_export() {
        assert_eq!(
            parse_notebook_command("export deploy"),
            NotebookCommand::Export { name: "deploy".to_string() }
        );
    }

    #[test]
    fn test_parse_notebook_command_help() {
        assert_eq!(parse_notebook_command(""), NotebookCommand::Help);
        assert_eq!(parse_notebook_command("open"), NotebookCommand::Help);
        assert_eq!(parse_notebook_command("create"), NotebookCommand::Help);
        assert_eq!(parse_notebook_command("import"), NotebookCommand::Help);
        assert_eq!(parse_notebook_command("export"), NotebookCommand::Help);
    }

    #[test]
    fn test_parse_notebook_command_bare_name() {
        // Bare name treated as "open <name>"
        assert_eq!(
            parse_notebook_command("deploy"),
            NotebookCommand::Open { name: "deploy".to_string() }
        );
    }

    #[test]
    fn test_notebook_help_text() {
        let help = notebook_help();
        assert!(help.contains("/notebook list"));
        assert!(help.contains("/notebook open"));
        assert!(help.contains("/notebook create"));
        assert!(help.contains("/notebook import"));
        assert!(help.contains("/notebook export"));
        assert!(help.contains("Aliases: /nb"));
    }
}
