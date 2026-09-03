//! What was said into a session, as one transcript draws it: a person's own
//! line on its bar, a subsystem reporting in on a marked row, and — in a room's
//! own view — the conversation itself.
//!
//! One `ItemBody::User` is every one of those. Which it is comes off the origin
//! the kernel stamped on it and the session it is being read in, both read at
//! render time (ADR-0002): nothing here remembers a thing, and no kind of
//! saying is a second representation of another.

use bingo_sdk::{ContentPart, Driver, Item, Origin};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::{OUTPUT_ROWS, Rows, bullet_style, kept, plain, returns, speaks, speaks_indent, under};
use crate::fold::Fold;
use crate::skill;
use crate::{commands, seats, theme};

/// The surfaces whose input is the machinery reporting in rather than
/// somebody speaking: a background job that ended, a message from another
/// session, a room's post, a scheduled turn, a command's own prompt. What they
/// deliver reads as a tool row does, because that is what it is — something
/// that happened, not something anyone said to you.
///
/// `command` is the one a person did set in motion, and it is here anyway: a
/// `/guide` puts a page of skill body in the journal under a line nobody
/// typed, and drawn as prose it is a wall of somebody else's words on the
/// person's own bar. It reads as the machinery it is, and where the command
/// was a skill the row is the run itself — `❖ Skill(guide) …`, the same row
/// the model's own way to that body draws ([`skill_row`]).
///
/// The set is closed, and this list is the only place it is written down: a
/// surface nobody has put here is loud. A new subsystem chooses its side by
/// being added or left out, deliberately, and the cost of each mistake says
/// which way to lean — a person's own words drawn as machinery is a wrong
/// nobody can undo by reading harder.
pub(super) const QUIET_SURFACES: &[&str] = &["agent", "bash", commands::SURFACE, ROOMS, "schedule"];

/// The surface everything a room sends wears: a post it fanned out before
/// ADR-0034 stopped copying them, and the nudge that wakes a seat with nothing
/// in it (`bingo-rooms`' own `room`). A surface may not import a plugin
/// (ADR-0001), so the word is the whole of the contract.
pub(super) const ROOMS: &str = "room";

/// The origin the kernel stamps on what a context contributor folded into the
/// head of a turn (`contributor:<id>`), for the one that reads a member's rooms
/// there — `[#design, since you last read]` and the posts under it (ADR-0034
/// §4).
const ROOMS_READING: &str = "contributor:rooms";

/// Whether a delivery is the machinery reporting in. The composer's pending
/// area asks the same question of what is still queued (ADR-0028), so the set
/// stays one list read from two places rather than two lists to keep in step.
pub(crate) fn quiet(origin: &Origin) -> bool {
    QUIET_SURFACES.contains(&origin.surface.as_str())
}

/// What a `User` item draws, which is a different thing in each of the two
/// places one is read.
///
/// In a room — a session nothing answers — every user item is a post, and the
/// room is the conversation: they are drawn as one ([`post`]).
///
/// Everywhere else, a room's activity is the room's: a member's transcript, the
/// holder's included, shows none of it, and a wake just opens a turn
/// (ADR-0034). The two things a room still puts in a member's journal are the
/// nudge and the reading its turn folded in; both are dropped here rather than
/// drawn, because the room's own view is where they are read and it is one
/// keystroke away. It is those two origins and nothing wider: a peer's message
/// carries a conversation of its own and still draws, and every other
/// contributor still speaks.
pub fn lines(
    item: &Item,
    parts: &[ContentPart],
    origin: &Origin,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    if rows.driver == Driver::Log {
        return post(item, parts, origin.principal.as_deref(), rows);
    }
    if rooms_machinery(origin) {
        return Vec::new();
    }
    match quiet(origin) {
        true => notice(item, parts, origin, fold, rows),
        false => user(parts, origin.principal.as_deref(), rows),
    }
}

/// One post in the room's own transcript: who said it, then the whole of what
/// they said.
///
/// A room is read the way a chat is read, so the name leads and the message
/// follows it in the text everything said is drawn in — every line of it,
/// wrapped as prose. Nothing here hangs dim under a `⎿` and nothing folds
/// away: that is the shape of a subsystem reporting in ([`notice`]), and under
/// it the body of a message reads as collapsed metadata rather than as what
/// somebody wrote.
///
/// A post nobody signed came from the session the room hangs under — a person
/// at this composer, or the model that holds it — and every seat reads that one
/// under the roster's own word for it.
fn post(
    item: &Item,
    parts: &[ContentPart],
    principal: Option<&str>,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let text = said(parts);
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let mut body: Vec<Line<'static>> = text
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::text())))
        .collect();
    if let Some(first) = body.first_mut() {
        first
            .spans
            .insert(0, name(principal.unwrap_or(seats::HOLDER)));
    }
    speaks(bullet_style(item.status, false), body, rows)
}

