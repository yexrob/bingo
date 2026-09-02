//! What a room says about a session sitting in it: which room, what its ear
//! hears there, what stands unread and what it owes.
//!
//! Nothing here is published and nothing is stored. A seat is composed at
//! render time from what the rooms plugin already writes — the room's own
//! `members` extension (a membership and the tree it draws as, in one payload,
//! §10 2026-09-02), the seat's own `ear:<name>` register, and the `owed`
//! signal on the room's parent (ADR-0022 §4). Joining them is the surface's
//! own business (ADR-0013 §4): a plugin describes what it knows, and which
//! rows sit beside which is a decision no plugin gets to make.
//!
//! A surface may not import a plugin (ADR-0001), so the names below are the
//! whole of the contract between them, and every payload is read as data:
//! a shape this does not recognise leaves the fact out rather than guessing.

use bingo_sdk::SessionState;
use jiff::Timestamp;
use serde_json::Value;

use crate::tree::{self, Status, Tree};

/// The plugin whose journal a seat is read out of.
const PLUGIN: &str = "bingo.rooms";
/// The kind a room's whole membership is published under (ADR-0011 §2).
const MEMBERS: &str = "members";
/// The seats in that payload that listen rather than answer (ADR-0029 §2).
const LISTENERS: &str = "listeners";
/// The kind one seat's own retuning is published under, before its name
/// (ADR-0029 §4).
const EAR: &str = "ear:";
/// The kind a seat's reading mark is published under, before its room
/// (ADR-0034 §2). It sits on the member's own session, not the room's.
const CURSOR: &str = "cursor:";
/// The `seq` of the room that mark has read up to.
const SEQ: &str = "seq";
/// The signal the room's parent carries while any answer is owed.
const OWED: &str = "owed";
/// The open debts that signal carries beside the table it draws as, and the
/// fields of one: which room it stands in, who has not answered, and the
/// moment it was asked (ADR-0022 §4).
const DEBTS: &str = "debts";
const ROOM: &str = "room";
const WHO: &str = "who";
const AT: &str = "at";
/// How long a post waits for a patient seat, in both payloads that say it.
const PATIENCE_S: &str = "patience_s";
/// The columns of the `owed` table, by the headers it publishes them under.
const ROOM_COLUMN: &str = "room";
const OWED_COLUMN: &str = "owed";
const ASKED_COLUMN: &str = "asked";

/// How a seat hears its room (ADR-0029 §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ear {
    /// Every post wakes it as it lands: today's seat, and the default.
    #[default]
    Live,
    /// Posts land held and are read whole at the seat's next turn. The
    /// patience is absent where the roster named the seat without one.
    Listening { patience_s: Option<u64> },
}

/// An answer a seat still owes, as the card it is read from says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owes {
    /// The moment the question was put (`debts[].at`). How long ago that was
    /// is the row's to say, from the clock the frame is drawn against.
    Since(Timestamp),
    /// The clock time a card published before the debts carried their own
    /// stamps says — `14:02`, with no date beside it. An age would have to
    /// invent the day and the zone it was written in, so that row says the
    /// time it was asked at and nothing more.
    At(String),
}

/// Where a session sits, as the room it sits in has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seat {
    /// The room's name, `#design`.
    pub room: String,
    pub ear: Ear,
    /// How many posts stand after this seat's reading mark (ADR-0034 §2),
    /// where there is a mark to read and it is behind the room's head. A seat
    /// at the head has nothing to say, and so has one whose journal carries no
    /// mark this surface recognises — a guess would be worse than a silence.
    pub unread: Option<u64>,
    /// The oldest answer it still owes there, where one stands.
    pub owes: Option<Owes>,
}

/// What a room's own row says: how many seats it has, and how many of them
/// owe an answer. A member with two debts is one debtor, as `/room` says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub seats: usize,
    pub owed: usize,
}

/// The seat a session holds, or nothing at all for one no room has seated.
/// A session sits in at most one room here: the first that names it, in the
/// tree's own order.
pub fn seat(tree: &Tree, state: &SessionState) -> Option<Seat> {
    let name = tree::name(state);
    let room = rooms(tree).find(|room| seats_of(room).iter().any(|held| same(held, &name)))?;
    Some(Seat {
        room: tree::name(room),
        ear: ear(room, &name),
        unread: unread(state, room),
        owes: owes(tree, room, &name),
    })
}

/// What stands unread in the room for this seat: the room's head, which is the
/// last frame of its journal this attachment carries, less the `seq` the
/// seat's own mark stopped at.
fn unread(state: &SessionState, room: &SessionState) -> Option<u64> {
    let read = read_to(state, &tree::name(room))?;
    room.seq.0.checked_sub(read).filter(|behind| *behind > 0)
}

