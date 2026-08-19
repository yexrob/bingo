//! The motion layer: one gate, seven tokens.
//!
//! Before this module every animated surface reinvented its own timing from the
//! raw host tick, and the `motion` setting reached only two of them — the
//! spinner, the loudest moving thing on screen, spun on regardless. That is the
//! bug this module exists to make impossible: **nothing animated may read the
//! tick directly**. Every moving surface asks a [`Motion`] for a token, and
//! [`Motion`] is built once from settings, so the gate is checked in exactly one
//! place.
//!
//! The seven tokens:
//!
//! | token | what moves | cadence |
//! |---|---|---|
//! | [`Motion::pulse`] | the status-row spinner glyph | one frame / 120 ms |
//! | [`Motion::beam`] | a glimmer sweeping the running verb | one pass / 2 s |
//! | [`Motion::stall`] | the verb turns warning-coloured | after 3 s of silence |
//! | [`Motion::settle`] | the completion line blinks accent | one 120 ms window |
//! | [`Motion::breath`] | the update banner's colour | 3 s sine, unchanged |
//! | [`Motion::title_glyph`] | the terminal title's prefix | one frame / 960 ms |
//! | [`Meter`] | a number that jumped eases to it | 300 ms ease-out |
//!
//! **Time is an argument, never a reading.** Every function here takes the host
//! tick (or a tick difference); none calls `Instant::now`. Tests drive time by
//! passing numbers, and the demand-gated render loop stays deterministic.
//!
//! **Gate semantics.** `motion: "off"` (or `BINGO_NO_MOTION=1`) rests every
//! *decoration*: the spinner freezes on `✻`, the beam yields nothing, the banner
//! holds its base colour, the title keeps a static `✳`, a jumped number snaps.
//! [`Motion::stall`] and [`Motion::settle`] keep reporting, because a stalled
//! turn and a finished one are **information** — the user who turned motion off
//! asked for stillness, not for less to be told.

use ratatui::style::Color;

use crate::tui::theme::Theme;

/// Host frame interval. The tick loop runs at this rate while anything is
/// moving; every token below converts ticks to milliseconds through it, which
/// is why a 120 ms frame is 120 ms and not "about four ticks".
pub const TICK_MS: u64 = 33;

/// Spinner frames: a star that grows and shrinks (`·` → `✻`/`✽` → `·`). The
/// sequence is a there-and-back cycle, so the glyph never jumps between sizes.
pub const SPINNERS: [char; 8] = ['·', '✢', '*', '✻', '✽', '✻', '*', '✢'];

/// The glyph a gated-off spinner rests on: the mid-size star, so a still
/// terminal reads as "running", not as "stuck at a dot".
pub const REST_GLYPH: char = SPINNERS[3];

/// Terminal-title frames while a turn runs: the `✳` marker alternating with the
/// two braille half-cells, which is how Claude Code animates a tab title without
/// making it flicker.
const TITLE_FRAMES: [char; 4] = ['✳', '⠂', '✳', '⠐'];

/// The title's prefix at rest — the same marker D79 already ships, unanimated.
pub const TITLE_REST_GLYPH: char = TITLE_FRAMES[0];

/// One spinner glyph lasts this long. Four times the frame interval: the old
/// code advanced every tick, which read as a blur rather than a rhythm.
const PULSE_MS: u64 = 120;

/// A glimmer crosses the running verb once per this period.
const BEAM_PERIOD_MS: u64 = 2_000;

/// How many cells the glimmer brightens at once. Eight rather than six (D93):
/// on a real terminal the six-cell window read as a rendering artefact rather
/// than as a sweep — there was never enough lit at once to see it travel.
const BEAM_WIDTH: usize = 8;

/// How far the swept cells travel from the verb's own colour towards the theme's
/// body text. The old glimmer moved `claude → claude_strong`, a step of ~18/255
/// per channel that a dark terminal swallowed whole.
const BEAM_LIFT: f64 = 0.55;

