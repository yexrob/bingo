//! Showing what is owed (ADR-0022 §4): the column `/room` gains, and the card
//! the room's parent carries while any debt stands. Both are drawn from the
//! fold at the moment they are asked for, and nothing here is kept.

use bingo_sdk::{HostHandle, SessionId, View};
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};

use crate::PLUGIN;
use crate::mentions::Mention;

/// The kind the card is published under. The latest payload is the whole of
/// it, and `Null` is what takes the card away (ADR-0013 §2).
pub const KIND: &str = "owed";

/// The facts the card rides with, beside the table it draws as.
const DEBTS: &str = "debts";

const HEADERS: [&str; 2] = ["room", "owed"];

/// One open debt, as the card carries it: the room it stands in, who has not
/// answered, and the moment it was asked. A reader that knows this signal
/// takes the age it wants from `at`; the card itself says none, because a
/// signal republished only when a debt opens or closes cannot keep one true.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Debt {
    pub room: String,
    pub who: String,
    pub at: Timestamp,
}

/// The `/room` cell: who owes, oldest first, and how long they have owed it.
/// A member with two debts is named once, for the older.
pub fn column(open: &[Mention], now: Timestamp) -> String {
    let mut said: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for mention in oldest_first(open) {
        let who = mention.owed_by.said();
        if seen.contains(&who) {
            continue;
        }
        said.push(format!("{who} {}", age(mention.at, now)));
        seen.push(who);
    }
    said.join(", ")
}

/// What one room owes, oldest first: the one mint, from the one fold. A member
/// with two debts owes twice here, and `column` is where two become one.
pub fn debts(title: &str, open: &[Mention]) -> Vec<Debt> {
    oldest_first(open)
        .into_iter()
        .map(|mention| Debt {
            room: title.to_string(),
            who: mention.owed_by.said(),
            at: mention.at,
        })
        .collect()
}

/// The card: a table while anything is owed, and nothing at all once the last
/// debt closes. The debts ride in the same payload as the table drawn from
/// them, the way a roster does (`room::payload`, ADR-0013 §2) — a surface that
/// knows only the vocabulary draws the table, and one that knows this signal
/// reads the facts, without either having to know about the other.
pub fn view(debts: Vec<Debt>) -> Value {
    if debts.is_empty() {
        return Value::Null;
    }
    let mut payload = drawn(&debts);
    payload[DEBTS] = json!(debts);
    payload
}

/// The two columns a person reads on the card: which room, and who has not
/// answered. The clock left it on 2026-09-02 — three columns were one wider
/// than the rail, and the age a person wants is the session list's to say from
/// `at`, drawn as it is asked for rather than as it was published.
fn drawn(debts: &[Debt]) -> Value {
    let view = View::Table {
        headers: HEADERS.map(str::to_string).to_vec(),
        rows: debts
            .iter()
            .map(|debt| vec![debt.room.clone(), debt.who.clone()])
            .collect(),
    };
    serde_json::to_value(view).unwrap_or_default()
}

/// Put it where a person looks: on the session the room hangs under, which is
/// the whole of who its members are.
pub async fn publish(host: &HostHandle, parent: &SessionId, payload: Value) {
    if let Err(error) = host.signal(parent, PLUGIN, KIND, payload).await {
        tracing::debug!(%error, "what the rooms owe was not published");
    }
}

fn oldest_first(open: &[Mention]) -> Vec<&Mention> {
    let mut sorted: Vec<&Mention> = open.iter().collect();
    sorted.sort_by_key(|mention| mention.at);
    sorted
}

/// How long a debt has stood, as a person says it.
fn age(at: Timestamp, now: Timestamp) -> String {
    let seconds = now.duration_since(at).as_secs().max(0);
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        _ => format!("{}h", seconds / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mentions::Owed;
    use bingo_sdk::ItemId;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a timestamp")
    }

    fn mention(owed_by: Owed, second: i64) -> Mention {
        Mention {
            owed_by,
            asker: "parent".into(),
            post: ItemId::from_raw(format!("itm_{second}")),
            at: at(second),
            head: "look again".into(),
        }
    }

    fn member(name: &str, second: i64) -> Mention {
        mention(Owed::Member(name.into()), second)
    }

    #[test]
    fn the_column_names_who_owes_oldest_first_with_how_long() {
        let open = [member("scout", 60), member("reviewer", 0)];
        assert_eq!(column(&open, at(3600)), "reviewer 1h, scout 59m");
        assert_eq!(column(&[], at(3600)), "", "a room owing nothing says so");
    }

    #[test]
    fn a_member_who_owes_twice_is_named_once_for_the_older() {
        let open = [member("scout", 300), member("scout", 60)];
        assert_eq!(column(&open, at(360)), "scout 5m");
    }

    #[test]
    fn an_age_reads_the_way_a_person_says_it() {
        assert_eq!(age(at(0), at(0)), "0s");
        assert_eq!(age(at(0), at(59)), "59s");
        assert_eq!(age(at(0), at(60)), "1m");
        assert_eq!(age(at(0), at(7200)), "2h");
        assert_eq!(age(at(60), at(0)), "0s", "a clock that went backwards");
    }

    #[test]
    fn the_room_is_owed_under_its_own_sigil() {
        assert_eq!(column(&[mention(Owed::Room, 0)], at(0)), "@all 0s");
    }

    /// A rostered holder is a debtor like any other, shown by the name
    /// everyone in the room uses for it (ADR-0028).
    #[test]
    fn the_holder_owes_by_the_name_its_members_call_it() {
        let open = [member(crate::name::PARENT, 0)];
        assert_eq!(column(&open, at(60)), "parent 1m");
        assert_eq!(debts("#design", &open)[0].who, "parent");
    }

    /// The whole of what a room's parent is signalled, asserted as one value:
    /// a row per debt, oldest first, and the moment each was asked beside them
    /// rather than in them. This is a payload other processes have already
    /// written and this one still reads, so it is a fixture and not a value
    /// this crate is free to reshape.
    #[test]
    fn the_card_carries_the_table_and_the_debts_it_is_drawn_from() {
        let open = [member("scout", 60), member("reviewer", 0)];
        assert_eq!(
            view(debts("#design", &open)),
            serde_json::json!({
                "kind": "table",
                "headers": ["room", "owed"],
                "rows": [["#design", "reviewer"], ["#design", "scout"]],
                "debts": [
                    {"room": "#design", "who": "reviewer", "at": "1970-01-01T00:00:00Z"},
                    {"room": "#design", "who": "scout", "at": "1970-01-01T00:01:00Z"},
                ],
            }),
            "oldest first, in both halves"
        );

        assert_eq!(
            view(debts("#design", &[])),
            Value::Null,
            "a null payload is what removes the card"
        );
    }

    /// The table rides beside the facts, so a surface that knows only the
    /// vocabulary draws the card and neither half has to know about the other.
    #[test]
    fn the_card_is_both_a_table_and_the_debts_beside_it() {
        let card = view(debts("#design", &[member("reviewer", 0)]));
        let view: View = serde_json::from_value(card).expect("a view a surface can draw");
        assert_eq!(view.fold(), "room · owed\n#design · reviewer");
    }
}