/// The `seq` this session's mark for one room stopped at. A mark is filed
/// under `cursor:<room>` and names its room in the payload as well; the
/// payload is what is read, so nothing turns on how the kind was spelled.
fn read_to(state: &SessionState, room: &str) -> Option<u64> {
    state
        .extensions
        .get(PLUGIN)?
        .iter()
        .filter(|(kind, _)| kind.starts_with(CURSOR))
        .find(|(_, mark)| marks(mark, room))
        .and_then(|(_, mark)| mark.get(SEQ)?.as_u64())
}

/// Whether a mark is this room's, as the mark itself says.
fn marks(mark: &Value, room: &str) -> bool {
    mark.get(ROOM)
        .and_then(Value::as_str)
        .is_some_and(|named| same(named, room))
}

/// What a room's row says about itself.
pub fn counts(tree: &Tree, room: &SessionState) -> Counts {
    let title = tree::name(room);
    let mut debtors: Vec<String> = Vec::new();
    for (who, _) in debts(tree, room, &title) {
        if !debtors.iter().any(|held| same(held, &who)) {
            debtors.push(who);
        }
    }
    Counts {
        seats: seats_of(room).len(),
        owed: debtors.len(),
    }
}

/// The sessions of the tree that answer nobody: a room is a `Log` session, and
/// that is the same fact the list groups its two runs by.
fn rooms(tree: &Tree) -> impl Iterator<Item = &SessionState> {
    tree.sessions().filter(|state| Status::of(state).is_none())
}

/// Who is in a room, as the room's own journal has it. Anything in the payload
/// that is not a name is not one.
fn seats_of(room: &SessionState) -> Vec<String> {
    published(room, MEMBERS)
        .and_then(|payload| payload.get(MEMBERS).cloned())
        .as_ref()
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What one seat hears: its own retuning where it has written one, else what
/// the roster declared for it — the two layers ADR-0029 §4 keeps apart, read
/// in that order.
fn ear(room: &SessionState, name: &str) -> Ear {
    retuned(room, name).unwrap_or_else(|| declared(room, name))
}

/// The seat's own `ear:<name>` register. A patience of zero is a live ear
/// said the long way round.
fn retuned(room: &SessionState, name: &str) -> Option<Ear> {
    let seconds = published(room, &format!("{EAR}{}", name.to_lowercase()))?
        .get(PATIENCE_S)?
        .as_u64()?;
    Some(listening(Some(seconds)))
}

/// The ear the roster was seated with. A membership with no listeners at all
/// — every room opened before there were ears — is all live.
fn declared(room: &SessionState, name: &str) -> Ear {
    published(room, MEMBERS)
        .and_then(|payload| payload.get(LISTENERS).cloned())
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|listeners| {
            listeners
                .iter()
                .find(|listener| names(listener, name))
                .cloned()
        })
        .map(|listener| listening(listener.get(PATIENCE_S).and_then(Value::as_u64)))
        .unwrap_or_default()
}

/// A patience as an ear: zero seconds is the live seat it describes.
fn listening(patience_s: Option<u64>) -> Ear {
    match patience_s {
        Some(0) => Ear::Live,
        patience_s => Ear::Listening { patience_s },
    }
}

/// Whether a listener names this seat. A door takes a bare name or a name with
/// the patience it asks for, and both shapes reach the journal.
fn names(listener: &Value, name: &str) -> bool {
    let said = listener
        .as_str()
        .or_else(|| listener.get("name").and_then(Value::as_str));
    said.is_some_and(|said| same(said, name))
}

/// The oldest answer this seat still owes. The debts are published oldest
/// first, so the first of them for a name is the one to say.
fn owes(tree: &Tree, room: &SessionState, name: &str) -> Option<Owes> {
    debts(tree, room, &tree::name(room))
        .into_iter()
        .find(|(who, _)| same(who, name))
        .map(|(_, owes)| owes)
}

/// The open debts of one room: who owes, and what a row can say about when.
/// They are signalled onto the room's *parent*, which is where a person looks
/// (ADR-0022 §4), so this reaches across the tree for them.
fn debts(tree: &Tree, room: &SessionState, title: &str) -> Vec<(String, Owes)> {
    let parent = room.summary.parent.as_ref().map(|link| &link.session);
    let Some(card) = parent
        .and_then(|id| tree.sessions().find(|state| &state.summary.id == id))
        .and_then(|state| live(state, OWED))
    else {
        return Vec::new();
    };
    facts(&card, title).unwrap_or_else(|| table(&card, title))
}

