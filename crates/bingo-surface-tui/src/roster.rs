//! The one list of sessions: what `↓` on an empty composer and `ctrl+g` both
//! open (design §3 "Teams", 2026-09-02).
//!
//! One column under two dim labels, the way a chat sidebar is grouped:
//! `Agents`, and the sessions that answer a model — a room's members among
//! them, never nested under the room, because a member *is* a session and the
//! list is where a person goes to reach one; then `Rooms`, and the rooms,
//! whose rows say how big they are and what they are owed. `↑`/`↓` walk the
//! whole of it and step over the labels, which are furniture and nowhere to
//! land.
//!
//! It is **typed into** (M55, 2026-09-04): a query narrows the column to the
//! rows [`crate::matching`] ranks for it — a session by its name and by the
//! room it sits in, a room by its own — and sits at the head of the list as one
//! dim line of what was typed. An empty query is the whole list, and is no
//! line at all.
//!
//! One [`crate::window`] over the whole list keeps the row the keyboard is on
//! in view, with a `…` at an end it cut; a label past that end is simply not
//! drawn, and one at the window's own edge stays.
//!
//! Nothing here is state. Which rows exist is the tree's, what each says is
//! read off the reducer and the rooms plugin's own payloads at render time,
//! and the whole of what the surface remembers is where the cursor is and what
//! has been typed.

use bingo_sdk::{SessionId, SessionState};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::clock::{self, Now};
use crate::matching;
use crate::seats::{self, Ear, Owes, Seat};
use crate::status;
use crate::theme;
use crate::tree::{self, Row, Status, Tree};
use crate::window;

/// Cells between a name and what its row says.
const SAYS_AT: usize = 2;
/// What each run of the list is called, while there is another beside it.
const AGENTS: &str = "Agents";
const ROOMS: &str = "Rooms";
/// The sigil a seat that wakes on every post wears. Listening is what a seat
/// does unless its roster asked otherwise (ADR-0034 §6), so the glyph is spent
/// on the seat that is the exception — the one a storm would go through. It is
/// the glyph, never the colour: the dot's own hue still says what the session
/// is doing.
const LIVE: &str = "~";

/// Where the keyboard is in the list: how far down the rows it may land on,
/// the labels between them counting for nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub at: usize,
}

/// The list in the two runs it draws as, each under a label of its own.
pub struct Listing<'r, 'a> {
    agents: Vec<&'r Row<'a>>,
    rooms: Vec<&'r Row<'a>>,
}

/// The sessions that answer a model, then the ones that answer nobody: the
/// same fact `Status::of` reports, which is what makes a room a room. Each run
/// is narrowed to what the query matches, so the two are ranked apart and a
/// label still stands over the run it names.
pub fn listing<'r, 'a>(tree: &Tree, rows: &'r [Row<'a>], query: &str) -> Listing<'r, 'a> {
    let (agents, rooms): (Vec<_>, Vec<_>) = rows.iter().partition(|row| row.status.is_some());
    Listing {
        agents: narrowed(tree, agents, query),
        rooms: narrowed(tree, rooms, query),
    }
}

/// One run of the list, narrowed to the rows the query matches and in the
/// order it ranks them. An empty query is every row in the tree's own order —
/// the matcher's answer, not a case of its own.
fn narrowed<'r, 'a>(tree: &Tree, run: Vec<&'r Row<'a>>, query: &str) -> Vec<&'r Row<'a>> {
    let searched: Vec<(String, &'r Row<'a>)> = run
        .into_iter()
        .map(|row| (searched(tree, row), row))
        .collect();
    matching::rank(query, &searched, |(words, _)| words.as_str())
        .into_iter()
        .map(|(_, row)| *row)
        .collect()
}

/// The words a row is found by: its own name, and the room it sits in, so a
/// person who knows where somebody is can type that instead of who. A room is
/// found by its name alone — the seats in it are rows of their own, and typing
/// the room brings every one of them up beside it.
fn searched(tree: &Tree, row: &Row<'_>) -> String {
    let seat = tree
        .state(row.session)
        .and_then(|state| seats::seat(tree, state));
    match seat {
        Some(seat) => format!("{} {}", row.name, seat.room),
        None => row.name.clone(),
    }
}

