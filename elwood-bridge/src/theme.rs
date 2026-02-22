//! Theme engine for Elwood's ANSI-based TUI rendering.
//!
//! Provides semantic color roles mapped to 24-bit true color ANSI escape
//! sequences. Two built-in themes ship by default: Tokyo Night (dark) and
//! Tokyo Night Light.
//!
//! ## Usage
//!
//! ```rust
//! use elwood_bridge::theme::ElwoodTheme;
//!
//! let theme = ElwoodTheme::tokyo_night();
//! let header = format!("{}{}Header{}", theme.ansi_bg(theme.bg_primary), theme.ansi_bold_fg(theme.accent), theme.reset());
//! ```

/// An RGB color triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    /// Create a color from an RGB tuple.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Create a color from a hex string (without leading `#`).
    /// Panics on invalid input.
    pub fn from_hex(hex: &str) -> Self {
        let hex = hex.trim_start_matches('#');
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
        Self { r, g, b }
    }

    /// Blend this color toward `other` by `amount` (0.0 = self, 1.0 = other).
    pub fn blend(self, other: Color, amount: f32) -> Color {
        let a = amount.clamp(0.0, 1.0);
        let inv = 1.0 - a;
        Color {
            r: (self.r as f32 * inv + other.r as f32 * a) as u8,
            g: (self.g as f32 * inv + other.g as f32 * a) as u8,
            b: (self.b as f32 * inv + other.b as f32 * a) as u8,
        }
    }
}

/// Semantic theme for the Elwood TUI.
///
/// Colors are grouped by role:
/// - **Surfaces**: backgrounds at different elevation levels
/// - **Text**: foreground colors at different emphasis levels
/// - **Accents**: status and interaction colors
/// - **Semantic**: block-type-specific accent colors
#[derive(Debug, Clone)]
pub struct ElwoodTheme {
    pub name: &'static str,

    // ── Surfaces ─────────────────────────────────────────────────────
    /// Main background.
    pub bg_primary: Color,
    /// Block backgrounds, input area, elevated surfaces.
    pub bg_secondary: Color,
    /// Hover, selected states.
    pub bg_tertiary: Color,

    // ── Text ────────────────────────────────────────────────────────
    /// Main text.
    pub fg_primary: Color,
    /// Dimmed text, metadata.
    pub fg_secondary: Color,
    /// Placeholders, hints.
    pub fg_muted: Color,
    /// Bright white for emphasis.
    pub fg_bright: Color,

    // ── Accents ─────────────────────────────────────────────────────
    /// Primary accent (blue).
    pub accent: Color,
    /// Success (green).
    pub success: Color,
    /// Warning (yellow/amber).
    pub warning: Color,
    /// Error (red).
    pub error: Color,
    /// Info (cyan).
    pub info: Color,

    // ── Semantic Block Colors ───────────────────────────────────────
    /// Agent message accent (border/prefix).
    pub agent_accent: Color,
    /// User message accent.
    pub user_accent: Color,
    /// Tool execution accent.
    pub tool_accent: Color,
    /// Block chrome border color.
    pub block_border: Color,
    /// Permission prompt accent.
    pub permission_accent: Color,
    /// Code block background.
    pub code_bg: Color,
}

impl ElwoodTheme {
    // ── ANSI Escape Helpers ─────────────────────────────────────────

    /// 24-bit true color foreground escape sequence.
    pub fn ansi_fg(&self, c: Color) -> String {
        format!("\x1b[38;2;{};{};{}m", c.r, c.g, c.b)
    }

    /// 24-bit true color background escape sequence.
    pub fn ansi_bg(&self, c: Color) -> String {
        format!("\x1b[48;2;{};{};{}m", c.r, c.g, c.b)
    }

    /// Bold + foreground color.
    pub fn ansi_bold_fg(&self, c: Color) -> String {
        format!("\x1b[1;38;2;{};{};{}m", c.r, c.g, c.b)
    }

    /// Dim + foreground color.
    pub fn ansi_dim_fg(&self, c: Color) -> String {
        format!("\x1b[2;38;2;{};{};{}m", c.r, c.g, c.b)
    }

    /// Italic + foreground color.
    pub fn ansi_italic_fg(&self, c: Color) -> String {
        format!("\x1b[3;38;2;{};{};{}m", c.r, c.g, c.b)
    }

    /// Foreground + background combined.
    pub fn ansi_fg_bg(&self, fg: Color, bg: Color) -> String {
        format!(
            "\x1b[38;2;{};{};{};48;2;{};{};{}m",
            fg.r, fg.g, fg.b, bg.r, bg.g, bg.b,
        )
    }

