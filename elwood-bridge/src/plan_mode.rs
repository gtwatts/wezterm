//! Plan mode: structured implementation plans with persistence.
//!
//! When the user types `/plan <description>`, the agent generates a structured
//! markdown plan. Plans are parsed into `PlanDocument` objects, saved to disk
//! as human-readable markdown, and rendered as interactive overlays.
//!
//! ## Plan Lifecycle
//!
//! 1. `/plan <description>` — LLM generates plan markdown
//! 2. Plan is parsed into `PlanDocument`
//! 3. User reviews/approves in `PlanViewer` overlay
//! 4. On approval, steps execute sequentially via agent messages
//! 5. Steps are marked complete as they finish
//!
//! ## Storage
//!
//! Plans are saved to `~/.elwood/plans/{timestamp}-{slug}.md`.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

/// Status of a plan document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// Plan has been generated but not yet approved.
    Draft,
    /// Plan has been approved for execution.
    Approved,
    /// Plan is currently being executed.
    InProgress,
    /// All steps have been completed.
    Completed,
    /// Plan was cancelled by the user.
    Cancelled,
}

impl PlanStatus {
    /// Return the human-readable label for this status.
    pub fn label(&self) -> &'static str {
        match self {
            PlanStatus::Draft => "Draft",
            PlanStatus::Approved => "Approved",
            PlanStatus::InProgress => "In Progress",
            PlanStatus::Completed => "Completed",
            PlanStatus::Cancelled => "Cancelled",
        }
    }

    /// Parse a status from a string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "draft" => Some(PlanStatus::Draft),
            "approved" => Some(PlanStatus::Approved),
            "in progress" | "in_progress" | "inprogress" => Some(PlanStatus::InProgress),
            "completed" | "complete" | "done" => Some(PlanStatus::Completed),
            "cancelled" | "canceled" => Some(PlanStatus::Cancelled),
            _ => None,
        }
    }
}

impl std::fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A single step in a plan.
#[derive(Debug, Clone)]
pub struct PlanStep {
    /// Description of what this step does.
    pub description: String,
    /// Whether this step has been completed.
    pub completed: bool,
    /// Optional sub-steps (bullet points under a step).
    pub substeps: Vec<String>,
}

/// A structured implementation plan.
#[derive(Debug, Clone)]
pub struct PlanDocument {
    /// Unique identifier (timestamp-based).
    pub id: String,
    /// Short title of the plan.
    pub title: String,
    /// The overall goal description.
    pub goal: String,
    /// Ordered list of steps.
    pub steps: Vec<PlanStep>,
    /// Files that will be affected.
    pub files: Vec<String>,
    /// When the plan was created.
    pub created_at: DateTime<Utc>,
    /// Current status.
    pub status: PlanStatus,
}

impl PlanDocument {
    /// Create a new empty plan with the given title and goal.
    pub fn create(title: &str, goal: &str) -> Self {
        let now = Utc::now();
        let id = now.format("%Y%m%d_%H%M%S").to_string();
        Self {
            id,
            title: title.to_string(),
            goal: goal.to_string(),
            steps: Vec::new(),
            files: Vec::new(),
            created_at: now,
            status: PlanStatus::Draft,
        }
    }

    /// Mark a step as completed by index. Returns false if index is out of bounds.
    pub fn complete_step(&mut self, index: usize) -> bool {
        if let Some(step) = self.steps.get_mut(index) {
            step.completed = true;
            // If all steps are done, mark the plan as completed
            if self.steps.iter().all(|s| s.completed) {
                self.status = PlanStatus::Completed;
            }
            true
        } else {
            false
        }
    }

    /// Toggle a step's completion status by index.
    pub fn toggle_step(&mut self, index: usize) -> bool {
        if let Some(step) = self.steps.get_mut(index) {
            step.completed = !step.completed;
            // Update plan status
            if self.steps.iter().all(|s| s.completed) {
                self.status = PlanStatus::Completed;
            } else if self.status == PlanStatus::Completed {
                self.status = PlanStatus::InProgress;
            }
            true
        } else {
            false
        }
    }

