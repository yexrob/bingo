//! The tokens and glyphs the whole surface draws with, in one place, so a
//! change of look is a change of one file rather than a hunt through the views.
//!
//! `docs/design/tui.md` §4 is the table this file is: eight tokens, each a
//! function of the palette, and one glyph table with an ASCII fallback. A view
//! never names a colour — it names a token — and a test asserts that no
//! `Color::` or `Modifier::` literal exists outside this file.
//!
//! The look is chosen once, from the environment: `NO_COLOR` strips colour,
//! `BINGO_ASCII=1` strips the glyphs, `COLORTERM` says whether 24-bit is safe.
//! [`choose`] is that decision as a pure function; the tests fix the look to
//! the ANSI table so a snapshot never depends on the terminal it ran in.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use unicode_width::UnicodeWidthStr;

// ---- the palettes -------------------------------------------------------

/// The colours of one look. Light joins dark when M11e reads the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub dim: Color,
    pub raised: Color,
    pub presence: Color,
    pub glow: Color,
    pub good: Color,
    pub bad: Color,
    pub mode: Color,
    /// The translucent grounds a diff's rows sit on — `good` and `bad` at the
    /// strength of the raised tint, so a hunk reads as one block.
    pub good_tint: Color,
    pub bad_tint: Color,
}

/// Warm off-white over the terminal's own dark ground.
pub const DARK: Palette = Palette {
    text: Color::Rgb(0xec, 0xe7, 0xdf),
    dim: Color::Rgb(0x8a, 0x84, 0x7a),
    raised: Color::Rgb(0x21, 0x1d, 0x17),
    presence: Color::Rgb(0xd9, 0x77, 0x57),
    glow: Color::Rgb(0xf2, 0xa0, 0x7c),
    good: Color::Rgb(0x8f, 0xcf, 0x8a),
    bad: Color::Rgb(0xe0, 0x65, 0x5a),
    mode: Color::Rgb(0x8f, 0xb4, 0xde),
    good_tint: Color::Rgb(0x1b, 0x2a, 0x1c),
    bad_tint: Color::Rgb(0x2d, 0x1a, 0x19),
};

/// How much colour a terminal is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colors {
    /// `NO_COLOR`: weight and shape carry every fact colour would have.
    Plain,
    /// The eight every terminal is sure of.
    Ansi,
    /// 24-bit, the native look.
    True(Palette),
}

// ---- the glyphs ---------------------------------------------------------

/// The forms of `docs/design/tui.md` §4, and the ASCII spellings of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// What the model says and does, and what a session is.
    pub bullet: &'static str,
    /// What came back.
    pub connector: &'static str,
    /// What you said.
    pub user: &'static str,
    /// The row a card or a dropdown is talking to.
    pub cursor: &'static str,
    /// bingo at work, one frame every [`SPARKLE_MS`].
    pub sparkles: [&'static str; 4],
    pub todo: &'static str,
    pub todo_done: &'static str,
    /// The permission mode on the status line.
    pub mode: &'static str,
    /// A rule between blocks, and the fill of a box's edge.
    pub rule: &'static str,
    /// What is left where something was folded away or cut short.
    pub ellipsis: &'static str,
    /// What opens an item of a list.
    pub point: &'static str,
    pub border: border::Set<'static>,
}

pub const UNICODE: Glyphs = Glyphs {
    bullet: "⏺",
    connector: "⎿",
    user: ">",
    cursor: "❯",
    sparkles: ["✻", "✢", "✶", "✽"],
    todo: "☐",
    todo_done: "☒",
    mode: "⏵⏵",
    rule: "─",
    ellipsis: "…",
    point: "•",
    border: border::ROUNDED,
};

/// `BINGO_ASCII=1`: the six characters of §7, each doing the job its shape
/// suggests — `>` says you, `*` is a bullet, `+` sparkles and turns a corner,
/// `x` crosses a box off, `-` connects and rules, `|` stands a wall up.
pub const ASCII: Glyphs = Glyphs {
    bullet: "*",
    connector: "-",
    user: ">",
    cursor: ">",
    sparkles: ["+", "+", "+", "+"],
    todo: "-",
    todo_done: "x",
    mode: ">>",
    rule: "-",
    ellipsis: "...",
    point: "-",
    border: border::Set {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        vertical_left: "|",
        vertical_right: "|",
        horizontal_top: "-",
        horizontal_bottom: "-",
    },
};

/// One frame of the sparkle, per `docs/design/tui.md` §6.
pub const SPARKLE_MS: u128 = 150;

