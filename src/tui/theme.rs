//! Semantic colour palette (dark tokens), plus bingo's own semantic tokens
//! (diff/code/thinking, etc.). All rendering goes through this one [`Theme`];
//! the style methods return [`SegStyle`].
//!
//! # The three text tiers
//!
//! Every piece of text in the UI sits on one of exactly three rungs, and the
//! rung is chosen by *what the text is for*, never by how faint it happens to
//! look in one terminal:
//!
//! | Slot | Accessor | Job |
//! |---|---|---|
//! | [`Theme::text`] | [`Theme::text()`] | the content itself — the model's prose, the user's words, code, table bodies |
//! | [`Theme::text_secondary`] | [`Theme::dim()`] | text *about* the content — result lines, tool output, diff context, quotes, labels |
//! | [`Theme::text_muted`] | [`Theme::muted()`] | furniture — hints, stamps, counters, rules, borders, the diff gutter |
//!
//! Both presets are fully RGB: named ANSI colours are the terminal's palette,
//! not ours, so a dark theme built from them shifts under the user's colour
//! scheme and stops being a design. [`Theme::downgrade_to_256`] is the single
//! sanctioned exception, and it fires only when the terminal cannot do 24-bit.

use ratatui::style::Color;

use crate::tui::line::SegStyle;

/// Semantic colour token set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Primary text — the content itself (tier 1).
    pub text: Color,
    /// Secondary text — text about the content (tier 2).
    pub text_secondary: Color,
    /// Muted text — chrome and furniture (tier 3).
    pub text_muted: Color,
    /// Primary accent (rgb(215,119,87)).
    pub claude: Color,
    /// Update-hint breathing peak (dark; rgb(232,137,107), the site's --accent-strong).
    pub claude_strong: Color,
    /// Update-hint breathing rest (light; rgb(176,82,39), brand deep orange).
    pub claude_deep: Color,
    /// Update-hint breathing peak (light; rgb(154,74,36)).
    pub claude_deep_strong: Color,
    /// Whether the theme is dark (the update-hint breathing stop picks the dark bright-orange / light deep-orange step).
    pub is_dark: bool,
    /// Permission-dialog accent (rgb(177,185,249)).
    pub permission: Color,
    /// Success (rgb(78,186,101)).
    pub success: Color,
    /// Error (rgb(255,107,128)).
    pub error: Color,
    /// Warning (rgb(255,193,7)).
    pub warning: Color,
    /// Plan mode (rgb(72,150,140)).
    pub plan_mode: Color,
    /// Accept-edits mode (rgb(175,135,255)).
    pub accept_edits: Color,
    /// Input-box bottom border (rgb(136,136,136)).
    pub prompt_border: Color,
    /// Bash-mode accent (rgb(253,93,177)).
    pub bash_border: Color,
    /// User-message bubble background (rgb(55,55,55)).
    pub user_message_bg: Color,
    /// Inline code.
    pub code_fg: Color,
    /// Code-block text.
    pub code_block_fg: Color,
    /// Code-block background.
    pub code_block_bg: Color,
    /// Links.
    pub link: Color,
    /// Math.
    pub math: Color,
    /// Heading colours (by level, 1-based; out of range falls back to the
    /// last entry).
    pub headings: Vec<Color>,
    /// Quote text.
    pub quote: Color,
    /// Quote left bar.
    pub quote_bar: Color,
    /// Completed task marker.
    pub task_done: Color,
    /// Open task marker.
    pub task_open: Color,
    /// List marker.
    pub list_marker: Color,
    /// Table grid lines.
    pub table_border: Color,
    /// Table header.
    pub table_header: Color,
    /// Horizontal rule.
    pub hr: Color,
    /// Footnote.
    pub footnote: Color,
    /// Thinking block (CC: dim italic `∴ Thinking`).
    pub thinking: Color,
    /// Running tool.
    pub tool_running: Color,
    /// Tool output preview.
    pub tool_output: Color,
    /// Diff hunk header.
    pub diff_hunk: Color,
    /// Diff context line.
    pub diff_context: Color,
}