    /// Count completed and total steps.
    pub fn progress(&self) -> (usize, usize) {
        let done = self.steps.iter().filter(|s| s.completed).count();
        (done, self.steps.len())
    }

    /// Index of the first uncompleted step, or None if all done.
    pub fn next_step_index(&self) -> Option<usize> {
        self.steps.iter().position(|s| !s.completed)
    }

    /// Serialize the plan to markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::with_capacity(1024);

        md.push_str(&format!("# {}\n\n", self.title));
        md.push_str(&format!("**Status**: {}\n", self.status));
        md.push_str(&format!(
            "**Created**: {}\n\n",
            self.created_at.format("%Y-%m-%d %H:%M UTC")
        ));
        md.push_str(&format!("## Goal\n\n{}\n\n", self.goal));

        md.push_str("## Steps\n\n");
        for (i, step) in self.steps.iter().enumerate() {
            let check = if step.completed { "x" } else { " " };
            md.push_str(&format!(
                "{}. [{}] {}\n",
                i + 1,
                check,
                step.description
            ));
            for substep in &step.substeps {
                md.push_str(&format!("   - {substep}\n"));
            }
        }

        if !self.files.is_empty() {
            md.push_str("\n## Files\n\n");
            for file in &self.files {
                md.push_str(&format!("- `{file}`\n"));
            }
        }

        md
    }
}

/// Parse LLM-generated markdown plan output into a `PlanDocument`.
///
/// Expected format:
/// ```text
/// # Plan Title
///
/// Goal: description of the goal
///
/// ## Steps
/// 1. [ ] First step
///    - sub-detail
/// 2. [ ] Second step
///
/// ## Files
/// - `src/main.rs`
/// ```
pub fn parse_llm_plan(llm_output: &str) -> PlanDocument {
    let mut title = String::new();
    let mut goal = String::new();
    let mut steps: Vec<PlanStep> = Vec::new();
    let mut files: Vec<String> = Vec::new();

    #[derive(PartialEq)]
    enum Section {
        None,
        Goal,
        Steps,
        Files,
    }

    let mut section = Section::None;
    let mut collecting_goal = false;

    for line in llm_output.lines() {
        let trimmed = line.trim();

        // Title: first `# ` line
        if title.is_empty() && trimmed.starts_with("# ") {
            title = trimmed.trim_start_matches("# ").trim().to_string();
            continue;
        }

        // Section headers
        if trimmed.starts_with("## ") {
            let header = trimmed.trim_start_matches("## ").trim().to_lowercase();
            if header.contains("step") || header.contains("plan") {
                section = Section::Steps;
                collecting_goal = false;
            } else if header.contains("file") {
                section = Section::Files;
                collecting_goal = false;
            } else if header.contains("goal") || header.contains("objective") {
                section = Section::Goal;
                collecting_goal = true;
            } else {
                section = Section::None;
                collecting_goal = false;
            }
            continue;
        }

        // "Goal:" on a single line (without ## header)
        if trimmed.starts_with("Goal:") || trimmed.starts_with("**Goal**:") {
            let g = trimmed
                .trim_start_matches("Goal:")
                .trim_start_matches("**Goal**:")
                .trim();
            if !g.is_empty() {
                goal = g.to_string();
            }
            collecting_goal = true;
            section = Section::Goal;
            continue;
        }

        if collecting_goal && section == Section::Goal && !trimmed.is_empty() {
            if !goal.is_empty() {
                goal.push(' ');
            }
            goal.push_str(trimmed);
            continue;
        }

        // Steps: numbered or checkbox lines
        if section == Section::Steps {
            // Sub-step: indented bullet
            if (trimmed.starts_with("- ") || trimmed.starts_with("* "))
                && (line.starts_with("   ") || line.starts_with('\t'))
            {
                let substep_text = trimmed
                    .trim_start_matches("- ")
                    .trim_start_matches("* ")
                    .trim()
                    .to_string();
                if let Some(last) = steps.last_mut() {
                    last.substeps.push(substep_text);
                }
                continue;
            }

            // Main step: `1. [x] description` or `1. description` or `- [ ] description`
            if let Some(step) = parse_step_line(trimmed) {
                steps.push(step);
                continue;
            }
        }

        // Files: lines starting with `- ` or containing backtick-wrapped paths
        if section == Section::Files && !trimmed.is_empty() {
            let file_text = trimmed
                .trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim_matches('`')
                .trim()
                .to_string();
            if !file_text.is_empty() {
                files.push(file_text);
            }
        }
    }

    // Fallback: if no title was found, use a generic one
    if title.is_empty() {
        title = "Implementation Plan".to_string();
    }

    // If no goal was found but we have the original description, leave it empty
    // (the caller can set it from the original user input)

    let now = Utc::now();
    let id = now.format("%Y%m%d_%H%M%S").to_string();

    PlanDocument {
        id,
        title,
        goal,
        steps,
        files,
        created_at: now,
        status: PlanStatus::Draft,
    }
}

