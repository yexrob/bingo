//! The cut a rewind makes. Pure: a transcript and a turn in, the items that
//! go out. The kernel never rewrites a journal — it appends the item that
//! says what was undone (ADR-0002 §3) — so this answers ids, not a new list.

use bingo_sdk::{Item, ItemId, TurnId};

/// The items a rewind to `to_turn` drops: that turn's first item and every
/// item after it, in transcript order. Everything after the cut goes,
/// whatever turn it belongs to and whether it belongs to one at all — a
/// notice recorded between turns happened after the line being taken back.
///
/// `None` when this transcript has no such turn: there is nothing to go back
/// to, which is a refusal rather than an empty cut.
pub fn dropped(items: &[Item], to_turn: &TurnId) -> Option<Vec<ItemId>> {
    let opened = items
        .iter()
        .position(|item| item.turn.as_ref() == Some(to_turn))?;
    Some(items[opened..].iter().map(|item| item.id.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ContentPart, ItemBody, ItemStatus, Origin};
    use jiff::Timestamp;

    fn item(id: &str, turn: Option<&str>) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: turn.map(TurnId::from_raw),
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::UNIX_EPOCH,
            completed_at: Some(Timestamp::UNIX_EPOCH),
            intent: None,
            body: ItemBody::User {
                parts: vec![ContentPart::text(id)],
                origin: Origin::surface("test"),
            },
            meta: Default::default(),
        }
    }

    fn transcript() -> Vec<Item> {
        vec![
            item("itm_1", Some("trn_1")),
            item("itm_2", Some("trn_1")),
            item("itm_3", Some("trn_2")),
            item("itm_4", None),
            item("itm_5", Some("trn_3")),
        ]
    }

    #[test]
    fn the_cut_is_the_turns_first_item_and_everything_after_it() {
        let dropped = dropped(&transcript(), &TurnId::from_raw("trn_2")).expect("a known turn");
        assert_eq!(
            dropped.iter().map(ItemId::as_str).collect::<Vec<_>>(),
            ["itm_3", "itm_4", "itm_5"],
            "the item between the turns went with them"
        );
    }

    #[test]
    fn the_first_turn_takes_the_whole_transcript() {
        assert_eq!(
            dropped(&transcript(), &TurnId::from_raw("trn_1"))
                .expect("a known turn")
                .len(),
            5
        );
    }

    #[test]
    fn a_turn_this_transcript_never_had_is_nothing_to_go_back_to() {
        assert!(dropped(&transcript(), &TurnId::from_raw("trn_9")).is_none());
        assert!(dropped(&[], &TurnId::from_raw("trn_1")).is_none());
    }
}
