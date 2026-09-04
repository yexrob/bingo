//! `View::Progress`: `████████░░ 80 % · label` (design §5). The fill ramps
//! `presence` → glow across its lit run — one of the two places §4 spends a
//! gradient — and the track is dim. Without a total there is no fraction to
//! show, so the head of the track carries the count instead and a three-cell
//! sheen walks the track to say the work is alive.

use std::time::Duration;

use ratatui::text::{Line, Span};

use crate::clock::{self, Now};
use crate::theme;

/// How wide the track is, in cells.
const TRACK: usize = 10;
/// The lit run of an unbounded bar, which walks the track.
const SHEEN: usize = 3;
/// One turn of that walk: ten cells at four a second (§6's rhythms).
const WALK: Duration = Duration::from_millis(2_500);
const FILLED: &str = "█";
const EMPTY: &str = "░";

/// Where the lit run starts and how long it is: the two shapes of a bar, told
/// apart by nothing else.
type Run = (usize, usize);

/// Where every unbounded bar on the screen is in its walk this frame. It is
/// free-running, like the breath and the pulse, so two bars are never out of
/// step with one another; still at the head when nothing may move (§6).
pub fn walk(now: Now) -> f32 {
    match now.motion {
        true => clock::phase(now, WALK),
        false => 0.0,
    }
}

pub fn lines(
    value: u64,
    total: Option<u64>,
    label: Option<&str>,
    width: usize,
    beat: f32,
) -> Vec<Line<'static>> {
    let (run, amount) = match total.filter(|total| *total > 0) {
        Some(total) => bounded(value, total),
        None => (walking(beat), value.to_string()),
    };
    let mut spans = track(run);
    spans.push(Span::styled(format!(" {amount}"), theme::text()));
    if let Some(label) = label {
        spans.push(Span::styled(
            crate::views::clip(
                &format!(" · {label}"),
                width.saturating_sub(TRACK + 1 + amount.len()),
            ),
            theme::dim(),
        ));
    }
    vec![Line::from(spans)]
}

/// How much of the track is lit, and the percentage that says it in words.
fn bounded(value: u64, total: u64) -> (Run, String) {
    let done = value.min(total);
    let percent = done * 100 / total;
    let lit = (percent as usize * TRACK).div_ceil(100);
    ((0, lit.min(TRACK)), format!("{percent} %"))
}

/// The sheen, at the step of the walk this frame is on: ten steps to the
/// track, so it moves four times a second and comes back round. Parked at the
/// head when nothing moves, which is where a still bar has always drawn it.
fn walking(beat: f32) -> Run {
    let step = (beat.clamp(0.0, 1.0) * TRACK as f32) as usize % TRACK;
    (step, SHEEN.min(TRACK))
}

/// The track's cells: the lit run ramps `presence` → glow across itself, the
/// rest is dim. A run that reaches the end comes back at the start, so the
/// sheen walks the track rather than falling off it.
fn track(run: Run) -> Vec<Span<'static>> {
    (0..TRACK)
        .map(|cell| match into(cell, run) {
            Some(share) => Span::styled(FILLED, theme::pulse(share)),
            None => Span::styled(EMPTY, theme::dim()),
        })
        .collect()
}

/// How far into the lit run a cell is, from 0 at its start to 1 at its head,
/// or nothing at all when the cell is not in it.
fn into(cell: usize, (at, len): Run) -> Option<f32> {
    let into = (cell + TRACK - at % TRACK) % TRACK;
    if into >= len {
        return None;
    }
    Some(match len > 1 {
        true => into as f32 / (len - 1) as f32,
        false => 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{later, scene, still};

    /// The lit cells of a bar, and how far into the run each of them is.
    fn run(run: Run) -> Vec<Option<f32>> {
        (0..TRACK).map(|cell| into(cell, run)).collect()
    }

    #[test]
    fn a_bounded_fill_lights_from_the_head_and_ramps_across_what_it_lit() {
        assert_eq!(bounded(0, 10).0, (0, 0));
        assert_eq!(bounded(8, 10).0, (0, 8));
        assert_eq!(bounded(10, 10).0, (0, 10));
        assert_eq!(bounded(99, 10).0, (0, 10), "and never past the end");
        let lit = run(bounded(5, 10).0);
        assert_eq!(lit[0], Some(0.0), "the first cell is `presence`");
        assert_eq!(lit[4], Some(1.0), "and the last one is the glow");
        assert_eq!(lit[5], None, "the rest of the track is not lit");
    }

    /// Ten steps to the track and 2.5 s to the turn: four a second.
    #[test]
    fn the_sheen_walks_the_track_and_comes_back_round() {
        assert_eq!(walking(0.0), (0, SHEEN));
        assert_eq!(walking(0.25), (2, SHEEN));
        assert_eq!(walking(0.95), (9, SHEEN));
        assert_eq!(walking(1.0), (0, SHEEN), "and it comes back round");
        let sheen = run(walking(0.95));
        assert_eq!(sheen[9], Some(0.0), "the run starts at the last cell");
        assert_eq!(sheen[0], Some(0.5), "and wraps rather than falling off");
        assert_eq!(sheen[1], Some(1.0));
        assert_eq!(sheen[2], None);
    }

    /// The walk is the wall clock's own, so two bars are on the same step;
    /// nothing may move stills it at the head.
    #[test]
    fn the_walk_is_free_running_and_parked_when_nothing_moves() {
        let (_, now) = scene();
        assert_eq!(walk(now), walk(later(now, 2_500)), "one turn of it");
        assert_ne!(walk(now), walk(later(now, 625)));
        assert_eq!(walk(still(now)), 0.0);
        assert_eq!(walking(walk(still(now))), (0, SHEEN), "parked at the head");
    }

    /// §4 spends a gradient in exactly two places, and the fill of a bar is one
    /// of them: `presence` at the foot of the lit run, its glow at the head.
    #[test]
    fn a_bounded_fill_ramps_from_presence_to_its_glow() {
        crate::theme::with(crate::painted::truecolor(), || {
            let spans = lines(50, Some(100), None, 80, 0.0);
            let lit: Vec<ratatui::style::Style> = spans[0]
                .spans
                .iter()
                .take(5)
                .map(|span| span.style)
                .collect();
            assert_eq!(lit.first().copied(), Some(theme::pulse(0.0)));
            assert_eq!(lit.last().copied(), Some(theme::pulse(1.0)));
            let mut steps: Vec<String> = lit.iter().map(|style| format!("{style:?}")).collect();
            steps.sort();
            steps.dedup();
            assert_eq!(
                steps.len(),
                5,
                "and every cell between them is its own step: {lit:?}"
            );
        });
        crate::theme::with(crate::painted::no_colour(), || {
            let spans = lines(50, Some(100), None, 80, 0.0);
            assert!(
                spans[0]
                    .spans
                    .iter()
                    .take(5)
                    .all(|span| span.style == ratatui::style::Style::new()),
                "without colour the shapes carry the bar and nothing else does"
            );
            assert_eq!(
                spans[0].to_string().trim_end(),
                "█████░░░░░ 50 %",
                "which is the same bar it always was"
            );
        });
    }
}
