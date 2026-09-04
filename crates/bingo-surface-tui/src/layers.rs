//! The two things that come over the frame, and how they arrive.
//!
//! A **card** is the dialog form — a permission, a question, the switcher: a
//! bordered box under the row that asked, with the only bright border on the
//! screen, revealed top-down over three frames. The form card is the one
//! exception, a band between two dim rules ([`Shape`]). A **sheet** is the whole frame
//! for a moment — help, the panel, the resume picker — sliding up from the
//! composer over four. Behind either, the world dims.
//!
//! Where a layer is in its arrival is a pure function of the clock, so every
//! frame of it is a test rather than something to watch for.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::clock::Anim;
use crate::theme;

/// A card comes down over this many frames (§6).
pub const CARD_FRAMES: u16 = 3;
/// A sheet slides up over this many.
pub const SHEET_FRAMES: u16 = 4;
/// How long one frame of an arrival lasts: the animation clock's own (§6).
pub const PER_FRAME: Duration = crate::clock::FRAME;

/// How far a layer has come in. `frame` counts from 0 (not yet on screen) to
/// `of` (all of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reveal {
    pub frame: u16,
    pub of: u16,
    /// Which way it is going, which is what "there is another frame after
    /// this one" depends on.
    closing: bool,
}

impl Reveal {
    /// All of it, at once: what a layer looks like where nothing may move.
    pub fn whole(of: u16) -> Self {
        Self {
            frame: of,
            of,
            closing: false,
        }
    }

    /// None of it: where nothing may move, what closes is closed.
    pub fn none(of: u16) -> Self {
        Self {
            frame: 0,
            of,
            closing: true,
        }
    }

    /// Where an arrival that started at `since` is now. Closing runs the same
    /// frames backwards, which is what `esc` shows.
    pub fn at(of: u16, since: Instant, now: Instant, closing: bool) -> Self {
        let step = Anim::new(since, PER_FRAME * u32::from(of)).step(now, of);
        let frame = match closing {
            false => step.saturating_add(1).min(of),
            true => of.saturating_sub(step),
        };
        Self { frame, of, closing }
    }

    /// Whether the next frame would draw it differently: on its way in until
    /// it is whole, on its way out until it has gone.
    pub fn moving(&self) -> bool {
        match self.closing {
            false => self.frame < self.of,
            true => self.frame > 0,
        }
    }

    pub fn gone(&self) -> bool {
        self.frame == 0
    }

    /// The rows of `total` this frame shows.
    fn rows(&self, total: u16) -> u16 {
        if self.frame >= self.of {
            return total;
        }
        (u32::from(total) * u32::from(self.frame) / u32::from(self.of.max(1))) as u16
    }
}

/// Everything on the screen goes dim. What is behind a layer is behind it
/// (§3): a style pass over what is already painted, so no view has to know
/// whether something is open above it.
pub fn dim(frame: &mut Frame) {
    hush(frame, frame.area());
}

/// The same pass over one region: what a transcript being stepped into
/// crossfades through (§6).
pub fn hush(frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buffer[(x, y)].set_style(theme::dim());
        }
    }
}

/// What a card is drawn as. A box is every card's shape and §2's law — the
/// only bright border on the screen. A **band** is the form card's alone, at
/// the user's word on 2026-09-04 (design §2, §10): no border, one dim rule
/// where the box's top edge was, because a set of questions with a mockup
/// beside it reads as a page of the transcript rather than as a box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Boxed,
    Band,
}

impl Shape {
    /// The rows the shape itself spends: two borders, or one rule.
    pub fn chrome(self) -> u16 {
        match self {
            Self::Boxed => 2,
            Self::Band => 1,
        }
    }

    /// The cells its lines are held in from each side: a box pads by one
    /// inside its border, a band starts where the transcript's own rows do.
    pub fn inset(self) -> u16 {
        match self {
            Self::Boxed => 2,
            Self::Band => 0,
        }
    }

    /// The rows `lines` need of `at`, the shape's own included.
    pub fn height(self, lines: usize, at: Rect) -> u16 {
        u16::try_from(lines)
            .unwrap_or(u16::MAX)
            .saturating_add(self.chrome())
            .min(at.height)
    }
}

/// A card at `at`, revealed top-down. Its top edge lands first, so it grows
/// downwards into the transcript and nothing under it moves. While the
/// kernel's guard is down its rows are dim — the edge is bright, because the
/// card has arrived; the answers are not yet listening.
///
/// A box's border wears [`theme::attention`], the one beat everything that
/// wants a person shares (§6): a card is on the screen exactly while it is
/// unanswered, so it is asking for the whole of its life and stops by going.
pub fn card(
    frame: &mut Frame,
    at: Rect,
    shape: Shape,
    lines: Vec<Line<'static>>,
    reveal: Reveal,
    guarded: bool,
    now: crate::clock::Now,
) {
    let lines = match guarded {
        true => lines.into_iter().map(hushed).collect(),
        false => lines,
    };
    let full = shape.height(lines.len(), at);
    let shown = reveal.rows(full);
    if shown == 0 {
        return;
    }
    let area = Rect {
        height: shown,
        ..at
    };
    frame.render_widget(Clear, area);
    match shape {
        Shape::Boxed => bordered(frame, area, shown == full, now),
        Shape::Band => ruled(frame, area),
    }
    body(frame, at, shape, lines, full, area.bottom());
}