/// **beam colour** — what a swept cell is painted, derived from the theme
/// rather than named as a literal, so both presets and any user override stay
/// in their own palette.
///
/// The pole is the theme's body text, which is the highest-contrast ink it owns
/// and therefore the direction visibility lives in — nearly white over a dark
/// ground, black over a light one. A dark theme's sweep brightens and a light
/// theme's darkens, which is what a glimmer has to do in each to be seen at all;
/// what they share is moving *away* from the verb's own colour rather than one
/// step along the same ramp. Terminals without truecolor take the pole itself: a
/// discrete two-tone sweep, the same trade the update banner makes.
pub fn beam_color(theme: &Theme, base: Color) -> Color {
    lerp_color(base, theme.text, BEAM_LIFT)
}

/// Silence longer than this, mid-turn, is worth saying out loud.
pub const STALL_MS: u64 = 3_000;

/// How long the completion blink lasts: one spinner frame, so it reads as the
/// spinner's last beat rather than as a new animation.
const SETTLE_MS: u64 = 120;

/// One terminal-title frame. Deliberately slow: a tab title that changes 30
/// times a second is a distraction in the window list, not a signal.
const TITLE_FRAME_MS: u64 = 960;

/// How long a numeric readout takes to travel to a new value.
const METER_MS: u64 = 300;

/// The update banner breathes over this many frames (3.0 s per cycle).
pub const UPDATE_BANNER_PERIOD: u64 = 90;

/// Milliseconds a tick count is worth.
const fn ms(ticks: u64) -> u64 {
    ticks.saturating_mul(TICK_MS)
}

/// The motion gate plus the seven tokens hanging off it.
///
/// Built once (from settings) and copied to every render site; there is no
/// interior state, so passing it around costs a bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Motion {
    on: bool,
}

impl Default for Motion {
    /// Motion on — the `auto` default. A `Chat` built without settings (tests,
    /// the entity modal) animates exactly as the shipped default does.
    fn default() -> Self {
        Self { on: true }
    }
}

impl Motion {
    /// A gate in an explicit position (tests, and the two callers that already
    /// know the answer).
    pub const fn new(on: bool) -> Self {
        Self { on }
    }

    /// Resolve the gate: settings key `motion: "off"`, or `BINGO_NO_MOTION` in
    /// the environment. This is the **only** place either is read.
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        let off = settings.motion.as_deref() == Some("off")
            || std::env::var_os("BINGO_NO_MOTION").is_some();
        Self::new(!off)
    }

    /// Whether decoration rests. The one shape the gate is ever read in — the
    /// setting is named for its off state, and so is every consumer.
    pub const fn off(self) -> bool {
        !self.on
    }

    /// **pulse** — the spinner glyph for this tick: one frame per 120 ms,
    /// resting on [`REST_GLYPH`] when motion is off.
    pub fn pulse(self, tick: u64) -> char {
        if self.off() {
            return REST_GLYPH;
        }
        let frame = ms(tick) / PULSE_MS;
        SPINNERS[(frame % SPINNERS.len() as u64) as usize]
    }

    /// **beam** — the half-open `[start, end)` character window the glimmer
    /// brightens, over a run of `len` characters.
    ///
    /// The window enters from the left edge and leaves off the right one, so the
    /// sweep covers `len + BEAM_WIDTH` positions per [`BEAM_PERIOD_MS`] and there
    /// is a dark beat between passes. `None` = nothing is lit right now (motion
    /// off, an empty run, or that dark beat).
    pub fn beam(self, tick: u64, len: usize) -> Option<(usize, usize)> {
        if self.off() || len == 0 {
            return None;
        }
        let span = (len + BEAM_WIDTH) as u64;
        let head = (ms(tick) % BEAM_PERIOD_MS) * span / BEAM_PERIOD_MS;
        let end = (head as usize).min(len);
        let start = (head as usize).saturating_sub(BEAM_WIDTH);
        (start < end).then_some((start, end))
    }

    /// **stall** — whether a busy turn has gone quiet for longer than
    /// [`STALL_MS`]. Reports under the gate: a stalled turn is information.
    pub fn stall(self, tick: u64, last_progress: u64) -> bool {
        ms(tick.saturating_sub(last_progress)) >= STALL_MS
    }

    /// **settle** — whether the completion line is still inside its accent
    /// window. A blink, not a loop: true for one [`SETTLE_MS`] window after
    /// `completed_at` and false forever after. Reports under the gate, for the
    /// same reason [`Motion::stall`] does.
    pub fn settle(self, tick: u64, completed_at: u64) -> bool {
        ms(tick.saturating_sub(completed_at)) < SETTLE_MS
    }

    /// **breath** — the update banner's colour at `frame` (see [`update_color`]
    /// for the curve; it is unchanged from the banner spec).
    pub fn breath(self, theme: &Theme, frame: u64) -> Color {
        update_color(theme, frame, self.off())
    }

    /// **title_glyph** — the terminal title's prefix for this tick: `✳` and the
    /// braille half-cells alternating every [`TITLE_FRAME_MS`], resting on
    /// [`TITLE_REST_GLYPH`] when motion is off.
    pub fn title_glyph(self, tick: u64) -> char {
        if self.off() {
            return TITLE_REST_GLYPH;
        }
        let frame = ms(tick) / TITLE_FRAME_MS;
        TITLE_FRAMES[(frame % TITLE_FRAMES.len() as u64) as usize]
    }
}