impl Theme {
    /// Dark preset (default).
    ///
    /// Built around the brand orange `#D77757` on the terminal's own near-black
    /// ground: a warm off-white body, a warm neutral grey ladder under it, and
    /// desaturated teal / sky / mauve for the structural tokens, so nothing in a
    /// code block or a diff shouts louder than the prose around it.
    pub fn dark() -> Self {
        Self {
            text: Color::Rgb(235, 231, 226),
            text_secondary: Color::Rgb(154, 148, 141),
            text_muted: Color::Rgb(107, 102, 96),
            claude: Color::Rgb(215, 119, 87),
            claude_strong: Color::Rgb(232, 137, 107),
            claude_deep: Color::Rgb(176, 82, 39),
            claude_deep_strong: Color::Rgb(154, 74, 36),
            is_dark: true,
            permission: Color::Rgb(177, 185, 249),
            success: Color::Rgb(78, 186, 101),
            error: Color::Rgb(255, 107, 128),
            warning: Color::Rgb(255, 193, 7),
            plan_mode: Color::Rgb(72, 150, 140),
            accept_edits: Color::Rgb(175, 135, 255),
            prompt_border: Color::Rgb(136, 136, 136),
            bash_border: Color::Rgb(253, 93, 177),
            user_message_bg: Color::Rgb(55, 55, 55),
            code_fg: Color::Rgb(217, 160, 91),
            code_block_fg: Color::Rgb(170, 170, 170),
            code_block_bg: Color::Rgb(24, 24, 24),
            link: Color::Rgb(127, 167, 217),
            math: Color::Rgb(192, 138, 209),
            headings: vec![
                Color::Rgb(235, 231, 226),
                Color::Rgb(127, 191, 180),
                Color::Rgb(127, 167, 217),
                Color::Rgb(154, 148, 141),
            ],
            quote: Color::Rgb(154, 148, 141),
            quote_bar: Color::Rgb(107, 102, 96),
            task_done: Color::Rgb(78, 186, 101),
            task_open: Color::Rgb(107, 102, 96),
            list_marker: Color::Rgb(154, 148, 141),
            table_border: Color::Rgb(107, 102, 96),
            table_header: Color::Rgb(235, 231, 226),
            hr: Color::Rgb(107, 102, 96),
            footnote: Color::Rgb(107, 102, 96),
            thinking: Color::Rgb(107, 102, 96),
            tool_running: Color::Rgb(127, 191, 180),
            tool_output: Color::Rgb(154, 148, 141),
            diff_hunk: Color::Rgb(127, 191, 180),
            diff_context: Color::Rgb(154, 148, 141),
        }
    }

    /// Light preset (black body text; the orange primary accent stays
    /// orange).
    pub fn light() -> Self {
        Self {
            text: Color::Rgb(0, 0, 0),
            text_secondary: Color::Rgb(102, 102, 102),
            // Retuned from rgb(175,175,175): tier 3 now carries text (hints,
            // stamps, the diff gutter), and 2.3:1 on white was not readable.
            text_muted: Color::Rgb(140, 140, 140),
            claude: Color::Rgb(215, 119, 87),
            claude_strong: Color::Rgb(232, 137, 107),
            claude_deep: Color::Rgb(176, 82, 39),
            claude_deep_strong: Color::Rgb(154, 74, 36),
            is_dark: false,
            permission: Color::Rgb(87, 105, 247),
            success: Color::Rgb(44, 122, 57),
            error: Color::Rgb(171, 43, 63),
            warning: Color::Rgb(150, 108, 30),
            plan_mode: Color::Rgb(0, 102, 102),
            accept_edits: Color::Rgb(135, 0, 255),
            prompt_border: Color::Rgb(153, 153, 153),
            bash_border: Color::Rgb(255, 0, 135),
            user_message_bg: Color::Rgb(240, 240, 240),
            code_fg: Color::Rgb(150, 108, 30),
            code_block_fg: Color::Rgb(60, 60, 60),
            code_block_bg: Color::Rgb(245, 245, 245),
            link: Color::Rgb(0, 90, 180),
            math: Color::Rgb(130, 0, 130),
            headings: vec![
                Color::Rgb(30, 30, 30),
                Color::Rgb(0, 102, 102),
                Color::Rgb(0, 90, 180),
                Color::Rgb(120, 120, 120),
            ],
            quote: Color::Rgb(90, 90, 90),
            quote_bar: Color::Rgb(0, 90, 180),
            task_done: Color::Rgb(44, 122, 57),
            task_open: Color::Rgb(153, 153, 153),
            list_marker: Color::Rgb(0, 102, 102),
            table_border: Color::Rgb(153, 153, 153),
            table_header: Color::Rgb(30, 30, 30),
            hr: Color::Rgb(153, 153, 153),
            footnote: Color::Rgb(120, 120, 120),
            thinking: Color::Rgb(102, 102, 102),
            tool_running: Color::Rgb(0, 102, 102),
            tool_output: Color::Rgb(90, 90, 90),
            diff_hunk: Color::Rgb(0, 102, 102),
            diff_context: Color::Rgb(90, 90, 90),
        }
    }
}

