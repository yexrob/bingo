//! The one list of sessions: what `↓` on an empty composer and `ctrl+g` both
//! open (design §3 "Teams", 2026-09-02).
//!
//! Two columns side by side. On the left the sessions that answer a model —
//! a room's members among them, never nested under the room, because a member
//! *is* a session and the list is where a person goes to reach one. On the
//! right the rooms, whose rows say how big they are and what they are owed.
//! `↑`/`↓` walk a column and `←`/`→` cross between them; each column keeps its
//! own cursor in view through [`crate::window`], so the row the keyboard is on
//! is never off the screen.
//!
//! Nothing here is state. Which rows exist is the tree's, what each says is
//! read off the reducer and the rooms plugin's own payloads at render time,
//! and the whole of what the surface remembers is where the cursor is.

use bingo_sdk::{SessionId, SessionState};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::clock::{self, Now};
use crate::seats::{self, Ear, Owes, Seat};
use crate::status;
use crate::theme;
use crate::tree::{self, Row, Status, Tree};
use crate::window;

/// Cells between the two columns.
const GUTTER: usize = 2;
/// Cells between a name and what its row says.
const SAYS_AT: usize = 2;
/// What each column is called, while there are two of them.
const SESSIONS: &str = "Sessions";
const ROOMS: &str = "Rooms";
/// The sigil a seat that listens rather than answers wears, as `/room` writes
/// one (`~watcher`). It is the glyph, never the colour: the dot's own hue
/// still says what the session is doing.
const LISTENS: &str = "~";

/// Which column the keyboard is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Sessions,
    Rooms,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Sessions => Side::Rooms,
            Side::Rooms => Side::Sessions,
        }
    }
}

/// Where the keyboard is in the list: which column, and how far down it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub side: Side,
    pub at: usize,
}

/// The list split in two, each column in the order the tree lists it.
pub struct Columns<'r, 'a> {
    pub sessions: Vec<&'r Row<'a>>,
    pub rooms: Vec<&'r Row<'a>>,
}

/// The sessions that answer a model, then the ones that answer nobody: the
/// same fact `Status::of` reports, which is what makes a room a room.
pub fn columns<'r, 'a>(rows: &'r [Row<'a>]) -> Columns<'r, 'a> {
    let (sessions, rooms) = rows.iter().partition(|row| row.status.is_some());
    Columns { sessions, rooms }
}

impl<'r, 'a> Columns<'r, 'a> {
    fn of(&self, side: Side) -> &[&'r Row<'a>] {
        match side {
            Side::Sessions => &self.sessions,
            Side::Rooms => &self.rooms,
        }
    }

    /// The column a one-column list draws: the cursor's own, unless it is
    /// empty and the other is not.
    fn only(&self, cursor: Cursor) -> Side {
        match self.of(cursor.side).is_empty() {
            true => cursor.side.other(),
            false => cursor.side,
        }
    }
}

impl Cursor {
    /// Where the session on screen sits in the list, so opening it puts the
    /// keyboard on the row a person is already looking at.
    pub fn on(columns: &Columns<'_, '_>, session: &SessionId) -> Cursor {
        [Side::Sessions, Side::Rooms]
            .into_iter()
            .find_map(|side| {
                let at = columns
                    .of(side)
                    .iter()
                    .position(|row| row.session == session)?;
                Some(Cursor { side, at })
            })
            .unwrap_or_default()
    }

    /// The row it names, or nothing at all where its column has none.
    pub fn row<'r, 'a>(&self, columns: &Columns<'r, 'a>) -> Option<&'r Row<'a>> {
        columns.of(self.side).get(self.at).copied()
    }

    /// One step along its own column. It stops at either end rather than
    /// wrapping: the other column is a step sideways, not the next row.
    pub fn step(self, columns: &Columns<'_, '_>, by: isize) -> Cursor {
        let last = columns.of(self.side).len().saturating_sub(1) as isize;
        Cursor {
            at: (self.at as isize + by).clamp(0, last).max(0) as usize,
            ..self
        }
    }