impl<'r, 'a> Listing<'r, 'a> {
    /// The rows the cursor walks, in the order they are drawn.
    fn walked(&self) -> impl Iterator<Item = &'r Row<'a>> + '_ {
        self.agents.iter().chain(self.rooms.iter()).copied()
    }

    fn len(&self) -> usize {
        self.agents.len() + self.rooms.len()
    }

    /// Whether the runs are told apart by a label at all: a name over the only
    /// kind of thing on screen separates nothing.
    fn labelled(&self) -> bool {
        !self.agents.is_empty() && !self.rooms.is_empty()
    }
}

impl Cursor {
    /// Where the session on screen sits in the list, so opening it puts the
    /// keyboard on the row a person is already looking at.
    pub fn on(listing: &Listing<'_, '_>, session: &SessionId) -> Cursor {
        listing
            .walked()
            .position(|row| row.session == session)
            .map(|at| Cursor { at })
            .unwrap_or_default()
    }

    /// The row it names, or nothing at all where the list has none.
    pub fn row<'r, 'a>(&self, listing: &Listing<'r, 'a>) -> Option<&'r Row<'a>> {
        listing.walked().nth(self.at)
    }

    /// One step down the column, over a label as if it were not there. It
    /// stops at either end rather than wrapping.
    pub fn step(self, listing: &Listing<'_, '_>, by: isize) -> Cursor {
        let last = listing.len().saturating_sub(1) as isize;
        Cursor {
            at: (self.at as isize + by).clamp(0, last).max(0) as usize,
        }
    }
}

/// The list as one frame draws it: its lines, and what a click on each of them
/// means. Where the lines landed is the frame's own business.
#[derive(Clone, Debug, Default)]
pub struct Roster {
    pub lines: Vec<Line<'static>>,
    /// Per drawn line, the cursor a click on it means. Nothing for a label or
    /// a `…` mark, which are furniture and answer nobody.
    pub rows: Vec<Option<Cursor>>,
}

/// The list in the room it has. The one renderer: `↓` and `ctrl+g` open the
/// same list, so there is one place it is drawn.
pub fn lines(
    tree: &Tree,
    rows: &[Row<'_>],
    cursor: Cursor,
    query: &str,
    width: usize,
    room: usize,
    now: Now,
) -> Roster {
    let listed = listed(tree, &listing(tree, rows, query), cursor, width, now);
    let at = walked_line(&listed, cursor);
    match asked(query, width) {
        Some(line) => headed(windowed(listed, at, room.saturating_sub(1)), line),
        None => windowed(listed, at, room),
    }
}

/// The line the query is typed on: dim, the list's width, and nothing at all
/// while nothing has been typed. The one-line-of-furniture rule holds because
/// the line is the person's own typing (§3, 2026-09-04).
fn asked(query: &str, width: usize) -> Option<Line<'static>> {
    if query.is_empty() {
        return None;
    }
    let spans = vec![Span::styled(
        format!("{} {query}{}", theme::find(), theme::caret()),
        theme::dim(),
    )];
    Some(Line::from(status::clip(spans, width)))
}

/// The query line sits at the head of the list and answers no click — it is
/// nowhere to land, as a label is.
fn headed(rows: Roster, asked: Line<'static>) -> Roster {
    let mut roster = Roster {
        lines: vec![asked],
        rows: vec![None],
    };
    roster.lines.extend(rows.lines);
    roster.rows.extend(rows.rows);
    roster
}