/// Theme setting: auto follows the terminal background brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeSetting {
    Auto,
    Dark,
    Light,
}

impl ThemeSetting {
    /// Parse from the settings string; missing / invalid values fall back to
    /// auto.
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(str::trim).unwrap_or("") {
            "dark" => Self::Dark,
            "light" => Self::Light,
            _ => Self::Auto,
        }
    }
}

impl Theme {
    /// Detect the terminal's truecolor capability
    /// (COLORTERM=truecolor/24bit).
    pub fn terminal_supports_truecolor() -> bool {
        std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false)
    }

    /// Parse `$COLORFGBG` (fg;bg or fg;other;bg, bg as an ANSI colour
    /// number): the rxvt convention is 0-6/8 = dark background, 7/9-15 =
    /// light.
    fn detect_from_color_fgbg() -> Option<bool> {
        let bg = std::env::var("COLORFGBG")
            .ok()?
            .split(';')
            .next_back()
            .and_then(|v| v.parse::<u8>().ok())?;
        if bg > 15 {
            return None;
        }
        Some(bg <= 6 || bg == 8)
    }

    /// Terminal query: write the query sequences to /dev/tty under a
    /// temporary raw mode and read the response back.
    ///
    /// Uses O_NONBLOCK + polling, never blocking at any point; keystrokes may
    /// be consumed during the query (for about the deadline duration, which
    /// is acceptable). Returns `None` without a tty (pipes/tests).
    pub(crate) async fn query_terminal(
        queries: &[&[u8]],
        deadline: std::time::Duration,
    ) -> Option<Vec<u8>> {
        use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
        use std::io::{Read, Write};

        let mut tty = crate::platform::open_tty()?;
        enable_raw_mode().ok()?;
        let mut buf: Vec<u8> = Vec::new();
        let result = async {
            for q in queries {
                tty.write_all(q).ok()?;
            }
            let mut chunk = [0u8; 64];
            let deadline = std::time::Instant::now() + deadline;
            loop {
                match tty.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Some(())
        }
        .await;
        disable_raw_mode().ok();
        result?;
        Some(buf)
    }

    /// Query the terminal's real background colour via OSC 11. Sends
    /// `ESC ] 11 ; ? ESC \`, reads back `rgb:RRRR/GGGG/BBBB`, and judges by
    /// BT.709 relative luminance: > 0.5 means light.
    pub async fn detect_system_theme() -> Option<bool> {
        let buf =
            Self::query_terminal(&[b"\x1b]11;?\x1b\\"], std::time::Duration::from_millis(200))
                .await?;
        theme_from_osc(&buf)
    }

    /// The current terminal's theme: resolved by setting (auto prefers the
    /// OSC 11 measured background, then `$COLORFGBG`, and finally falls back
    /// to dark). Without truecolor support the RGB colours are downgraded to
    /// 256-colour approximations (otherwise the terminal ignores the 24-bit
    /// sequences and all text falls back to the default colour).
    pub fn for_terminal(setting: ThemeSetting, detected: Option<bool>) -> Self {
        let t = match setting {
            ThemeSetting::Dark => Theme::dark(),
            ThemeSetting::Light => Theme::light(),
            ThemeSetting::Auto => match detected.or_else(Self::detect_from_color_fgbg) {
                Some(true) => Theme::dark(),
                Some(false) => Theme::light(),
                None => Theme::dark(),
            },
        };
        if Self::terminal_supports_truecolor() {
            t
        } else {
            t.downgrade_to_256()
        }
    }

    /// Every colour slot, paired with its name — the one enumeration of the
    /// palette. [`Theme::downgrade_to_256`] walks it, and so do the tests that
    /// assert no slot escapes into the terminal's own ANSI palette; a new field
    /// that is not listed here is a field that silently opts out of both.
    fn slots_mut(&mut self) -> Vec<(&'static str, &mut Color)> {
        let mut out: Vec<(&'static str, &mut Color)> = vec![
            ("text", &mut self.text),
            ("text_secondary", &mut self.text_secondary),
            ("text_muted", &mut self.text_muted),
            ("claude", &mut self.claude),
            ("claude_strong", &mut self.claude_strong),
            ("claude_deep", &mut self.claude_deep),
            ("claude_deep_strong", &mut self.claude_deep_strong),
            ("permission", &mut self.permission),
            ("success", &mut self.success),
            ("error", &mut self.error),
            ("warning", &mut self.warning),
            ("plan_mode", &mut self.plan_mode),
            ("accept_edits", &mut self.accept_edits),
            ("prompt_border", &mut self.prompt_border),
            ("bash_border", &mut self.bash_border),
            ("user_message_bg", &mut self.user_message_bg),
            ("code_fg", &mut self.code_fg),
            ("code_block_fg", &mut self.code_block_fg),
            ("code_block_bg", &mut self.code_block_bg),
            ("link", &mut self.link),
            ("math", &mut self.math),
            ("quote", &mut self.quote),
            ("quote_bar", &mut self.quote_bar),
            ("task_done", &mut self.task_done),
            ("task_open", &mut self.task_open),
            ("list_marker", &mut self.list_marker),
            ("table_border", &mut self.table_border),
            ("table_header", &mut self.table_header),
            ("hr", &mut self.hr),
            ("footnote", &mut self.footnote),
            ("thinking", &mut self.thinking),
            ("tool_running", &mut self.tool_running),
            ("tool_output", &mut self.tool_output),
            ("diff_hunk", &mut self.diff_hunk),
            ("diff_context", &mut self.diff_context),
        ];
        out.extend(self.headings.iter_mut().map(|c| ("headings", c)));
        out
    }

    /// Map every RGB colour to a 256-colour (Indexed) approximation.
    fn downgrade_to_256(mut self) -> Self {
        for (_, slot) in self.slots_mut() {
            *slot = to_ansi256(*slot);
        }
        self
    }

    /// Primary text (tier 1) — the content itself.
    pub fn text(&self) -> SegStyle {
        SegStyle::fg(self.text)
    }
    /// Secondary text (tier 2) — text *about* the content: result lines, tool
    /// output, diff context, quotes, labels.
    pub fn dim(&self) -> SegStyle {
        SegStyle::fg(self.text_secondary)
    }
    /// Muted text (tier 3) — furniture: hints, stamps, counters, rules, the
    /// diff gutter. The faintest rung that is still meant to be read.
    pub fn muted(&self) -> SegStyle {
        SegStyle::fg(self.text_muted)
    }
    /// Bold.
    pub fn strong(&self) -> SegStyle {
        SegStyle::plain().bold()
    }
    /// Italic.
    pub fn emphasis(&self) -> SegStyle {
        SegStyle::plain().italic()
    }
    /// Strikethrough (a real `CROSSED_OUT`, with a muted foreground to keep
    /// CC's completed-state look).
    pub fn strikethrough(&self) -> SegStyle {
        SegStyle::fg(self.text_secondary).strikethrough()
    }
    /// Inline code.
    pub fn code(&self) -> SegStyle {
        SegStyle::fg(self.code_fg)
    }
    /// Code block.
    pub fn code_block(&self) -> SegStyle {
        SegStyle::fg(self.code_block_fg).with_bg(self.code_block_bg)
    }
    /// Links (underlined).
    pub fn link(&self) -> SegStyle {
        SegStyle::fg(self.link).underline()
    }
    /// Math (italic).
    pub fn math(&self) -> SegStyle {
        SegStyle::fg(self.math).italic()
    }
    /// Heading (levels start at 1; 0 or out of range falls back).
    pub fn heading(&self, level: u8) -> SegStyle {
        let color = if level == 0 {
            self.text
        } else {
            self.headings
                .get((level - 1) as usize)
                .copied()
                .unwrap_or(*self.headings.last().unwrap_or(&self.text))
        };
        SegStyle::fg(color).bold().underline()
    }
    /// Quote text.
    pub fn quote(&self) -> SegStyle {
        SegStyle::fg(self.quote)
    }
    /// Quote left bar.
    pub fn quote_bar(&self) -> SegStyle {
        SegStyle::fg(self.quote_bar)
    }
    /// Completed task marker.
    pub fn task_done(&self) -> SegStyle {
        SegStyle::fg(self.task_done).bold()
    }
    /// Open task marker.
    pub fn task_open(&self) -> SegStyle {
        SegStyle::fg(self.task_open)
    }
    /// List marker.
    pub fn list_marker(&self) -> SegStyle {
        SegStyle::fg(self.list_marker)
    }
    /// Table grid lines.
    pub fn table_border(&self) -> SegStyle {
        SegStyle::fg(self.table_border)
    }
    /// Table header.
    pub fn table_header(&self) -> SegStyle {
        SegStyle::fg(self.table_header).bold()
    }
    /// Horizontal rule.
    pub fn hr(&self) -> SegStyle {
        SegStyle::fg(self.hr)
    }
    /// Footnote.
    pub fn footnote(&self) -> SegStyle {
        SegStyle::fg(self.footnote)
    }
    /// Thinking block (dim italic).
    pub fn thinking(&self) -> SegStyle {
        SegStyle::fg(self.thinking).italic()
    }
    /// Completed tool.
    pub fn tool_done(&self) -> SegStyle {
        SegStyle::fg(self.success).bold()
    }
    /// Failed tool.
    pub fn tool_error(&self) -> SegStyle {
        SegStyle::fg(self.error).bold()
    }
    /// Tool stopped by the user's interrupt (neither success nor failure).
    pub fn tool_interrupted(&self) -> SegStyle {
        SegStyle::fg(self.warning).bold()
    }
    /// Tool output preview.
    pub fn tool_output(&self) -> SegStyle {
        SegStyle::fg(self.tool_output)
    }
    /// Diff hunk header.
    pub fn diff_hunk(&self) -> SegStyle {
        SegStyle::fg(self.diff_hunk).bold()
    }
    /// Diff addition.
    pub fn diff_added(&self) -> SegStyle {
        SegStyle::fg(self.success)
    }
    /// Diff removal.
    pub fn diff_removed(&self) -> SegStyle {
        SegStyle::fg(self.error)
    }
    /// Diff context.
    pub fn diff_context(&self) -> SegStyle {
        SegStyle::fg(self.diff_context)
    }
    /// Permission title.
    pub fn permission(&self) -> SegStyle {
        SegStyle::fg(self.permission).bold()
    }
}