    /// Across to the other column, at the nearest row it has. A column with
    /// nothing in it is nowhere to go.
    pub fn cross(self, columns: &Columns<'_, '_>) -> Cursor {
        let side = self.side.other();
        match columns.of(side).len() {
            0 => self,
            len => Cursor {
                side,
                at: self.at.min(len - 1),
            },
        }
    }
}

/// The list as one frame draws it: its lines, and what a click on each of them
/// means. Where the lines landed is the frame's own business.
#[derive(Clone, Debug, Default)]
pub struct Roster {
    pub lines: Vec<Line<'static>>,
    /// Where the rooms column begins, in cells from the list's left edge;
    /// `None` while there is only one column.
    pub split: Option<u16>,
    /// Per drawn line, the cursor a click on it means — on the left of the
    /// split and on the right. Nothing for a heading, a `…` mark, or a column
    /// that ran out of rows.
    pub rows: Vec<(Option<Cursor>, Option<Cursor>)>,
}

/// The list in the room it has. The one renderer: `↓` and `ctrl+g` open the
/// same list, so there is one place it is drawn.
pub fn lines(
    tree: &Tree,
    rows: &[Row<'_>],
    cursor: Cursor,
    width: usize,
    room: usize,
    now: Now,
) -> Roster {
    let columns = columns(rows);
    match columns.sessions.is_empty() || columns.rooms.is_empty() {
        true => alone(tree, &columns, cursor, width, room, now),
        false => beside(tree, &columns, cursor, width, room, now),
    }
}

/// One column, the whole width, and no heading over it: a name for the only
/// thing on screen is furniture that separates nothing.
fn alone(
    tree: &Tree,
    columns: &Columns<'_, '_>,
    cursor: Cursor,
    width: usize,
    room: usize,
    now: Now,
) -> Roster {
    let side = columns.only(cursor);
    let at = match side == cursor.side {
        true => cursor.at,
        false => 0,
    };
    let (lines, which) = column(rendered(tree, columns, side, cursor, width, now), at, room);
    Roster {
        lines,
        split: None,
        rows: which
            .into_iter()
            .map(|at| (at.map(|at| Cursor { side, at }), None))
            .collect(),
    }
}

/// Both columns, under their names. The rooms take what their widest row asks
/// for and no more than half the list; the sessions, whose rows are the long
/// ones, take the rest.
fn beside(
    tree: &Tree,
    columns: &Columns<'_, '_>,
    cursor: Cursor,
    width: usize,
    room: usize,
    now: Now,
) -> Roster {
    let rooms = rendered(tree, columns, Side::Rooms, cursor, width, now);
    let right = wanted(&rooms, width);
    let left = width.saturating_sub(right + GUTTER).max(1);
    let sessions = rendered(tree, columns, Side::Sessions, cursor, left, now);
    let heading = room >= 2;
    let each = room - usize::from(heading);
    let (kept, which) = column(sessions, walked(cursor, Side::Sessions), each);
    let (rooms, room_rows) = column(rooms, walked(cursor, Side::Rooms), each);
    let mut lines = Vec::with_capacity(room);
    let mut rows = Vec::with_capacity(room);
    if heading {
        lines.push(headings(left, right));
        rows.push((None, None));
    }
    for at in 0..kept.len().max(rooms.len()) {
        lines.push(paired(kept.get(at), rooms.get(at), left, right));
        rows.push((
            which.get(at).copied().flatten().map(sits(Side::Sessions)),
            room_rows.get(at).copied().flatten().map(sits(Side::Rooms)),
        ));
    }
    Roster {
        lines,
        split: u16::try_from(left + GUTTER).ok(),
        rows,
    }
}

fn sits(side: Side) -> impl Fn(usize) -> Cursor {
    move |at| Cursor { side, at }
}

/// Where a column's own window is centred: the cursor's row while the keyboard
/// is in it, else its head — the other column is read, not walked.
fn walked(cursor: Cursor, side: Side) -> usize {
    match cursor.side == side {
        true => cursor.at,
        false => 0,
    }
}

/// One column's rows, as they read before any of them is windowed or clipped.
fn rendered(
    tree: &Tree,
    columns: &Columns<'_, '_>,
    side: Side,
    cursor: Cursor,
    width: usize,
    now: Now,
) -> Vec<Line<'static>> {
    let rows = columns.of(side);
    let name_column = rows.iter().map(|row| row.name.width()).max().unwrap_or(0);
    rows.iter()
        .enumerate()
        .map(|(at, row)| {
            let on = cursor.side == side && cursor.at == at;
            match side {
                Side::Sessions => session_line(tree, row, name_column, on, now),
                Side::Rooms => room_line(tree, row, name_column, on, now),
            }
        })
        .map(|line| Line::from(status::clip(line.spans, width)))
        .collect()
}