/// Parse a single step line into a `PlanStep`, if it matches expected patterns.
fn parse_step_line(line: &str) -> Option<PlanStep> {
    let trimmed = line.trim();

    // Pattern: `N. [x] description` or `N. [ ] description`
    // Pattern: `N. description`
    // Pattern: `- [x] description` or `- [ ] description`

    // Try numbered: `1. [x] ...` or `1. ...`
    if let Some(rest) = strip_number_prefix(trimmed) {
        let (completed, description) = strip_checkbox(rest);
        if !description.is_empty() {
            return Some(PlanStep {
                description,
                completed,
                substeps: Vec::new(),
            });
        }
    }

    // Try bullet: `- [x] ...` or `- ...`
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        let rest = &trimmed[2..];
        let (completed, description) = strip_checkbox(rest);
        if !description.is_empty() {
            return Some(PlanStep {
                description,
                completed,
                substeps: Vec::new(),
            });
        }
    }

    None
}

/// Strip a leading number and period (e.g., `1. ` -> rest).
fn strip_number_prefix(s: &str) -> Option<&str> {
    let dot_pos = s.find('.')?;
    let num_part = &s[..dot_pos];
    if num_part.chars().all(|c| c.is_ascii_digit()) && !num_part.is_empty() {
        let rest = s[dot_pos + 1..].trim_start();
        Some(rest)
    } else {
        None
    }
}

/// Strip a leading checkbox `[x]` or `[ ]` and return (completed, remaining).
fn strip_checkbox(s: &str) -> (bool, String) {
    let trimmed = s.trim();
    if trimmed.starts_with("[x]") || trimmed.starts_with("[X]") {
        (true, trimmed[3..].trim().to_string())
    } else if trimmed.starts_with("[ ]") {
        (false, trimmed[3..].trim().to_string())
    } else {
        (false, trimmed.to_string())
    }
}

/// Create a slug from a title (lowercase, hyphens, max 40 chars).
fn slugify(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse multiple hyphens
    let mut result = String::new();
    let mut last_was_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !last_was_hyphen {
                result.push(c);
            }
            last_was_hyphen = true;
        } else {
            result.push(c);
            last_was_hyphen = false;
        }
    }
    let trimmed = result.trim_matches('-');
    if trimmed.len() > 40 {
        trimmed[..40].trim_end_matches('-').to_string()
    } else {
        trimmed.to_string()
    }
}

/// Return the plans directory path (~/.elwood/plans/).
fn plans_dir() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".elwood")
        .join("plans")
}

/// Save a plan to disk as a markdown file.
///
/// Returns the path where the plan was saved.
pub fn save_plan(plan: &PlanDocument) -> std::io::Result<PathBuf> {
    let dir = plans_dir();
    std::fs::create_dir_all(&dir)?;

    let slug = slugify(&plan.title);
    let filename = format!("{}-{slug}.md", plan.id);
    let path = dir.join(filename);

    std::fs::write(&path, plan.to_markdown())?;
    Ok(path)
}