/// The box's own edge. While it is still coming down it has no foot: the
/// bottom border belongs to the last frame, not to every one of them.
fn bordered(frame: &mut Frame, area: Rect, whole: bool, now: crate::clock::Now) {
    let sides = match whole {
        true => Borders::ALL,
        false => Borders::TOP | Borders::LEFT | Borders::RIGHT,
    };
    frame.render_widget(
        Block::new()
            .borders(sides)
            .border_type(BorderType::Rounded)
            .border_style(theme::attention(now)),
        area,
    );
}

/// The band's own edge: one dim rule the width of the region, where the box's
/// top border was. The rule that closes the band is a line of the card, so
/// what hangs under it — the way out and the keys — hangs outside it.
fn ruled(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(rule(usize::from(area.width))),
        Rect { height: 1, ..area },
    );
}

/// One dim rule `width` cells wide: the stroke a box draws its edge with, so a
/// band and a box are one line and not two facts (§4).
pub fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled(theme::rule().repeat(width), theme::dim()))
}

/// The card's own rows, in the room the shape leaves them.
fn body(
    frame: &mut Frame,
    at: Rect,
    shape: Shape,
    lines: Vec<Line<'static>>,
    full: u16,
    bottom: u16,
) {
    let inner = Rect {
        x: at.x + shape.inset(),
        y: at.y + 1,
        width: at.width.saturating_sub(2 * shape.inset()),
        height: full.saturating_sub(shape.chrome()),
    };
    let rows = inner.height.min(bottom.saturating_sub(inner.y));
    if rows == 0 || inner.width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(fitted(lines, inner.height as usize)),
        Rect {
            height: rows,
            ..inner
        },
    );
}

/// One row with every colour taken out of it: what a card wears until it can
/// be answered.
fn hushed(line: Line<'static>) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|span| Span::styled(span.content, theme::dim()))
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// What a card taller than its room shows: its title, then its newest rows —
/// the question and its answers are what was asked for, so the preview is
/// what gives way.
fn fitted(lines: Vec<Line<'static>>, rows: usize) -> Vec<Line<'static>> {
    if lines.len() <= rows {
        return lines;
    }
    if rows < 2 {
        return lines.into_iter().take(rows).collect();
    }
    let mut out = vec![lines[0].clone()];
    out.extend(lines[lines.len() - (rows - 1)..].iter().cloned());
    out
}

/// A sheet fills `at` from its foot upwards: it comes out of the composer.
pub fn sheet(frame: &mut Frame, at: Rect, lines: Vec<Line<'static>>, reveal: Reveal) {
    let shown = reveal.rows(at.height);
    if shown == 0 {
        return;
    }
    let area = Rect {
        y: at.bottom() - shown,
        height: shown,
        ..at
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).style(theme::raised()), area);
}

/// Where a card sits: under the row that asked, and against the foot of the
/// region when that row is not on the screen or the box would hang off it.
pub fn under(region: Rect, row: Option<u16>, height: u16) -> Rect {
    let height = height.min(region.height);
    let top = match row {
        Some(row) if row + height <= region.bottom() => row,
        _ => region.bottom().saturating_sub(height),
    };
    Rect {
        y: top.max(region.y),
        height,
        ..region
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_card_comes_down_over_three_frames() {
        let start = now();
        let frames: Vec<u16> = (0..5)
            .map(|i| Reveal::at(CARD_FRAMES, start, start + PER_FRAME * i, false).frame)
            .collect();
        assert_eq!(frames, vec![1, 2, 3, 3, 3], "it is on screen at once");
        assert!(Reveal::at(CARD_FRAMES, start, start, false).moving());
        assert!(!Reveal::at(CARD_FRAMES, start, start + PER_FRAME * 3, false).moving());
    }

    #[test]
    fn esc_runs_the_same_frames_backwards() {
        let start = now();
        let frames: Vec<u16> = (0..5)
            .map(|i| Reveal::at(CARD_FRAMES, start, start + PER_FRAME * i, true).frame)
            .collect();
        assert_eq!(frames, vec![3, 2, 1, 0, 0]);
        assert!(
            Reveal::at(CARD_FRAMES, start, start, true).moving(),
            "a layer on its way out has frames left to draw"
        );
        assert!(Reveal::at(CARD_FRAMES, start, start + PER_FRAME * 3, true).gone());
        assert!(!Reveal::at(CARD_FRAMES, start, start + PER_FRAME * 3, true).moving());
    }

    #[test]
    fn each_frame_shows_its_share_of_the_rows() {
        let of = CARD_FRAMES;
        let rows = |frame| {
            Reveal {
                frame,
                of,
                closing: false,
            }
            .rows(9)
        };
        assert_eq!(rows(0), 0);
        assert_eq!(rows(1), 3);
        assert_eq!(rows(2), 6);
        assert_eq!(rows(3), 9);
    }

    #[test]
    fn a_sheet_takes_four_frames_to_fill_its_region() {
        let of = SHEET_FRAMES;
        let rows = |frame| {
            Reveal {
                frame,
                of,
                closing: false,
            }
            .rows(20)
        };
        assert_eq!([rows(1), rows(2), rows(3), rows(4)], [5, 10, 15, 20]);
    }

    #[test]
    fn a_card_hangs_under_the_row_that_asked() {
        let region = Rect::new(0, 0, 80, 20);
        assert_eq!(under(region, Some(4), 6), Rect::new(0, 4, 80, 6));
    }

    #[test]
    fn a_card_with_no_room_under_that_row_sits_at_the_foot() {
        let region = Rect::new(0, 0, 80, 20);
        assert_eq!(under(region, Some(18), 6), Rect::new(0, 14, 80, 6));
        assert_eq!(under(region, None, 6), Rect::new(0, 14, 80, 6));
        assert_eq!(
            under(region, Some(2), 40),
            region,
            "a card taller than the region is the region"
        );
    }
}