/// One column as it is drawn: the run of rows that keeps its cursor in view,
/// and which row of the list each drawn line is. Both come off the one window,
/// so the lines and what a click on them means cannot disagree.
fn column(
    lines: Vec<Line<'static>>,
    at: usize,
    room: usize,
) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
    let window = window::of(lines.len(), at, room);
    let mut which: Vec<Option<usize>> = Vec::new();
    if window.above {
        which.push(None);
    }
    which.extend(window.run.clone().map(Some));
    if window.below {
        which.push(None);
    }
    (window::around(lines, at, room), which)
}

/// How wide the rooms column asks to be, and the most it may have.
fn wanted(rooms: &[Line<'static>], width: usize) -> usize {
    rooms
        .iter()
        .map(Line::width)
        .chain([ROOMS.width()])
        .max()
        .unwrap_or(0)
        .min(width / 2)
}

/// The two column names, laid out and cut by the same rule their rows are: a
/// heading that outgrew its column would be the one row on screen that did.
fn headings(left: usize, right: usize) -> Line<'static> {
    let name = |text: &str| Line::from(Span::styled(text.to_string(), theme::dim()));
    paired(Some(&name(SESSIONS)), Some(&name(ROOMS)), left, right)
}

/// Two columns' rows on one line: the left padded out to its width, the right
/// after the gutter. A column that has run out of rows pads to air.
fn paired(
    left: Option<&Line<'static>>,
    right: Option<&Line<'static>>,
    width: usize,
    rooms: usize,
) -> Line<'static> {
    let spans =
        |line: Option<&Line<'static>>| line.cloned().map(|line| line.spans).unwrap_or_default();
    let mut out = status::clip(spans(left), width);
    let used: usize = out.iter().map(|span| span.content.width()).sum();
    out.push(Span::raw(" ".repeat(width.saturating_sub(used) + GUTTER)));
    out.extend(status::clip(spans(right), rooms));
    Line::from(out)
}

