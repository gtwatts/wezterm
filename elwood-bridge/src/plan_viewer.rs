//! Interactive plan viewer overlay.
//!
//! Renders a plan as a navigable box overlay in the chat area with
//! keyboard controls for reviewing, editing, and approving plans.
//!
//! ## Keybindings
//!
//! | Key       | Action                       |
//! |-----------|------------------------------|
//! | `j`/Down  | Move cursor down             |
//! | `k`/Up    | Move cursor up               |
//! | `Space`   | Toggle step completion        |
//! | `Enter`   | Approve plan                 |
//! | `e`       | Edit current step text       |
//! | `Esc`/`q` | Cancel / close viewer        |

use crate::plan_mode::{PlanDocument, PlanStatus};

// ─── Color Palette (TokyoNight, matching screen.rs) ─────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
#[allow(dead_code)]
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";

const FG: (u8, u8, u8) = (192, 202, 245);
const SUCCESS: (u8, u8, u8) = (158, 206, 106);
const ERROR: (u8, u8, u8) = (247, 118, 142);
const ACCENT: (u8, u8, u8) = (122, 162, 247);
const MUTED: (u8, u8, u8) = (86, 95, 137);
const BORDER: (u8, u8, u8) = (59, 66, 97);
const SELECTION: (u8, u8, u8) = (40, 44, 66);
const INFO: (u8, u8, u8) = (125, 207, 255);
const WARNING: (u8, u8, u8) = (224, 175, 104);

fn fgc(c: (u8, u8, u8)) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}
fn bgc(c: (u8, u8, u8)) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

const CLEAR_EOL: &str = "\x1b[K";

// Box drawing
const BOX_TL: char = '\u{256D}'; // ╭
const BOX_TR: char = '\u{256E}'; // ╮
const BOX_BL: char = '\u{2570}'; // ╰
const BOX_BR: char = '\u{256F}'; // ╯
const BOX_H: char = '\u{2500}'; // ─
const BOX_V: char = '\u{2502}'; // │
const _BOX_SEP: char = '\u{251C}'; // ├
const _BOX_SEP_R: char = '\u{2524}'; // ┤

// Check marks
const CHECK_EMPTY: &str = "\u{2610}"; // ☐
const CHECK_FILLED: &str = "\u{2611}"; // ☑

/// Action returned from the plan viewer after user interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAction {
    /// User approved the plan for execution.
    Approve,
    /// User cancelled/closed the viewer.
    Cancel,
}

/// Interactive plan viewer state.
pub struct PlanViewer {
    /// The plan being viewed.
    pub plan: PlanDocument,
    /// Current cursor position (index into steps).
    pub cursor: usize,
    /// Whether the viewer is in edit mode for a step description.
    pub editing: bool,
    /// Edit buffer (when editing a step description).
    pub edit_buffer: String,
}

impl PlanViewer {
    /// Create a new plan viewer for the given plan.
    pub fn new(plan: PlanDocument) -> Self {
        Self {
            plan,
            cursor: 0,
            editing: false,
            edit_buffer: String::new(),
        }
    }

    /// Move cursor down (wraps at bottom).
    pub fn move_down(&mut self) {
        if !self.plan.steps.is_empty() {
            self.cursor = (self.cursor + 1) % self.plan.steps.len();
        }
    }

    /// Move cursor up (wraps at top).
    pub fn move_up(&mut self) {
        if !self.plan.steps.is_empty() {
            if self.cursor == 0 {
                self.cursor = self.plan.steps.len() - 1;
            } else {
                self.cursor -= 1;
            }
        }
    }

    /// Toggle the completion status of the step under the cursor.
    pub fn toggle_current(&mut self) {
        self.plan.toggle_step(self.cursor);
    }

    /// Enter edit mode for the current step.
    pub fn start_edit(&mut self) {
        if let Some(step) = self.plan.steps.get(self.cursor) {
            self.edit_buffer = step.description.clone();
            self.editing = true;
        }
    }

