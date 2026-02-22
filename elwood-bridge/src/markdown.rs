//! Rich markdown rendering for terminal output.
//!
//! Converts markdown text to ANSI escape sequences for display in the terminal.
//! Supports code blocks, headers, lists, tables, bold/italic, links, and blockquotes.
//!
//! Uses the TokyoNight color palette consistent with the rest of the Elwood TUI.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, CodeBlockKind};

// ─── ANSI Constants (TokyoNight palette) ────────────────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const ITALIC: &str = "\x1b[3m";
const UNDERLINE: &str = "\x1b[4m";
const REVERSE: &str = "\x1b[7m";

// TokyoNight true-color helpers
fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn bg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

// Palette colors
const FG: (u8, u8, u8) = (192, 202, 245);     // #c0caf5
const ACCENT: (u8, u8, u8) = (122, 162, 247);  // #7aa2f7
const MUTED: (u8, u8, u8) = (86, 95, 137);     // #565f89
const CYAN: (u8, u8, u8) = (125, 207, 255);    // #7dcfff
const CODE_BG: (u8, u8, u8) = (36, 40, 59);    // #24283b
const BORDER: (u8, u8, u8) = (59, 66, 97);     // #3b4261

fn fgc(c: (u8, u8, u8)) -> String { fg(c.0, c.1, c.2) }
fn bgc(c: (u8, u8, u8)) -> String { bg(c.0, c.1, c.2) }

// ─── Public API ─────────────────────────────────────────────────────────

/// Render markdown text to ANSI-escaped terminal output.
///
/// Parses `text` as CommonMark and converts each element to styled ANSI
/// escape sequences suitable for display in a VT100/xterm terminal.
pub fn render_markdown(text: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(text, opts);
    let mut renderer = AnsiRenderer::new();
    renderer.render(parser);
    renderer.output
}

/// Heuristic detection of whether text contains markdown formatting.
///
/// Returns `true` if the text appears to contain markdown syntax such as
/// fenced code blocks, headers, or list items. Used to decide whether to
/// pass text through [`render_markdown`] or display it as plain text.
pub fn is_markdown(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            return true;
        }
        if trimmed.starts_with("## ") || trimmed.starts_with("### ") || trimmed.starts_with("# ") {
            return true;
        }
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            return true;
        }
        if trimmed.starts_with("> ") {
            return true;
        }
        if trimmed.starts_with("| ") && trimmed.ends_with('|') {
            return true;
        }
        // Ordered list: "1. ", "2. ", etc.
        if let Some(pos) = trimmed.find(". ") {
            if pos > 0 && pos <= 3 && trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    // Inline: check for **bold** or `code`
    if text.contains("**") || text.contains("``") {
        return true;
    }
    false
}

// ─── Renderer State Machine ─────────────────────────────────────────────

struct AnsiRenderer {
    output: String,
    /// Bold nesting depth.
    bold_depth: usize,
    /// Italic nesting depth.
    italic_depth: usize,
    /// Inside a fenced code block.
    in_code_block: bool,
    /// Language label for current code block.
    code_lang: String,
    /// Accumulated code block content.
    code_buffer: String,
    /// Current list nesting depth (0 = not in list).
    list_depth: usize,
    /// Stack of list types: Some(start_number) for ordered, None for unordered.
    list_stack: Vec<Option<u64>>,
    /// Current item index within each list level.
    list_counters: Vec<u64>,
    /// Inside a blockquote.
    in_blockquote: bool,
    /// Blockquote text accumulator.
    quote_buffer: String,
    /// Inside a heading.
    heading_level: Option<u8>,
    /// Whether we just ended a block element (for paragraph spacing).
    needs_newline: bool,
    /// Link URL accumulator.
    link_url: Option<String>,
    /// Table state.
    in_table: bool,
    table_head: bool,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
}