/// A session's row: its dot, its name, and what it is doing.
fn session_line(tree: &Tree, row: &Row<'_>, name: usize, on: bool, now: Now) -> Line<'static> {
    let state = tree.state(row.session);
    let seat = state.and_then(|state| seats::seat(tree, state));
    let mut spans = vec![theme::cursor_span(on), dot(row, seat.as_ref())];
    spans.extend(named(&row.name, name, on));
    spans.extend(says(row, state, seat, now));
    Line::from(spans)
}

/// A room's row: its name — whose `#` is its own sigil, so it wears no dot —
/// and how many seats it has.
fn room_line(tree: &Tree, row: &Row<'_>, name: usize, on: bool, now: Now) -> Line<'static> {
    let mut spans = vec![theme::cursor_span(on)];
    spans.extend(named(&row.name, name, on));
    spans.extend(gap(seats_said(tree, row, now)));
    Line::from(spans)
}

/// The name column: padded to the widest in its own column, and said in weight
/// rather than hue, so `NO_COLOR` still says which row the keyboard is on.
fn named(name: &str, column: usize, on: bool) -> Vec<Span<'static>> {
    let style = match on {
        true => theme::text(),
        false => theme::dim(),
    };
    vec![Span::styled(
        format!("{name}{}", " ".repeat(column.saturating_sub(name.width()))),
        style,
    )]
}

/// The dot a session's row wears, and the sigil a listening seat wears
/// instead. The colour is the roster's own either way, so the glyph adds a
/// fact rather than replacing one.
fn dot(row: &Row<'_>, seat: Option<&Seat>) -> Span<'static> {
    let listening = matches!(seat.map(|seat| seat.ear), Some(Ear::Listening { .. }));
    let glyph = match listening {
        true => LISTENS,
        false => theme::bullet(),
    };
    let cell = theme::bullet().width().max(LISTENS.width());
    Span::styled(
        format!("{glyph}{} ", " ".repeat(cell.saturating_sub(glyph.width()))),
        tree::bullet_style(row.status, row.attention),
    )
}

/// What a session's row says after its name, in the order a narrow column
/// gives it up: what it is doing, where it sits, what it hears there, what it
/// owes, whether it wants you — and last of all what it has spent.
///
/// The tail is what the clip takes (§10, 2026-08-31: the preview gives way,
/// never the answers). A count of tools is a thing to glance at; a debt and a
/// question are things to act on, so they are drawn where they survive.
fn says(
    row: &Row<'_>,
    state: Option<&SessionState>,
    seat: Option<Seat>,
    now: Now,
) -> Vec<Span<'static>> {
    let mut said = vec![Span::styled(doing(row, state), theme::dim())];
    if let Some(seat) = seat {
        said.push(Span::styled(format!("in {}", seat.room), theme::dim()));
        if let Ear::Listening { patience_s } = seat.ear {
            said.push(Span::styled(listening(patience_s), theme::dim()));
        }
        if let Some(owes) = seat.owes {
            said.push(Span::styled(owed(&owes, now), theme::attention(now)));
        }
    }
    if row.attention {
        said.push(Span::styled("needs you".to_string(), theme::attention(now)));
    }
    said.push(Span::styled(spent(row, state), theme::dim()));
    gap(dotted(said))
}

/// What a session is doing, as a row says it. One that has not run a turn yet
/// says what it was asked instead: it is the whole of what there is to know
/// about it, and a row that said `idle` would waste the line.
fn doing(row: &Row<'_>, state: Option<&SessionState>) -> String {
    let Some(status) = row.status else {
        return String::new();
    };
    match (status, state) {
        // A row the store answered with has no state here to read.
        (_, None) => status.label().to_string(),
        (Status::Idle, Some(state)) => {
            tree::brief(state).unwrap_or_else(|| status.label().to_string())
        }
        _ => status.label().to_string(),
    }
}

/// `3 tools · 1.2k tokens · 40s`: what a session has spent on its work. A
/// session that has not started spent nothing worth a column, and the clock is
/// left out where it has not moved — nothing is learned from `0s`.
fn spent(row: &Row<'_>, state: Option<&SessionState>) -> String {
    let Some(state) =
        state.filter(|_| matches!(row.status, Some(Status::Running) | Some(Status::Done)))
    else {
        return String::new();
    };
    let mut said = tree::spent(state);
    let seconds = tree::seconds(state);
    if seconds > 0 {
        said.push_str(&format!(" · {seconds}s"));
    }
    said
}

/// What a seat owes, as its row says it: how long the answer has stood,
/// counted at draw time from the moment the card carries — the card cannot
/// count it, because it is republished only when a debt opens or closes.
///
/// A card from before the debts carried their own stamps has only the clock
/// time the question was asked at (`14:02`) and no date under it, so that row
/// says the time rather than an age it would have to invent.
fn owed(owes: &Owes, now: Now) -> String {
    match owes {
        Owes::Since(at) => format!("owes an answer · {}", clock::span(now.past(*at))),
        Owes::At(asked) => format!("owes an answer since {asked}"),
    }
}

/// How a seat hears its room, where it is not the live ear every seat has by
/// default. A patience the roster named without one is the word alone.
fn listening(patience_s: Option<u64>) -> String {
    match patience_s {
        Some(seconds) => format!("listening · {seconds}s"),
        None => "listening".to_string(),
    }
}

/// What a room's row says: how big it is, and what it is owed.
fn seats_said(tree: &Tree, row: &Row<'_>, now: Now) -> Vec<Span<'static>> {
    // A room only the store knows of has no journal here to count.
    let Some(state) = tree.state(row.session) else {
        return Vec::new();
    };
    let counts = seats::counts(tree, state);
    let mut said = vec![Span::styled(
        format!("{} seat{}", counts.seats, plural(counts.seats)),
        theme::dim(),
    )];
    if counts.owed > 0 {
        said.push(Span::styled(
            format!("{} owed", counts.owed),
            theme::attention(now),
        ));
    }
    dotted(said)
}

fn plural(n: usize) -> &'static str {
    match n == 1 {
        true => "",
        false => "s",
    }
}

/// The parts of a row with ` · ` between them; a part with nothing in it takes
/// no separator with it.
fn dotted(parts: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for part in parts.into_iter().filter(|part| !part.content.is_empty()) {
        if !out.is_empty() {
            out.push(Span::styled(" · ".to_string(), theme::dim()));
        }
        out.push(part);
    }
    out
}

/// The air between a name and what its row says, and none at all after a row
/// that says nothing.
fn gap(said: Vec<Span<'static>>) -> Vec<Span<'static>> {
    if said.is_empty() {
        return said;
    }
    let mut out = vec![Span::raw(" ".repeat(SAYS_AT))];
    out.extend(said);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// A root, two sub-agents seated in a room — one of them listening, one
    /// owing an answer — and the room itself: one row of every kind there is.
    fn team() -> Tree {
        let mut frames = busy_child("reviewer");
        frames.extend([
            agent_frame(3, 7, agent_announced(3, "watcher")),
            log_frame(8, log_announced("#design")),
            log_frame(
                9,
                extended(
                    "bingo.rooms",
                    "members",
                    roster_payload(&["reviewer", "watcher"], &[("watcher", 300)]),
                ),
            ),
            frame(
                10,
                signalled(
                    "bingo.rooms",
                    "owed",
                    owed_payload(&[("#design", "reviewer", 22)]),
                ),
            ),
            frame(
                11,
                bingo_sdk::Event::ItemCompleted {
                    item: user("itm_0", "what is in this workspace?"),
                },
            ),
        ]);
        folded_tree(frames)
    }

    fn drawn_at(tree: &Tree, cursor: Cursor, width: usize, room: usize) -> Vec<String> {
        lines(tree, &tree.rows(), cursor, width, room, scene().1)
            .lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect()
    }

    #[test]
    fn the_sessions_are_one_column_and_the_rooms_the_other() {
        let drawn = drawn_at(&team(), Cursor::default(), 80, 8);
        assert_eq!(
            drawn,
            vec![
                "Sessions                                             Rooms",
                "❯ ⏺ project   what is in this workspace?               #design  2 seats · 1 owed",
                "  ⏺ reviewer  running · in #design · owes an answe…",
                "  ~ watcher   idle · in #design · listening · 300s",
            ],
            "the root first, then the agents, and the rooms beside them"
        );
    }

    /// The glyph says a seat listens; the colour goes on saying what the
    /// session is doing, so neither fact is spent on the other.
    #[test]
    fn a_listening_seat_wears_the_sigil_and_keeps_its_own_colour() {
        let tree = team();
        let rows = tree.rows();
        let sides = columns(&rows);
        let watcher = sides
            .sessions
            .iter()
            .find(|row| row.name == "watcher")
            .expect("the listening seat");
        let glyph = dot(
            watcher,
            seats::seat(&tree, tree.state(watcher.session).expect("it")).as_ref(),
        );
        assert!(glyph.content.starts_with('~'), "{glyph:?}");
        assert_eq!(glyph.style, tree::bullet_style(watcher.status, false));
    }

    #[test]
    fn a_room_says_how_many_seats_it_has_and_what_it_is_owed() {
        let drawn = drawn_at(&team(), Cursor::default(), 120, 8);
        assert!(
            drawn
                .iter()
                .any(|row| row.contains("#design  2 seats · 1 owed")),
            "{drawn:#?}"
        );
    }

    /// A session that has not run a turn says the thing it was asked; the
    /// rest say what they are doing and what they have spent.
    #[test]
    fn a_session_that_has_not_run_says_what_it_was_asked() {
        let drawn = drawn_at(&team(), Cursor::default(), 120, 8);
        assert!(
            drawn[1].contains("project   what is in this workspace?"),
            "{drawn:#?}"
        );
        assert!(
            drawn[2].contains(
                "reviewer  running · in #design · owes an answer · 22m · 3 tools · 1.2k tokens"
            ),
            "one that has run says what it is doing, then where it sits and what \
             it owes, and only then what it has spent: {drawn:#?}"
        );
    }

    /// Colour never carries a fact alone (§4): a row that wants a person says
    /// so in words as well as in the hue it pulses.
    #[test]
    fn a_row_that_wants_a_person_says_so_in_words() {
        let tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, opened(child_permission())),
        ]);
        let drawn = drawn_at(&tree, Cursor::default(), 80, 8);
        assert!(
            drawn[1].contains("reviewer  idle · needs you"),
            "{drawn:#?}"
        );
    }

    /// A row the store answered with is not here to be read: it says the one
    /// thing that is true of it.
    #[test]
    fn a_stored_row_says_that_it_is_stored_and_nothing_more() {
        let tree = Tree::new(state());
        let stored = [stored_summary("ses_3", "archivist")];
        let rows = tree::roster(&tree, &stored);
        let drawn: Vec<String> = lines(&tree, &rows, Cursor::default(), 80, 8, scene().1)
            .lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect();
        assert_eq!(drawn[1], "  ⏺ archivist  stored");
    }

    #[test]
    fn one_kind_of_session_alone_is_one_column_with_no_heading() {
        let tree = folded_tree(busy_child("reviewer"));
        let drawn = drawn_at(&tree, Cursor::default(), 80, 8);
        assert_eq!(
            drawn.len(),
            2,
            "no heading over the only column: {drawn:#?}"
        );
        assert!(drawn[0].starts_with("❯ ⏺ project"), "{drawn:#?}");
    }

    // ---- walking ---------------------------------------------------------

    fn walk(tree: &Tree, from: Cursor, by: isize) -> Cursor {
        let rows = tree.rows();
        from.step(&columns(&rows), by)
    }

    #[test]
    fn a_column_is_walked_to_its_ends_and_stops_there() {
        let tree = team();
        let top = Cursor::default();
        assert_eq!(walk(&tree, top, -1), top, "the first row is the first row");
        assert_eq!(walk(&tree, top, 1).at, 1);
        let last = Cursor {
            side: Side::Sessions,
            at: 2,
        };
        assert_eq!(walk(&tree, last, 1), last, "and the last is the last");
    }

    #[test]
    fn the_arrows_cross_to_the_other_column_at_the_nearest_row() {
        let tree = team();
        let rows = tree.rows();
        let sides = columns(&rows);
        let deep = Cursor {
            side: Side::Sessions,
            at: 2,
        };
        assert_eq!(
            deep.cross(&sides),
            Cursor {
                side: Side::Rooms,
                at: 0
            },
            "the rooms column has one row to land on"
        );
        assert_eq!(
            deep.cross(&sides).cross(&sides),
            Cursor {
                side: Side::Sessions,
                at: 0
            },
            "and back to the row of the same number"
        );
    }

    #[test]
    fn a_column_with_nothing_in_it_is_nowhere_to_cross_to() {
        let tree = folded_tree(busy_child("reviewer"));
        let rows = tree.rows();
        let sides = columns(&rows);
        let at = Cursor::default();
        assert_eq!(at.cross(&sides), at);
    }

    #[test]
    fn the_cursor_opens_on_the_row_of_the_session_in_view() {
        let mut tree = team();
        tree.show(&log_id());
        let rows = tree.rows();
        let sides = columns(&rows);
        assert_eq!(
            Cursor::on(&sides, tree.view()),
            Cursor {
                side: Side::Rooms,
                at: 0
            }
        );

        tree.show(&child_id());
        let rows = tree.rows();
        let sides = columns(&rows);
        assert_eq!(
            Cursor::on(&sides, tree.view()),
            Cursor {
                side: Side::Sessions,
                at: 1
            }
        );
    }

    // ---- the window ------------------------------------------------------

    /// The bug this list was drawn through `window::around` for: past the room
    /// it has, the row the keyboard is on must still be one of the rows drawn.
    #[test]
    fn a_column_longer_than_its_room_keeps_the_row_the_cursor_is_on() {
        let mut frames = busy_child("reviewer");
        frames.push(log_frame(8, log_announced("#design")));
        frames
            .extend((20..32).map(|i| agent_frame(i, i, agent_announced(i, &format!("scout {i}")))));
        let tree = folded_tree(frames);
        let at = tree.rows().len() - 2;
        let drawn = drawn_at(
            &tree,
            Cursor {
                side: Side::Sessions,
                at,
            },
            120,
            6,
        );
        assert_eq!(drawn.len(), 6, "the heading and five rows: {drawn:#?}");
        assert!(
            drawn.iter().any(|row| row.contains("❯ ⏺ scout 31")),
            "the last row of the column, with the cursor on it: {drawn:#?}"
        );
        assert!(
            drawn[1].trim_start().starts_with('…'),
            "and the list says it goes on above: {drawn:#?}"
        );
    }

    /// The other column shows its head while the keyboard is not in it: it is
    /// there to be read, not walked.
    #[test]
    fn the_column_the_keyboard_is_not_in_shows_its_head() {
        let mut frames = busy_child("reviewer");
        frames.extend((20..26).map(|i| {
            woken(
                i,
                bingo_sdk::SessionSummary {
                    id: bingo_sdk::SessionId::from_raw(format!("ses_{i}")),
                    title: Some(format!("#room{i}")),
                    ..log_summary("#design")
                },
            )
        }));
        let tree = folded_tree(frames);
        let drawn = drawn_at(&tree, Cursor::default(), 120, 4);
        assert!(drawn[1].contains("#room20"), "the first room: {drawn:#?}");
        assert!(
            drawn
                .last()
                .is_some_and(|row| row.trim_end().ends_with('…')),
            "and says the rooms go on below: {drawn:#?}"
        );
    }

    /// What a click on each drawn line means, taken from the same window the
    /// lines came out of — so the two can never disagree.
    #[test]
    fn every_drawn_row_says_which_row_of_the_list_it_is() {
        let tree = team();
        let rows = tree.rows();
        let roster = lines(&tree, &rows, Cursor::default(), 80, 8, scene().1);
        assert_eq!(roster.rows.len(), roster.lines.len());
        assert_eq!(roster.rows[0], (None, None), "the heading answers nothing");
        assert_eq!(
            roster.rows[1],
            (
                Some(Cursor {
                    side: Side::Sessions,
                    at: 0
                }),
                Some(Cursor {
                    side: Side::Rooms,
                    at: 0
                })
            )
        );
        assert_eq!(roster.rows[3].1, None, "the rooms column ran out of rows");
        assert!(roster.split.is_some());
    }

    #[test]
    fn no_width_makes_a_row_wider_than_the_list() {
        let tree = team();
        for width in [8usize, 20, 40, 80, 120] {
            for line in lines(&tree, &tree.rows(), Cursor::default(), width, 8, scene().1).lines {
                assert!(line.width() <= width, "at {width}: {line:?}");
            }
        }
    }
}