/// The debts the card carries beside the table it draws as: the moment each
/// question was put, which is what lets a row say how long it has stood.
/// `None` where the payload has no debts at all — a card left by a process
/// that published only the table, whose clock time the fallback reads.
fn facts(card: &Value, title: &str) -> Option<Vec<(String, Owes)>> {
    let debts = card.get(DEBTS)?.as_array()?;
    let said = |debt: &Value, field: &str| debt.get(field)?.as_str().map(str::to_string);
    Some(
        debts
            .iter()
            .filter(|debt| said(debt, ROOM).is_some_and(|named| same(&named, title)))
            .filter_map(|debt| Some((said(debt, WHO)?, Owes::Since(asked(debt)?))))
            .collect(),
    )
}

/// The moment a debt was asked, as the payload states it: RFC 3339, the
/// spelling every timestamp on the wire wears.
fn asked(debt: &Value) -> Option<Timestamp> {
    debt.get(AT)?.as_str()?.parse().ok()
}

/// The `owed` table, read by its own headers rather than by the order its
/// columns happen to be in: it is a `View::Table` a plugin wrote, not a shape
/// this surface gets to assume. Only a card without debts is read this far,
/// and only such a card has the `asked` column this wants.
fn table(card: &Value, title: &str) -> Vec<(String, Owes)> {
    let column = |header: &str| {
        card.get("headers")?
            .as_array()?
            .iter()
            .position(|cell| cell.as_str() == Some(header))
    };
    let (Some(room), Some(who), Some(asked)) = (
        column(ROOM_COLUMN),
        column(OWED_COLUMN),
        column(ASKED_COLUMN),
    ) else {
        return Vec::new();
    };
    let cell = |row: &Value, at: usize| row.get(at)?.as_str().map(str::to_string);
    card.get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|row| cell(row, room).is_some_and(|named| same(&named, title)))
                .filter_map(|row| Some((cell(row, who)?, Owes::At(cell(row, asked)?))))
                .collect()
        })
        .unwrap_or_default()
}

/// The whole of one kind a plugin journaled into a session (ADR-0013 §2).
fn published<'a>(state: &'a SessionState, kind: &str) -> Option<&'a Value> {
    state.extensions.get(PLUGIN)?.get(kind)
}

/// The whole of one kind a plugin is signalling onto a session's stream. It
/// is named for the lane rather than the call, so the fixture that writes one
/// keeps the plugin's own word for it.
fn live(state: &SessionState, kind: &str) -> Option<Value> {
    state.signals.get(PLUGIN)?.get(kind).cloned()
}