/// Terminal bell, written out of band between frames.
pub const BELL: &[u8] = b"\x07";

// ---- the look, chosen once ----------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub colors: Colors,
    pub glyphs: &'static Glyphs,
}

/// What the environment asks for. Truecolor is announced by `COLORTERM`; a
/// terminal that says nothing gets the eight it is sure of.
pub fn choose(no_color: bool, colorterm: Option<&str>, ascii: bool) -> Theme {
    let colors = match (no_color, colorterm) {
        (true, _) => Colors::Plain,
        (false, Some("truecolor" | "24bit")) => Colors::True(DARK),
        (false, _) => Colors::Ansi,
    };
    Theme {
        colors,
        glyphs: if ascii { &ASCII } else { &UNICODE },
    }
}

impl Theme {
    /// A foreground token: the palette's colour with 24 bits, the nearest of
    /// the eight without, and nothing at all under `NO_COLOR`.
    fn fg(self, ansi: Color, exact: fn(Palette) -> Color) -> Style {
        match self.colors {
            Colors::Plain => Style::new(),
            Colors::Ansi => Style::new().fg(ansi),
            Colors::True(palette) => Style::new().fg(exact(palette)),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The look one test draws in. Thread-local because the suite runs in
    /// parallel and a process-wide switch would leak between tests.
    static OVERRIDE: std::cell::Cell<Option<Theme>> = const { std::cell::Cell::new(None) };
}

/// The look under test: the ANSI table unless a test asked for another, so a
/// snapshot never depends on the terminal that ran it.
#[cfg(test)]
fn current() -> Theme {
    OVERRIDE.with(std::cell::Cell::get).unwrap_or(Theme {
        colors: Colors::Ansi,
        glyphs: &UNICODE,
    })
}

#[cfg(not(test))]
fn current() -> Theme {
    static CHOSEN: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
    *CHOSEN.get_or_init(|| {
        choose(
            std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var_os("BINGO_ASCII").is_some_and(|v| v == "1"),
        )
    })
}

/// Draw whatever `f` draws in another look.
#[cfg(test)]
pub fn with<R>(theme: Theme, f: impl FnOnce() -> R) -> R {
    let previous = OVERRIDE.with(|slot| slot.replace(Some(theme)));
    let out = f();
    OVERRIDE.with(|slot| slot.set(previous));
    out
}

// ---- the tokens ---------------------------------------------------------

/// Answers, what you type, option labels — the warm off-white.
pub fn text() -> Style {
    current().fg(Color::Reset, |p| p.text)
}

/// Results under `⎿`, thinking, hints, the status line.
pub fn dim() -> Style {
    match current().colors {
        Colors::True(palette) => Style::new().fg(palette.dim),
        _ => Style::new().add_modifier(Modifier::DIM),
    }
}

/// The bar behind a `>` line, a card's surface, a rail card. Never text.
pub fn raised() -> Style {
    match current().colors {
        Colors::True(palette) => Style::new().bg(palette.raised),
        _ => Style::new(),
    }
}

/// bingo's own colour: the sparkle, a live `⏺`, a card's border, the focused
/// row. The one warm colour, and the only one that moves.
pub fn presence() -> Style {
    current().fg(Color::Yellow, |p| p.presence)
}

/// The bright half of a pulse. The table of §4 carries it, and M11c is what
/// spends it: everything that breathes moves between this and [`presence`].
#[allow(dead_code, reason = "the pulse that spends it is M11c's")]
pub fn glow() -> Style {
    current().fg(Color::LightYellow, |p| p.glow)
}

pub fn good() -> Style {
    current().fg(Color::Green, |p| p.good)
}

pub fn bad() -> Style {
    current().fg(Color::Red, |p| p.bad)
}

/// The `⏵⏵` on the status line, and links — the one cool colour.
pub fn mode() -> Style {
    current().fg(Color::Blue, |p| p.mode)
}

/// A diff's rows: `good` and `bad` on their own tints, which only 24 bits can
/// draw; the column's `+` and `-` carry it everywhere else.
pub fn added() -> Style {
    tinted(good(), |p| p.good_tint)
}

pub fn removed() -> Style {
    tinted(bad(), |p| p.bad_tint)
}

fn tinted(fg: Style, tint: fn(Palette) -> Color) -> Style {
    match current().colors {
        Colors::True(palette) => fg.bg(tint(palette)),
        _ => fg,
    }
}

pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

pub fn italic() -> Style {
    Style::new().add_modifier(Modifier::ITALIC)
}

pub fn struck() -> Style {
    Style::new().add_modifier(Modifier::CROSSED_OUT)
}

pub fn link() -> Style {
    mode().add_modifier(Modifier::UNDERLINED)
}

/// A token as a drawn cell wears it: a terminal gives every cell a colour, so
/// an unpainted one reads back as `Reset` rather than as nothing. Comparing a
/// buffer's cell to a token goes through here.
#[cfg(test)]
pub fn as_drawn(style: Style) -> Style {
    let mut cell = ratatui::buffer::Cell::default();
    cell.set_style(style);
    cell.style()
}

pub fn level(level: bingo_sdk::Level) -> Style {
    match level {
        bingo_sdk::Level::Info => dim(),
        bingo_sdk::Level::Warn => presence(),
        bingo_sdk::Level::Error => bad(),
    }
}

// ---- the glyph table ----------------------------------------------------

pub fn glyphs() -> &'static Glyphs {
    current().glyphs
}

/// What the model says and does: `⏺` at column 0, its text at 2.
pub fn bullet() -> &'static str {
    glyphs().bullet
}

