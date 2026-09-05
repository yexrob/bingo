//! The tokens and glyphs the whole surface draws with, in one place, so a
//! change of look is a change of one file rather than a hunt through the views.
//!
//! `docs/design/tui.md` §4 is the table this file is: the tokens, and one
//! glyph table with an ASCII fallback. A view never names a colour — it names
//! a token — and a test asserts that no `Color::` or `Modifier::` literal
//! exists outside this file.
//!
//! Two of the tokens are not the palette's to choose. Body text is the
//! terminal's own foreground and secondary text its own dim, so a terminal
//! that flips its scheme remaps every line of prose itself, at once and in its
//! scrollback (M73). What is left in the palette is what colour *means* —
//! presence, good, bad, the mode, the grounds — and that is the only part a
//! later answer about the ground can change.
//!
//! The look is chosen from the environment: `NO_COLOR` strips colour,
//! `BINGO_ASCII=1` strips the glyphs, `COLORTERM` says whether 24-bit is safe.
//! [`choose`] is that decision as a pure function; the tests fix the look to
//! the ANSI table so a snapshot never depends on the terminal it ran in.
//!
//! One part of it is not settled once. Which of the two palettes a terminal
//! wants is the terminal's own to say, and a terminal that follows the system's
//! appearance changes its ground under a running surface — so the ground is a
//! slot a later answer may replace ([`swap`]), every frame after it wears the
//! other palette, and nothing here caches a `Style` (M71).

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use unicode_width::UnicodeWidthStr;

// ---- the palettes -------------------------------------------------------

/// The colours of one look — the accents, and the grounds they are read on.
/// The ink is not among them: body text is the terminal's own foreground and
/// secondary text its own dim, in every look (M73), so a palette says only
/// what colour *means* here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
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

/// The accents over a terminal's own dark ground.
pub const DARK: Palette = Palette {
    raised: Color::Rgb(0x21, 0x1d, 0x17),
    presence: Color::Rgb(0xd9, 0x77, 0x57),
    glow: Color::Rgb(0xf2, 0xa0, 0x7c),
    good: Color::Rgb(0x8f, 0xcf, 0x8a),
    bad: Color::Rgb(0xe0, 0x65, 0x5a),
    mode: Color::Rgb(0x8f, 0xb4, 0xde),
    good_tint: Color::Rgb(0x1b, 0x2a, 0x1c),
    bad_tint: Color::Rgb(0x2d, 0x1a, 0x19),
};