    /// ANSI reset.
    pub fn reset(&self) -> &'static str {
        "\x1b[0m"
    }

    /// Bold.
    pub fn bold(&self) -> &'static str {
        "\x1b[1m"
    }

    /// Dim.
    pub fn dim(&self) -> &'static str {
        "\x1b[2m"
    }

    /// Italic.
    pub fn italic(&self) -> &'static str {
        "\x1b[3m"
    }

    // ── Built-in Themes ─────────────────────────────────────────────

    /// Tokyo Night dark theme (default).
    ///
    /// Inspired by the Tokyo Night color scheme with Warp-compatible semantic
    /// roles for agent, tool, user, and error blocks.
    pub fn tokyo_night() -> Self {
        Self {
            name: "Tokyo Night",

            bg_primary: Color::new(26, 27, 38),       // #1A1B26
            bg_secondary: Color::new(36, 40, 59),      // #24283B
            bg_tertiary: Color::new(40, 44, 66),       // #282C42

            fg_primary: Color::new(192, 202, 245),     // #C0CAF5
            fg_secondary: Color::new(169, 177, 214),   // #A9B1D6
            fg_muted: Color::new(86, 95, 137),         // #565F89
            fg_bright: Color::new(220, 225, 252),      // #DCE1FC

            accent: Color::new(122, 162, 247),         // #7AA2F7
            success: Color::new(158, 206, 106),        // #9ECE6A
            warning: Color::new(224, 175, 104),        // #E0AF68
            error: Color::new(247, 118, 142),          // #F7768E
            info: Color::new(125, 207, 255),           // #7DCFFF

            agent_accent: Color::new(122, 162, 247),   // #7AA2F7 (blue)
            user_accent: Color::new(158, 206, 106),    // #9ECE6A (green)
            tool_accent: Color::new(187, 154, 247),    // #BB9AF7 (purple)
            block_border: Color::new(59, 66, 97),      // #3B4261
            permission_accent: Color::new(224, 175, 104), // #E0AF68 (amber)
            code_bg: Color::new(31, 35, 53),           // #1F2335
        }
    }

    /// Tokyo Night Light theme.
    pub fn tokyo_night_light() -> Self {
        Self {
            name: "Tokyo Night Light",

            bg_primary: Color::new(213, 214, 219),     // #D5D6DB
            bg_secondary: Color::new(224, 225, 230),    // #E0E1E6
            bg_tertiary: Color::new(200, 201, 210),     // #C8C9D2

            fg_primary: Color::new(52, 59, 88),        // #343B58
            fg_secondary: Color::new(107, 111, 143),   // #6B6F8F
            fg_muted: Color::new(139, 143, 160),       // #8B8FA0
            fg_bright: Color::new(26, 27, 38),         // #1A1B26

            accent: Color::new(46, 89, 168),           // #2E59A8
            success: Color::new(72, 117, 43),          // #48752B
            warning: Color::new(143, 102, 24),         // #8F6618
            error: Color::new(140, 44, 64),            // #8C2C40
            info: Color::new(16, 104, 152),            // #106898

            agent_accent: Color::new(46, 89, 168),     // #2E59A8
            user_accent: Color::new(72, 117, 43),      // #48752B
            tool_accent: Color::new(113, 73, 168),     // #7149A8
            block_border: Color::new(192, 196, 208),   // #C0C4D0
            permission_accent: Color::new(143, 102, 24), // #8F6618
            code_bg: Color::new(232, 232, 236),        // #E8E8EC
        }
    }

    /// Get a theme by name. Falls back to tokyo_night for unknown names.
    pub fn by_name(name: &str) -> Self {
        match name {
            "tokyo_night" | "Tokyo Night" | "dark" => Self::tokyo_night(),
            "tokyo_night_light" | "Tokyo Night Light" | "light" => Self::tokyo_night_light(),
            _ => Self::tokyo_night(),
        }
    }
}

