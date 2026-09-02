//! The rows of a list that fit around the row its cursor is on.
//!
//! Every list a person walks — the `/` menu, the `ctrl+g` switcher, the resume
//! and rewind pickers, the `ctrl+t` panel — is longer than the room it is
//! drawn in sooner or later. What it shows then is a window: a run of its rows
//! with the cursor inside it, and a `…` line wherever the list goes on past an
//! end. The cursor's row is never the one given up: where there is no room for
//! both a mark and it, the mark goes.
//!
//! Where the window sits is derived from the cursor and nothing else, so
//! walking a list needs no remembered scroll to keep in step with it.
//!
//! [`crate::roster`] draws its one column through this, labels and all, so the
//! row the keyboard is on is on the screen wherever down the list it is.

use std::ops::Range;

use ratatui::text::{Line, Span};

use crate::theme;

/// What a list draws in the room it has: the run of rows it shows, and the
/// ends it cut short. The run and a line for each cut end come to exactly the
/// room asked for — or to the whole list, when it fits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub run: Range<usize>,
    pub above: bool,
    pub below: bool,
}

/// The rows that fit around the one the cursor is on, the list flowing under
/// it in both directions.
pub fn around(lines: Vec<Line<'static>>, selected: usize, rows: usize) -> Vec<Line<'static>> {
    let at = window(lines.len(), selected, rows);
    framed(lines, at)
}

/// The same window over a list whose rows are more than lines — a card's row
/// knows the answer it belongs to, and a click is answered against that.
pub fn of(len: usize, selected: usize, rows: usize) -> Window {
    window(len, selected, rows)
}

/// The rows that fit from the one the cursor is on, for a list whose rows
/// carry their own under them: what the cursor names is read with what belongs
/// to it, so the view starts there rather than around there. The row before it
/// is what a mark for the list above takes.
pub fn onward(lines: Vec<Line<'static>>, at: usize, rows: usize) -> Vec<Line<'static>> {
    let len = lines.len();
    let window = placed(len, at.saturating_sub(1), at, rows);
    framed(lines, window)
}

/// The brick: which rows of `len` a cursor on `selected` keeps in view in
/// `rows` of room. Half the room goes above the cursor, so the list flows both
/// ways under it.
fn window(len: usize, selected: usize, rows: usize) -> Window {
    let start = selected.saturating_sub(rows.min(len) / 2);
    placed(len, start, selected, rows)
}

/// The window a run starting at `start` comes to: pulled back from the end of
/// the list when it would hang off it, and giving up a row at each cut end for
/// the mark that says so — never the row the cursor is on.
fn placed(len: usize, start: usize, cursor: usize, rows: usize) -> Window {
    let shown = rows.min(len);
    if shown == 0 {
        return Window {
            run: 0..0,
            above: false,
            below: false,
        };
    }
    if shown == len {
        return Window {
            run: 0..len,
            above: false,
            below: false,
        };
    }
    // Wherever it was asked to start, the run holds the cursor and hangs off
    // neither end of the list.
    let start = start
        .min(cursor)
        .max(cursor.saturating_sub(shown - 1))
        .min(len - shown);
    let end = start + shown;
    let above = start > 0 && start < cursor;
    let below = end < len && cursor + 1 < end;
    Window {
        run: (start + usize::from(above))..(end - usize::from(below)),
        above,
        below,
    }
}

/// The lines a window draws: its run, with a `…` at each end it cut.
fn framed(lines: Vec<Line<'static>>, at: Window) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len().min(at.run.len() + 2));
    if at.above {
        out.push(cut());
    }
    out.extend(lines.into_iter().skip(at.run.start).take(at.run.len()));
    if at.below {
        out.push(cut());
    }
    out
}

/// What says the list goes on past this end: the strip's own mark (§3), in the
/// column a row's text sits in.
pub fn cut() -> Line<'static> {
    Line::from(vec![
        theme::cursor_span(false),
        Span::styled(theme::ellipsis().to_string(), theme::dim()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list of `len` rows named by their own index, so a window's contents
    /// read as the numbers it kept.
    fn rows(len: usize) -> Vec<Line<'static>> {
        (0..len).map(|i| Line::from(i.to_string())).collect()
    }

    fn drawn(len: usize, selected: usize, rows_of_room: usize) -> Vec<String> {
        around(rows(len), selected, rows_of_room)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_list_shorter_than_its_room_is_drawn_whole() {
        assert_eq!(drawn(3, 1, 8), vec!["0", "1", "2"]);
        assert_eq!(drawn(8, 7, 8), vec!["0", "1", "2", "3", "4", "5", "6", "7"]);
        assert_eq!(drawn(0, 0, 8), Vec::<String>::new());
    }

    #[test]
    fn a_cursor_at_the_top_keeps_the_first_row_and_marks_the_foot() {
        assert_eq!(drawn(20, 0, 4), vec!["0", "1", "2", "  …"]);
    }

    #[test]
    fn a_cursor_in_the_middle_marks_both_ends() {
        assert_eq!(drawn(20, 10, 5), vec!["  …", "9", "10", "11", "  …"]);
    }

    #[test]
    fn a_cursor_at_the_end_keeps_the_last_row_and_marks_the_head() {
        assert_eq!(drawn(20, 19, 4), vec!["  …", "17", "18", "19"]);
    }

    /// Where there is room for one row, it is the one the cursor is on: a mark
    /// nobody can read a row beside is not worth the row it would take.
    #[test]
    fn a_room_of_one_row_spends_it_on_the_cursor() {
        assert_eq!(drawn(20, 12, 1), vec!["12"]);
        assert_eq!(drawn(20, 0, 1), vec!["0"]);
        assert_eq!(drawn(20, 19, 1), vec!["19"]);
    }

    #[test]
    fn a_room_of_two_rows_keeps_the_cursor_and_the_mark_that_still_fits() {
        assert_eq!(drawn(20, 12, 2), vec!["  …", "12"]);
        assert_eq!(drawn(20, 0, 2), vec!["0", "  …"]);
    }

    #[test]
    fn no_room_draws_nothing() {
        assert_eq!(drawn(20, 12, 0), Vec::<String>::new());
        assert_eq!(drawn(0, 0, 0), Vec::<String>::new());
    }

    /// The two rules a caller may lean on, over every list, cursor and room
    /// small enough to enumerate: a window never draws more than its room, and
    /// the row the cursor is on is always one of the rows it drew.
    #[test]
    fn a_window_never_outgrows_its_room_and_never_loses_the_cursor() {
        for len in 0..12 {
            for selected in 0..len {
                for room in 0..12 {
                    let lines = drawn(len, selected, room);
                    assert_eq!(lines.len(), room.min(len), "{len} rows, {room} of room");
                    let wanted = selected.to_string();
                    assert_eq!(
                        lines.contains(&wanted),
                        room > 0,
                        "row {selected} of {len} in {room} of room: {lines:?}"
                    );
                }
            }
        }
    }

    /// A list whose rows carry their own under them starts at the cursor, so
    /// what it names is read with what belongs to it.
    #[test]
    fn onward_starts_at_the_row_the_cursor_is_on() {
        let drawn = |at, room| {
            onward(rows(20), at, room)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(drawn(0, 4), vec!["0", "1", "2", "  …"]);
        assert_eq!(drawn(10, 4), vec!["  …", "10", "11", "  …"]);
        assert_eq!(drawn(19, 4), vec!["  …", "17", "18", "19"]);
        assert_eq!(drawn(10, 1), vec!["10"]);
    }
}