impl AnsiRenderer {
    fn new() -> Self {
        Self {
            output: String::with_capacity(4096),
            bold_depth: 0,
            italic_depth: 0,
            in_code_block: false,
            code_lang: String::new(),
            code_buffer: String::new(),
            list_depth: 0,
            list_stack: Vec::new(),
            list_counters: Vec::new(),
            in_blockquote: false,
            quote_buffer: String::new(),
            heading_level: None,
            needs_newline: false,
            link_url: None,
            in_table: false,
            table_head: false,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
        }
    }

    fn render(&mut self, parser: Parser<'_>) {
        for event in parser {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(tag) => self.end_tag(tag),
                Event::Text(text) => self.text(&text),
                Event::Code(code) => self.code_span(&code),
                Event::SoftBreak => self.soft_break(),
                Event::HardBreak => self.hard_break(),
                Event::Rule => self.horizontal_rule(),
                _ => {}
            }
        }
    }

    fn ensure_newline(&mut self) {
        if self.needs_newline {
            self.output.push_str("\r\n");
            self.needs_newline = false;
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.ensure_newline();
                let lvl = match level {
                    pulldown_cmark::HeadingLevel::H1 => 1,
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    pulldown_cmark::HeadingLevel::H4 => 4,
                    pulldown_cmark::HeadingLevel::H5 => 5,
                    pulldown_cmark::HeadingLevel::H6 => 6,
                };
                self.heading_level = Some(lvl);
                // Start heading style
                match lvl {
                    1 => self.output.push_str(&format!("{BOLD}{UNDERLINE}{}", fgc(ACCENT))),
                    2 => self.output.push_str(&format!("{BOLD}{}", fgc(ACCENT))),
                    3 => self.output.push_str(&format!("{BOLD}{DIM}{}", fgc(FG))),
                    _ => self.output.push_str(&format!("{BOLD}{}", fgc(FG))),
                }
            }

            Tag::Paragraph => {
                self.ensure_newline();
            }

            Tag::Strong => {
                self.bold_depth += 1;
                if !self.in_table {
                    self.output.push_str(BOLD);
                } else {
                    self.current_cell.push_str(BOLD);
                }
            }

            Tag::Emphasis => {
                self.italic_depth += 1;
                if !self.in_table {
                    self.output.push_str(ITALIC);
                } else {
                    self.current_cell.push_str(ITALIC);
                }
            }

            Tag::CodeBlock(kind) => {
                self.ensure_newline();
                self.in_code_block = true;
                self.code_buffer.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
            }

            Tag::List(start) => {
                if self.list_depth == 0 {
                    self.ensure_newline();
                }
                self.list_depth += 1;
                self.list_stack.push(start);
                self.list_counters.push(start.unwrap_or(0));
            }

            Tag::Item => {
                // Render the bullet/number prefix
                let indent = "  ".repeat(self.list_depth);
                let bullet = if let Some(Some(_start)) = self.list_stack.last() {
                    // Ordered list
                    let counter = self.list_counters.last().copied().unwrap_or(1);
                    format!("{counter}.")
                } else {
                    "\u{2022}".to_string() // bullet •
                };
                self.output.push_str(&format!("{indent}{}{bullet}{RESET} ", fgc(MUTED)));
            }

            Tag::BlockQuote => {
                self.ensure_newline();
                self.in_blockquote = true;
                self.quote_buffer.clear();
            }

            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.output.push_str(UNDERLINE);
            }

            Tag::Table(_) => {
                self.ensure_newline();
                self.in_table = true;
                self.table_rows.clear();
                self.table_head = false;
            }

            Tag::TableHead => {
                self.table_head = true;
                self.current_row.clear();
            }

            Tag::TableRow => {
                self.current_row.clear();
            }

            Tag::TableCell => {
                self.current_cell.clear();
            }

            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_level) => {
                self.output.push_str(RESET);
                self.output.push_str("\r\n");
                self.needs_newline = true;
                self.heading_level = None;
            }

            TagEnd::Paragraph => {
                if self.in_blockquote {
                    // Will be flushed when blockquote ends
                } else {
                    self.output.push_str(RESET);
                    self.output.push_str("\r\n");
                    self.needs_newline = true;
                }
            }

            TagEnd::Strong => {
                self.bold_depth = self.bold_depth.saturating_sub(1);
                if !self.in_table {
                    self.output.push_str(RESET);
                    // Restore italic if still active
                    if self.italic_depth > 0 {
                        self.output.push_str(ITALIC);
                    }
                } else {
                    self.current_cell.push_str(RESET);
                }
            }

            TagEnd::Emphasis => {
                self.italic_depth = self.italic_depth.saturating_sub(1);
                if !self.in_table {
                    self.output.push_str(RESET);
                    // Restore bold if still active
                    if self.bold_depth > 0 {
                        self.output.push_str(BOLD);
                    }
                } else {
                    self.current_cell.push_str(RESET);
                }
            }

            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.render_code_block();
                self.needs_newline = true;
            }

            TagEnd::List(_ordered) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.list_stack.pop();
                self.list_counters.pop();
                if self.list_depth == 0 {
                    self.needs_newline = true;
                }
            }

            TagEnd::Item => {
                self.output.push_str("\r\n");
                // Increment ordered list counter
                if let Some(counter) = self.list_counters.last_mut() {
                    *counter += 1;
                }
            }

            TagEnd::BlockQuote => {
                self.in_blockquote = false;
                self.render_blockquote();
                self.needs_newline = true;
            }

            TagEnd::Link => {
                self.output.push_str(RESET);
                if let Some(url) = self.link_url.take() {
                    self.output.push_str(&format!(" {DIM}{}", fgc(MUTED)));
                    self.output.push_str(&format!("({url})"));
                    self.output.push_str(RESET);
                }
            }

            TagEnd::Table => {
                self.in_table = false;
                self.render_table();
                self.needs_newline = true;
            }

            TagEnd::TableHead => {
                self.table_head = false;
                self.table_rows.push(self.current_row.clone());
            }

            TagEnd::TableRow => {
                self.table_rows.push(self.current_row.clone());
            }

            TagEnd::TableCell => {
                self.current_row.push(self.current_cell.clone());
            }

            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.in_code_block {
            self.code_buffer.push_str(text);
            return;
        }

        if self.in_blockquote {
            self.quote_buffer.push_str(text);
            return;
        }

        if self.in_table {
            self.current_cell.push_str(text);
            return;
        }

        // Apply foreground color for normal text
        self.output.push_str(&fgc(FG));
        self.output.push_str(text);
        if self.bold_depth == 0 && self.italic_depth == 0 && self.heading_level.is_none() {
            self.output.push_str(RESET);
        }
    }

    fn code_span(&mut self, code: &str) {
        if self.in_table {
            self.current_cell.push_str(&format!(
                "{REVERSE}{DIM}{code}{RESET}",
            ));
            return;
        }
        // Inline code: reverse video + dim
        self.output.push_str(&format!(
            "{REVERSE}{DIM}{code}{RESET}",
        ));
    }

    fn soft_break(&mut self) {
        if self.in_blockquote {
            self.quote_buffer.push(' ');
        } else if self.in_table {
            self.current_cell.push(' ');
        } else {
            self.output.push(' ');
        }
    }

    fn hard_break(&mut self) {
        if self.in_blockquote {
            self.quote_buffer.push('\n');
        } else {
            self.output.push_str("\r\n");
        }
    }

    fn horizontal_rule(&mut self) {
        self.ensure_newline();
        let line: String = std::iter::repeat('\u{2500}').take(80).collect();
        self.output.push_str(&format!("{}{line}{RESET}\r\n", fgc(BORDER)));
        self.needs_newline = true;
    }

    // ─── Block Renderers ────────────────────────────────────────────

    fn render_code_block(&mut self) {
        let border = fgc(BORDER);
        let muted = fgc(MUTED);
        let code_bg = bgc(CODE_BG);
        let code_fg = fgc(CYAN);

        // Language label
        if !self.code_lang.is_empty() {
            self.output.push_str(&format!(
                "  {DIM}{muted}{}{RESET}\r\n",
                self.code_lang,
            ));
        }

        // Render each line with indent + dim vertical bar
        let content = self.code_buffer.trim_end_matches('\n');
        for line in content.lines() {
            self.output.push_str(&format!(
                "  {border}\u{2502}{RESET} {code_bg}{code_fg}{line}{RESET}\r\n",
            ));
        }
    }

    fn render_blockquote(&mut self) {
        let border = fgc(BORDER);
        let muted = fgc(MUTED);

        for line in self.quote_buffer.lines() {
            self.output.push_str(&format!(
                "  {border}\u{2502}{RESET} {DIM}{muted}{line}{RESET}\r\n",
            ));
        }
    }

    fn render_table(&mut self) {
        if self.table_rows.is_empty() {
            return;
        }

        // Calculate column widths
        let num_cols = self.table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return;
        }

        let mut col_widths = vec![0usize; num_cols];
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                let visible_len = strip_ansi_len(cell);
                if visible_len > col_widths[i] {
                    col_widths[i] = visible_len;
                }
            }
        }

        let border = fgc(BORDER);

        // Top border: ┌──────┬──────┐
        self.output.push_str(&border);
        self.output.push('\u{250C}'); // ┌
        for (i, &w) in col_widths.iter().enumerate() {
            let fill: String = std::iter::repeat('\u{2500}').take(w + 2).collect();
            self.output.push_str(&fill);
            if i < num_cols - 1 {
                self.output.push('\u{252C}'); // ┬
            }
        }
        self.output.push('\u{2510}'); // ┐
        self.output.push_str(RESET);
        self.output.push_str("\r\n");

        for (row_idx, row) in self.table_rows.iter().enumerate() {
            // Data row: │ cell │ cell │
            self.output.push_str(&border);
            self.output.push('\u{2502}'); // │
            self.output.push_str(RESET);
            for (i, &w) in col_widths.iter().enumerate() {
                let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
                let visible_len = strip_ansi_len(cell);
                let pad = w.saturating_sub(visible_len);
                if row_idx == 0 {
                    // Header row: bold
                    self.output.push_str(&format!(" {BOLD}{cell}{RESET}"));
                } else {
                    self.output.push_str(&format!(" {}{cell}{RESET}", fgc(FG)));
                }
                for _ in 0..pad {
                    self.output.push(' ');
                }
                self.output.push(' ');
                self.output.push_str(&border);
                self.output.push('\u{2502}'); // │
                self.output.push_str(RESET);
            }
            self.output.push_str("\r\n");

            // Separator after header: ├──────┼──────┤
            if row_idx == 0 && self.table_rows.len() > 1 {
                self.output.push_str(&border);
                self.output.push('\u{251C}'); // ├
                for (i, &w) in col_widths.iter().enumerate() {
                    let fill: String = std::iter::repeat('\u{2500}').take(w + 2).collect();
                    self.output.push_str(&fill);
                    if i < num_cols - 1 {
                        self.output.push('\u{253C}'); // ┼
                    }
                }
                self.output.push('\u{2524}'); // ┤
                self.output.push_str(RESET);
                self.output.push_str("\r\n");
            }
        }

        // Bottom border: └──────┴──────┘
        self.output.push_str(&border);
        self.output.push('\u{2514}'); // └
        for (i, &w) in col_widths.iter().enumerate() {
            let fill: String = std::iter::repeat('\u{2500}').take(w + 2).collect();
            self.output.push_str(&fill);
            if i < num_cols - 1 {
                self.output.push('\u{2534}'); // ┴
            }
        }
        self.output.push('\u{2518}'); // ┘
        self.output.push_str(RESET);
        self.output.push_str("\r\n");
    }
}

