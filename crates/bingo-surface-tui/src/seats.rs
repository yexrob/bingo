//! What a room says about a session sitting in it: which room, what its ear
//! hears there, and what it owes.
//!
//! Nothing here is published and nothing is stored. A seat is composed at
//! render time from what the rooms plugin already writes — the room's own
//! `members` extension (a membership and the tree it draws as, in one payload,
//! §10 2026-09-02), the seat's own `ear:<name>` register, and the `owed`
//! signal on the room's parent (ADR-0022 §4). Joining them is the surface's
//! own business (ADR-0013 §4): a plugin describes what it knows, and which
//! rows sit beside which is a decision no plugin gets to make.
//!
//! A surface may not import a plugin (ADR-0001), so the four names below are
//! the whole of the contract between them, and every payload is read as data:
//! a shape this does not recognise leaves the fact out rather than guessing.

use bingo_sdk::SessionState;
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
/// The signal the room's parent carries while any answer is owed.
const OWED: &str = "owed";
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

/// Where a session sits, as the room it sits in has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seat {
    /// The room's name, `#design`.
    pub room: String,
    pub ear: Ear,
    /// The clock time an unanswered question was put to it. The signal carries
    /// the time it was asked at and not how long ago that was, so this is what
    /// a row can say without inventing a date to subtract from.
    pub owes_since: Option<String>,
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
        owes_since: owes_since(tree, room, &name),
    })
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
/// that is the same fact the list splits its two columns on.
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

/// When the oldest answer this seat still owes was asked for. The debts are
/// published oldest first, so the first row for a name is the one to say.
fn owes_since(tree: &Tree, room: &SessionState, name: &str) -> Option<String> {
    debts(tree, room, &tree::name(room))
        .into_iter()
        .find(|(who, _)| same(who, name))
        .map(|(_, asked)| asked)
}

/// The open debts of one room: who owes, and when it was asked. They are
/// signalled onto the room's *parent*, which is where a person looks
/// (ADR-0022 §4), so this reaches across the tree for them.
fn debts(tree: &Tree, room: &SessionState, title: &str) -> Vec<(String, String)> {
    let parent = room.summary.parent.as_ref().map(|link| &link.session);
    let Some(card) = parent
        .and_then(|id| tree.sessions().find(|state| &state.summary.id == id))
        .and_then(|state| live(state, OWED))
    else {
        return Vec::new();
    };
    table(&card, title)
}

/// The `owed` table, read by its own headers rather than by the order its
/// columns happen to be in: it is a `View::Table` a plugin wrote, not a shape
/// this surface gets to assume.
fn table(card: &Value, title: &str) -> Vec<(String, String)> {
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
                .filter_map(|row| Some((cell(row, who)?, cell(row, asked)?)))
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
                owed_payload(&[("#design", "reviewer", "14:02")]),
            ),
        ));
        folded_tree(frames)
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
                owes_since: Some("14:02".into()),
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
                owes_since: None,
            })
        );
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

    /// `owed::rows` is one row per debt; a member that owes twice is one
    /// debtor, and the oldest of its debts is the one its row says.
    #[test]
    fn a_member_that_owes_twice_is_one_debtor_and_says_the_older() {
        let mut tree = seated();
        tree.apply(&frame(
            6,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[
                    ("#design", "reviewer", "14:02"),
                    ("#design", "reviewer", "14:09"),
                ]),
            ),
        ));
        let room = tree
            .sessions()
            .find(|state| tree::name(state) == "#design")
            .expect("the room");
        assert_eq!(counts(&tree, room).owed, 1);
        assert_eq!(
            of(&tree, "reviewer").and_then(|seat| seat.owes_since),
            Some("14:02".into())
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
                owed_payload(&[("#ops", "reviewer", "14:02")]),
            ),
        ));
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.owes_since), None);
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
        assert_eq!(of(&tree, "reviewer").and_then(|seat| seat.owes_since), None);

        tree.apply(&log_frame(
            7,
            extended("bingo.rooms", "members", serde_json::json!(7)),
        ));
        assert_eq!(of(&tree, "reviewer"), None, "nobody is seated by a number");
    }
}
