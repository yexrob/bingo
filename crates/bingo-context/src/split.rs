//! Where a compaction cuts.

use bingo_sdk::{Item, ItemBody, TurnId};

use crate::{estimate, tail};

/// Items kept verbatim however cheap they are. A tool round spends items fast
/// — one call each — so a smaller tail could be four tool calls and nothing of
/// the exchange that motivated them.
const KEEP_RECENT: usize = 12;

/// The boundary: `items[..split]` is what the summary replaces, and
/// `items[split]` is the item the kernel is told the cut stands before.
///
/// The later of the two floors wins — the count keeps the conversation
/// readable, the budget keeps one fat tool result from riding through every
/// compaction untouched — and the answer then moves off any seam that would
/// leave half a tool round behind.
pub fn split(items: &[Item], keep_budget: u64) -> usize {
    let Some(last) = items.len().checked_sub(1) else {
        return 0;
    };
    let floor = by_count(items).max(by_budget(items, keep_budget)).min(last);
    match forward(items, floor) {
        Some(at) => at,
        // Every seam from here on is inside a tool round, so the cut goes back
        // instead: compacting more than asked still shrinks, and the kernel
        // discards a summary that does not.
        None => backward(items, floor),
    }
}

fn by_count(items: &[Item]) -> usize {
    items.len().saturating_sub(KEEP_RECENT)
}

fn by_budget(items: &[Item], budget: u64) -> usize {
    tail::first_within(items, budget, estimate::item)
}

fn forward(items: &[Item], from: usize) -> Option<usize> {
    (from..items.len()).find(|&at| !splits_a_round(items, at))
}

fn backward(items: &[Item], from: usize) -> usize {
    (0..=from)
        .rev()
        .find(|&at| !splits_a_round(items, at))
        .unwrap_or(0)
}

/// A cut here would summarise away part of a round and keep the rest: the fold
/// puts one round's calls in a single assistant message and their results in
/// the user message after it, so a call whose siblings are gone reaches the
/// model as an answer to a question it can no longer see.
fn splits_a_round(items: &[Item], at: usize) -> bool {
    let round = round_of(&items[at]);
    items[..at]
        .iter()
        .any(|i| is_call(i) && round_of(i) == round)
}

fn round_of(item: &Item) -> (Option<&TurnId>, u32) {
    (item.turn.as_ref(), item.round)
}

fn is_call(item: &Item) -> bool {
    matches!(item.body, ItemBody::ToolCall { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{assistant, in_round, tool, user};
    use proptest::prelude::*;

    const GENEROUS: u64 = 1_000_000;

    fn talk(n: usize) -> Vec<Item> {
        (0..n).map(|i| user(&format!("u{i}"), "hello")).collect()
    }

    #[test]
    fn a_short_journal_has_nothing_to_cut() {
        assert_eq!(split(&[], GENEROUS), 0);
        assert_eq!(split(&talk(5), GENEROUS), 0);
    }

    #[test]
    fn a_generous_budget_still_keeps_only_the_last_twelve() {
        assert_eq!(split(&talk(20), GENEROUS), 8);
    }

    #[test]
    fn a_heavy_tail_cuts_further_than_the_count_would() {
        let mut items = talk(20);
        items[15] = user("fat", &"x".repeat(40_000));
        assert!(split(&items, 1_000) > 8);
    }

    #[test]
    fn the_cut_moves_off_a_tool_round_instead_of_splitting_it() {
        let mut items = talk(3);
        for i in 0..15 {
            items.push(in_round(
                tool(&format!("t{i}"), "Read", r#"{"path":"/a"}"#, Some("ok")),
                7,
            ));
        }
        for i in 0..5 {
            items.push(in_round(assistant(&format!("a{i}"), "read them"), 8));
        }
        // The count alone would cut at 11, in the middle of the round.
        assert_eq!(by_count(&items), 11);
        assert!(splits_a_round(&items, 11));
        assert_eq!(split(&items, GENEROUS), 18);
    }

    #[test]
    fn a_journal_that_is_one_long_round_cuts_before_it() {
        let mut items = talk(2);
        for i in 0..20 {
            items.push(in_round(
                tool(&format!("t{i}"), "Read", r#"{"path":"/a"}"#, Some("ok")),
                3,
            ));
        }
        assert_eq!(split(&items, GENEROUS), 2);
    }

    /// The shapes a journal is built from, one item each.
    #[derive(Clone, Debug)]
    enum Shape {
        Talk,
        Answer,
        Call,
    }

    fn any_shape() -> impl Strategy<Value = (Shape, u32)> {
        (
            prop_oneof![Just(Shape::Talk), Just(Shape::Answer), Just(Shape::Call)],
            0u32..6,
        )
    }

    fn journal(shapes: &[(Shape, u32)]) -> Vec<Item> {
        shapes
            .iter()
            .enumerate()
            .map(|(i, (shape, round))| {
                let id = format!("i{i}");
                let item = match shape {
                    Shape::Talk => user(&id, "hello there"),
                    Shape::Answer => assistant(&id, "on it"),
                    Shape::Call => tool(&id, "Read", r#"{"path":"/a"}"#, Some("ok")),
                };
                in_round(item, *round)
            })
            .collect()
    }

    proptest! {
        #[test]
        fn the_boundary_never_shares_a_round_with_a_call_before_it(
            shapes in proptest::collection::vec(any_shape(), 0..40),
            budget in 0u64..5_000,
        ) {
            let items = journal(&shapes);
            let at = split(&items, budget);
            prop_assert!(at <= items.len().saturating_sub(1));
            if !items.is_empty() {
                prop_assert!(!splits_a_round(&items, at));
            }
        }
    }
}
