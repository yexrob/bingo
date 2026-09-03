//! How much of a block is on the screen.
//!
//! One fact per item, and one place that answers it. A block a person has not
//! touched is not in the map at all: its kind says where it starts, so the
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
pub fn fold_of(folds: &Folds, item: &Item) -> Fold {
    folds.get(&item.id).copied().unwrap_or_else(|| start(item))
}

/// Where a kind starts, and where its cycle comes back to.
///
/// A thought that is over starts **shut**: it is read past, not read, and the
/// five rows M34-B left under it were five rows of somebody else's working
/// spent on every turn. A thought still being had starts at its peek, because
/// the two rows moving under `✻ Thinking…` are the whole of what says it is
/// thinking. Everything else starts where it always did — a result, a notice
/// and an action keep the cut they have always kept.
fn start(item: &Item) -> Fold {
    match &item.body {
        ItemBody::Reasoning { .. } if thinking_is_over(item) => Fold::Shut,
        _ => Fold::Peek,
    }
}

/// A thought that has been had — and not one of an ACP agent's own calls,
/// which is a reasoning item too (ADR-0035 §4) and draws as a tool row. What
/// came back from a call is read, so it peeks the way every other result does.
fn thinking_is_over(item: &Item) -> bool {
    item.completed_at.is_some() && !crate::acp::is_call(item)
}

/// A click is one gesture on one row (§7): it advances the fold one step, and
/// from open comes back to where the kind starts. That is the whole of why a
/// thought that is over has three states and everything else has two — the
/// start is the only thing they differ by, and it is written once, in
/// [`start`].
pub fn cycled(item: &Item, fold: Fold) -> Fold {
    match fold {
        Fold::Shut => Fold::Peek,
        Fold::Peek => Fold::Open,
        Fold::Open => start(item),
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
    fn a_thought_that_is_over_starts_shut_and_everything_else_peeks() {
        assert_eq!(fold_of(&Folds::new(), &reasoning(Some(2))), Fold::Shut);
        assert_eq!(fold_of(&Folds::new(), &reasoning(None)), Fold::Peek);
        assert_eq!(fold_of(&Folds::new(), &a_result()), Fold::Peek);
    }

    /// Three states on a thought that is over, two on everything else — from
    /// the one rule that the cycle comes back to where the kind starts.
    #[test]
    fn a_click_cycles_three_states_on_a_thought_and_two_on_a_result() {
        let thought = reasoning(Some(2));
        let mut fold = fold_of(&Folds::new(), &thought);
        let walk: Vec<Fold> = (0..3)
            .map(|_| {
                fold = cycled(&thought, fold);
                fold
            })
            .collect();
        assert_eq!(walk, vec![Fold::Peek, Fold::Open, Fold::Shut]);

        let result = a_result();
        let mut fold = fold_of(&Folds::new(), &result);
        let walk: Vec<Fold> = (0..2)
            .map(|_| {
                fold = cycled(&result, fold);
                fold
            })
            .collect();
        assert_eq!(walk, vec![Fold::Open, Fold::Peek], "no shut to fall into");
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