impl Default for ElwoodTheme {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

// ── Spinner frames ──────────────────────────────────────────────────────

/// Braille dot spinner frames for tool execution indicators.
pub const SPINNER_FRAMES: &[&str] = &[
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}",
    "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

/// Block-element progress bar characters.
pub const PROGRESS_FULL: char = '\u{2588}';  // Full block
pub const PROGRESS_EMPTY: char = '\u{2591}'; // Light shade

/// Streaming/thinking indicator (pulsing dot).
pub const THINKING_DOT: &str = "\u{25CF}"; // Black circle

/// Format a progress bar string.
///
/// ```rust
/// use elwood_bridge::theme::format_progress_bar;
/// let bar = format_progress_bar(50, 20);
/// assert!(bar.contains('\u{2588}')); // Full blocks
/// assert!(bar.contains('\u{2591}')); // Empty blocks
/// ```
pub fn format_progress_bar(percent: u8, width: usize) -> String {
    let pct = (percent as usize).min(100);
    let filled = (pct * width) / 100;
    let empty = width.saturating_sub(filled);
    let mut bar = String::with_capacity(width + 10);
    for _ in 0..filled {
        bar.push(PROGRESS_FULL);
    }
    for _ in 0..empty {
        bar.push(PROGRESS_EMPTY);
    }
    bar.push_str(&format!(" {}%", pct));
    bar
}

/// Get the spinner frame for a given tick count.
pub fn spinner_frame(tick: usize) -> &'static str {
    SPINNER_FRAMES[tick % SPINNER_FRAMES.len()]
}

// ── Box Drawing Constants ───────────────────────────────────────────────

/// Rounded corner box drawing characters.
pub mod box_chars {
    pub const TL: char = '\u{256D}'; // Top-left rounded
    pub const TR: char = '\u{256E}'; // Top-right rounded
    pub const BL: char = '\u{2570}'; // Bottom-left rounded
    pub const BR: char = '\u{256F}'; // Bottom-right rounded
    pub const H: char = '\u{2500}';  // Horizontal
    pub const V: char = '\u{2502}';  // Vertical
    pub const DOUBLE_H: char = '\u{2550}'; // Double horizontal

    /// Build a horizontal line of `width` characters.
    pub fn hline(width: usize) -> String {
        std::iter::repeat(H).take(width).collect()
    }

    /// Build a top border: `TL` + fill + `TR`.
    pub fn top_border(width: usize) -> String {
        let inner = width.saturating_sub(2);
        format!("{TL}{}{TR}", hline(inner))
    }

    /// Build a bottom border: `BL` + fill + `BR`.
    pub fn bottom_border(width: usize) -> String {
        let inner = width.saturating_sub(2);
        format!("{BL}{}{BR}", hline(inner))
    }

    /// Build a top border with an embedded title:
    /// `TL + H + title + fill + TR`
    pub fn top_border_with_title(title: &str, width: usize) -> String {
        let title_len = title.chars().count();
        let fill_len = width.saturating_sub(title_len + 3); // TL + H + title + TR
        let fill: String = std::iter::repeat(H).take(fill_len).collect();
        format!("{TL}{H}{title}{fill}{TR}")
    }

    /// Build a bottom border with an embedded footer (right-aligned):
    /// `BL + fill + footer + BR`
    pub fn bottom_border_with_footer(footer: &str, footer_visible_len: usize, width: usize) -> String {
        let fill_len = width.saturating_sub(footer_visible_len + 2); // BL + BR
        let fill: String = std::iter::repeat(H).take(fill_len).collect();
        format!("{BL}{fill}{footer}{BR}")
    }
}

