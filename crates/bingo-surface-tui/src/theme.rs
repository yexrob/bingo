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

/// The same eight, over a light ground. Each is [`DARK`]'s hue at the other
/// end of its own lightness: the warm off-white becomes a warm near-black,
/// `presence` deepens until it reads on paper, and `raised` is one step *down*
/// from the background rather than one step up — the tint is what gives the
/// frame depth either way (design §4).
pub const LIGHT: Palette = Palette {
    text: Color::Rgb(0x24, 0x20, 0x1a),
    dim: Color::Rgb(0x77, 0x71, 0x67),
    raised: Color::Rgb(0xee, 0xe8, 0xdd),
    presence: Color::Rgb(0xb2, 0x4f, 0x2c),
    glow: Color::Rgb(0xd9, 0x77, 0x57),
    good: Color::Rgb(0x35, 0x72, 0x30),
    bad: Color::Rgb(0xb0, 0x2f, 0x24),
    mode: Color::Rgb(0x2f, 0x5f, 0x99),
    good_tint: Color::Rgb(0xe0, 0xef, 0xdd),
    bad_tint: Color::Rgb(0xf7, 0xe2, 0xdf),
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
    /// A skill run, in the bullet's place — one cell, so nothing under the row
    /// shifts.
    pub skill: &'static str,
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
    /// A tree node with siblings under it, and the last one.
    pub branch: &'static str,
    pub corner: &'static str,
    /// What is left where something was folded away or cut short.
    pub ellipsis: &'static str,
    /// What opens an item of a list.
    pub point: &'static str,
    pub border: border::Set<'static>,
}

pub const UNICODE: Glyphs = Glyphs {
    bullet: "⏺",
    skill: "❖",
    connector: "⎿",
    user: ">",
    cursor: "❯",
    sparkles: ["✻", "✢", "✶", "✽"],
    todo: "☐",
    todo_done: "☒",
    mode: "⏵⏵",
    rule: "─",
    branch: "├",
    corner: "└",
    ellipsis: "…",
    point: "•",
    border: border::ROUNDED,
};

/// `BINGO_ASCII=1`: the six characters of §7, each doing the job its shape
/// suggests — `>` says you, `*` is a bullet, `+` sparkles and turns a corner,
/// `x` crosses a box off, `-` connects and rules, `|` stands a wall up.
///
/// A skill spends no seventh character: the row says `Skill(guide)` in words,
/// so the glyph is what a glance finds and never the only place the fact is.
pub const ASCII: Glyphs = Glyphs {
    bullet: "*",
    skill: "*",
    connector: "-",
    user: ">",
    cursor: ">",
    sparkles: ["+", "+", "+", "+"],
    todo: "-",
    todo_done: "x",
    mode: ">>",
    rule: "-",
    branch: "+",
    corner: "+",
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

/// Which of the two truecolor palettes a person wants: the one their terminal
/// is already wearing, or one they named.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    #[default]
    Terminal,
    Light,
    Dark,
}

/// `BINGO_THEME`. It sits with `BINGO_MOTION` and `BINGO_ASCII` rather than in
/// the settings file because the look is chosen before the kernel is up: a
/// setting would arrive after the first frame had already been drawn.
pub fn look(setting: Option<&str>) -> Look {
    match setting {
        Some("light") => Look::Light,
        Some("dark") => Look::Dark,
        _ => Look::Terminal,
    }
}

/// Everything the look is chosen from, read once at start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    /// `NO_COLOR`, which wins over all of it.
    pub no_color: bool,
    /// `COLORTERM` says 24 bits are safe.
    pub truecolor: bool,
    pub ascii: bool,
    pub look: Look,
    /// Whether the terminal answered that its background is light. `None`
    /// where it did not answer or was never asked.
    pub light: Option<bool>,
}