    /// Cancel editing.
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// Submit the edit and update the step description.
    pub fn submit_edit(&mut self) {
        if let Some(step) = self.plan.steps.get_mut(self.cursor) {
            if !self.edit_buffer.trim().is_empty() {
                step.description = self.edit_buffer.trim().to_string();
            }
        }
        self.editing = false;
        self.edit_buffer.clear();
    }

    /// Insert a character into the edit buffer.
    pub fn edit_insert_char(&mut self, c: char) {
        self.edit_buffer.push(c);
    }

    /// Backspace in the edit buffer.
    pub fn edit_backspace(&mut self) {
        self.edit_buffer.pop();
    }

    /// Render the plan viewer as ANSI-formatted text.
    pub fn render(&self, terminal_width: u16) -> String {
        let w = (terminal_width as usize).max(40);
        let box_width = (w - 4).min(76);
        let inner_width = box_width - 4; // 2 for border chars, 2 for padding

        let border = fgc(BORDER);
        let accent = fgc(ACCENT);
        let fg = fgc(FG);
        let muted = fgc(MUTED);
        let success = fgc(SUCCESS);
        let error = fgc(ERROR);
        let info = fgc(INFO);
        let warning = fgc(WARNING);
        let sel_bg = bgc(SELECTION);

        let pad = 2; // left margin

        let mut out = String::with_capacity(4096);
        out.push_str("\r\n");

        // Status badge
        let (status_color, status_label) = match self.plan.status {
            PlanStatus::Draft => (&warning, "Draft"),
            PlanStatus::Approved => (&success, "Approved"),
            PlanStatus::InProgress => (&info, "In Progress"),
            PlanStatus::Completed => (&success, "Completed"),
            PlanStatus::Cancelled => (&error, "Cancelled"),
        };

        // ── Top border with title and status ──────────────────────────
        let title_text = format!(" Plan: {} ", self.plan.title);
        let status_text = format!(" {} ", status_label);
        let title_len = title_text.chars().count();
        let status_len = status_text.chars().count();
        let fill_len = box_width
            .saturating_sub(title_len + status_len + 2); // +2 for corners

        out.push_str(&format!(
            "{:>pad$}{border}{BOX_TL}{accent}{BOLD}{title_text}{RESET}{border}",
            "",
        ));
        out.push_str(&format!("{}", BOX_H.to_string().repeat(fill_len)));
        out.push_str(&format!(
            "{status_color}{BOLD}{status_text}{RESET}{border}{BOX_TR}{RESET}{CLEAR_EOL}\r\n",
        ));

        // ── Goal ──────────────────────────────────────────────────────
        // Empty line
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_V}{RESET}{:>inner$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
            "",
            "",
            inner = box_width - 2,
        ));

        if !self.plan.goal.is_empty() {
            let goal_label = "Goal: ";
            let goal_text = truncate(&self.plan.goal, inner_width - goal_label.len());
            out.push_str(&format!(
                "{:>pad$}{border}{BOX_V}{RESET} {muted}{goal_label}{fg}{goal_text}",
                "",
            ));
            let used = goal_label.len() + goal_text.chars().count() + 1;
            let remaining = box_width.saturating_sub(used + 2);
            out.push_str(&format!(
                "{:>remaining$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                "",
            ));
        }

        // ── Separator ─────────────────────────────────────────────────
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_V}{RESET}{:>inner$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
            "",
            "",
            inner = box_width - 2,
        ));

        // ── Steps ─────────────────────────────────────────────────────
        let (done, total) = self.plan.progress();
        let steps_header = format!("Steps ({done}/{total}):");
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_V}{RESET} {info}{BOLD}{steps_header}{RESET}",
            "",
        ));
        let used = steps_header.chars().count() + 1;
        let remaining = box_width.saturating_sub(used + 2);
        out.push_str(&format!(
            "{:>remaining$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
            "",
        ));

        for (i, step) in self.plan.steps.iter().enumerate() {
            let is_selected = i == self.cursor;
            let check = if step.completed {
                format!("{success}{CHECK_FILLED}{RESET}")
            } else {
                format!("{muted}{CHECK_EMPTY}{RESET}")
            };

            let desc_color = if step.completed {
                &muted
            } else if is_selected {
                &accent
            } else {
                &fg
            };

            let prefix = if is_selected { "> " } else { "  " };

            // If editing this step, show the edit buffer
            let description = if self.editing && is_selected {
                format!("{}{UNDERLINE}{}{RESET}", fgc(INFO), self.edit_buffer)
            } else {
                let desc_text = truncate(&step.description, inner_width - 8);
                format!("{desc_color}{desc_text}{RESET}")
            };

            let sel_start = if is_selected {
                format!("{sel_bg}")
            } else {
                String::new()
            };
            let sel_end = if is_selected { RESET } else { "" };

            let step_num = format!("{}.", i + 1);

            out.push_str(&format!(
                "{:>pad$}{border}{BOX_V}{RESET}{sel_start} {prefix}{check} {muted}{step_num}{RESET} {description}{sel_end}",
                "",
            ));
            // Fill to right border
            // This is approximate — ANSI escapes make char counting tricky.
            // We pad generously and let CLEAR_EOL handle overflow.
            let visual_len = prefix.len() + 2 + step_num.len() + 1 + visible_len(&step.description).min(inner_width - 8) + 1;
            let fill = box_width.saturating_sub(visual_len + 2);
            out.push_str(&format!(
                "{:>fill$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                "",
            ));

            // Substeps
            for substep in &step.substeps {
                let sub_text = truncate(substep, inner_width - 10);
                let sub_color = if step.completed { &muted } else { &muted };
                out.push_str(&format!(
                    "{:>pad$}{border}{BOX_V}{RESET}      {sub_color}{DIM}- {sub_text}{RESET}",
                    "",
                ));
                let sub_vis = 6 + 2 + visible_len(&sub_text);
                let sub_fill = box_width.saturating_sub(sub_vis + 2);
                out.push_str(&format!(
                    "{:>sub_fill$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                    "",
                ));
            }
        }

        // ── Files ─────────────────────────────────────────────────────
        if !self.plan.files.is_empty() {
            // Empty line
            out.push_str(&format!(
                "{:>pad$}{border}{BOX_V}{RESET}{:>inner$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                "",
                "",
                inner = box_width - 2,
            ));

            out.push_str(&format!(
                "{:>pad$}{border}{BOX_V}{RESET} {info}Files:{RESET}",
                "",
            ));
            let fill = box_width.saturating_sub(8);
            out.push_str(&format!(
                "{:>fill$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                "",
            ));

            for file in &self.plan.files {
                let file_text = truncate(file, inner_width - 4);
                out.push_str(&format!(
                    "{:>pad$}{border}{BOX_V}{RESET}   {muted}`{file_text}`{RESET}",
                    "",
                ));
                let fv = 3 + 1 + visible_len(&file_text) + 1;
                let ff = box_width.saturating_sub(fv + 2);
                out.push_str(&format!(
                    "{:>ff$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
                    "",
                ));
            }
        }

        // ── Empty line ────────────────────────────────────────────────
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_V}{RESET}{:>inner$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
            "",
            "",
            inner = box_width - 2,
        ));

        // ── Key hints ─────────────────────────────────────────────────
        let hints = if self.editing {
            format!(
                " {muted}[{accent}Enter{muted}] Save  [{accent}Esc{muted}] Cancel{RESET}"
            )
        } else {
            format!(
                " {muted}[{accent}Enter{muted}] Approve  [{accent}e{muted}] Edit  [{accent}Space{muted}] Toggle  [{accent}Esc{muted}] Cancel{RESET}"
            )
        };
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_V}{RESET}{hints}",
            "",
        ));
        // Approximate fill for hints
        let hint_vis = if self.editing { 26 } else { 48 };
        let hf = box_width.saturating_sub(hint_vis + 2);
        out.push_str(&format!(
            "{:>hf$}{border}{BOX_V}{RESET}{CLEAR_EOL}\r\n",
            "",
        ));

        // ── Bottom border ─────────────────────────────────────────────
        out.push_str(&format!(
            "{:>pad$}{border}{BOX_BL}{}{BOX_BR}{RESET}{CLEAR_EOL}\r\n",
            "",
            BOX_H.to_string().repeat(box_width - 2),
        ));

        out
    }
}