/// Approximate visible length of a string (strip ANSI escape codes).
fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            len += 1;
        }
    }
    len
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip all ANSI escape sequences for easier assertion.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for ch in s.chars() {
            if ch == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if ch == 'm' {
                    in_escape = false;
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn test_render_header_h1() {
        let output = render_markdown("# Hello World");
        assert!(output.contains(BOLD));
        assert!(output.contains(UNDERLINE));
        let plain = strip_ansi(&output);
        assert!(plain.contains("Hello World"));
    }

    #[test]
    fn test_render_header_h2() {
        let output = render_markdown("## Section Two");
        assert!(output.contains(BOLD));
        // H2 has bold but NOT underline
        let plain = strip_ansi(&output);
        assert!(plain.contains("Section Two"));
    }

    #[test]
    fn test_render_header_h3() {
        let output = render_markdown("### Subsection");
        assert!(output.contains(BOLD));
        assert!(output.contains(DIM));
        let plain = strip_ansi(&output);
        assert!(plain.contains("Subsection"));
    }

    #[test]
    fn test_render_bold() {
        let output = render_markdown("This is **bold** text");
        assert!(output.contains(BOLD));
        let plain = strip_ansi(&output);
        assert!(plain.contains("bold"));
    }

    #[test]
    fn test_render_italic() {
        let output = render_markdown("This is *italic* text");
        assert!(output.contains(ITALIC));
        let plain = strip_ansi(&output);
        assert!(plain.contains("italic"));
    }

    #[test]
    fn test_render_bold_italic() {
        let output = render_markdown("This is ***bold italic*** text");
        assert!(output.contains(BOLD));
        assert!(output.contains(ITALIC));
        let plain = strip_ansi(&output);
        assert!(plain.contains("bold italic"));
    }

    #[test]
    fn test_render_code_span() {
        let output = render_markdown("Use the `println!` macro");
        assert!(output.contains(REVERSE));
        assert!(output.contains(DIM));
        let plain = strip_ansi(&output);
        assert!(plain.contains("println!"));
    }

    #[test]
    fn test_render_code_block() {
        let input = "```\nfn main() {\n    println!(\"hello\");\n}\n```";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        assert!(plain.contains("fn main()"));
        assert!(plain.contains("println!"));
        // Should have vertical bar prefix
        assert!(output.contains("\u{2502}"));
    }

    #[test]
    fn test_render_code_block_with_language() {
        let input = "```rust\nlet x = 42;\n```";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        assert!(plain.contains("rust"));
        assert!(plain.contains("let x = 42;"));
    }

    #[test]
    fn test_render_unordered_list() {
        let input = "- Item one\n- Item two\n- Item three";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        assert!(plain.contains("\u{2022}")); // bullet
        assert!(plain.contains("Item one"));
        assert!(plain.contains("Item two"));
        assert!(plain.contains("Item three"));
    }

    #[test]
    fn test_render_ordered_list() {
        let input = "1. First\n2. Second\n3. Third";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        assert!(plain.contains("1."));
        assert!(plain.contains("2."));
        assert!(plain.contains("3."));
        assert!(plain.contains("First"));
    }

    #[test]
    fn test_render_nested_list() {
        // CommonMark nested list: continuation indent must align past the
        // parent bullet (2 spaces for `- ` prefix).
        let input = "- Outer\n  - Inner\n  - Inner2\n- Outer2";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        // All items should appear in the output
        assert!(plain.contains("Outer"));
        assert!(plain.contains("Inner"));
        assert!(plain.contains("Outer2"));
        // Bullet characters should be present
        assert!(plain.contains("\u{2022}")); // bullet •
    }

    #[test]
    fn test_render_blockquote() {
        let input = "> This is a quote";
        let output = render_markdown(input);
        assert!(output.contains("\u{2502}")); // vertical bar
        assert!(output.contains(DIM));
        let plain = strip_ansi(&output);
        assert!(plain.contains("This is a quote"));
    }

    #[test]
    fn test_render_link() {
        let input = "[Rust](https://www.rust-lang.org)";
        let output = render_markdown(input);
        assert!(output.contains(UNDERLINE));
        let plain = strip_ansi(&output);
        assert!(plain.contains("Rust"));
        assert!(plain.contains("(https://www.rust-lang.org)"));
    }

    #[test]
    fn test_render_horizontal_rule() {
        let input = "Above\n\n---\n\nBelow";
        let output = render_markdown(input);
        assert!(output.contains("\u{2500}")); // horizontal line char
        let plain = strip_ansi(&output);
        assert!(plain.contains("Above"));
        assert!(plain.contains("Below"));
    }

    #[test]
    fn test_render_table() {
        let input = "| Header 1 | H2 |\n|---|---|\n| data | value |";
        let output = render_markdown(input);
        let plain = strip_ansi(&output);
        assert!(plain.contains("Header 1"));
        assert!(plain.contains("data"));
        assert!(plain.contains("value"));
        // Box drawing
        assert!(output.contains("\u{250C}")); // ┌
        assert!(output.contains("\u{2518}")); // ┘
        assert!(output.contains("\u{2502}")); // │
        assert!(output.contains("\u{2500}")); // ─
    }

    #[test]
    fn test_is_markdown_positive() {
        assert!(is_markdown("# Hello"));
        assert!(is_markdown("## Section"));
        assert!(is_markdown("```rust\ncode\n```"));
        assert!(is_markdown("- item one\n- item two"));
        assert!(is_markdown("> quote"));
        assert!(is_markdown("| col1 | col2 |"));
        assert!(is_markdown("1. first item"));
        assert!(is_markdown("This has **bold** text"));
    }

    #[test]
    fn test_is_markdown_negative() {
        assert!(!is_markdown("Hello world"));
        assert!(!is_markdown("Just a simple sentence."));
        assert!(!is_markdown("No special formatting here"));
        assert!(!is_markdown("2024-01-15 date format"));
    }

    #[test]
    fn test_render_complex_document() {
        let input = r#"## Agent Response

Here is the analysis of your code:

**Key findings:**

1. The `main` function has a bug on line 42
2. Missing error handling in `process_data`
3. Unused import on line 3

### Code Fix

```rust
fn main() -> Result<(), Box<dyn Error>> {
    let data = process_data()?;
    println!("{data}");
    Ok(())
}
```

> Note: This fix also addresses the unused import warning.

For more details, see the [Rust Book](https://doc.rust-lang.org/book/).

---

| Component | Status |
|-----------|--------|
| main.rs | Fixed |
| lib.rs | OK |

- Run `cargo test` to verify
- Run `cargo clippy` for additional checks
"#;

        let output = render_markdown(input);
        let plain = strip_ansi(&output);

        // Headers
        assert!(plain.contains("Agent Response"));
        assert!(plain.contains("Code Fix"));

        // Bold
        assert!(output.contains(BOLD));
        assert!(plain.contains("Key findings:"));

        // Ordered list
        assert!(plain.contains("1."));
        assert!(plain.contains("2."));

        // Code block
        assert!(plain.contains("fn main()"));
        assert!(plain.contains("process_data"));

        // Blockquote
        assert!(plain.contains("Note:"));

        // Link
        assert!(plain.contains("Rust Book"));
        assert!(plain.contains("doc.rust-lang.org"));

        // Table
        assert!(plain.contains("Component"));
        assert!(plain.contains("Fixed"));

        // Unordered list
        assert!(plain.contains("cargo test"));
        assert!(plain.contains("cargo clippy"));
    }

    #[test]
    fn test_strip_ansi_len() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len("\x1b[1mbold\x1b[0m"), 4);
        assert_eq!(strip_ansi_len(""), 0);
    }

    #[test]
    fn test_render_empty_input() {
        let output = render_markdown("");
        assert!(output.is_empty() || output.chars().all(|c| c.is_whitespace()));
    }

    #[test]
    fn test_render_plain_text_passthrough() {
        let output = render_markdown("Just plain text");
        let plain = strip_ansi(&output);
        assert!(plain.contains("Just plain text"));
    }
}