/// Who said it: the one span a post spends on emphasis, so the eye finds the
/// author before it reads the line.
fn name(principal: &str) -> Span<'static> {
    Span::styled(format!("{principal}: "), theme::text().patch(theme::bold()))
}

/// Whether this is the rooms plugin talking rather than somebody in the room.
fn rooms_machinery(origin: &Origin) -> bool {
    origin.surface == ROOMS || origin.surface == ROOMS_READING
}

/// What a `User` item says, as its parts spell it.
fn said(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("")
}

/// A subsystem's notice, marked the way a tool row is: the bullet, the one
/// line that says what happened, and the rest of it hanging under a `⎿` —
/// dim, folded, subordinate. The text already leads with the outcome, so the
/// first line is the summary and nothing has to be invented for it.
fn notice(
    item: &Item,
    parts: &[ContentPart],
    origin: &Origin,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let text = said(parts);
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
    // A blank line between the headline and the rest is a separator in the
    // text — a command's prompt puts one there — and under a `⎿` it would be
    // a row spent on nothing.
    let rest = rest.trim_start_matches('\n');
    // A skill is one row however it was asked for, so the typed line gives the
    // headline up to the run's own signature. The line itself stays in the
    // item, which is where a rewind reads it back.
    let style = bullet_style(item.status, false);
    let mut out = match skill::of(item, rows.commands) {
        Some(run) => super::skill_row(run, style, rows),
        None => speaks(
            style,
            vec![headline(
                head,
                origin.principal.as_deref(),
                elsewhere(origin, rows),
            )],
            rows,
        ),
    };
    if !rest.trim().is_empty() {
        out.extend(returns(kept(plain(rest), fold, OUTPUT_ROWS, None), rows));
    }
    out
}

/// The conversation a delivery says it came from, where saying it tells a
/// person something: in a member's own transcript a room post is one of
/// several conversations arriving, and in the room's own it is the only one.
fn elsewhere<'a>(origin: &'a Origin, rows: &Rows<'_>) -> Option<&'a str> {
    origin
        .conversation
        .as_deref()
        .filter(|room| Some(*room) != rows.title)
}

/// The marked line itself: the sender's name where the origin carries one —
/// an agent, a room's member — where they said it, and what happened.
fn headline(head: &str, principal: Option<&str>, conversation: Option<&str>) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(name) = principal {
        let bold = theme::text().patch(theme::bold());
        spans.push(Span::styled(name.to_string(), bold));
        match conversation {
            // Who is bold, where is furniture: the room is dim so the name
            // still wins the row (design §2).
            Some(room) => spans.push(Span::styled(format!(" in {room}: "), theme::dim())),
            None => spans.push(Span::styled(": ".to_string(), bold)),
        }
    }
    spans.push(Span::styled(head.to_string(), theme::text()));
    Line::from(spans)
}

/// A person's own line, on a bar the width of the transcript. An origin that
/// names a principal is somebody else speaking — a channel's correspondent, a
/// person writing from elsewhere — so the line says who, as a chat does. Where
/// they said it is the view one is looking at; saying it again would be noise.
fn user(parts: &[ContentPart], principal: Option<&str>, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let text = said(parts);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut body: Vec<Line<'static>> = text
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::text())))
        .collect();
    if let Some(name) = principal
        && let Some(first) = body.first_mut()
    {
        first.spans.insert(
            0,
            Span::styled(format!("{name}: "), theme::text().patch(theme::bold())),
        );
    }
    let mark = Span::styled(format!("{} ", theme::user()), theme::dim());
    under(mark, body, speaks_indent(), rows.measure())
        .into_iter()
        .map(|line| bar(line, rows.width))
        .collect()
}

/// The raised bar behind a `>` line: it runs to the edge of the transcript,
/// so what you said is a band and not a sentence.
fn bar(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = line.spans;
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    let mut line = Line::from(spans);
    line.style = theme::raised();
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(surface: &str) -> Origin {
        Origin {
            surface: surface.into(),
            principal: None,
            conversation: Some("#design".into()),
        }
    }

    /// The two origins a room writes into a member, and the neighbours each of
    /// them is one character from. What every one of these *draws* is asserted
    /// where the transcript is rendered whole; this is the boundary itself.
    #[test]
    fn only_the_rooms_two_origins_are_its_machinery() {
        assert!(rooms_machinery(&from(ROOMS)));
        assert!(rooms_machinery(&from(ROOMS_READING)));
        for other in [
            "agent",
            "bash",
            "schedule",
            "tui",
            "rooms",
            "contributor:experience:recall",
            "contributor:rooms:owed",
        ] {
            assert!(!rooms_machinery(&from(other)), "{other} is not a room's");
        }
    }
}