/// A room compares names in any case, so a reader of its journal does too.
fn same(one: &str, other: &str) -> bool {
    one.eq_ignore_ascii_case(other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// A root with a room under it, two sub-agents seated in it, one of them
    /// listening, and a debt the room's parent is signalling.
    fn seated() -> Tree {
        let mut frames = vec![
            child_frame(1, announced("reviewer")),
            agent_frame(3, 2, agent_announced(3, "watcher")),
            log_frame(3, log_announced("#design")),
            log_frame(
                4,
                extended(
                    "bingo.rooms",
                    "members",
                    roster_payload(&["reviewer", "watcher"], &[("watcher", 300)]),
                ),
            ),
        ];
        frames.push(frame(
            5,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[("#design", "reviewer", 22)]),
            ),
        ));
        folded_tree(frames)
    }

    /// The moment a debt `minutes` old was asked at, against the clock every
    /// scene is drawn with.
    fn asked_minutes_ago(minutes: i64) -> Owes {
        Owes::Since(ts() - jiff::SignedDuration::from_mins(minutes))
    }

    fn of(tree: &Tree, name: &str) -> Option<Seat> {
        let state = tree
            .sessions()
            .find(|state| tree::name(state) == name)
            .expect("a session by that name");
        seat(tree, state)
    }

    #[test]
    fn a_member_sits_in_the_room_whose_roster_names_it() {
        let tree = seated();
        assert_eq!(
            of(&tree, "reviewer"),
            Some(Seat {
                room: "#design".into(),
                ear: Ear::Live,
                unread: None,
                owes: Some(asked_minutes_ago(22)),
            })
        );
        assert_eq!(of(&tree, "project"), None, "the root sits in no room");
    }

    #[test]
    fn a_listener_wears_the_patience_the_roster_declared_for_it() {
        assert_eq!(
            of(&seated(), "watcher").map(|seat| seat.ear),
            Some(Ear::Listening {
                patience_s: Some(300)
            })
        );
    }

    /// The two layers of ADR-0029 §4: a seat's own register outranks what the
    /// roster declared, and a cleared one gives the declaration back.
    #[test]
    fn a_seat_that_retuned_its_own_ear_is_heard_over_the_roster() {
        let mut tree = seated();
        tree.apply(&log_frame(
            6,
            extended(
                "bingo.rooms",
                "ear:watcher",
                serde_json::json!({"patience_s": 60}),
            ),
        ));
        assert_eq!(
            of(&tree, "watcher").map(|seat| seat.ear),
            Some(Ear::Listening {
                patience_s: Some(60)
            })
        );

        tree.apply(&log_frame(
            7,
            extended(
                "bingo.rooms",
                "ear:watcher",
                serde_json::json!({"patience_s": 0}),
            ),
        ));
        assert_eq!(
            of(&tree, "watcher").map(|seat| seat.ear),
            Some(Ear::Live),
            "no patience at all is the live seat it describes"
        );
    }

    /// The shape a room opened before there were ears left in its journal.
    #[test]
    fn a_roster_written_before_there_were_ears_seats_everyone_live() {
        let tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            log_frame(2, log_announced("#design")),
            log_frame(
                3,
                extended(
                    "bingo.rooms",
                    "members",
                    serde_json::json!({"members": ["reviewer"]}),
                ),
            ),
        ]);
        assert_eq!(
            of(&tree, "reviewer"),
            Some(Seat {
                room: "#design".into(),
                ear: Ear::Live,
                unread: None,
                owes: None,
            })
        );
    }

    /// The one fact "seen" derives from (ADR-0034 §2): the room's head less
    /// the `seq` the seat's own mark stopped at, and nothing at all once the
    /// mark has caught up.
    #[test]
    fn a_seat_behind_the_rooms_head_counts_what_it_has_not_read() {
        let mut tree = seated();
        tree.apply(&child_frame(6, room_cursor("#design", 1)));
        assert_eq!(
            of(&tree, "reviewer").and_then(|seat| seat.unread),
            Some(3),
            "the room's journal reaches seq 4 and the mark stopped at 1"
        );

        tree.apply(&child_frame(7, room_cursor("#design", 4)));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.unread), None);
    }

    /// A mark says which room it belongs to, and only that room's head is
    /// counted against it. A payload of another shape leaves the fact out.
    #[test]
    fn a_mark_of_another_room_or_another_shape_counts_nothing() {
        let mut tree = seated();
        tree.apply(&child_frame(6, room_cursor("#ops", 1)));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.unread), None);

        tree.apply(&child_frame(
            7,
            extended(
                "bingo.rooms",
                "cursor:#design",
                serde_json::json!({"room": "#design", "seq": "the first"}),
            ),
        ));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.unread), None);
    }

    #[test]
    fn a_room_says_how_many_seats_it_has_and_how_many_of_them_owe() {
        let tree = seated();
        let room = tree
            .sessions()
            .find(|state| tree::name(state) == "#design")
            .expect("the room");
        assert_eq!(counts(&tree, room), Counts { seats: 2, owed: 1 });
    }

    /// The card is one debt per row; a member that owes twice is one debtor,
    /// and the oldest of its debts is the one its row says.
    #[test]
    fn a_member_that_owes_twice_is_one_debtor_and_says_the_older() {
        let mut tree = seated();
        tree.apply(&frame(
            6,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[("#design", "reviewer", 22), ("#design", "reviewer", 15)]),
            ),
        ));
        let room = tree
            .sessions()
            .find(|state| tree::name(state) == "#design")
            .expect("the room");
        assert_eq!(counts(&tree, room).owed, 1);
        assert_eq!(
            of(&tree, "reviewer").and_then(|seat| seat.owes),
            Some(asked_minutes_ago(22))
        );
    }

    /// Another room's debts are another room's: the table names the room in
    /// every row, and only the rows that name this one are read.
    #[test]
    fn a_debt_in_another_room_is_not_this_rooms() {
        let mut tree = seated();
        tree.apply(&frame(
            6,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[("#ops", "reviewer", 22)]),
            ),
        ));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.owes), None);
    }

    /// A card a process before the debts published: the clock time is all it
    /// has, so that is what the seat carries and what its row will say. This
    /// is a payload already in people's journals, not a shape this surface is
    /// free to stop reading.
    #[test]
    fn a_card_from_before_the_debts_says_the_clock_time_it_carries() {
        let mut tree = seated();
        tree.apply(&frame(
            6,
            signalled(
                "bingo.rooms",
                "owed",
                owed_table_payload(&[("#design", "reviewer", "14:02")]),
            ),
        ));
        assert_eq!(
            of(&tree, "reviewer").and_then(|seat| seat.owes),
            Some(Owes::At("14:02".into()))
        );
    }

    /// A payload this surface does not recognise leaves the fact out. The
    /// journal is somebody else's, and a guess would be worse than a silence.
    #[test]
    fn a_payload_of_another_shape_says_nothing_rather_than_guessing() {
        let mut tree = seated();
        tree.apply(&frame(
            6,
            signalled("bingo.rooms", "owed", serde_json::json!({"kind": "text"})),
        ));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.owes), None);

        tree.apply(&log_frame(
            7,
            extended("bingo.rooms", "members", serde_json::json!(7)),
        ));
        assert_eq!(of(&tree, "reviewer"), None, "nobody is seated by a number");
    }
}
