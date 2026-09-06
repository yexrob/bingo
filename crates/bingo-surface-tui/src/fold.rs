//! How much of a block is on the screen.
//!
//! One fact per item, and one place that answers it. A block a person has not
//! touched is not in the map at all: every kind starts at its peek, and the
//! default is written once here rather than remembered at every row that draws
//! one. The keyboard and the pointer both write into this map (§7), so a block
//! is open in one way only.

use std::collections::BTreeMap;

use bingo_sdk::{Item, ItemBody, ItemId};

/// How much of what hangs under a row is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// The row alone.
    Shut,
    /// The rows the kind keeps back to: a thought's two, a result's five.
    Peek,
    /// The whole of it.
    Open,
}

/// What a person has opened or shut, by item. Everything else wears the
/// default its kind has.
pub type Folds = BTreeMap<ItemId, Fold>;

/// How much of this item is shown. Every row that folds asks this and nothing
/// else.
///
/// Every kind peeks (2026-09-06, user-directed): a thought holds the two rows
/// it was streaming when the last delta landed, a result keeps the five-row
/// cut it always kept. Nothing a block wears changes at the moment it ends, so
/// nothing above it moves.
pub fn fold_of(folds: &Folds, item: &Item) -> Fold {
    folds.get(&item.id).copied().unwrap_or(Fold::Peek)
}

/// Which kind has a shut for the ring to reach: a thought that has been had,
/// and not one of an ACP agent's own calls, which is a reasoning item too
/// (ADR-0035 §4) and draws as a tool row. What came back from a call is read,
/// so it has the two states every other result has.
fn shuts(item: &Item) -> bool {
    matches!(item.body, ItemBody::Reasoning { .. })
        && item.completed_at.is_some()
        && !crate::acp::is_call(item)
}

/// A click is one gesture on one row (§7): it advances the fold one step, and
/// from the last of them comes back to the peek every kind starts at. A
/// thought that is over is the one kind with a shut, and the shut sits on the
/// far side of the whole — reached by going round, never met on arrival,
/// because the rows a thought ends on are the rows it was already wearing.
pub fn cycled(item: &Item, fold: Fold) -> Fold {
    match fold {
        Fold::Peek => Fold::Open,
        Fold::Open if shuts(item) => Fold::Shut,
        Fold::Open | Fold::Shut => Fold::Peek,
    }
}

/// `ctrl+o` only ever opens further (§7): a shut block lifts to its peek, a
/// peek to the whole, and the whole takes the sheet — which is what `None`
/// says, since the sheet is not a fold.
pub fn deeper(fold: Fold) -> Option<Fold> {
    match fold {
        Fold::Shut => Some(Fold::Peek),
        Fold::Peek => Some(Fold::Open),
        Fold::Open => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{item, ts};
    use bingo_sdk::ItemStatus;

    fn reasoning(seconds: Option<i64>) -> Item {
        let mut thought = item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text: "the manifest first".into(),
                provider_metadata: Default::default(),
            },
        );
        thought.completed_at = seconds.map(|s| ts() + jiff::SignedDuration::from_secs(s));
        thought
    }

    fn a_result() -> Item {
        item(
            "itm_2",
            ItemStatus::Completed,
            ItemBody::Assistant {
                text: "done".into(),
            },
        )
    }

    /// The one table this module is: where each kind starts.
    #[test]
    fn every_kind_starts_at_its_peek() {
        assert_eq!(fold_of(&Folds::new(), &reasoning(Some(2))), Fold::Peek);
        assert_eq!(fold_of(&Folds::new(), &reasoning(None)), Fold::Peek);
        assert_eq!(fold_of(&Folds::new(), &a_result()), Fold::Peek);
    }

    /// The two rings a click walks: three states where the kind has a shut,
    /// two where it has none — and the shut on the far side of the whole,
    /// never where the block was met.
    #[test]
    fn a_click_goes_round_three_states_on_a_thought_and_two_on_a_result() {
        assert_eq!(
            round(&reasoning(Some(2)), 3),
            vec![Fold::Open, Fold::Shut, Fold::Peek]
        );
        assert_eq!(
            round(&reasoning(None), 2),
            vec![Fold::Open, Fold::Peek],
            "a thought still being had has no shut: it is the one thing saying it thinks"
        );
        assert_eq!(
            round(&a_result(), 2),
            vec![Fold::Open, Fold::Peek],
            "no shut to fall into"
        );
    }

    /// Where a run of clicks on one row leaves it, from the fold its kind
    /// starts at.
    fn round(item: &Item, clicks: usize) -> Vec<Fold> {
        let mut fold = fold_of(&Folds::new(), item);
        (0..clicks)
            .map(|_| {
                fold = cycled(item, fold);
                fold
            })
            .collect()
    }

    /// An agent's own call is a reasoning item that is not a thought
    /// (ADR-0035 §4): what came back from it is read, so it starts where every
    /// other result does and has two states rather than three.
    #[test]
    fn an_agents_own_call_peeks_the_way_a_result_does() {
        let call = crate::test_support::agent_call(
            "itm_1",
            "read Read src/lib.rs",
            serde_json::json!({
                "external": true, "kind": "read", "status": "completed",
                "title": "Read src/lib.rs",
                "content": [
                    { "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }
                ]
            }),
        );
        assert_eq!(fold_of(&Folds::new(), &call), Fold::Peek);
        assert_eq!(
            cycled(&call, Fold::Open),
            Fold::Peek,
            "no shut to fall into"
        );
    }

    /// A person's own entry wins over the kind's start.
    #[test]
    fn what_a_person_set_is_what_the_row_wears() {
        let thought = reasoning(Some(2));
        let folds: Folds = [(thought.id.clone(), Fold::Open)].into_iter().collect();
        assert_eq!(fold_of(&folds, &thought), Fold::Open);
    }

    #[test]
    fn ctrl_o_climbs_to_the_sheet_and_never_back() {
        assert_eq!(deeper(Fold::Shut), Some(Fold::Peek));
        assert_eq!(deeper(Fold::Peek), Some(Fold::Open));
        assert_eq!(deeper(Fold::Open), None, "the sheet is not a fold");
    }
}