/// What the environment asks for. Truecolor is announced by `COLORTERM`; a
/// terminal that says nothing gets the eight it is sure of. `NO_COLOR` wins
/// over every other answer, the terminal's own included.
pub fn choose(ask: Ask) -> Theme {
    let palette = match (ask.look, ask.light) {
        (Look::Light, _) | (Look::Terminal, Some(true)) => LIGHT,
        _ => DARK,
    };
    let colors = match (ask.no_color, ask.truecolor) {
        (true, _) => Colors::Plain,
        (false, true) => Colors::True(palette),
        (false, false) => Colors::Ansi,
    };
    Theme {
        colors,
        glyphs: if ask.ascii { &ASCII } else { &UNICODE },
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

/// The look this run draws in, settled by [`detect`] before the first frame.
#[cfg(not(test))]
static CHOSEN: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();

#[cfg(not(test))]
fn current() -> Theme {
    *CHOSEN.get_or_init(|| choose(asked()))
}

/// Settle the look, asking the terminal what colour its background is where
/// that is what decides it. Called once, before the terminal is taken: a probe
/// writes an escape and waits for the answer, which is not something a draw may
/// do. A test never reaches it — the suite fixes the look instead.
#[cfg(not(test))]
pub fn detect() {
    let _ = CHOSEN.set(choose(asked()));
}

#[cfg(test)]
pub fn detect() {}

/// How long a probe of the terminal is given, this one and the graphics probe
/// beside it ([`crate::graphics`]). A terminal that has not answered by then is
/// one whose answer would have arrived after the first frame.
#[cfg(not(test))]
pub(crate) const PROBE: std::time::Duration = std::time::Duration::from_millis(400);

#[cfg(not(test))]
fn asked() -> Ask {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let truecolor = matches!(
        std::env::var("COLORTERM").ok().as_deref(),
        Some("truecolor" | "24bit")
    );
    let look = look(std::env::var("BINGO_THEME").ok().as_deref());
    Ask {
        // The terminal is asked only where its answer would change something:
        // a probe nobody reads is a probe not worth its milliseconds.
        light: (!no_color && truecolor && look == Look::Terminal)
            .then(background)
            .flatten(),
        no_color,
        truecolor,
        ascii: std::env::var_os("BINGO_ASCII").is_some_and(|v| v == "1"),
        look,
    }
}

/// Whether the terminal says its background is light (OSC 10/11). A terminal
/// that will not say leaves the dark palette standing.
#[cfg(not(test))]
fn background() -> Option<bool> {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = PROBE;
    terminal_colorsaurus::theme_mode(options)
        .ok()
        .map(|mode| mode == terminal_colorsaurus::ThemeMode::Light)
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

/// The bright half of a pulse: everything that breathes moves between this
/// and [`presence`]. Views reach it through [`pulse`] and [`comet`] rather
/// than directly, because what moves is a phase and not a colour.
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

// ---- what moves (design §6) ---------------------------------------------
//
// Every one of these takes a phase — a number between 0 and 1 that
// [`crate::clock`] derived from the clock — and answers with the token at that
// point. The colour stays here; the timing stays there; a view names neither.

/// The steps a breath is drawn in. Twenty-four bits can show five; the eight
/// have only the two ends, and a terminal without colour has neither.
pub const BREATH_STEPS: u8 = 5;

/// How dim a breath goes: 65 % of `presence` (§6).
const BREATH_FLOOR: f32 = 0.65;

/// What wants a person pulses once a second (§6).
pub const PULSE: std::time::Duration = std::time::Duration::from_secs(1);

/// bingo breathing — the activity row's sparkle and the input box's border
/// while a turn runs. `level` is 0 at the bottom of the breath and 1 at the
/// top of it.
pub fn breath(level: f32) -> Style {
    let level = steps(level, BREATH_STEPS);
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(level, dim(), presence()),
        Colors::True(palette) => Style::new().fg(scaled(
            palette.presence,
            BREATH_FLOOR + (1.0 - BREATH_FLOOR) * level,
        )),
    }
}

/// A live `⏺` pulsing between `presence` and its glow.
pub fn pulse(level: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(level, presence(), glow()),
        Colors::True(palette) => Style::new().fg(mix(palette.presence, palette.glow, level)),
    }
}

/// The comet tail on streaming text: the cell that just arrived wears the
/// glow, and cools to `text` as it ages.
pub fn comet(age: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(age, glow(), text()),
        Colors::True(palette) => Style::new().fg(mix(palette.glow, palette.text, age)),
    }
}

/// What wants a person, wherever it is said — the `N needs you` notice, a
/// waiting child's row, its line in the switcher. It alternates with plain
/// text once a second so the eye is drawn back to it, and rests on bingo's
/// own colour where nothing may move (§6).
pub fn attention(now: crate::clock::Now) -> Style {
    match now.motion && crate::clock::alternating(now, PULSE) {
        true => text(),
        false => presence(),
    }
}

/// A notice arriving and leaving: `dim` at both edges of its life, its own
/// level's colour while it is there to be read.
pub fn fading(level: bingo_sdk::Level, t: f32) -> Style {
    let arrived = self::level(level);
    match current().colors {
        Colors::True(palette) => match arrived.fg {
            Some(colour) => Style::new().fg(mix(palette.dim, colour, t)),
            None => arrived,
        },
        _ => match t >= 1.0 {
            true => arrived,
            false => dim(),
        },
    }
}