/// Load a plan from a markdown file on disk.
pub fn load_plan(path: &Path) -> std::io::Result<PlanDocument> {
    let content = std::fs::read_to_string(path)?;
    let mut plan = parse_llm_plan(&content);

    // Try to extract the ID from the filename (timestamp prefix)
    if let Some(filename) = path.file_stem().and_then(|n| n.to_str()) {
        if filename.len() >= 15 {
            plan.id = filename[..15].to_string();
        }
    }

    // Check for status line in the content
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("**Status**:") || trimmed.starts_with("**Status**:") {
            let status_str = trimmed
                .trim_start_matches("**Status**:")
                .trim_start_matches("**Status**:")
                .trim();
            if let Some(status) = PlanStatus::from_str_loose(status_str) {
                plan.status = status;
            }
        }
    }

    Ok(plan)
}

/// List all saved plans.
///
/// Returns `(path, title, status)` tuples sorted by creation time (newest first).
pub fn list_plans() -> Vec<(PathBuf, String, PlanStatus)> {
    let dir = plans_dir();
    let mut plans = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                if let Ok(plan) = load_plan(&path) {
                    plans.push((path, plan.title, plan.status));
                }
            }
        }
    }

    // Sort by filename (which starts with timestamp) — newest first
    plans.sort_by(|a, b| b.0.cmp(&a.0));
    plans
}

/// The system prompt prefix sent to the LLM to generate structured plans.
pub const PLAN_GENERATION_PROMPT: &str = "\
Generate a structured implementation plan in the following markdown format.
Use numbered steps with checkboxes. Include substeps as indented bullets.
List affected files in a Files section.

Format:
# Plan Title

## Goal
A clear description of the objective.

## Steps
1. [ ] First major step
   - Sub-detail or consideration
   - Another sub-detail
2. [ ] Second major step
3. [ ] Third major step

## Files
- `path/to/file1.rs`
- `path/to/file2.rs`

Now create a plan for the following:

";