/// What came back: `⎿` at column 2, its text at 5.
pub fn connector() -> &'static str {
    glyphs().connector
}

pub fn user() -> &'static str {
    glyphs().user
}

pub fn cursor() -> &'static str {
    glyphs().cursor
}

/// `❯` marks what the keyboard talks to (design §7), and the rows it does not
/// talk to keep its width so nothing shifts when the focus moves.
pub fn cursor_span(focused: bool) -> ratatui::text::Span<'static> {
    match focused {
        true => ratatui::text::Span::styled(format!("{} ", cursor()), presence()),
        false => ratatui::text::Span::raw(" ".repeat(cursor().width() + 1)),
    }
}

pub fn todo(done: bool) -> &'static str {
    let glyphs = glyphs();
    if done { glyphs.todo_done } else { glyphs.todo }
}

pub fn rule() -> &'static str {
    glyphs().rule
}

/// What is left where something was folded away, cut short, or is still on
/// its way: a fold's `… +12 lines`, a middle-elided path, `Thinking…`.
pub fn ellipsis() -> &'static str {
    glyphs().ellipsis
}

/// What opens an item of a list, and the wall a quotation hangs from — the
/// border's own upright, so a box and a quote agree.
pub fn point() -> &'static str {
    glyphs().point
}

pub fn wall() -> &'static str {
    glyphs().border.vertical_left
}

pub fn border() -> border::Set<'static> {
    glyphs().border
}

/// bingo, standing still: the sparkle's first frame, which is what a finished
/// thought and the welcome box wear.
pub fn spark() -> &'static str {
    glyphs().sparkles[0]
}

