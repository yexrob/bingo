//! Showing what is owed (ADR-0022 §4): the column `/room` gains, and the card
//! the room's parent carries while any debt stands. Both are drawn from the
//! fold at the moment they are asked for, and nothing here is kept.

use bingo_sdk::{HostHandle, SessionId, View};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::Value;

use crate::PLUGIN;
use crate::mentions::Mention;

/// The kind the card is published under. The latest payload is the whole of
/// it, and `Null` is what takes the card away (ADR-0013 §2).
pub const KIND: &str = "owed";

const HEADERS: [&str; 3] = ["room", "owed", "asked"];

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

/// One row per open debt in one room, oldest first.
pub fn rows(title: &str, open: &[Mention]) -> Vec<Vec<String>> {
    oldest_first(open)
        .into_iter()
        .map(|mention| vec![title.to_string(), mention.owed_by.said(), asked(mention.at)])
        .collect()
}

/// The card: a table while anything is owed, and nothing at all once the last
/// debt closes.
pub fn view(rows: Vec<Vec<String>>) -> Value {
    if rows.is_empty() {
        return Value::Null;
    }
    let view = View::Table {
        headers: HEADERS.map(str::to_string).to_vec(),
        rows,
    };
    serde_json::to_value(view).unwrap_or(Value::Null)
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

/// The clock time a question was asked. A card holds the fact rather than an
/// age, which would be wrong a second after it was drawn.
fn asked(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%H:%M")
        .to_string()
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
        assert_eq!(rows("#design", &open)[0][1], "parent");
    }

    #[test]
    fn the_card_is_a_row_per_debt_and_nothing_at_all_when_none_stand() {
        let open = [member("scout", 60), member("reviewer", 0)];
        let card = view(rows("#design", &open));
        assert_eq!(card["kind"], "table");
        assert_eq!(card["headers"], serde_json::json!(HEADERS));
        let rows = card["rows"].as_array().expect("rows").clone();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "#design");
        assert_eq!(rows[0][1], "reviewer", "oldest first");
        assert_eq!(rows[1][1], "scout");

        assert_eq!(
            view(rows_of_nothing()),
            Value::Null,
            "a null payload is what removes the card"
        );
    }

    fn rows_of_nothing() -> Vec<Vec<String>> {
        rows("#design", &[])
    }
}