/// Which of the drawn lines the row the keyboard is on became. It is asked of
/// the lines themselves rather than counted from the labels: a second sum of
/// where a label falls is a second place to get it wrong.
fn walked_line(listed: &[(Line<'static>, Option<Cursor>)], cursor: Cursor) -> usize {
    listed
        .iter()
        .position(|(_, of)| *of == Some(cursor))
        .unwrap_or(0)
}

/// Every line the list has before the window takes it: each run under its
/// label, and the cursor each line answers to.
fn listed(
    tree: &Tree,
    listing: &Listing<'_, '_>,
    cursor: Cursor,
    width: usize,
    now: Now,
) -> Vec<(Line<'static>, Option<Cursor>)> {
    let agents = rendered(&listing.agents, 0, cursor, width, |row, column, on| {
        session_line(tree, row, column, on, now)
    });
    let from = listing.agents.len();
    let rooms = rendered(&listing.rooms, from, cursor, width, |row, column, on| {
        room_line(tree, row, column, on, now)
    });
    let labelled = listing.labelled();
    let mut out = under(AGENTS, agents, 0, labelled, width);
    out.extend(under(ROOMS, rooms, from, labelled, width));
    out
}

/// One run of the list: its label, where there is another run to tell it from,
/// and its rows under it, each with the row of the list it is.
fn under(
    label: &str,
    lines: Vec<Line<'static>>,
    from: usize,
    labelled: bool,
    width: usize,
) -> Vec<(Line<'static>, Option<Cursor>)> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(Line<'static>, Option<Cursor>)> = Vec::new();
    if labelled {
        out.push((labelled_line(label, width), None));
    }
    out.extend(
        lines
            .into_iter()
            .enumerate()
            .map(|(at, line)| (line, Some(Cursor { at: from + at }))),
    );
    out
}

/// What a run is called: dim, at the margin the rows are indented from, and
/// never a row the cursor can be on. The column is what tells a dim label from
/// a dim row — nothing else in the list starts there but the `❯` itself.
fn labelled_line(label: &str, width: usize) -> Line<'static> {
    let spans = vec![Span::styled(label.to_string(), theme::dim())];
    Line::from(status::clip(spans, width))
}