/// The light that crosses a tool's name as its answer lands: `good` at the
/// crest and the row's own weight where it has passed. The bullet is what
/// says the call finished; this says only how fresh that is (§6), so a name
/// the light has left carries no colour of its own.
pub fn landing(level: f32) -> Style {
    if level <= 0.0 {
        return bold();
    }
    match current().colors {
        Colors::Plain => bold(),
        Colors::Ansi => two_ways(level, bold(), good().patch(bold())),
        Colors::True(palette) => bold().fg(mix(palette.text, palette.good, level)),
    }
}

/// A failure cooling into the words behind it: `bad` where it lands and
/// `text` once it has settled. The bullet stays `bad`, so what cools is how
/// fresh the failure is and never whether there was one (§4).
pub fn cooling(t: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(t, bad(), text()),
        Colors::True(palette) => Style::new().fg(mix(palette.bad, palette.text, t)),
    }
}

/// The context notice warming from `dim` towards `bad` as the window fills.
pub fn warming(t: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(t, dim(), bad()),
        Colors::True(palette) => Style::new().fg(mix(palette.dim, palette.bad, t)),
    }
}

/// A ramp the eight colours cannot draw collapses to its two ends, and the
/// halfway mark is where it changes over.
fn two_ways(level: f32, low: Style, high: Style) -> Style {
    match level >= 0.5 {
        true => high,
        false => low,
    }
}

/// A continuous level, snapped to the steps a ramp is drawn in.
fn steps(level: f32, steps: u8) -> f32 {
    let last = f32::from(steps.saturating_sub(1)).max(1.0);
    (level.clamp(0.0, 1.0) * last).round() / last
}

/// `t` of the way from one colour to another. Anything but 24 bits has no
/// room between two colours, so the far end is the answer.
fn mix(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else {
        return to;
    };
    let t = t.clamp(0.0, 1.0);
    let at = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Color::Rgb(at(r1, r2), at(g1, g2), at(b1, b2))
}

/// A colour at a share of its own brightness — how a breath dims without
/// leaving the hue it belongs to.
fn scaled(colour: Color, share: f32) -> Color {
    let Color::Rgb(r, g, b) = colour else {
        return colour;
    };
    let share = share.clamp(0.0, 1.0);
    let at = |c: u8| (f32::from(c) * share).round() as u8;
    Color::Rgb(at(r), at(g), at(b))
}

// ---- what highlighted code is drawn in (design §5) ----------------------
//
// Three inks and no rainbow. Colour on this screen is spent on state — what
// is live, what wants a person, what failed — so syntax gets the two tokens
// it can have without competing: the one cool colour for the words that make
// a language a language, and `dim` for the words meant for a reader. The rest
// of the code is text, like every other answer.

/// What a highlighter may say about a run of code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Keyword,
    Comment,
    Plain,
}

/// The TextMate scope prefixes each ink answers to — the vocabulary every
/// syntax in the set is written in — tried in this order. A scope that matches
/// none of them is [`Ink::Plain`]; a syntax this table has never heard of
/// still reads as code, which is why the fallback is the answer and not a
/// colour of its own.
pub const INKS: &[(Ink, &str)] = &[
    (Ink::Comment, "comment"),
    // An operator is punctuation, not vocabulary: colouring `=` and `&&`
    // would put the cool colour on every other cell of a line.
    (Ink::Plain, "keyword.operator"),
    (Ink::Keyword, "keyword"),
    (Ink::Keyword, "storage"),
    (Ink::Keyword, "constant.language"),
    (Ink::Keyword, "variable.language"),
    (Ink::Keyword, "entity.name.tag"),
];