/// **meter** — a number that eases to its target instead of snapping to it.
///
/// A token counter that jumps by 300 mid-stream reads as a glitch; the same jump
/// crossed in 300 ms reads as counting. State is three integers, and the value is
/// a pure function of the tick, so the display never drifts from the target: at
/// `start + 300 ms` it *is* the target, exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meter {
    /// Where this travel began.
    from: u64,
    /// Where it is going. Also the value once the travel is over.
    to: u64,
    /// The tick the travel began on.
    start: u64,
}

impl Meter {
    /// Aim at a new value from wherever the display currently is. A repeat of
    /// the current target is a no-op, so a stream that reports the same count
    /// twice does not restart the travel.
    pub fn retarget(&mut self, target: u64, tick: u64, motion: Motion) {
        if target == self.to {
            return;
        }
        self.from = self.value(tick, motion);
        self.to = target;
        self.start = tick;
    }

    /// Jump to `value` with no travel (a new turn: the counter restarts at 0 and
    /// must not be seen easing down from the last turn's total).
    pub fn reset(&mut self, value: u64, tick: u64) {
        *self = Self {
            from: value,
            to: value,
            start: tick,
        };
    }

    /// The value to display at `tick`. Motion off → the target, always.
    pub fn value(&self, tick: u64, motion: Motion) -> u64 {
        if motion.off() {
            return self.to;
        }
        let elapsed = ms(tick.saturating_sub(self.start));
        if elapsed >= METER_MS {
            return self.to;
        }
        let t = elapsed as f64 / METER_MS as f64;
        // Ease-out quadratic: fast off the mark, gentle into the target, which
        // is what makes a counter read as settling rather than as sliding.
        let eased = 1.0 - (1.0 - t) * (1.0 - t);
        let from = self.from as f64;
        (from + (self.to as f64 - from) * eased).round().max(0.0) as u64
    }
}

/// Update-banner breathing colours (update-banner spec §2, pure and testable):
/// - `motion_off` → always rest (static, banner kept);
/// - no truecolor (theme downgraded to 256 colors) → discrete two-step: 60-frame
///   cycle, peak for the first 12 frames (≥400ms);
/// - truecolor → sine breathing: `t = 0.5 − 0.5·cos(2π·phase/90)`, frame 0 = rest
///   (trough), 45 = peak, 90 = back to rest; linear interpolation per sRGB channel.
///
/// Stops: dark `#D77757 ↔ #E8896B` (≥6.24:1 throughout); light `#B05227 ↔ #9A4A24`
/// (≥4.72:1 throughout).
///
/// Moved here from `chat_tail.rs` by D87, curve and stops untouched: it was the
/// one honest animation bingo had, and it is now the `breath` token.
pub fn update_color(theme: &Theme, frame: u64, motion_off: bool) -> Color {
    let rest = if theme.is_dark {
        theme.claude
    } else {
        theme.claude_deep
    };
    if motion_off {
        return rest;
    }
    let peak = if theme.is_dark {
        theme.claude_strong
    } else {
        theme.claude_deep_strong
    };
    if !matches!(theme.claude_strong, Color::Rgb(..)) {
        // Discrete two-step (256-color terminal): 60-frame cycle, peak 400ms (12 frames) → rest 1600ms.
        return if frame % 60 < 12 { peak } else { rest };
    }
    let phase = (frame % UPDATE_BANNER_PERIOD) as f64;
    let t = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * phase / UPDATE_BANNER_PERIOD as f64).cos();
    lerp_color(rest, peak, t)
}