/// The sparkle's frame for an elapsed duration, so the animation is a pure
/// function of the clock rather than a counter someone has to advance.
pub fn sparkle(elapsed: std::time::Duration) -> &'static str {
    let sparkles = glyphs().sparkles;
    sparkles[(elapsed.as_millis() / SPARKLE_MS) as usize % sparkles.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii() -> Theme {
        Theme {
            colors: Colors::Ansi,
            glyphs: &ASCII,
        }
    }

    /// Every drawing source of this crate, with its test module cut off, as
    /// `scripts/check_discipline.sh` cuts them. This file, the fixtures and
    /// the screen catalogue are left out: one holds the table and the others
    /// only read it back.
    fn sources() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("src").expect("the crate's own sources") {
            let path = entry.expect("a readable entry").path();
            let name = path.file_name().expect("a named file").to_string_lossy();
            if path.extension().is_none_or(|e| e != "rs")
                || name == "theme.rs"
                || name == "test_support.rs"
                || name == "screens.rs"
                || name == "painted.rs"
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a readable source");
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            out.push((name.into_owned(), code.to_string()));
        }
        out
    }

    #[test]
    fn no_view_names_a_colour_of_its_own() {
        let leaked: Vec<String> = sources()
            .into_iter()
            .flat_map(|(name, code)| {
                code.lines()
                    .filter(|line| line.contains("Color::") || line.contains("Modifier::"))
                    .map(|line| format!("{name}: {}", line.trim()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "a colour outside the token table: {leaked:#?}"
        );
    }

    /// The table of §4, and the only places each token is spent. A new call
    /// site is a taste decision, so it is written down here before it is made.
    #[test]
    fn every_token_is_spent_where_the_design_says() {
        let allowed: &[(&str, &[&str])] = &[
            // answers, what you type, option labels, a command's own words
            (
                "text",
                &[
                    "composer.rs",
                    "dialog.rs",
                    "markdown.rs",
                    "panel.rs",
                    "preview.rs",
                    "search.rs",
                    "transcript.rs",
                    "tree.rs",
                    "view.rs",
                    "welcome.rs",
                ],
            ),
            // results, hints, chrome — everything that recedes
            (
                "dim",
                &[
                    "composer.rs",
                    "dialog.rs",
                    "keys.rs",
                    "layers.rs",
                    "markdown.rs",
                    "panel.rs",
                    "preview.rs",
                    "search.rs",
                    "status.rs",
                    "transcript.rs",
                    "tree.rs",
                    "view.rs",
                    "welcome.rs",
                ],
            ),
            // the bar behind a `>` line, a sheet's surface, a selection, a search hit
            (
                "raised",
                &["layers.rs", "search.rs", "select.rs", "transcript.rs"],
            ),
            // a live bullet, the focus cursor, a card's border, a session that
            // is asking, the current search hit, the selected run
            (
                "presence",
                &[
                    "dialog.rs",
                    "layers.rs",
                    "search.rs",
                    "select.rs",
                    "status.rs",
                    "transcript.rs",
                    "tree.rs",
                    "view.rs",
                    "welcome.rs",
                ],
            ),
            // M11c's pulse
            ("glow", &[]),
            // a finished bullet
            ("good", &["transcript.rs", "tree.rs"]),
            // a failed bullet, a failed turn, the gate turned off, a full window
            ("bad", &["status.rs", "transcript.rs"]),
            // the mode on the status line; links reach it through `link`
            ("mode", &["status.rs"]),
        ];
        for (token, files) in allowed {
            let mut seen: Vec<String> = sources()
                .into_iter()
                .filter(|(_, code)| code.contains(&format!("theme::{token}()")))
                .map(|(name, _)| name)
                .collect();
            seen.sort();
            assert_eq!(&seen, files, "where `{token}` is spent");
        }
    }

    #[test]
    fn the_environment_picks_the_look_and_no_colour_wins() {
        assert_eq!(choose(false, None, false).colors, Colors::Ansi);
        assert_eq!(
            choose(false, Some("truecolor"), false).colors,
            Colors::True(DARK)
        );
        assert_eq!(
            choose(false, Some("24bit"), false).colors,
            Colors::True(DARK)
        );
        assert_eq!(choose(false, Some("8bit"), false).colors, Colors::Ansi);
        assert_eq!(choose(true, Some("truecolor"), false).colors, Colors::Plain);
        assert_eq!(choose(false, None, true).glyphs, &ASCII);
        assert_eq!(choose(false, None, false).glyphs, &UNICODE);
    }

    #[test]
    fn no_colour_keeps_the_weights_and_drops_the_hues() {
        with(
            Theme {
                colors: Colors::Plain,
                glyphs: &UNICODE,
            },
            || {
                for token in [text(), presence(), glow(), good(), bad(), mode(), raised()] {
                    assert_eq!(token, Style::new(), "NO_COLOR spends no colour");
                }
                assert_eq!(dim(), Style::new().add_modifier(Modifier::DIM));
                assert_eq!(bold(), Style::new().add_modifier(Modifier::BOLD));
            },
        );
    }

    #[test]
    fn the_ascii_table_spells_every_glyph_in_one_cell() {
        with(ascii(), || {
            assert_eq!(bullet(), "*");
            assert_eq!(connector(), "-");
            assert_eq!(cursor(), ">");
            assert_eq!(todo(false), "-");
            assert_eq!(todo(true), "x");
            assert_eq!(sparkle(std::time::Duration::ZERO), "+");
            assert_eq!(border().top_left, "+");
            assert_eq!(ellipsis(), "...");
        });
    }

    #[test]
    fn the_sparkle_walks_its_four_frames_on_the_clock() {
        let at = |ms| sparkle(std::time::Duration::from_millis(ms));
        assert_eq!(at(0), "✻");
        assert_eq!(at(SPARKLE_MS as u64), "✢");
        assert_eq!(at(SPARKLE_MS as u64 * 3), "✽");
        assert_eq!(at(SPARKLE_MS as u64 * 4), "✻", "it comes back round");
    }

    #[test]
    fn a_look_is_only_switched_for_the_thread_that_asked() {
        assert_eq!(glyphs(), &UNICODE);
        with(ascii(), || assert_eq!(glyphs(), &ASCII));
        assert_eq!(glyphs(), &UNICODE, "the override is given back");
    }
}