/// One ink as a token of §4's table.
pub fn ink(ink: Ink) -> Style {
    match ink {
        Ink::Keyword => mode(),
        Ink::Comment => dim(),
        Ink::Plain => text(),
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

/// A skill run, in the bullet's place: `❖ Skill(guide) …` (design §4). The
/// diamond is not one of the sparkle's four — those are bingo working — and
/// not a circle, so a skill is found by shape before it is read.
pub fn skill() -> &'static str {
    glyphs().skill
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

/// A tree node that has siblings under it; [`corner`] is the last of them.
pub fn branch() -> &'static str {
    glyphs().branch
}

pub fn corner() -> &'static str {
    glyphs().corner
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
        read(std::path::Path::new("src"), &mut out);
        out
    }

    fn read(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("the crate's own sources") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                read(&path, out);
                continue;
            }
            let name = path
                .strip_prefix("src")
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if path.extension().is_none_or(|e| e != "rs")
                || matches!(
                    name.as_str(),
                    "doubles.rs"
                        // The kitty protocol carries a picture's id in the
                        // foreground colour (M46): a number in a colour's
                        // clothing, and the one place outside this table
                        // that has to spell one.
                        | "graphics/kitty.rs"
                        | "motion.rs"
                        | "motion/landing.rs"
                        | "painted.rs"
                        | "screens.rs"
                        | "screens/colours.rs"
                        | "test_support.rs"
                        | "tests.rs"
                        | "theme.rs"
                )
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a readable source");
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            out.push((name, code.to_string()));
        }
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
            (
                "text",
                &[
                    "composer.rs",
                    "dialog.rs",
                    "markdown.rs",
                    "pager.rs",
                    "panel.rs",
                    "preview.rs",
                    "rail.rs",
                    "rewind.rs",
                    // The row a person is on in the one list of sessions:
                    // weight rather than hue, so `NO_COLOR` still says which
                    // row the keyboard is talking to.
                    "roster.rs",
                    "search.rs",
                    "status.rs",
                    "transcript.rs",
                    // What was said into a session, and who by: the name a
                    // post carries and the words after it.
                    "transcript/said.rs",
                    "view.rs",
                    "views/actions.rs",
                    "views/keyvalue.rs",
                    "views/list.rs",
                    "views/panel.rs",
                    "views/progress.rs",
                    "views/table.rs",
                    "views/text.rs",
                    "views/tree.rs",
                    "welcome.rs",
                ],
            ),
            (
                "dim",
                &[
                    "composer.rs",
                    "composer/strip.rs",
                    "dialog.rs",
                    "keys.rs",
                    "layers.rs",
                    "markdown.rs",
                    "pager.rs",
                    "panel.rs",
                    "preview.rs",
                    "rail.rs",
                    "rewind.rs",
                    "roster.rs",
                    "search.rs",
                    "status.rs",
                    "transcript.rs",
                    // The chip that names a picture a terminal cannot draw
                    // (§5's image row).
                    "transcript/pictured.rs",
                    "transcript/said.rs",
                    "tree.rs",
                    "view.rs",
                    "views/actions.rs",
                    "views/badge.rs",
                    "views/code.rs",
                    "views/keyvalue.rs",
                    "views/list.rs",
                    "views/progress.rs",
                    "views/table.rs",
                    "views/tree.rs",
                    "welcome.rs",
                    // The `…` that says a list a cursor walks goes on past
                    // the end of its window: a hint, as the strip's is.
                    "window.rs",
                ],
            ),
            (
                "raised",
                &[
                    "layers.rs",
                    "rail.rs",
                    "search.rs",
                    "select.rs",
                    // The bar a person's own line is a band on.
                    "transcript/said.rs",
                ],
            ),
            (
                "presence",
                &[
                    "dialog.rs",
                    "panel.rs",
                    "search.rs",
                    "select.rs",
                    "transcript.rs",
                    "tree.rs",
                    "view.rs",
                    "views/actions.rs",
                    "views/badge.rs",
                    "welcome.rs",
                ],
            ),
            ("glow", &[]),
            ("good", &["transcript.rs", "tree.rs", "views/badge.rs"]),
            ("bad", &["transcript.rs", "views/badge.rs"]),
            ("mode", &["status.rs"]),
            ("breath", &["view.rs"]),
            // The ramp `presence` → glow: a live bullet, the fill of a bar,
            // and the light a sent line runs along the box's border — one
            // gradient, wherever §4 sanctions one.
            ("pulse", &["transcript.rs", "view.rs", "views/progress.rs"]),
            ("comet", &["transcript.rs"]),
            ("landing", &["transcript.rs"]),
            ("cooling", &["transcript.rs"]),
            ("fading", &["status.rs"]),
            ("warming", &["status.rs"]),
            (
                "attention",
                // The card's border is the fourth place one beat says a
                // thing wants a person, and no longer the exception.
                &["layers.rs", "roster.rs", "status.rs", "transcript.rs"],
            ),
            // Highlighted code reaches the table through one door, so no view
            // has to know that a keyword and the mode badge share a colour.
            ("ink", &["highlight.rs"]),
        ];
        for (token, files) in allowed {
            let mut seen: Vec<String> = sources()
                .into_iter()
                .filter(|(_, code)| code.contains(&format!("theme::{token}(")))
                .map(|(name, _)| name)
                .collect();
            seen.sort();
            assert_eq!(&seen, files, "where `{token}` is spent");
        }
    }

    /// Twenty-four bits and nothing else asked for.
    fn asking() -> Ask {
        Ask {
            truecolor: true,
            ..Ask::default()
        }
    }

    #[test]
    fn the_environment_picks_the_look_and_no_colour_wins() {
        assert_eq!(choose(Ask::default()).colors, Colors::Ansi);
        assert_eq!(choose(asking()).colors, Colors::True(DARK));
        assert_eq!(
            choose(Ask {
                no_color: true,
                ..asking()
            })
            .colors,
            Colors::Plain,
        );
        assert_eq!(
            choose(Ask {
                ascii: true,
                ..Ask::default()
            })
            .glyphs,
            &ASCII,
        );
        assert_eq!(choose(Ask::default()).glyphs, &UNICODE);
    }

    #[test]
    fn a_terminal_that_answers_light_gets_the_light_palette() {
        let terminal = |light| choose(Ask { light, ..asking() }).colors;
        assert_eq!(terminal(Some(true)), Colors::True(LIGHT));
        assert_eq!(terminal(Some(false)), Colors::True(DARK));
        assert_eq!(terminal(None), Colors::True(DARK), "and silence is dark");
    }

    #[test]
    fn a_named_look_outranks_what_the_terminal_answered() {
        let named = |look, light| {
            choose(Ask {
                look,
                light,
                ..asking()
            })
            .colors
        };
        assert_eq!(named(Look::Dark, Some(true)), Colors::True(DARK));
        assert_eq!(named(Look::Light, Some(false)), Colors::True(LIGHT));
        assert_eq!(look(Some("light")), Look::Light);
        assert_eq!(look(Some("dark")), Look::Dark);
        assert_eq!(look(Some("terminal")), Look::Terminal);
        assert_eq!(look(Some("sepia")), Look::Terminal, "and so is nonsense");
    }

    #[test]
    fn no_colour_beats_a_terminal_that_answered_light() {
        assert_eq!(
            choose(Ask {
                no_color: true,
                light: Some(true),
                look: Look::Light,
                ..asking()
            })
            .colors,
            Colors::Plain,
        );
    }

    /// One row of §4's table: its name and the value it takes in a palette.
    type Token = (&'static str, fn(Palette) -> Color);

    /// Every token of §4's table, in the order the table lists them.
    const TOKENS: &[Token] = &[
        ("text", |p| p.text),
        ("dim", |p| p.dim),
        ("raised", |p| p.raised),
        ("presence", |p| p.presence),
        ("glow", |p| p.glow),
        ("good", |p| p.good),
        ("bad", |p| p.bad),
        ("mode", |p| p.mode),
        ("good_tint", |p| p.good_tint),
        ("bad_tint", |p| p.bad_tint),
    ];

    fn hex(colour: Color) -> String {
        match colour {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            other => format!("{other:?}"),
        }
    }

    /// The two palettes side by side: the same tokens, the same meanings, and
    /// nothing between them but what each is worth.
    #[test]
    fn light_and_dark_differ_only_in_what_each_token_is_worth() {
        let rows = TOKENS
            .iter()
            .map(|(name, of)| {
                format!(
                    "{name:<10} dark {}   light {}",
                    hex(of(DARK)),
                    hex(of(LIGHT))
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!("palettes", rows);
        for (name, of) in TOKENS {
            assert_ne!(of(DARK), of(LIGHT), "`{name}` is the same in both");
        }
    }

    /// How bright a colour reads, near enough for "is this the light one".
    fn luma(colour: Color) -> f32 {
        let Color::Rgb(r, g, b) = colour else {
            return 0.0;
        };
        0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)
    }

    #[test]
    fn each_palette_reads_against_its_own_ground() {
        assert!(
            luma(DARK.text) > luma(DARK.raised),
            "warm off-white on dark"
        );
        assert!(
            luma(LIGHT.text) < luma(LIGHT.raised),
            "warm near-black on paper"
        );
        for (name, of) in TOKENS {
            if *name == "raised" || name.ends_with("_tint") {
                continue;
            }
            assert!(
                luma(of(LIGHT)) < luma(LIGHT.raised),
                "`{name}` must read on the light ground"
            );
        }
        assert!(
            luma(DARK.raised) < luma(DARK.text) && luma(LIGHT.raised) > luma(LIGHT.text),
            "the tint steps away from the background either way"
        );
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
            assert_eq!(skill(), "*", "and a skill says so in words instead");
        });
    }

    /// A glyph drawn in the bullet's place takes the bullet's room: the text
    /// under a row is indented past the mark, not past whichever glyph the row
    /// happened to draw.
    #[test]
    fn a_mark_in_the_bullets_place_is_the_bullets_width() {
        for look in [
            Theme {
                colors: Colors::Ansi,
                glyphs: &UNICODE,
            },
            ascii(),
        ] {
            with(look, || assert_eq!(skill().width(), bullet().width()));
        }
    }

    #[test]
    fn the_sparkle_walks_its_four_frames_on_the_clock() {
        let at = |ms| sparkle(std::time::Duration::from_millis(ms));
        assert_eq!(at(0), "✻");
        assert_eq!(at(SPARKLE_MS as u64), "✢");
        assert_eq!(at(SPARKLE_MS as u64 * 3), "✽");
        assert_eq!(at(SPARKLE_MS as u64 * 4), "✻", "it comes back round");
    }

    fn dark() -> Theme {
        Theme {
            colors: Colors::True(DARK),
            glyphs: &UNICODE,
        }
    }

    #[test]
    fn a_breath_is_five_steps_of_presence_on_twenty_four_bits() {
        with(dark(), || {
            let sampled: Vec<Style> = (0..=4).map(|i| breath(i as f32 / 4.0)).collect();
            let mut distinct = sampled.clone();
            distinct.dedup();
            assert_eq!(distinct.len(), 5, "five steps, all different");
            assert_eq!(sampled[4], presence(), "the top of it is presence itself");
            assert_eq!(
                sampled[0],
                Style::new().fg(scaled(DARK.presence, 0.65)),
                "and the bottom is 65 % of it"
            );
            assert_eq!(breath(0.1), sampled[0], "a level between steps snaps");
        });
    }

    #[test]
    fn a_ramp_the_eight_colours_cannot_draw_takes_its_two_ends() {
        assert_eq!(breath(0.0), dim());
        assert_eq!(breath(1.0), presence());
        assert_eq!(pulse(0.0), presence());
        assert_eq!(pulse(1.0), glow());
        assert_eq!(comet(0.0), glow());
        assert_eq!(comet(1.0), text());
        assert_eq!(warming(0.0), dim());
        assert_eq!(warming(1.0), bad());
    }

    #[test]
    fn a_ramp_on_twenty_four_bits_passes_between_its_ends() {
        with(dark(), || {
            assert_eq!(pulse(0.0).fg, Some(DARK.presence));
            assert_eq!(pulse(1.0).fg, Some(DARK.glow));
            let middle = pulse(0.5).fg.expect("a colour between the two");
            assert_ne!(middle, DARK.presence);
            assert_ne!(middle, DARK.glow);
            assert_eq!(comet(0.0).fg, Some(DARK.glow));
            assert_eq!(comet(1.0).fg, Some(DARK.text));
            assert_eq!(warming(0.0).fg, Some(DARK.dim));
            assert_eq!(warming(1.0).fg, Some(DARK.bad));
        });
    }

    #[test]
    fn a_notice_arrives_out_of_dim_and_leaves_into_it() {
        let level = bingo_sdk::Level::Error;
        assert_eq!(fading(level, 0.0), dim());
        assert_eq!(fading(level, 1.0), bad());
        with(dark(), || {
            assert_eq!(fading(level, 0.0).fg, Some(DARK.dim));
            assert_eq!(fading(level, 1.0).fg, Some(DARK.bad));
        });
    }

    #[test]
    fn nothing_that_moves_spends_a_colour_under_no_colour() {
        with(no_colour(), || {
            for style in [breath(1.0), pulse(1.0), comet(0.0), warming(1.0)] {
                assert_eq!(style, Style::new());
            }
            assert_eq!(fading(bingo_sdk::Level::Error, 1.0), Style::new());
        });
    }

    fn no_colour() -> Theme {
        Theme {
            colors: Colors::Plain,
            glyphs: &UNICODE,
        }
    }

    #[test]
    fn a_look_is_only_switched_for_the_thread_that_asked() {
        assert_eq!(glyphs(), &UNICODE);
        with(ascii(), || assert_eq!(glyphs(), &ASCII));
        assert_eq!(glyphs(), &UNICODE, "the override is given back");
    }
}