/// Colour → 256-colour approximation, passing non-RGB colours through. The
/// single downgrade entry point every palette shares (the avatar/conversation
/// skin in [`crate::tui::avatar`] carries its own colours and comes down the
/// same way).
pub(crate) fn to_ansi256(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        c => c,
    }
}

/// RGB → 256-colour (Indexed) approximation: 6×6×6 cube + grey ramp.
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    // Greyscale: r==g==b and not near a cube node
    if r == g && g == b {
        let v = r;
        if v < 8 {
            return 16;
        }
        if v > 248 {
            return 231;
        }
        // 24-level greyscale approximation (step 10)
        let idx = (((v as u16 - 8) + 5) / 10).clamp(0, 23) as u8;
        return 232 + idx;
    }
    let q = |v: u8| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

/// Parse an OSC 11 colour response (`rgb:RRRR/GGGG/BBBB` or `#RRGGBB`),
/// returning by BT.709 relative luminance: `true` = dark background,
/// `false` = light. Returns `None` when unparseable.
fn theme_from_osc(data: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(data).ok()?;
    let payload = text.split("]11;").nth(1)?.split("\x1b\\").next()?;
    let (r, g, b) = if let Some(rest) = payload.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let comp = |p: Option<&str>| -> Option<f64> {
            let hex = p?.trim_end_matches(';');
            let n = hex.len();
            if !(1..=4).contains(&n) {
                return None;
            }
            Some(u16::from_str_radix(hex, 16).ok()? as f64 / (16f64.powi(n as i32) - 1.0))
        };
        (
            comp(parts.next())?,
            comp(parts.next())?,
            comp(parts.next())?,
        )
    } else {
        let rest = payload.strip_prefix('#')?;
        let n = rest.len() / 3;
        if !(1..=4).contains(&n) || rest.len() % 3 != 0 {
            return None;
        }
        let comp = |s: &str| -> Option<f64> {
            Some(u16::from_str_radix(s, 16).ok()? as f64 / (16f64.powi(n as i32) - 1.0))
        };
        (
            comp(&rest[..n])?,
            comp(&rest[n..2 * n])?,
            comp(&rest[2 * n..])?,
        )
    };
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(luminance <= 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_matches_claude_code_tokens() {
        let t = Theme::dark();
        assert_eq!(t.claude, Color::Rgb(215, 119, 87));
        assert_eq!(t.permission, Color::Rgb(177, 185, 249));
        assert_eq!(t.user_message_bg, Color::Rgb(55, 55, 55));
    }

    /// The point of D92: the dark preset is ours, not the terminal's. A named
    /// ANSI colour resolves to whatever colour scheme the user happens to run,
    /// so a palette containing one is a palette we do not actually control.
    #[test]
    fn both_presets_are_fully_rgb() {
        for (name, mut theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (slot, color) in theme.slots_mut() {
                assert!(
                    matches!(color, Color::Rgb(..)),
                    "{name}.{slot} is {color:?}; every palette slot must be RGB"
                );
            }
        }
    }

    /// The semantic vocabulary has to stay legible as a vocabulary: two tokens
    /// that mean different things and render identically are one token wearing
    /// two names.
    #[test]
    fn semantic_slots_are_pairwise_distinct() {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let semantic = [
                ("text", theme.text),
                ("text_secondary", theme.text_secondary),
                ("text_muted", theme.text_muted),
                ("claude", theme.claude),
                ("permission", theme.permission),
                ("success", theme.success),
                ("warning", theme.warning),
                ("error", theme.error),
            ];
            for (i, (a_name, a)) in semantic.iter().enumerate() {
                for (b_name, b) in &semantic[i + 1..] {
                    assert_ne!(a, b, "{name}: {a_name} and {b_name} are the same colour");
                }
            }
        }
    }

    /// The three tiers must actually descend, in both presets — otherwise
    /// "primary / secondary / muted" is naming without a hierarchy behind it.
    #[test]
    fn text_tiers_form_a_ladder() {
        let luminance = |c: Color| match c {
            Color::Rgb(r, g, b) => {
                0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)
            }
            other => panic!("expected RGB, got {other:?}"),
        };
        let dark = Theme::dark();
        assert!(luminance(dark.text) > luminance(dark.text_secondary));
        assert!(luminance(dark.text_secondary) > luminance(dark.text_muted));
        // Light inverts: the ladder descends away from the background, which on
        // a light ground means downward in luminance is upward in prominence.
        let light = Theme::light();
        assert!(luminance(light.text) < luminance(light.text_secondary));
        assert!(luminance(light.text_secondary) < luminance(light.text_muted));
    }

    /// The accessors have to hand back the tier they are named for; a sweep
    /// that moves a site from `dim()` to `muted()` is only meaningful if the
    /// two differ.
    #[test]
    fn tier_accessors_return_their_own_slot() {
        let t = Theme::dark();
        assert_eq!(t.text().fg, Some(t.text));
        assert_eq!(t.dim().fg, Some(t.text_secondary));
        assert_eq!(t.muted().fg, Some(t.text_muted));
        assert_ne!(t.dim().fg, t.muted().fg);
    }

    /// The light preset was already RGB, so D92 only had to move the one slot
    /// that gained a job. Everything else has to be exactly where it was.
    #[test]
    fn light_preset_only_moved_its_muted_tier() {
        let t = Theme::light();
        assert_eq!(t.text, Color::Rgb(0, 0, 0));
        assert_eq!(t.text_secondary, Color::Rgb(102, 102, 102));
        assert_eq!(t.claude, Color::Rgb(215, 119, 87));
        assert_eq!(t.success, Color::Rgb(44, 122, 57));
        assert_eq!(t.error, Color::Rgb(171, 43, 63));
        assert_eq!(t.warning, Color::Rgb(150, 108, 30));
        assert_eq!(t.code_block_bg, Color::Rgb(245, 245, 245));
        assert_eq!(t.diff_hunk, Color::Rgb(0, 102, 102));
        // The one deliberate change: rgb(175,175,175) was 2.3:1 on white.
        assert_eq!(t.text_muted, Color::Rgb(140, 140, 140));
    }

    #[test]
    fn light_differs() {
        // Light theme: black body text, light background; the orange primary
        // accent is the same in both themes.
        assert_eq!(Theme::dark().claude, Theme::light().claude);
        assert_ne!(Theme::dark().text, Theme::light().text);
        assert_ne!(
            Theme::dark().user_message_bg,
            Theme::light().user_message_bg
        );
    }

    #[test]
    fn rgb_downgrades_to_ansi256() {
        // Primary orange → 256 colour
        let downgraded = Theme::dark().downgrade_to_256();
        assert_eq!(downgraded.claude, Color::Indexed(173));
        assert_eq!(downgraded.permission, Color::Indexed(147));
        assert_eq!(downgraded.user_message_bg, Color::Indexed(237));
        // Every slot comes down, including the markdown/diff layer that used to
        // be named ANSI and therefore passed through untouched.
        let mut downgraded = downgraded;
        for (name, slot) in downgraded.slots_mut() {
            assert!(
                matches!(slot, Color::Indexed(_)),
                "{name} did not downgrade: {slot:?}"
            );
        }
    }

    #[test]
    fn rgb_to_ansi256_approximation() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
        // Red cube
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
    }

    #[test]
    fn heading_falls_back_for_deep_levels() {
        let t = Theme::dark();
        assert_eq!(t.heading(1).fg, Some(t.text), "h1 = primary text");
        assert_eq!(
            t.heading(9).fg,
            Some(t.text_secondary),
            "deep levels fall back to the last entry"
        );
        assert_eq!(t.heading(0).fg, Some(t.text), "level 0 = plain text color");
    }

    #[test]
    fn theme_setting_parses() {
        assert_eq!(ThemeSetting::parse(None), ThemeSetting::Auto);
        assert_eq!(ThemeSetting::parse(Some("dark")), ThemeSetting::Dark);
        assert_eq!(ThemeSetting::parse(Some("light")), ThemeSetting::Light);
        assert_eq!(ThemeSetting::parse(Some("  dark ")), ThemeSetting::Dark);
        assert_eq!(ThemeSetting::parse(Some("weird")), ThemeSetting::Auto);
        assert_eq!(ThemeSetting::parse(Some("")), ThemeSetting::Auto);
    }

    /// Expected colour under the ambient truecolor support: without COLORTERM
    /// (e.g. CI runners) RGB colours are downgraded to 256-colour approximations.
    fn expect_color(rgb: (u8, u8, u8)) -> Color {
        let (r, g, b) = rgb;
        if Theme::terminal_supports_truecolor() {
            Color::Rgb(r, g, b)
        } else {
            Color::Indexed(rgb_to_ansi256(r, g, b))
        }
    }

    #[test]
    fn for_terminal_respects_setting() {
        // An explicit theme does not depend on terminal detection.
        let dark = Theme::for_terminal(ThemeSetting::Dark, None);
        let light = Theme::for_terminal(ThemeSetting::Light, None);
        assert_eq!(dark.text, expect_color((235, 231, 226)));
        assert_eq!(light.text, expect_color((0, 0, 0)));
    }

    #[test]
    fn auto_uses_detected_background() {
        // Some(true) = dark background.
        let dark = Theme::for_terminal(ThemeSetting::Auto, Some(true));
        assert_eq!(dark.text, expect_color((235, 231, 226)));
        // Some(false) = light background.
        let light = Theme::for_terminal(ThemeSetting::Auto, Some(false));
        assert_eq!(light.text, expect_color((0, 0, 0)));
    }

    #[test]
    fn osc11_parses_rgb_components() {
        // Dark: low luminance → true.
        assert_eq!(
            theme_from_osc(b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
            Some(true)
        );
        assert_eq!(
            theme_from_osc(b"\x1b]11;rgb:1a1a/1a1a/1a1a\x1b\\"),
            Some(true)
        );
        // Light: high luminance → false.
        assert_eq!(
            theme_from_osc(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some(false)
        );
        // Short form (Terminal.app / kitty etc., 1-4 hex digits).
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:ff/ff/ff\x1b\\"), Some(false));
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:00/00/00\x1b\\"), Some(true));
        // Nibble #RRGGBB form.
        assert_eq!(theme_from_osc(b"\x1b]11;#ffffff\x1b\\"), Some(false));
        assert_eq!(theme_from_osc(b"\x1b]11;#000000\x1b\\"), Some(true));
        // Unparseable → None.
        assert_eq!(theme_from_osc(b"garbage"), None);
        assert_eq!(theme_from_osc(b"\x1b]11;rgb:zzzz/0000/0000\x1b\\"), None);
    }
}