/// Status icons.
pub mod icons {
    pub const CHECK: &str = "\u{2714}";   // Heavy check mark
    pub const CROSS: &str = "\u{2718}";   // Heavy ballot X
    pub const GEAR: &str = "\u{2699}";    // Gear
    pub const ARROW_R: &str = "\u{25B8}"; // Right-pointing small triangle
    pub const COLLAPSED: &str = "\u{25B8}"; // Same as arrow for collapsed state
    pub const EXPANDED: &str = "\u{25BE}"; // Down-pointing small triangle
    pub const LIGHTNING: &str = "\u{26A1}"; // Lightning bolt (for quick actions)
    pub const INFO_BRACKET: &str = "[!]";
    pub const NEXT_BRACKET: &str = "[>]";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_new() {
        let c = Color::new(255, 128, 0);
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 128);
        assert_eq!(c.b, 0);
    }

    #[test]
    fn test_color_from_hex() {
        let c = Color::from_hex("#7AA2F7");
        assert_eq!(c.r, 122);
        assert_eq!(c.g, 162);
        assert_eq!(c.b, 247);
    }

    #[test]
    fn test_color_from_hex_no_hash() {
        let c = Color::from_hex("9ECE6A");
        assert_eq!(c.r, 158);
        assert_eq!(c.g, 206);
        assert_eq!(c.b, 106);
    }

    #[test]
    fn test_color_blend() {
        let black = Color::new(0, 0, 0);
        let white = Color::new(255, 255, 255);
        let mid = black.blend(white, 0.5);
        assert_eq!(mid.r, 127);
        assert_eq!(mid.g, 127);
        assert_eq!(mid.b, 127);
    }

    #[test]
    fn test_color_blend_clamp() {
        let a = Color::new(100, 100, 100);
        let b = Color::new(200, 200, 200);
        let result = a.blend(b, 1.5); // Over 1.0, should clamp
        assert_eq!(result, b);
    }

    #[test]
    fn test_tokyo_night_defaults() {
        let theme = ElwoodTheme::tokyo_night();
        assert_eq!(theme.name, "Tokyo Night");
        assert_eq!(theme.bg_primary, Color::new(26, 27, 38));
        assert_eq!(theme.accent, Color::new(122, 162, 247));
    }

    #[test]
    fn test_tokyo_night_light_distinct() {
        let dark = ElwoodTheme::tokyo_night();
        let light = ElwoodTheme::tokyo_night_light();
        assert_ne!(dark.bg_primary, light.bg_primary);
        assert_ne!(dark.fg_primary, light.fg_primary);
    }

    #[test]
    fn test_default_is_tokyo_night() {
        let theme = ElwoodTheme::default();
        assert_eq!(theme.name, "Tokyo Night");
    }

    #[test]
    fn test_by_name() {
        let dark = ElwoodTheme::by_name("dark");
        assert_eq!(dark.name, "Tokyo Night");
        let light = ElwoodTheme::by_name("light");
        assert_eq!(light.name, "Tokyo Night Light");
        let fallback = ElwoodTheme::by_name("unknown_theme");
        assert_eq!(fallback.name, "Tokyo Night");
    }

    #[test]
    fn test_ansi_fg() {
        let theme = ElwoodTheme::default();
        let seq = theme.ansi_fg(Color::new(122, 162, 247));
        assert_eq!(seq, "\x1b[38;2;122;162;247m");
    }

    #[test]
    fn test_ansi_bg() {
        let theme = ElwoodTheme::default();
        let seq = theme.ansi_bg(Color::new(26, 27, 38));
        assert_eq!(seq, "\x1b[48;2;26;27;38m");
    }

    #[test]
    fn test_ansi_bold_fg() {
        let theme = ElwoodTheme::default();
        let seq = theme.ansi_bold_fg(Color::new(100, 200, 50));
        assert_eq!(seq, "\x1b[1;38;2;100;200;50m");
    }

    #[test]
    fn test_ansi_fg_bg() {
        let theme = ElwoodTheme::default();
        let seq = theme.ansi_fg_bg(Color::new(255, 255, 255), Color::new(0, 0, 0));
        assert_eq!(seq, "\x1b[38;2;255;255;255;48;2;0;0;0m");
    }

    #[test]
    fn test_reset() {
        let theme = ElwoodTheme::default();
        assert_eq!(theme.reset(), "\x1b[0m");
    }

    #[test]
    fn test_spinner_frame() {
        assert_eq!(spinner_frame(0), "\u{280B}");
        assert_eq!(spinner_frame(1), "\u{2819}");
        // Wraps around
        assert_eq!(spinner_frame(10), spinner_frame(0));
    }

    #[test]
    fn test_format_progress_bar() {
        let bar = format_progress_bar(50, 10);
        assert!(bar.contains(PROGRESS_FULL));
        assert!(bar.contains(PROGRESS_EMPTY));
        assert!(bar.contains("50%"));
    }

    #[test]
    fn test_format_progress_bar_full() {
        let bar = format_progress_bar(100, 10);
        let full_count = bar.chars().filter(|&c| c == PROGRESS_FULL).count();
        assert_eq!(full_count, 10);
    }

    #[test]
    fn test_format_progress_bar_empty() {
        let bar = format_progress_bar(0, 10);
        let empty_count = bar.chars().filter(|&c| c == PROGRESS_EMPTY).count();
        assert_eq!(empty_count, 10);
    }

    #[test]
    fn test_box_chars_hline() {
        let line = box_chars::hline(5);
        assert_eq!(line, "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");
    }

    #[test]
    fn test_box_chars_top_border() {
        let border = box_chars::top_border(10);
        assert!(border.starts_with('\u{256D}'));
        assert!(border.ends_with('\u{256E}'));
        assert_eq!(border.chars().count(), 10);
    }

    #[test]
    fn test_box_chars_bottom_border() {
        let border = box_chars::bottom_border(10);
        assert!(border.starts_with('\u{2570}'));
        assert!(border.ends_with('\u{256F}'));
    }

    #[test]
    fn test_box_chars_top_border_with_title() {
        let border = box_chars::top_border_with_title(" Agent ", 30);
        assert!(border.starts_with('\u{256D}'));
        assert!(border.contains("Agent"));
        assert!(border.ends_with('\u{256E}'));
    }
}