/// Truncate a string to at most `max_chars` visible characters, appending "..." if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars < 4 {
        return s.chars().take(max_chars).collect();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 3).collect();
        format!("{truncated}...")
    }
}

/// Count visible characters in a string (ASCII approximation).
fn visible_len(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_mode::{PlanStep, PlanDocument};

    fn sample_plan() -> PlanDocument {
        let mut plan = PlanDocument::create("Build REST API", "Create a REST API with auth");
        plan.steps = vec![
            PlanStep {
                description: "Set up project structure".into(),
                completed: false,
                substeps: vec!["Create Cargo.toml".into()],
            },
            PlanStep {
                description: "Implement user model".into(),
                completed: false,
                substeps: vec![],
            },
            PlanStep {
                description: "Add authentication endpoints".into(),
                completed: false,
                substeps: vec![],
            },
            PlanStep {
                description: "Write integration tests".into(),
                completed: false,
                substeps: vec![],
            },
        ];
        plan.files = vec![
            "src/main.rs".into(),
            "src/auth.rs".into(),
            "tests/".into(),
        ];
        plan
    }

    #[test]
    fn test_viewer_navigation() {
        let mut viewer = PlanViewer::new(sample_plan());
        assert_eq!(viewer.cursor, 0);

        viewer.move_down();
        assert_eq!(viewer.cursor, 1);

        viewer.move_down();
        viewer.move_down();
        viewer.move_down(); // wraps
        assert_eq!(viewer.cursor, 0);

        viewer.move_up(); // wraps to last
        assert_eq!(viewer.cursor, 3);
    }

    #[test]
    fn test_viewer_toggle() {
        let mut viewer = PlanViewer::new(sample_plan());
        assert!(!viewer.plan.steps[0].completed);

        viewer.toggle_current();
        assert!(viewer.plan.steps[0].completed);

        viewer.toggle_current();
        assert!(!viewer.plan.steps[0].completed);
    }

    #[test]
    fn test_viewer_edit() {
        let mut viewer = PlanViewer::new(sample_plan());
        assert!(!viewer.editing);

        viewer.start_edit();
        assert!(viewer.editing);
        assert_eq!(viewer.edit_buffer, "Set up project structure");

        viewer.edit_buffer.clear();
        viewer.edit_insert_char('N');
        viewer.edit_insert_char('e');
        viewer.edit_insert_char('w');
        assert_eq!(viewer.edit_buffer, "New");

        viewer.edit_backspace();
        assert_eq!(viewer.edit_buffer, "Ne");

        viewer.submit_edit();
        assert!(!viewer.editing);
        assert_eq!(viewer.plan.steps[0].description, "Ne");
    }

    #[test]
    fn test_viewer_cancel_edit() {
        let mut viewer = PlanViewer::new(sample_plan());
        viewer.start_edit();
        viewer.edit_buffer = "Changed".into();
        viewer.cancel_edit();

        assert!(!viewer.editing);
        assert_eq!(viewer.plan.steps[0].description, "Set up project structure");
    }

    #[test]
    fn test_render_contains_key_elements() {
        let viewer = PlanViewer::new(sample_plan());
        let rendered = viewer.render(80);

        assert!(rendered.contains("Build REST API"));
        assert!(rendered.contains("Draft"));
        assert!(rendered.contains("Set up project structure"));
        assert!(rendered.contains("Implement user model"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("Approve"));
        assert!(rendered.contains("Cancel"));
    }

    #[test]
    fn test_render_editing_mode() {
        let mut viewer = PlanViewer::new(sample_plan());
        viewer.start_edit();
        let rendered = viewer.render(80);
        assert!(rendered.contains("Save"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world foo bar", 10), "hello w...");
        assert_eq!(truncate("ab", 2), "ab");
    }

    #[test]
    fn test_viewer_empty_plan() {
        let plan = PlanDocument::create("Empty", "Nothing here");
        let mut viewer = PlanViewer::new(plan);
        // Navigation on empty should not panic
        viewer.move_down();
        viewer.move_up();
        viewer.toggle_current();

        let rendered = viewer.render(80);
        assert!(rendered.contains("Empty"));
    }
}