/// One run's rows, as they read before the window takes them: each name padded
/// to its own run's widest, and the line cut to the width the list has.
fn rendered(
    rows: &[&Row<'_>],
    from: usize,
    cursor: Cursor,
    width: usize,
    line: impl Fn(&Row<'_>, usize, bool) -> Line<'static>,
) -> Vec<Line<'static>> {
    let name_column = rows.iter().map(|row| row.name.width()).max().unwrap_or(0);
    rows.iter()
        .enumerate()
        .map(|(at, row)| line(row, name_column, cursor.at == from + at))
        .map(|line| Line::from(status::clip(line.spans, width)))
        .collect()
}

/// The run of lines that keeps the row the keyboard is on in view, and which
/// row of the list each of them is. Both come off the one window, so the lines
/// and what a click on them means cannot disagree.
fn windowed(listed: Vec<(Line<'static>, Option<Cursor>)>, at: usize, room: usize) -> Roster {
    let window = window::of(listed.len(), at, room);
    let mut roster = Roster::default();
    if window.above {
        roster.lines.push(window::cut());
        roster.rows.push(None);
    }
    for (line, cursor) in listed
        .into_iter()
        .skip(window.run.start)
        .take(window.run.len())
    {
        roster.lines.push(line);
        roster.rows.push(cursor);
    }
    if window.below {
        roster.lines.push(window::cut());
        roster.rows.push(None);
    }
    roster
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

/// The name column: padded to the widest in its own run, and said in weight
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

/// The dot a session's row wears, and the sigil a live seat wears instead. The
/// colour is the roster's own either way, so the glyph adds a fact rather than
/// replacing one.
fn dot(row: &Row<'_>, seat: Option<&Seat>) -> Span<'static> {
    let live = matches!(seat.map(|seat| seat.ear), Some(Ear::Live));
    let glyph = match live {
        true => LIVE,
        false => theme::bullet(),
    };
    let cell = theme::bullet().width().max(LIVE.width());
    Span::styled(
        format!("{glyph}{} ", " ".repeat(cell.saturating_sub(glyph.width()))),
        tree::bullet_style(row.status, row.attention),
    )
}

/// What a session's row says after its name, in the order a narrow column
/// gives it up: what it is doing, where it sits, what stands unread there,
/// what it hears, what it owes, whether it wants you — and last of all what it
/// has spent.
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
        if let Some(unread) = seat.unread {
            said.push(Span::styled(format!("{unread} unread"), theme::dim()));
        }
        said.push(Span::styled(heard(seat.ear), theme::dim()));
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

/// How a seat hears its room, where that is worth a word at all. The patient
/// ear with no patience beside it is the one a bare name asks for (ADR-0034
/// §6), and a word on every row says nothing about any of them; the two ears a
/// roster had to ask for are the two worth saying.
fn heard(ear: Ear) -> String {
    match ear {
        Ear::Live => "live".to_string(),
        Ear::Listening {
            patience_s: Some(seconds),
        } => format!("listening · {seconds}s"),
        Ear::Listening { patience_s: None } => String::new(),
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

    /// A root, two sub-agents seated in a room — one on the live ear it asked
    /// for and owing an answer, one on a patience of its own — and the room
    /// itself: one row of every kind there is.
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
                    roster_payload(
                        &["reviewer", "watcher"],
                        &[("reviewer", 0), ("watcher", 600)],
                    ),
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
        // The posts a reading mark has to stand behind.
        frames.extend(
            (1..=4u64)
                .map(|n| posted(9 + n, &format!("itm_p{n}"), "watcher", &format!("post {n}"))),
        );
        folded_tree(frames)
    }

    fn drawn_at(tree: &Tree, cursor: Cursor, width: usize, room: usize) -> Vec<String> {
        narrowed_at(tree, cursor, "", width, room)
    }

    /// The list as one query narrows it, drawn.
    fn narrowed_at(
        tree: &Tree,
        cursor: Cursor,
        query: &str,
        width: usize,
        room: usize,
    ) -> Vec<String> {
        lines(tree, &tree.rows(), cursor, query, width, room, scene().1)
            .lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect()
    }

    #[test]
    fn the_agents_come_first_and_the_rooms_under_their_own_label() {
        let drawn = drawn_at(&team(), Cursor::default(), 80, 8);
        assert_eq!(
            drawn,
            vec![
                "Agents",
                "❯ ⏺ project   what is in this workspace?",
                "  ~ reviewer  running · in #design · live · owes an answer · 22m · 3 tools · 1.…",
                "  ⏺ watcher   idle · in #design · listening · 600s",
                "Rooms",
                "  #design  2 seats · 1 owed",
            ],
            "the root first, then the agents, then the rooms under their label"
        );
    }

    /// The glyph says a seat wakes on every post; the colour goes on saying
    /// what the session is doing, so neither fact is spent on the other.
    #[test]
    fn a_live_seat_wears_the_sigil_and_keeps_its_own_colour() {
        let tree = team();
        let rows = tree.rows();
        let listed = listing(&tree, &rows, "");
        let reviewer = listed
            .agents
            .iter()
            .find(|row| row.name == "reviewer")
            .expect("the live seat");
        let glyph = dot(
            reviewer,
            seats::seat(&tree, tree.state(reviewer.session).expect("it")).as_ref(),
        );
        assert!(glyph.content.starts_with('~'), "{glyph:?}");
        assert_eq!(glyph.style, tree::bullet_style(reviewer.status, false));
    }

    /// The ear nearly every seat wears is the one no row says (ADR-0034 §6):
    /// a glyph and a word spent on the norm would say nothing about any row.
    #[test]
    fn a_seat_on_the_default_ear_says_nothing_about_what_it_hears() {
        let tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            log_frame(2, log_announced("#design")),
            log_frame(
                3,
                extended("bingo.rooms", "members", roster_payload(&["reviewer"], &[])),
            ),
        ]);
        let drawn = drawn_at(&tree, Cursor::default(), 80, 8);
        assert_eq!(
            drawn[2], "  ⏺ reviewer  idle · in #design",
            "the plain dot and the room, and not a word about the ear: {drawn:#?}"
        );
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
                "reviewer  running · in #design · live · owes an answer · 22m · 3 tools · 1.2k tokens"
            ),
            "one that has run says what it is doing, then where it sits and what \
             it owes, and only then what it has spent: {drawn:#?}"
        );
    }

    /// A member whose mark is behind the room's head says how much of it
    /// stands unread, beside the room it stands in (ADR-0034 §2).
    #[test]
    fn a_member_behind_the_room_says_how_much_it_has_not_read() {
        let mut tree = team();
        tree.apply(&log_frame(14, room_cursor("reviewer", "itm_p1")));
        let drawn = drawn_at(&tree, Cursor::default(), 120, 8);
        assert!(
            drawn[2].contains("in #design · 3 unread ·"),
            "the room holds four posts and the mark stopped at the first: {drawn:#?}"
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
        let drawn: Vec<String> = lines(&tree, &rows, Cursor::default(), "", 80, 8, scene().1)
            .lines
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect();
        assert_eq!(drawn[1], "  ⏺ archivist  stored");
    }

    #[test]
    fn one_kind_of_session_alone_wears_no_label() {
        let tree = folded_tree(busy_child("reviewer"));
        let drawn = drawn_at(&tree, Cursor::default(), 80, 8);
        assert_eq!(drawn.len(), 2, "a label over the only run: {drawn:#?}");
        assert!(drawn[0].starts_with("❯ ⏺ project"), "{drawn:#?}");
    }

    // ---- the query -------------------------------------------------------

    /// M55: the list is typed into. What the query does not match is not on
    /// it, and what a person typed stands at its head.
    #[test]
    fn a_query_narrows_the_column_and_says_what_was_typed() {
        let drawn = narrowed_at(&team(), Cursor::default(), "rev", 80, 8);
        assert_eq!(
            drawn,
            vec![
                "⌕ rev▌",
                "❯ ~ reviewer  running · in #design · live · owes an answer · 22m · 3 tools · 1.…",
            ],
            "one row, and no label over the only run left: {drawn:#?}"
        );
    }

    /// A room is found by its name, and so is every seat sitting in it: a
    /// person who knows where somebody is types that instead of who.
    #[test]
    fn a_room_and_the_seats_in_it_answer_to_the_rooms_name() {
        let drawn = narrowed_at(&team(), Cursor::default(), "design", 120, 8);
        assert_eq!(
            drawn,
            vec![
                "⌕ design▌",
                "Agents",
                "❯ ~ reviewer  running · in #design · live · owes an answer · 22m · 3 tools · 1.2k tokens",
                "  ⏺ watcher   idle · in #design · listening · 600s",
                "Rooms",
                "  #design  2 seats · 1 owed",
            ],
            "the room, and the two seats in it: {drawn:#?}"
        );
    }

    /// A query nothing matches leaves the line it was typed on and nothing
    /// else — the list is empty, not wrong.
    #[test]
    fn a_query_nothing_matches_is_the_line_alone() {
        let drawn = narrowed_at(&team(), Cursor::default(), "zzz", 80, 8);
        assert_eq!(drawn, vec!["⌕ zzz▌"]);
    }

    /// The line is the list's, so the window has one row fewer for the rows —
    /// the room asked for is the room drawn, query line and all.
    #[test]
    fn the_query_line_takes_a_row_of_the_lists_own_room() {
        let mut frames = busy_child("reviewer");
        frames
            .extend((20..32).map(|i| agent_frame(i, i, agent_announced(i, &format!("scout {i}")))));
        let tree = folded_tree(frames);
        let drawn = narrowed_at(&tree, Cursor { at: 6 }, "scout", 120, 6);
        assert_eq!(
            drawn.len(),
            6,
            "six rows of room, six rows drawn: {drawn:#?}"
        );
        assert_eq!(drawn[0], "⌕ scout▌", "and the line is the head of them");
    }

    // ---- walking ---------------------------------------------------------

    fn walk(tree: &Tree, from: Cursor, by: isize) -> Cursor {
        let rows = tree.rows();
        from.step(&listing(tree, &rows, ""), by)
    }

    #[test]
    fn the_column_is_walked_to_its_ends_and_stops_there() {
        let tree = team();
        let top = Cursor::default();
        assert_eq!(walk(&tree, top, -1), top, "the first row is the first row");
        assert_eq!(walk(&tree, top, 1).at, 1);
        let last = Cursor { at: 3 };
        assert_eq!(walk(&tree, last, 1), last, "and the last is the last");
    }

    /// The labels are furniture: the walk goes from the last agent to the
    /// first room without a step that lands on nothing.
    #[test]
    fn the_walk_steps_over_the_label_between_the_runs() {
        let tree = team();
        let rows = tree.rows();
        let listed = listing(&tree, &rows, "");
        let last_agent = Cursor { at: 2 };
        let next = last_agent.step(&listed, 1);
        assert_eq!(
            next.row(&listed).map(|row| row.name.clone()),
            Some("#design".to_string())
        );
    }

    #[test]
    fn the_cursor_opens_on_the_row_of_the_session_in_view() {
        let mut tree = team();
        tree.show(&log_id());
        let rows = tree.rows();
        let listed = listing(&tree, &rows, "");
        assert_eq!(Cursor::on(&listed, tree.view()), Cursor { at: 3 });

        tree.show(&child_id());
        let rows = tree.rows();
        let listed = listing(&tree, &rows, "");
        assert_eq!(Cursor::on(&listed, tree.view()), Cursor { at: 1 });
    }

    // ---- the window ------------------------------------------------------

    /// The bug this list was drawn through the window for: past the room it
    /// has, the row the keyboard is on must still be one of the rows drawn.
    #[test]
    fn a_list_longer_than_its_room_keeps_the_row_the_cursor_is_on() {
        let mut frames = busy_child("reviewer");
        frames.push(log_frame(8, log_announced("#design")));
        frames
            .extend((20..32).map(|i| agent_frame(i, i, agent_announced(i, &format!("scout {i}")))));
        let tree = folded_tree(frames);
        // The last agent: the room is the one row under it.
        let at = tree.rows().len() - 2;
        let drawn = drawn_at(&tree, Cursor { at }, 120, 6);
        assert_eq!(
            drawn.len(),
            6,
            "six rows of room, six rows drawn: {drawn:#?}"
        );
        assert!(
            drawn.iter().any(|row| row.contains("❯ ⏺ scout 31")),
            "the last agent, with the cursor on it: {drawn:#?}"
        );
        assert!(
            drawn[0].trim_start().starts_with('…'),
            "and the list says it goes on above: {drawn:#?}"
        );
    }

    /// A label past the end of the window is simply not drawn; the one at the
    /// window's own top edge stays.
    #[test]
    fn a_label_outside_the_window_is_not_drawn() {
        let mut frames = busy_child("reviewer");
        frames.push(log_frame(8, log_announced("#design")));
        frames
            .extend((20..32).map(|i| agent_frame(i, i, agent_announced(i, &format!("scout {i}")))));
        let tree = folded_tree(frames);
        let deep = drawn_at(&tree, Cursor { at: 10 }, 120, 6);
        assert!(
            !deep.iter().any(|row| row.trim() == "Agents"),
            "the label is far above the window: {deep:#?}"
        );
        let top = drawn_at(&tree, Cursor::default(), 120, 6);
        assert_eq!(top[0].trim(), "Agents", "and at the head it is drawn");
    }

    /// What a click on each drawn line means, taken from the same window the
    /// lines came out of — so the two can never disagree.
    #[test]
    fn every_drawn_row_says_which_row_of_the_list_it_is() {
        let tree = team();
        let rows = tree.rows();
        let roster = lines(&tree, &rows, Cursor::default(), "", 80, 8, scene().1);
        assert_eq!(roster.rows.len(), roster.lines.len());
        assert_eq!(roster.rows[0], None, "a label answers nothing");
        assert_eq!(roster.rows[1], Some(Cursor { at: 0 }));
        assert_eq!(roster.rows[4], None, "and nor does the second label");
        assert_eq!(roster.rows[5], Some(Cursor { at: 3 }), "the room's own row");
    }

    #[test]
    fn no_width_makes_a_row_wider_than_the_list() {
        let tree = team();
        for width in [8usize, 20, 40, 80, 120] {
            for query in ["", "a long query nobody would type"] {
                for line in lines(
                    &tree,
                    &tree.rows(),
                    Cursor::default(),
                    query,
                    width,
                    8,
                    scene().1,
                )
                .lines
                {
                    assert!(line.width() <= width, "at {width} for {query:?}: {line:?}");
                }
            }
        }
    }
}
