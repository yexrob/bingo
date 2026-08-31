//! The quick cycle: `↓` on an empty composer turns the status line into a
//! strip of chips — one per session in the tree, the ones that answer a model
//! first and then the rooms — and `←`/`→` walk it, switching the view as they
//! go (design §3, 2026-08-31).
//!
//! It is the status line's other content, never another row: the strip takes
//! the one line of furniture for as long as it is up, so no row of the frame
//! moves when it opens or closes (§3, nothing jumps).
//!
//! Nothing here is state. The chip a person is on *is* the session on screen,
//! so walking the strip is a view switch rather than a pending choice, and the
//! surface remembers only that the strip is open.

use std::ops::Range;

use bingo_sdk::SessionId;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::status;
use crate::theme;
use crate::tree::{self, Row};

/// The strip at this width: the chips that fit around the one a person is on,
/// with the ellipsis at whichever end is cut short.
pub fn strip(rows: &[Row<'_>], view: &SessionId, width: usize) -> Line<'static> {
    let order = ordered(rows);
    let selected = at(&order, view);
    let widths: Vec<usize> = order.iter().map(|row| chip(row, false).width()).collect();
    let shown = window(&widths, selected, width);
    let mut spans = Vec::new();
    if shown.start > 0 {
        spans.push(cut());
    }
    for (offset, row) in order[shown.clone()].iter().enumerate() {
        spans.extend(chip(row, shown.start + offset == selected).spans);
    }
    if shown.end < order.len() {
        spans.push(cut());
    }
    Line::from(status::clip(spans, width))
}

/// The session one step along the strip, wrapping at either end — a cycle goes
/// round. `None` where there is nowhere else to be.
pub fn step(rows: &[Row<'_>], view: &SessionId, by: isize) -> Option<SessionId> {
    let order = ordered(rows);
    if order.len() < 2 {
        return None;
    }
    let next = (at(&order, view) as isize + by).rem_euclid(order.len() as isize) as usize;
    order.get(next).map(|row| row.session.clone())
}

/// The order the strip reads in: what answers a model first, then the rooms,
/// each keeping the order the tree lists them in.
fn ordered<'r, 'a>(rows: &'r [Row<'a>]) -> Vec<&'r Row<'a>> {
    let (agents, rooms): (Vec<_>, Vec<_>) = rows.iter().partition(|row| row.status.is_some());
    [agents, rooms].concat()
}

/// Where the session on screen sits in that order.
fn at(order: &[&Row<'_>], view: &SessionId) -> usize {
    order
        .iter()
        .position(|row| row.session == view)
        .unwrap_or(0)
}

/// One session as the strip draws it: the cursor's own column, the state dot
/// the roster already draws, and the name. A room answers nothing, so it wears
/// no dot — and its name carries its own `#`.
fn chip(row: &Row<'_>, viewed: bool) -> Line<'static> {
    let mut spans = vec![theme::cursor_span(viewed)];
    if row.status.is_some() {
        spans.push(Span::styled(
            format!("{} ", theme::bullet()),
            tree::bullet_style(row.status, row.attention),
        ));
    }
    let name = match viewed {
        true => theme::text(),
        false => theme::dim(),
    };
    spans.push(Span::styled(row.name.clone(), name));
    Line::from(spans)
}

/// What says the strip goes on past this end.
fn cut() -> Span<'static> {
    Span::styled(theme::ellipsis().to_string(), theme::dim())
}

/// Which chips are on the strip: the widest run around the one a person is on
/// that fits, grown a chip at a time from either side of it. The chip they are
/// on is always drawn, and the strip flows under it.
fn window(widths: &[usize], selected: usize, width: usize) -> Range<usize> {
    let mut range = selected..(selected + 1).min(widths.len());
    loop {
        let right = grow(widths, &mut range, width, true);
        let left = grow(widths, &mut range, width, false);
        if !right && !left {
            return range;
        }
    }
}

/// One more chip on one end, while the strip has room for it.
fn grow(widths: &[usize], range: &mut Range<usize>, width: usize, rightwards: bool) -> bool {
    let wider = match rightwards {
        true if range.end < widths.len() => range.start..range.end + 1,
        false if range.start > 0 => range.start - 1..range.end,
        _ => return false,
    };
    if cells(widths, &wider) > width {
        return false;
    }
    *range = wider;
    true
}

/// What a run of chips costs, counting the ellipsis of each end it cuts.
fn cells(widths: &[usize], range: &Range<usize>) -> usize {
    let chips: usize = widths[range.clone()].iter().sum();
    let cuts = usize::from(range.start > 0) + usize::from(range.end < widths.len());
    chips + cuts * theme::ellipsis().width()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use crate::tree::Tree;

    /// A root, a sub-agent at work and a room, which is every kind of chip.
    fn tree() -> Tree {
        let mut frames = busy_child("reviewer");
        frames.push(log_frame(9, log_announced("#design")));
        folded_tree(frames)
    }

    fn drawn(tree: &Tree, width: usize) -> String {
        strip(&tree.rows(), tree.view(), width).to_string()
    }

    #[test]
    fn the_strip_is_the_sessions_agents_first_and_then_the_rooms() {
        let tree = tree();
        assert_eq!(drawn(&tree, 80), "❯ ⏺ project  ⏺ reviewer  #design");
    }

    #[test]
    fn the_chip_a_person_is_on_is_the_session_on_screen() {
        let mut walked = tree();
        walked.show(&child_id());
        assert_eq!(drawn(&walked, 80), "  ⏺ project❯ ⏺ reviewer  #design");
    }

    /// The dot is the roster's, and the name says which of them is on screen
    /// in a weight rather than a hue, so `NO_COLOR` loses nothing of it.
    #[test]
    fn the_dot_takes_the_rosters_colour_and_the_name_the_glance() {
        let tree = tree();
        let rows = tree.rows();
        let spans = strip(&rows, tree.view(), 80);
        let styled: Vec<(String, ratatui::style::Style)> = spans
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect();
        assert_eq!(styled[1].1, theme::dim(), "the root has not run yet");
        assert_eq!(styled[2], ("project".to_string(), theme::text()));
        assert_eq!(styled[4].1, theme::presence(), "the child is at work");
        assert_eq!(styled[5], ("reviewer".to_string(), theme::dim()));
    }

    #[test]
    fn walking_the_strip_goes_round_in_both_directions() {
        let tree = tree();
        let rows = tree.rows();
        let names = |by| {
            step(&rows, tree.view(), by).map(|id| {
                rows.iter()
                    .find(|row| *row.session == id)
                    .expect("a row")
                    .name
                    .clone()
            })
        };
        assert_eq!(names(1).as_deref(), Some("reviewer"));
        assert_eq!(names(-1).as_deref(), Some("#design"), "back past the first");
    }

    #[test]
    fn a_session_alone_has_nowhere_to_walk_to() {
        let tree = solo(&state());
        assert_eq!(step(&tree.rows(), tree.view(), 1), None);
    }

    /// A strip too long for the row keeps the chip a person is on and says
    /// which ends it cut.
    #[test]
    fn the_strip_scrolls_under_the_chip_you_are_on() {
        let at_the_root = drawn(&tree(), 24);
        assert!(at_the_root.ends_with('…'), "{at_the_root}");
        assert!(at_the_root.width() <= 24, "{at_the_root}");

        let mut walked = tree();
        walked.show(&child_id());
        let at_the_child = drawn(&walked, 24);
        assert!(at_the_child.contains("reviewer"), "{at_the_child}");
        assert!(at_the_child.starts_with('…'), "{at_the_child}");
        assert!(at_the_child.width() <= 24, "{at_the_child}");
    }

    #[test]
    fn no_width_makes_the_strip_wider_than_its_row() {
        let mut walked = tree();
        walked.show(&log_id());
        for width in [1usize, 4, 8, 12, 20, 40, 80] {
            assert!(drawn(&walked, width).width() <= width, "at {width}");
        }
    }
}