/// The same accents over a light ground. Each is [`DARK`]'s hue at the other
/// end of its own lightness: `presence` deepens until it reads on paper, and
/// `raised` is one step *down* from the background rather than one step up —
/// the tint is what gives the frame depth either way (design §4).
pub const LIGHT: Palette = Palette {
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
    /// Where typing lands, in a row the terminal's own cursor is not parked
    /// on — a query line.
    pub caret: &'static str,
    /// bingo at work, one frame every [`SPARKLE_MS`].
    pub sparkles: [&'static str; 4],
    /// A box a person ticks: `[0]` open, `[1]` done. One field because it is
    /// one fact in two states, as the sparkles are one in four.
    pub todo: [&'static str; 2],
    /// A task on the session's list, by where it stands: `[0]` still to do,
    /// `[1]` being done, `[2]` done — Claude Code's own three (M74), which
    /// are not the box a person ticks: a task is the model's to move on.
    pub task: [&'static str; 3],
    /// A tick with no box drawn round it: what a form's `Submit` tab wears and
    /// what stands inside a multi-select's own brackets.
    pub tick: &'static str,
    /// The permission mode on the status line.
    pub mode: &'static str,
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
    caret: "▌",
    sparkles: ["✻", "✢", "✶", "✽"],
    todo: ["☐", "☒"],
    task: ["◻", "◼", "✔"],
    tick: "✔",
    mode: "⏵⏵",
    branch: "├",
    corner: "└",
    ellipsis: "…",
    point: "•",
    border: border::ROUNDED,
};

/// `BINGO_ASCII=1`: the six characters of §7, each doing the job its shape
/// suggests — `>` says you, `*` is a bullet, `+` sparkles and turns a corner,
/// `x` crosses a box off and ticks one, `-` connects and rules, `|` stands a
/// wall up and stands where typing lands.
///
/// A skill spends no seventh character: the row says `Skill(guide)` in words,
/// so the glyph is what a glance finds and never the only place the fact is.
pub const ASCII: Glyphs = Glyphs {
    bullet: "*",
    skill: "*",
    connector: "-",
    user: ">",
    cursor: ">",
    caret: "|",
    sparkles: ["+", "+", "+", "+"],
    todo: ["-", "x"],
    // Still to do, being done — the bullet, because it is the one the model
    // is on — and done.
    task: ["-", "*", "x"],
    tick: "x",
    mode: ">>",
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

// ---- the look, and the ground it may change under -----------------------

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

/// Everything the look is chosen from that no answer can change: what the
/// environment asks for, read once at start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ask {
    /// `NO_COLOR`, which wins over all of it.
    pub no_color: bool,
    /// `COLORTERM` says 24 bits are safe.
    pub truecolor: bool,
    pub ascii: bool,
    pub look: Look,
}

/// What the environment asks for, over the ground the terminal last said it
/// has — `None` where it did not answer or was never asked. Truecolor is
/// announced by `COLORTERM`; a terminal that says nothing gets the eight it is
/// sure of. `NO_COLOR` wins over every other answer, the terminal's own
/// included.
pub fn choose(ask: Ask, light: Option<bool>) -> Theme {
    let palette = match (ask.look, light) {
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
pub fn current() -> Theme {
    OVERRIDE.with(std::cell::Cell::get).unwrap_or(Theme {
        colors: Colors::Ansi,
        glyphs: &UNICODE,
    })
}

/// What the environment asked for, read once: nothing in it can change while
/// the run lasts.
#[cfg(not(test))]
static ASK: std::sync::OnceLock<Ask> = std::sync::OnceLock::new();

/// What the terminal last said its ground was — the one part of the look that
/// may change under a running surface (M71), so the one part that is a slot
/// rather than a settled value. A number and not a lock: [`current`] is read
/// on every span of every frame.
#[cfg(not(test))]
static GROUND: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNASKED);

/// The three things the terminal may have said about its ground, as [`GROUND`]
/// holds them.
const UNASKED: u8 = 0;
const LIGHT_GROUND: u8 = 1;
const DARK_GROUND: u8 = 2;

/// What one of those numbers says.
fn ground(said: u8) -> Option<bool> {
    match said {
        LIGHT_GROUND => Some(true),
        DARK_GROUND => Some(false),
        _ => None,
    }
}

/// And the number for one of those answers.
fn number(ground: Option<bool>) -> u8 {
    match ground {
        Some(true) => LIGHT_GROUND,
        Some(false) => DARK_GROUND,
        None => UNASKED,
    }
}

/// The look this frame is drawn in. Read on every span, and read again by the
/// two memos of a drawing — the transcript's blocks and the highlighter's rows
/// — which a change of look throws away.
#[cfg(not(test))]
pub fn current() -> Theme {
    let said = GROUND.load(std::sync::atomic::Ordering::Relaxed);
    choose(*ASK.get_or_init(asked), ground(said))
}

/// Settle what the environment says and ask the terminal what colour its
/// background is, where that is what decides it. Called once, before the
/// terminal is taken: a probe writes an escape and waits for the answer, which
/// is not something a draw may do — and, since the answer is a slot of its
/// own, not something a draw can do either. A test never reaches it — the
/// suite fixes the look instead.
#[cfg(not(test))]
pub fn detect() {
    let _ = ASK.set(asked());
    if follows() {
        GROUND.store(number(background()), std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
pub fn detect() {}

/// The terminal said again ([`crate::run::look`]): `true` where the look
/// actually changed and the screen is now wearing the wrong ink, `false`
/// where the answer is the one already standing — or where nothing follows the
/// terminal at all, which [`follows`] is the question for.
#[cfg(not(test))]
pub fn swap(light: bool) -> bool {
    let before = current();
    GROUND.store(number(Some(light)), std::sync::atomic::Ordering::Relaxed);
    current() != before
}

/// The same under test, on the thread-local look [`with`] fixes, so a swap in
/// one test is not a swap in another and nothing leaks out of the closure.
#[cfg(test)]
pub fn swap(light: bool) -> bool {
    let before = current();
    let after = match before.colors {
        Colors::True(_) => Theme {
            colors: choose(
                Ask {
                    truecolor: true,
                    ..Ask::default()
                },
                Some(light),
            )
            .colors,
            ..before
        },
        _ => before,
    };
    OVERRIDE.with(|slot| slot.set(Some(after)));
    after != before
}

/// Whether the terminal is the one that says what the look is, and so whether
/// it is worth asking — once before the first frame, and again while the run
/// lasts. A named look, a terminal of eight colours and `NO_COLOR` all answer
/// no: there is nothing an answer would change, and a person who named a look
/// is not asked again.
#[cfg(not(test))]
pub fn follows() -> bool {
    let ask = *ASK.get_or_init(asked);
    !ask.no_color && ask.truecolor && ask.look == Look::Terminal
}

/// The same under test, where the look is the one [`with`] fixed: the two
/// palettes are the truecolor look's, and nothing else has a ground to follow.
#[cfg(test)]
pub fn follows() -> bool {
    matches!(current().colors, Colors::True(_))
}

/// How long a probe of the terminal is given, this one and the graphics probe
/// beside it ([`crate::graphics`]). A terminal that has not answered by then is
/// one whose answer would have arrived after the first frame.
#[cfg(not(test))]
pub(crate) const PROBE: std::time::Duration = std::time::Duration::from_millis(400);

/// How long the graphics probe is given when tmux is carrying the questions
/// and the answers, and tmux has said it carries them ([`crate::graphics`],
/// M60 brick 3).
///
/// Three legs where a bare terminal has one: the question crosses to tmux's
/// server, waits there for the flush that hands it to the client's terminal,
/// and the answer comes back the same way through the server into this pane —
/// each leg waiting on tmux's own event loop rather than on the terminal's.
/// [`PROBE`] is one leg's worth, and the box this was reported from (tmux
/// 3.6b under Ghostty) took longer than one; three is the same clock read
/// three times rather than a second number to keep in step.
///
/// It is public because the pty harness's late scene has to outlast it, and a
/// window the test spells for itself is a window that drifts.
#[cfg(not(test))]
pub const PROBE_THROUGH: std::time::Duration =
    std::time::Duration::from_millis(3 * PROBE.as_millis() as u64);

#[cfg(not(test))]
fn asked() -> Ask {
    Ask {
        no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        truecolor: matches!(
            std::env::var("COLORTERM").ok().as_deref(),
            Some("truecolor" | "24bit")
        ),
        ascii: std::env::var_os("BINGO_ASCII").is_some_and(|v| v == "1"),
        look: look(std::env::var("BINGO_THEME").ok().as_deref()),
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

// ---- and asked again (M71) ----------------------------------------------

/// The question, asked again while the run lasts at the moments a change of
/// ground is likely ([`crate::run::look`]): `OSC 11`, the background colour,
/// which is what the probe asks for too. The answer comes back through
/// crossterm's key stream, where [`crate::late`] hears it whole and
/// [`answered`] reads it.
pub const QUESTION: &[u8] = b"\x1b]11;?\x1b\\";

/// What an `OSC 11` reply says the ground is: `Some(true)` for a light one.
/// Anything that is not an answer to that question — the graphics probe's own
/// replies, an `OSC 10` foreground, a colour spec in a shape `xparsecolor`
/// never wrote — is `None`, and leaves the look standing.
pub fn answered(reply: &[u8]) -> Option<bool> {
    let (red, green, blue) = colour(spec(reply)?)?;
    Some(background_is_light(red, green, blue))
}

/// The colour spec inside the reply, between `ESC ] 11 ;` and whichever
/// terminator the terminal chose.
fn spec(reply: &[u8]) -> Option<&[u8]> {
    let body = reply.strip_prefix(b"\x1b]11;")?;
    let body = body
        .strip_suffix(b"\x1b\\")
        .or_else(|| body.strip_suffix(b"\x07"))?;
    (!body.is_empty()).then_some(body)
}

/// An X11 colour string as `xparsecolor` reads the ones terminals answer with,
/// which is the shape `terminal_colorsaurus` accepted for the first answer:
/// `rgb:` and one to four hex digits a channel, or the older `#` form.
fn colour(spec: &[u8]) -> Option<(u16, u16, u16)> {
    let text = std::str::from_utf8(spec).ok()?;
    match text.strip_prefix("rgb:") {
        Some(written) => channels(written),
        None => shifted(text.strip_prefix('#')?),
    }
}

/// `rgb:<red>/<green>/<blue>`, each channel scaled from the width it was
/// written in: `h` is four bits, `hhhh` sixteen. A fourth channel is `rgba:`,
/// which this question is never answered with.
fn channels(written: &str) -> Option<(u16, u16, u16)> {
    let mut parts = written.split('/');
    let (red, green, blue) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    Some((scale(red)?, scale(green)?, scale(blue)?))
}

fn scale(digits: &str) -> Option<u16> {
    let width = u32::try_from(digits.len()).ok()?;
    if !(1..=4).contains(&width) {
        return None;
    }
    let most = 16u32.pow(width) - 1;
    let value = u32::from_str_radix(digits, 16).ok()?;
    u16::try_from(u32::from(u16::MAX) * value / most).ok()
}

/// The older `#<red><green><blue>`, whose digits are the most significant bits
/// of each channel rather than a scale: `#3a7` is `#3000a0007000`.
fn shifted(digits: &str) -> Option<(u16, u16, u16)> {
    let width = digits.len() / 3;
    if !digits.len().is_multiple_of(3) || !(1..=4).contains(&width) {
        return None;
    }
    let channel = |n: usize| {
        let part = digits.get(n * width..(n + 1) * width)?;
        Some(u16::from_str_radix(part, 16).ok()? << ((4 - width) * 4))
    };
    Some((channel(0)?, channel(1)?, channel(2)?))
}

/// Whether a ground of this colour is a light one, by the lightness
/// `terminal_colorsaurus` measures — the same maths the first answer went
/// through, so the probe and a later reply cannot disagree about one colour.
///
/// The probe has the ink beside the ground and compares the two; a reply to
/// this one question carries only the ground, and the threshold is then the
/// perceptual middle grey the crate falls back to when ink and ground are
/// worth the same.
pub fn background_is_light(red: u16, green: u16, blue: u16) -> bool {
    terminal_colorsaurus::Color::rgb(red, green, blue).perceived_lightness() > 0.5
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

/// Answers, what you type, option labels — the terminal's own foreground, in
/// every look (M73). No palette names it: a terminal that flips its scheme
/// remaps every cell of prose itself, on the spot and in its scrollback, which
/// is why Codex and Claude Code both draw their prose with no colour at all.
/// It is spelled `Reset` rather than left unsaid so that it can be patched
/// over a colour — the end of a ramp, a bold name a light has left.
pub fn text() -> Style {
    Style::new().fg(Color::Reset)
}

/// Results under `⎿`, thinking, hints, the status line: the terminal's own
/// dim, in every look (M73) — a weight and not a hue, so it follows the ink
/// wherever the ink goes, and a hairline drawn in it reads on either ground.
pub fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
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
/// glow, the cells behind it cool through `presence`, and the tail beyond
/// [`SETTLES`] is the resting ink. One of the two gradients §4 sanctions, and
/// the one place a ramp is still drawn towards the ink — which the terminal
/// owns since M73, so the ink is where the ramp *ends* rather than a colour it
/// can pass through.
pub fn comet(age: f32) -> Style {
    let age = settled(age);
    match current().colors {
        Colors::Plain => Style::new(),
        Colors::Ansi => two_ways(age, glow(), text()),
        Colors::True(palette) if age < SETTLES => {
            Style::new().fg(mix(palette.glow, palette.presence, age / SETTLES))
        }
        Colors::True(_) => text(),
    }
}

/// A border with light on it: the hairline it rests as below `SETTLES`,
/// bingo's own colour there, and its glow at 1.
///
/// The opening's own ramp (§11), the same shape as [`comet`]: the warm half
/// is a gradient between the two accents, and the cold half is what the
/// border rests as. A hairline is a weight and not a hue (§4), so there is
/// nothing to mix towards — the light hands back to it, as streaming words
/// hand back to `text`.
pub fn hairline(warmth: f32) -> Style {
    match current().colors {
        Colors::True(palette) if settled(warmth) >= SETTLES => Style::new().fg(mix(
            palette.presence,
            palette.glow,
            (settled(warmth) - SETTLES) / (1.0 - SETTLES),
        )),
        Colors::True(_) => dim(),
        // Three stops and eight colours: the two ends and the one in the
        // middle, which is the only warm colour either of them has.
        _ => match settled(warmth) {
            warm if warm >= 0.75 => glow(),
            warm if warm >= 0.25 => presence(),
            _ => dim(),
        },
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

/// A notice arriving and leaving: the words' own weight at both edges of its
/// life, its own level's colour while it is there to be read. `dim` is a
/// weight and not a hue since M73, so there is nothing left to mix through —
/// the notice is dim until it has arrived, as it has always been on the eight.
pub fn fading(level: bingo_sdk::Level, t: f32) -> Style {
    match t >= 1.0 {
        true => self::level(level),
        false => dim(),
    }
}

/// The light that crosses a tool's name as its answer lands: `good` at the
/// crest and the row's own weight where it has passed. The bullet is what
/// says the call finished; this says only how fresh that is (§6), so a name
/// the light has left carries no colour of its own — and that is the
/// terminal's ink now (M73), which no ramp reaches, so the light is the two
/// stops the eight colours always drew it in.
pub fn landing(level: f32) -> Style {
    if level <= 0.0 {
        return bold();
    }
    match current().colors {
        Colors::Plain => bold(),
        _ => two_ways(level, bold(), good().patch(bold())),
    }
}

/// A failure cooling into the words behind it: `bad` where it lands and the
/// resting ink once it has settled. The bullet stays `bad`, so what cools is
/// how fresh the failure is and never whether there was one (§4).
pub fn cooling(t: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        _ => two_ways(t, bad(), text()),
    }
}

/// The context notice warming from the words' own weight towards `bad` as the
/// window fills. It starts where secondary text does, which is a weight and
/// no colour at all (M73), so the notice takes `bad` at [`SETTLES`] rather
/// than creeping towards it through a grey.
pub fn warming(t: f32) -> Style {
    match current().colors {
        Colors::Plain => Style::new(),
        _ => two_ways(t, dim(), bad()),
    }
}

/// Whether this run draws twenty-four bits. Every beat of the opening is a
/// ramp — the tail behind the light, the mark cooling, the words settling — and
/// eight colours are a ramp with three steps in it, so the piece does not play
/// at all where they are all there is (§11).
pub fn full_colour() -> bool {
    matches!(current().colors, Colors::True(_))
}

/// A share of something, held to 0 and 1 — and to 0 where the arithmetic it
/// came out of had nothing to divide, so no `NaN` ever reaches a colour.
fn settled(share: f32) -> f32 {
    match share.is_nan() {
        true => 0.0,
        false => share.clamp(0.0, 1.0),
    }
}

/// Where a ramp hands over to its other end: the halfway mark, for the eight
/// colours that cannot draw a ramp at all and for the ramps 24 bits cannot
/// finish either, because the ink at the end of them is the terminal's (M73).
const SETTLES: f32 = 0.5;

/// A ramp the eight colours cannot draw collapses to its two ends, and
/// [`SETTLES`] is where it changes over.
fn two_ways(level: f32, low: Style, high: Style) -> Style {
    match level >= SETTLES {
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

/// Every foreground §4's table can put in a cell, in the look now standing:
/// the accents, and each ramp between them sampled finely enough that any
/// point one of them reaches is in the set. It is derived from the tokens
/// themselves rather than from the palette, so a ramp added to the table is in
/// it without anybody remembering to say so.
///
/// What a drawn screen carries outside this set, `Reset` aside — and `Reset`
/// is the ink and the dim both since M73 — is a colour nobody sanctioned.
/// `crate::screens` holds every screen to it.
#[cfg(test)]
pub fn spendable() -> std::collections::HashSet<Color> {
    let mut out: std::collections::HashSet<Color> = std::collections::HashSet::new();
    let mut keep = |style: Style| {
        if let Some(colour) = style.fg {
            out.insert(colour);
        }
    };
    for style in [presence(), glow(), good(), bad(), mode()] {
        keep(style);
    }
    for step in 0..=SAMPLES {
        let t = step as f32 / SAMPLES as f32;
        for style in [
            breath(t),
            pulse(t),
            comet(t),
            landing(t),
            cooling(t),
            warming(t),
        ] {
            keep(style);
        }
        for level in [
            bingo_sdk::Level::Info,
            bingo_sdk::Level::Warn,
            bingo_sdk::Level::Error,
        ] {
            keep(fading(level, t));
        }
    }
    out
}

/// How finely [`spendable`] samples a ramp. A ramp's channels move by at most
/// 255 over its whole length, so a thousand steps reach every colour any `t`
/// can land on.
#[cfg(test)]
const SAMPLES: u32 = 1_000;

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

/// Where typing lands on a row the terminal's own cursor is not parked on.
pub fn caret() -> &'static str {
    glyphs().caret
}

/// `❯` marks what the keyboard talks to (design §7), and the rows it does not
/// talk to keep its width so nothing shifts when the focus moves.
pub fn cursor_span(focused: bool) -> ratatui::text::Span<'static> {
    match focused {
        true => ratatui::text::Span::styled(format!("{} ", cursor()), presence()),
        false => ratatui::text::Span::raw(" ".repeat(cursor().width() + 1)),
    }
}

/// The three marks a task on the list can wear, still to do first
/// ([`Glyphs::task`]); which one a task gets is [`crate::tasks`]'s to say.
pub fn tasks() -> [&'static str; 3] {
    glyphs().task
}

pub fn todo(done: bool) -> &'static str {
    glyphs().todo[usize::from(done)]
}

/// The tick a card spends where no box is drawn round it: `✔ Submit` on a
/// form's tab row, and the mark inside a multi-select's `[✔]`.
pub fn tick() -> &'static str {
    glyphs().tick
}

/// A rule between blocks: the same stroke a box draws its edge with, because
/// they are one line and not two facts.
pub fn rule() -> &'static str {
    glyphs().border.horizontal_top
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
                        // The storyboard turns a drawn cell back into pixels
                        // and into an escape, so it reads colours off the
                        // tokens rather than naming any (M69).
                        | "opening/storyboard.rs"
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
                    // The working word at rest, where nothing may move.
                    "activity.rs",
                    "composer.rs",
                    "dialog.rs",
                    // The question a form is on, and the option under its
                    // cursor: the same weight the dialog's rows wear.
                    "form.rs",
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
                    // A task still to do, and the mark before one.
                    "tasks.rs",
                    "transcript.rs",
                    // The line of a shell command a person ran themselves.
                    "transcript/ran.rs",
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
                    // The verb row's clock and count, and a queued line.
                    "activity.rs",
                    "composer.rs",
                    "dialog.rs",
                    // A form's tabs it is not on, the descriptions, and the
                    // preview pane — a mockup is read past, so it is dim.
                    "form.rs",
                    // The `+N` after the last thumbnail a band of them shows,
                    // wherever one stands: the composer's strip, and a
                    // person's own `>` block.
                    "graphics/band.rs",
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
                    "shells.rs",
                    "status.rs",
                    // The summary between turns, a task that is done, its
                    // owner, and the line that counts what was cut.
                    "tasks.rs",
                    "transcript.rs",
                    // A folded result's own rows and its `+N lines` line.
                    "transcript/output.rs",
                    // The chip that names a picture a terminal cannot draw
                    // (§5's image row).
                    "transcript/pictured.rs",
                    // The `$` a shell line the person ran is written under.
                    "transcript/ran.rs",
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
                    // The sparkle at rest, and a turn that is retrying.
                    "activity.rs",
                    // Which session asked, named after a card's own title —
                    // every card's head is written in one place (ADR-0010 §3).
                    "form.rs",
                    "panel.rs",
                    "search.rs",
                    "select.rs",
                    // The mark before the task being done: what the model
                    // is on, in the colour of its working.
                    "tasks.rs",
                    "transcript.rs",
                    "tree.rs",
                    "views/actions.rs",
                    "views/badge.rs",
                    "welcome.rs",
                ],
            ),
            ("glow", &[]),
            // A task that is done wears the mark a finished call does.
            (
                "good",
                &["tasks.rs", "transcript.rs", "tree.rs", "views/badge.rs"],
            ),
            (
                "bad",
                &["transcript.rs", "transcript/ran.rs", "views/badge.rs"],
            ),
            ("mode", &["status.rs"]),
            ("breath", &["activity.rs"]),
            // The ramp `presence` → glow: a live bullet, the fill of a bar,
            // and the light a sent line runs along the box's border — one
            // gradient, wherever §4 sanctions one.
            (
                "pulse",
                &[
                    // The mark igniting, which is the head's own light spent
                    // on it (§11).
                    "opening/frame.rs",
                    "transcript.rs",
                    "view.rs",
                    "views/progress.rs",
                ],
            ),
            (
                "comet",
                &["activity.rs", "opening/frame.rs", "transcript.rs"],
            ),
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
            // The one border the light draws, and the one breath it takes
            // when the drawing is done (§11).
            ("hairline", &["opening/frame.rs"]),
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
        assert_eq!(choose(Ask::default(), None).colors, Colors::Ansi);
        assert_eq!(choose(asking(), None).colors, Colors::True(DARK));
        assert_eq!(
            choose(
                Ask {
                    no_color: true,
                    ..asking()
                },
                None
            )
            .colors,
            Colors::Plain,
        );
        assert_eq!(
            choose(
                Ask {
                    ascii: true,
                    ..Ask::default()
                },
                None
            )
            .glyphs,
            &ASCII,
        );
        assert_eq!(choose(Ask::default(), None).glyphs, &UNICODE);
    }

    #[test]
    fn a_terminal_that_answers_light_gets_the_light_palette() {
        let terminal = |light| choose(asking(), light).colors;
        assert_eq!(terminal(Some(true)), Colors::True(LIGHT));
        assert_eq!(terminal(Some(false)), Colors::True(DARK));
        assert_eq!(terminal(None), Colors::True(DARK), "and silence is dark");
    }

    #[test]
    fn a_named_look_outranks_what_the_terminal_answered() {
        let named = |look, light| choose(Ask { look, ..asking() }, light).colors;
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
            choose(
                Ask {
                    no_color: true,
                    look: Look::Light,
                    ..asking()
                },
                Some(true)
            )
            .colors,
            Colors::Plain,
        );
    }

    // ---- and the ground that may change under it (M71) ------------------

    /// The look follows the terminal: an answer that says the other thing is
    /// the palette every draw after it wears, and one that says the same thing
    /// is nothing at all.
    #[test]
    fn a_ground_answered_again_swaps_the_palette() {
        with(crate::painted::truecolor(), || {
            assert_eq!(current().colors, Colors::True(DARK));
            assert!(swap(true), "the ground turned light");
            assert_eq!(current().colors, Colors::True(LIGHT));
            assert!(!swap(true), "and says so again");
            assert_eq!(current().colors, Colors::True(LIGHT));
            assert!(swap(false), "and back");
            assert_eq!(current().colors, Colors::True(DARK));
        });
    }

    /// A swap changes the palette and nothing else: the glyph table a person
    /// asked for is not the terminal's to say.
    #[test]
    fn a_swap_leaves_the_glyphs_where_they_were() {
        with(
            Theme {
                colors: Colors::True(DARK),
                glyphs: &ASCII,
            },
            || {
                assert!(swap(true));
                assert_eq!(current().glyphs, &ASCII);
            },
        );
    }

    /// Where there is no ground to follow there is nothing to swap: eight
    /// colours and `NO_COLOR` both stand still, and say so.
    #[test]
    fn a_look_with_no_ground_to_follow_never_swaps() {
        for look in [crate::painted::no_colour(), crate::painted::ascii()] {
            with(look, || {
                assert!(!follows(), "{:?} has no ground", look.colors);
                assert!(!swap(true));
                assert_eq!(current().colors, look.colors);
            });
        }
        with(crate::painted::truecolor(), || {
            assert!(follows(), "and the native look does")
        });
    }

    /// What the terminal said, as the one number the slot holds it in.
    #[test]
    fn the_ground_survives_the_number_it_is_kept_in() {
        for said in [Some(true), Some(false), None] {
            assert_eq!(ground(number(said)), said);
        }
        assert_eq!(ground(97), None, "and a number nothing wrote is silence");
    }

    /// An `OSC 11` reply, read the way `xparsecolor` reads the colour strings
    /// terminals answer with — both terminators, every channel width, and the
    /// older `#` form (M71).
    #[test]
    fn an_osc_eleven_reply_says_which_ground_the_terminal_has() {
        for (reply, ground) in [
            (b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\".as_slice(), Some(false)),
            (b"\x1b]11;rgb:1e/1e/2e\x07".as_slice(), Some(false)),
            (b"\x1b]11;rgb:0000/0000/0000\x1b\\".as_slice(), Some(false)),
            (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".as_slice(), Some(true)),
            (b"\x1b]11;rgb:f/f/f\x1b\\".as_slice(), Some(true)),
            (b"\x1b]11;rgb:fdfd/f6f6/e3e3\x07".as_slice(), Some(true)),
            (b"\x1b]11;#fdf6e3\x1b\\".as_slice(), Some(true)),
            (b"\x1b]11;#1e1e2e\x1b\\".as_slice(), Some(false)),
        ] {
            assert_eq!(
                answered(reply),
                ground,
                "{:?}",
                String::from_utf8_lossy(reply)
            );
        }
    }

    /// And nothing else is one. A reply to another question, a reply cut short
    /// and a spec in a shape nobody writes all leave the look standing rather
    /// than guessing at a palette.
    #[test]
    fn what_is_not_an_answer_to_that_question_changes_no_look() {
        for reply in [
            b"\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\".as_slice(),
            b"\x1b_Gi=31;OK\x1b\\".as_slice(),
            b"\x1bP>|ghostty 1.3.1\x1b\\".as_slice(),
            b"\x1b]11;rgb:1e1e/1e1e/2e2e".as_slice(),
            b"\x1b]11;\x1b\\".as_slice(),
            b"\x1b]11;rgb:1e1e/2e2e\x1b\\".as_slice(),
            b"\x1b]11;rgb:1e1e/1e1e/2e2e/ffff\x1b\\".as_slice(),
            b"\x1b]11;rgb:zzzz/1e1e/2e2e\x1b\\".as_slice(),
            b"\x1b]11;rgb:11111/1e1e/2e2e\x1b\\".as_slice(),
            b"\x1b]11;#ff\x1b\\".as_slice(),
            b"\x1b]11;teal\x1b\\".as_slice(),
        ] {
            assert_eq!(
                answered(reply),
                None,
                "{:?}",
                String::from_utf8_lossy(reply)
            );
        }
    }

    /// The threshold is the perceptual middle grey `terminal_colorsaurus`
    /// falls back to, so the probe's own answer and a later reply about the
    /// same colour cannot disagree. The pivot is a grey either side of `L* 50`.
    #[test]
    fn the_ground_is_light_from_the_middle_grey_up() {
        let grey = |v: u16| background_is_light(v * 0x101, v * 0x101, v * 0x101);
        assert!(!grey(0x75), "just under the middle");
        assert!(grey(0x79), "and just over");
        assert!(!grey(0x00));
        assert!(grey(0xff));
    }

    /// One row of §4's table: its name and the value it takes in a palette.
    type Token = (&'static str, fn(Palette) -> Color);

    /// Every token of §4's table, in the order the table lists them.
    const TOKENS: &[Token] = &[
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

    /// Every accent reads against the ground its own palette is drawn on. The
    /// ink is not among them any more (M73): what a terminal's own foreground
    /// reads against is the terminal's business, as it is for every other
    /// program.
    #[test]
    fn each_palette_reads_against_its_own_ground() {
        for (name, of) in TOKENS {
            if *name == "raised" || name.ends_with("_tint") {
                continue;
            }
            assert!(
                luma(of(LIGHT)) < luma(LIGHT.raised),
                "`{name}` must read on the light ground"
            );
            assert!(
                luma(of(DARK)) > luma(DARK.raised),
                "`{name}` must read on the dark one"
            );
        }
        assert!(
            luma(DARK.raised) < luma(LIGHT.raised),
            "and each tint is at its own ground's end of the scale"
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
                for token in [presence(), glow(), good(), bad(), mode(), raised()] {
                    assert_eq!(token, Style::new(), "NO_COLOR spends no colour");
                }
                assert_eq!(
                    as_drawn(text()),
                    as_drawn(Style::new()),
                    "and the ink it does spell is the terminal's own"
                );
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
            assert_eq!(caret(), "|");
            assert_eq!(rule(), "-", "the rule is the box's own stroke");
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
        });
    }

    /// A ramp whose far end is the resting ink stops at colour's own edge: the
    /// tail is drawn while it is warm and handed to the terminal's foreground
    /// where it has cooled, in every look (M73).
    #[test]
    fn a_ramp_towards_the_ink_ends_on_the_terminals_own() {
        with(dark(), || {
            assert_eq!(comet(0.0).fg, Some(DARK.glow));
            let cooling = comet(SETTLES / 2.0).fg.expect("a colour on the way");
            assert_ne!(cooling, DARK.glow);
            assert_ne!(cooling, DARK.presence);
            assert_eq!(comet(SETTLES), text(), "and the ink takes the rest");
            assert_eq!(comet(1.0), text());
            assert_eq!(self::cooling(1.0), text());
            assert_eq!(landing(1.0), good().patch(bold()));
            assert_eq!(landing(0.1), bold());
            assert_eq!(warming(0.0), dim());
            assert_eq!(warming(1.0), bad());
        });
    }

    /// The opening's own ramp: the comet's shape, and both ends of it are
    /// somewhere the border already rests — the hairline at one end and the
    /// glow the light's head wears at the other (§11).
    #[test]
    fn a_border_with_light_on_it_runs_from_its_own_hairline_to_the_glow() {
        for look in [dark(), light()] {
            with(look, || {
                assert_eq!(hairline(0.0), dim(), "at rest it is the hairline");
                assert_eq!(hairline(0.5), presence(), "halfway, bingo's colour");
                assert_eq!(hairline(1.0), glow(), "and under the head, its glow");
                assert_eq!(hairline(-1.0), dim(), "clamped");
                assert_eq!(hairline(2.0), glow());
                assert_eq!(hairline(f32::NAN), dim(), "and never a colour of NaN");
                assert_eq!(hairline(0.25), dim(), "the cold half is the hairline");
                let warming = hairline(0.75).fg.expect("a colour between the two");
                assert_ne!(warming, glow().fg.expect("the glow"));
                assert_ne!(warming, presence().fg.expect("bingo's own"));
            });
        }
        assert_eq!(hairline(0.0), dim(), "and the eight take the three stops");
        assert_eq!(hairline(0.5), presence());
        assert_eq!(hairline(1.0), glow());
    }

    fn light() -> Theme {
        Theme {
            colors: Colors::True(LIGHT),
            glyphs: &UNICODE,
        }
    }

    #[test]
    fn a_notice_arrives_out_of_dim_and_leaves_into_it() {
        let level = bingo_sdk::Level::Error;
        for look in [dark(), ascii()] {
            with(look, || {
                assert_eq!(fading(level, 0.0), dim());
                assert_eq!(fading(level, 1.0), bad());
            });
        }
    }

    /// The two tokens the terminal owns: no look, and no answer about the
    /// ground, puts a colour of ours in prose or in what stands behind it
    /// (M73). This is the whole of why a scheme flip is instant.
    #[test]
    fn the_ink_and_the_dim_are_the_terminals_own_in_every_look() {
        for look in [
            crate::painted::no_colour(),
            crate::painted::ascii(),
            crate::painted::truecolor(),
            crate::painted::daylight(),
        ] {
            with(look, || {
                assert_eq!(text(), Style::new().fg(Color::Reset), "{:?}", look.colors);
                assert_eq!(
                    dim(),
                    Style::new().add_modifier(Modifier::DIM),
                    "{:?}",
                    look.colors
                );
            });
        }
    }

    #[test]
    fn nothing_that_moves_spends_a_colour_under_no_colour() {
        with(no_colour(), || {
            for style in [
                breath(1.0),
                pulse(1.0),
                comet(0.0),
                warming(1.0),
                hairline(1.0),
            ] {
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