/// Render a plan as ANSI-formatted text for display in the chat area.
///
/// This is a simple inline rendering (not the full overlay). Used when
/// the plan is first generated before the viewer opens.
pub fn render_plan_inline(plan: &PlanDocument, width: usize) -> String {
    // Color palette (matching screen.rs Tokyo Night)
    const RESET: &str = "\x1b[0m";
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";

    let accent = "\x1b[38;2;122;162;247m";
    let success = "\x1b[38;2;158;206;106m";
    let muted = "\x1b[38;2;86;95;137m";
    let fg = "\x1b[38;2;192;202;245m";
    let info = "\x1b[38;2;125;207;255m";

    let w = width.max(40);
    let inner = w.saturating_sub(4);

    let mut out = String::with_capacity(2048);

    // Title bar
    out.push_str(&format!(
        "\r\n{muted}{DIM}{}{RESET}\r\n",
        "\u{2500}".repeat(w)
    ));
    out.push_str(&format!(
        "  {accent}{BOLD}{}{RESET}  {muted}{DIM}[{}]{RESET}\r\n\r\n",
        plan.title,
        plan.status.label()
    ));

    // Goal
    if !plan.goal.is_empty() {
        out.push_str(&format!("  {fg}Goal: {}{RESET}\r\n\r\n", plan.goal));
    }

    // Steps
    let (done, total) = plan.progress();
    out.push_str(&format!(
        "  {info}{BOLD}Steps{RESET} {muted}({done}/{total}){RESET}\r\n\r\n"
    ));

    for (i, step) in plan.steps.iter().enumerate() {
        let check = if step.completed {
            format!("{success}\u{2611}{RESET}")
        } else {
            format!("{muted}\u{2610}{RESET}")
        };
        let desc_color = if step.completed { muted } else { fg };
        out.push_str(&format!(
            "  {check} {desc_color}{}.{RESET} {desc_color}{}{RESET}\r\n",
            i + 1,
            step.description
        ));

        for substep in &step.substeps {
            let sub_trunc = if substep.len() > inner - 8 {
                format!("{}...", &substep[..inner - 11])
            } else {
                substep.clone()
            };
            out.push_str(&format!(
                "      {muted}{DIM}- {sub_trunc}{RESET}\r\n"
            ));
        }
    }

    // Files
    if !plan.files.is_empty() {
        out.push_str(&format!("\r\n  {info}{BOLD}Files{RESET}\r\n"));
        for file in &plan.files {
            out.push_str(&format!("    {muted}`{file}`{RESET}\r\n"));
        }
    }

    out.push_str(&format!(
        "\r\n{muted}{DIM}{}{RESET}\r\n",
        "\u{2500}".repeat(w)
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_plan() {
        let plan = PlanDocument::create("Test Plan", "Test the plan system");
        assert_eq!(plan.title, "Test Plan");
        assert_eq!(plan.goal, "Test the plan system");
        assert_eq!(plan.status, PlanStatus::Draft);
        assert!(plan.steps.is_empty());
        assert!(!plan.id.is_empty());
    }

    #[test]
    fn test_plan_progress() {
        let mut plan = PlanDocument::create("Test", "Test");
        plan.steps = vec![
            PlanStep {
                description: "Step 1".into(),
                completed: true,
                substeps: vec![],
            },
            PlanStep {
                description: "Step 2".into(),
                completed: false,
                substeps: vec![],
            },
            PlanStep {
                description: "Step 3".into(),
                completed: false,
                substeps: vec![],
            },
        ];

        assert_eq!(plan.progress(), (1, 3));
        assert_eq!(plan.next_step_index(), Some(1));
    }

    #[test]
    fn test_complete_step() {
        let mut plan = PlanDocument::create("Test", "Test");
        plan.steps = vec![
            PlanStep {
                description: "Step 1".into(),
                completed: false,
                substeps: vec![],
            },
            PlanStep {
                description: "Step 2".into(),
                completed: false,
                substeps: vec![],
            },
        ];

        assert!(plan.complete_step(0));
        assert!(plan.steps[0].completed);
        assert_eq!(plan.status, PlanStatus::Draft); // not all done

        assert!(plan.complete_step(1));
        assert_eq!(plan.status, PlanStatus::Completed); // all done

        assert!(!plan.complete_step(5)); // out of bounds
    }

    #[test]
    fn test_toggle_step() {
        let mut plan = PlanDocument::create("Test", "Test");
        plan.status = PlanStatus::InProgress;
        plan.steps = vec![PlanStep {
            description: "Only step".into(),
            completed: false,
            substeps: vec![],
        }];

        assert!(plan.toggle_step(0));
        assert!(plan.steps[0].completed);
        assert_eq!(plan.status, PlanStatus::Completed);

        assert!(plan.toggle_step(0));
        assert!(!plan.steps[0].completed);
        assert_eq!(plan.status, PlanStatus::InProgress);
    }

    #[test]
    fn test_parse_llm_plan_basic() {
        let md = "\
# Build REST API

## Goal
Create a REST API with authentication and tests.

## Steps
1. [ ] Set up project structure
   - Create Cargo.toml
   - Add dependencies
2. [ ] Implement user model
3. [x] Write integration tests

## Files
- `src/main.rs`
- `src/auth.rs`
- `tests/api_test.rs`
";
        let plan = parse_llm_plan(md);

        assert_eq!(plan.title, "Build REST API");
        assert_eq!(plan.goal, "Create a REST API with authentication and tests.");
        assert_eq!(plan.steps.len(), 3);
        assert!(!plan.steps[0].completed);
        assert!(!plan.steps[1].completed);
        assert!(plan.steps[2].completed);
        assert_eq!(plan.steps[0].substeps.len(), 2);
        assert_eq!(plan.steps[0].substeps[0], "Create Cargo.toml");
        assert_eq!(plan.files.len(), 3);
        assert_eq!(plan.files[0], "src/main.rs");
    }

    #[test]
    fn test_parse_llm_plan_no_title() {
        let md = "\
## Steps
1. [ ] Do something
2. [ ] Do something else
";
        let plan = parse_llm_plan(md);
        assert_eq!(plan.title, "Implementation Plan");
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_parse_step_line_numbered() {
        let step = parse_step_line("1. [ ] Set up project").unwrap();
        assert_eq!(step.description, "Set up project");
        assert!(!step.completed);

        let step = parse_step_line("3. [x] Done thing").unwrap();
        assert_eq!(step.description, "Done thing");
        assert!(step.completed);
    }

    #[test]
    fn test_parse_step_line_no_checkbox() {
        let step = parse_step_line("1. Set up project").unwrap();
        assert_eq!(step.description, "Set up project");
        assert!(!step.completed);
    }

    #[test]
    fn test_parse_step_line_bullet() {
        let step = parse_step_line("- [ ] A bullet step").unwrap();
        assert_eq!(step.description, "A bullet step");
        assert!(!step.completed);
    }

    #[test]
    fn test_parse_step_line_invalid() {
        assert!(parse_step_line("Just some text").is_none());
        assert!(parse_step_line("").is_none());
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Build REST API"), "build-rest-api");
        assert_eq!(slugify("Hello  World!!"), "hello-world");
        assert_eq!(slugify("simple"), "simple");
    }

    #[test]
    fn test_to_markdown_roundtrip() {
        let mut plan = PlanDocument::create("Test Plan", "Test roundtrip");
        plan.steps = vec![
            PlanStep {
                description: "First step".into(),
                completed: false,
                substeps: vec!["detail a".into(), "detail b".into()],
            },
            PlanStep {
                description: "Second step".into(),
                completed: true,
                substeps: vec![],
            },
        ];
        plan.files = vec!["src/main.rs".into(), "tests/test.rs".into()];

        let md = plan.to_markdown();
        let parsed = parse_llm_plan(&md);

        assert_eq!(parsed.title, "Test Plan");
        assert_eq!(parsed.steps.len(), 2);
        assert!(!parsed.steps[0].completed);
        assert!(parsed.steps[1].completed);
        assert_eq!(parsed.steps[0].substeps.len(), 2);
        assert_eq!(parsed.files.len(), 2);
    }

    #[test]
    fn test_plan_status_from_str() {
        assert_eq!(PlanStatus::from_str_loose("Draft"), Some(PlanStatus::Draft));
        assert_eq!(
            PlanStatus::from_str_loose("in progress"),
            Some(PlanStatus::InProgress)
        );
        assert_eq!(
            PlanStatus::from_str_loose("COMPLETED"),
            Some(PlanStatus::Completed)
        );
        assert_eq!(PlanStatus::from_str_loose("unknown"), None);
    }

    #[test]
    fn test_save_and_load_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = PlanDocument::create("Save Test", "Test saving");
        plan.steps = vec![PlanStep {
            description: "A step".into(),
            completed: false,
            substeps: vec![],
        }];

        // Save to temp dir
        let slug = slugify(&plan.title);
        let filename = format!("{}-{slug}.md", plan.id);
        let path = dir.path().join(filename);
        std::fs::write(&path, plan.to_markdown()).unwrap();

        // Load back
        let loaded = load_plan(&path).unwrap();
        assert_eq!(loaded.title, "Save Test");
        assert_eq!(loaded.steps.len(), 1);
    }

    #[test]
    fn test_render_plan_inline() {
        let mut plan = PlanDocument::create("My Plan", "Do the thing");
        plan.steps = vec![
            PlanStep {
                description: "Step one".into(),
                completed: true,
                substeps: vec![],
            },
            PlanStep {
                description: "Step two".into(),
                completed: false,
                substeps: vec!["sub-a".into()],
            },
        ];
        plan.files = vec!["src/lib.rs".into()];

        let rendered = render_plan_inline(&plan, 80);
        assert!(rendered.contains("My Plan"));
        assert!(rendered.contains("Step one"));
        assert!(rendered.contains("Step two"));
        assert!(rendered.contains("sub-a"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("1/2")); // progress
    }
}