/// Per-channel sRGB linear interpolation (the two stops are close; no gamma
/// correction — the spec notes it is out of scope).
fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return b;
    };
    let l = |x: u8, y: u8| {
        (x as f64 + (y as f64 - x as f64) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Motion = Motion::new(true);
    const OFF: Motion = Motion::new(false);

    /// Ticks that a millisecond count is worth, rounded up — the boundary a
    /// cadence test wants ("the first tick at or past 120 ms").
    fn ticks_for(millis: u64) -> u64 {
        millis.div_ceil(TICK_MS)
    }

    /// The gate is one switch, and it rests every decoration: no glyph cycling,
    /// no sweep, no breathing, no title frames, no easing.
    #[test]
    fn the_gate_rests_every_decoration() {
        for tick in [0u64, 1, 4, 17, 91, 500, 10_000] {
            assert_eq!(OFF.pulse(tick), REST_GLYPH, "spinner rests on ✻");
            assert_eq!(OFF.beam(tick, 12), None, "no sweep");
            assert_eq!(OFF.title_glyph(tick), TITLE_REST_GLYPH, "title rests on ✳");
        }
        let dark = Theme::dark();
        assert_eq!(OFF.breath(&dark, 45), dark.claude, "banner holds its base");
        let mut meter = Meter::default();
        meter.retarget(4_000, 0, OFF);
        assert_eq!(meter.value(0, OFF), 4_000, "a number snaps");
    }

    /// …but never stops *telling*: a stall and a completion are states of the
    /// turn, and the user who asked for stillness still gets the colour.
    #[test]
    fn the_gate_still_reports_stall_and_settle() {
        let stall_tick = ticks_for(STALL_MS);
        assert!(OFF.stall(stall_tick, 0), "a stalled turn still says so");
        assert!(!OFF.stall(stall_tick - 1, 0));
        assert!(OFF.settle(0, 0), "a finished turn still blinks accent");
        assert!(!OFF.settle(ticks_for(SETTLE_MS), 0));
    }

    /// One glyph per 120 ms — not one per tick, which is what it used to be and
    /// what made the spinner a blur.
    #[test]
    fn pulse_advances_one_frame_per_120ms() {
        // The frame index is the elapsed-milliseconds quotient, and the glyph is
        // that index into the unchanged sequence.
        for tick in 0..200u64 {
            let frame = (tick * TICK_MS) / PULSE_MS;
            assert_eq!(
                ON.pulse(tick),
                SPINNERS[(frame % 8) as usize],
                "tick {tick} belongs to frame {frame}"
            );
        }
        // A glyph holds for a whole window and then moves on.
        assert_eq!(ON.pulse(0), SPINNERS[0]);
        assert_eq!(ON.pulse(3), SPINNERS[0], "99ms is still frame 0");
        assert_eq!(ON.pulse(4), SPINNERS[1], "132ms is frame 1");
        assert_eq!(ON.pulse(7), SPINNERS[1]);
        assert_eq!(ON.pulse(8), SPINNERS[2], "264ms is frame 2");
        // The sequence order survives, and so does the sequence itself.
        assert_eq!(SPINNERS[0], '·');
        assert!(SPINNERS.contains(&'✻'));
        assert!(!SPINNERS.contains(&'⠋'), "the star family, not braille");
    }

    /// The old spinner would have run ~3.6× faster over the same wall time; the
    /// point of the token is exactly that it does not.
    #[test]
    fn pulse_is_slower_than_the_raw_tick_it_replaced() {
        let one_second = ticks_for(1_000);
        let frames = (0..one_second)
            .map(|t| ON.pulse(t))
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] != w[1])
            .count();
        assert_eq!(frames, 8, "≈8 glyph changes per second, one per 120ms");
    }

    /// The glimmer walks left to right, stays inside the text, and comes back
    /// around once per period.
    #[test]
    fn beam_sweeps_monotonically_and_wraps() {
        let len = 12usize;
        let period = ticks_for(BEAM_PERIOD_MS);
        let mut last_start = 0usize;
        let mut lit_at_all = false;
        for tick in 0..period {
            let Some((start, end)) = ON.beam(tick, len) else {
                continue;
            };
            assert!(start < end, "an empty window is None, not a window");
            assert!(end <= len, "the sweep never runs off the text");
            assert!(end - start <= BEAM_WIDTH, "the window has a fixed width");
            assert!(start >= last_start, "the sweep only moves forward");
            last_start = start;
            lit_at_all = true;
        }
        assert!(lit_at_all, "something is lit during a period");
        assert!(last_start > 0, "the sweep reaches the far end");
        // Wrap: the far end is reached just before the period is up, and the
        // next pass starts over from the left edge.
        let head = |tick: u64| ON.beam(tick, len).map_or(0, |(start, _)| start);
        assert!(head(period - 1) > 0, "at the far end before the wrap");
        assert_eq!(head(period), 0, "back at the left edge after it");
    }

    /// A short verb is swept too, and a zero-length one is simply not lit.
    #[test]
    fn beam_is_bounded_by_the_text_it_sweeps() {
        assert_eq!(ON.beam(10, 0), None, "nothing to light");
        for tick in 0..ticks_for(BEAM_PERIOD_MS) {
            if let Some((start, end)) = ON.beam(tick, 3) {
                assert!(start < end && end <= 3);
            }
        }
    }

    /// D93: the sweep is wide enough and bright enough to be seen.
    ///
    /// The old glimmer moved one step along the accent ramp — `claude` to
    /// `claude_strong`, about 18/255 per channel — over six cells, and on a
    /// real dark terminal it read as nothing at all. Both halves are asserted
    /// against the theme rather than against literals, so a retuned palette
    /// keeps the property instead of silently losing it.
    #[test]
    fn the_beam_is_wide_and_bright_enough_to_notice() {
        // Wide enough: a long verb is lit BEAM_WIDTH cells at a time.
        let widest = (0..ticks_for(BEAM_PERIOD_MS))
            .filter_map(|tick| ON.beam(tick, 40))
            .map(|(start, end)| end - start)
            .max()
            .unwrap_or(0);
        assert_eq!(
            widest, 8,
            "the window opens to its full width — eight cells, where six read              as an artefact rather than as a sweep"
        );
        assert_eq!(widest, BEAM_WIDTH, "and the constant is what it opens to");

        // Bright enough, in both themes, and by a wider margin than the step it
        // replaced.
        let channel_distance = |a: Color, b: Color| match (a, b) {
            (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
                (ar.abs_diff(br) as u32) + (ag.abs_diff(bg) as u32) + (ab.abs_diff(bb) as u32)
            }
            _ => u32::MAX,
        };
        for theme in [Theme::dark(), Theme::light()] {
            let base = theme.claude;
            let lit = beam_color(&theme, base);
            assert_ne!(lit, base, "a glimmer the colour of the verb is not one");
            let was = channel_distance(base, theme.claude_strong);
            let now = channel_distance(base, lit);
            assert!(
                now > was * 2,
                "dark={} old step {was}, new step {now}",
                theme.is_dark
            );
        }
    }

    /// Three seconds of silence flips the verb; anything arriving resets it.
    #[test]
    fn stall_flips_at_three_seconds_and_progress_resets_it() {
        let stall_tick = ticks_for(STALL_MS);
        for tick in 0..stall_tick {
            assert!(!ON.stall(tick, 0), "tick {tick} is under 3s");
        }
        assert!(ON.stall(stall_tick, 0), "crossing 3s flips");
        assert!(ON.stall(stall_tick + 500, 0), "and stays flipped");
        // Progress moves the baseline, not the threshold.
        assert!(!ON.stall(stall_tick + 500, stall_tick + 500));
        assert!(!ON.stall(stall_tick + 500, 500 + 1));
    }

    /// A blink: accent for exactly the first window, rest from then on.
    #[test]
    fn settle_is_one_window_and_then_rest() {
        let window = ticks_for(SETTLE_MS);
        for tick in 0..window {
            assert!(ON.settle(tick, 0), "tick {tick} is inside the blink");
        }
        for tick in window..window + 50 {
            assert!(!ON.settle(tick, 0), "tick {tick} has rested");
        }
        // A completion that has not happened yet cannot be settling backwards.
        assert!(ON.settle(3, 5), "a clamped difference is inside the window");
    }

    /// The travel lands on the target within 300 ms, never overshoots, and only
    /// ever moves one way.
    #[test]
    fn meter_eases_to_its_target_within_300ms() {
        let mut meter = Meter::default();
        meter.retarget(1_000, 0, ON);
        let window = ticks_for(METER_MS);
        let mut prev = meter.value(0, ON);
        assert!(prev < 1_000, "it starts short of the target");
        for tick in 1..window {
            let v = meter.value(tick, ON);
            assert!(v >= prev, "monotonic: {v} after {prev}");
            assert!(v <= 1_000, "never overshoots");
            prev = v;
        }
        assert_eq!(meter.value(window, ON), 1_000, "arrives within 300ms");
        assert_eq!(meter.value(window + 999, ON), 1_000, "and stays");
    }

    /// Re-aiming mid-travel starts from what is on screen, so the number never
    /// jumps backwards to catch up.
    #[test]
    fn meter_retargets_from_the_displayed_value() {
        let mut meter = Meter::default();
        meter.retarget(1_000, 0, ON);
        let mid = meter.value(4, ON);
        meter.retarget(2_000, 4, ON);
        assert_eq!(meter.value(4, ON), mid, "no jump at the hand-off");
        assert_eq!(meter.value(4 + ticks_for(METER_MS), ON), 2_000);
        // Re-aiming at the value it is already heading for changes nothing.
        let before = meter.value(6, ON);
        meter.retarget(2_000, 6, ON);
        assert_eq!(meter.value(6, ON), before, "a repeat target is a no-op");
        // A new turn resets rather than eases down from the old total.
        meter.reset(0, 9);
        assert_eq!(meter.value(9, ON), 0);
    }

    /// The title changes about once a second, not thirty times.
    #[test]
    fn title_frames_alternate_on_960ms_boundaries() {
        assert_eq!(ON.title_glyph(0), '✳');
        let one = ticks_for(TITLE_FRAME_MS);
        assert_eq!(ON.title_glyph(one - 1), '✳', "still the first frame");
        assert_eq!(ON.title_glyph(one), '⠂');
        assert_eq!(ON.title_glyph(one * 2), '✳', "the marker comes back");
        assert_eq!(ON.title_glyph(one * 3), '⠐');
        assert_eq!(ON.title_glyph(one * 4), '✳', "and the cycle wraps");
        // The throttle this buys: one change per 960ms, so ~1 title write/second.
        let changes = (0..ticks_for(4_000))
            .map(|t| ON.title_glyph(t))
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] != w[1])
            .count();
        assert_eq!(changes, 4, "four frame changes in four seconds");
    }

    /// The breathing curve is the banner's, unchanged by the move: the wave test
    /// that guards it lives with the banner's other tests (`chat_tests_a.rs`).
    /// This one only asserts the token delegates to it.
    #[test]
    fn breath_is_the_banner_curve_behind_the_gate() {
        let dark = Theme::dark();
        assert_eq!(ON.breath(&dark, 45), update_color(&dark, 45, false));
        assert_eq!(OFF.breath(&dark, 45), update_color(&dark, 45, true));
    }
}
