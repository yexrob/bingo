//! A run of thoughts: what one is, and what it says.
//!
//! A provider that closes a reasoning item and opens the next as the model
//! goes on thinking hands the kernel a row of thoughts with nothing between
//! them — ten of them in the journal M79 was written from, 57 to 149
//! characters each, 13 to 31 seconds each. Each carries its own encrypted
//! state and replays as its own item, so the kernel keeps ten and nothing
//! upstream of a surface merges them (ADR-0002). A person reads them as one
//! thought that took three and a half minutes, and the transcript says so at
//! render time: the run draws as one block, hung on its **last** item.
//!
//! This is the one place that knows what a run is; everything else asks. It is
//! pure and reads the items it is given — what a run *draws* is
//! [`crate::transcript`]'s, and which item a fold, a click or a sheet lands on
//! follows from the one rule that the last of a run is the one with a block.

use bingo_sdk::{Item, ItemBody, ItemId, SessionState};
use jiff::SignedDuration;

/// Whether this item is a thought: a reasoning item that is not one of an ACP
/// agent's own calls, which is a reasoning item too (ADR-0035 §4) and draws as
/// a tool row ([`crate::acp`]).
pub fn is_thought(item: &Item) -> bool {
    matches!(item.body, ItemBody::Reasoning { .. }) && !crate::acp::is_call(item)
}

/// The thoughts adjacent before `at`: the run the item there ends, itself left
/// out.
///
/// Adjacent is adjacent. Anything the model did between two thoughts is
/// something a person read between them, so it breaks the run — a call that
/// draws no row of its own (M74's task calls) breaks it too, because a run is
/// a fact about the transcript's order and not about what each item spends.
pub fn run_before(items: &[Item], at: usize) -> &[Item] {
    let end = at.min(items.len());
    let mut from = end;
    while from > 0 && is_thought(&items[from - 1]) {
        from -= 1;
    }
    &items[from..end]
}

/// Whether the item after `at` is a thought — which is what says the item at
/// `at` is not the last of its run.
pub fn next_is_thought(items: &[Item], at: usize) -> bool {
    items.get(at + 1).is_some_and(is_thought)
}

/// Whether this item's own block is the one on the screen. Every item's is,
/// except a thought a newer one joined: the run draws on its last, so the
/// fold, the click and the sheet are all that one's.
pub fn ends_its_run(state: &SessionState, item: &Item) -> bool {
    if !is_thought(item) {
        return true;
    }
    !at(state, &item.id).is_some_and(|i| next_is_thought(&state.items, i))
}

/// What a run says, as one thought: the texts of its thoughts in order, a
/// blank line between them — each item is a paragraph of the same thinking.
/// The ones that came back empty (redacted, or summarised to nothing) say
/// nothing and add no break.
pub fn text(run: &[&Item]) -> String {
    run.iter()
        .filter_map(|item| crate::transcript::thought(item))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// How long a run took: the sum of what each of its finished thoughts took,
/// and not the span from the first to the last. The gaps between them are the
/// round trips the model made in between; a heading that counted those would
/// be saying how long the turn was rather than how long the thinking was.
pub fn took(run: &[&Item]) -> SignedDuration {
    run.iter()
        .filter_map(|item| Some(item.completed_at?.duration_since(item.started_at)))
        .fold(SignedDuration::ZERO, SignedDuration::saturating_add)
}

/// The whole run an item belongs to, itself among them: what its sheet opens.
/// Empty for anything that is not a thought, and for an id this state does not
/// hold.
pub fn run_of<'a>(state: &'a SessionState, id: &ItemId) -> Vec<&'a Item> {
    let items = &state.items;
    let Some(here) = at(state, id).filter(|i| is_thought(&items[*i])) else {
        return Vec::new();
    };
    let from = here - run_before(items, here).len();
    let mut to = here + 1;
    while to < items.len() && is_thought(&items[to]) {
        to += 1;
    }
    items[from..to].iter().collect()
}

/// Where an item sits in the transcript.
fn at(state: &SessionState, id: &ItemId) -> Option<usize> {
    state.items.iter().position(|item| &item.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{agent_call, assistant, folded, frame, item, ts};
    use bingo_sdk::{Event, ItemStatus};

    /// A thought that is over, with its own id, text and span.
    fn thought(id: &str, text: &str, seconds: i64) -> Item {
        let mut thought = item(
            id,
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text: text.into(),
                provider_metadata: Default::default(),
            },
        );
        thought.completed_at = Some(ts() + jiff::SignedDuration::from_secs(seconds));
        thought
    }

    /// A thought still being had: nothing has closed it.
    fn thinking(id: &str, text: &str) -> Item {
        let mut thought = thought(id, text, 0);
        thought.status = ItemStatus::Running;
        thought.completed_at = None;
        thought
    }

    fn stated(items: Vec<Item>) -> SessionState {
        folded(
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| frame(i as u64 + 1, Event::ItemCompleted { item }))
                .collect(),
        )
    }

    /// One of the agent's own calls: a reasoning item that is not a thought
    /// (ADR-0035 §4), so it breaks a run the way any other row does.
    fn call(id: &str) -> Item {
        agent_call(
            id,
            "read Read src/lib.rs",
            serde_json::json!({
                "external": true, "kind": "read", "status": "completed",
                "title": "Read src/lib.rs",
                "content": [
                    { "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }
                ]
            }),
        )
    }

    #[test]
    fn a_thought_is_a_reasoning_item_that_is_not_a_call() {
        assert!(is_thought(&thought("itm_1", "why", 1)));
        assert!(is_thought(&thinking("itm_1", "why")));
        assert!(!is_thought(&call("itm_1")));
        assert!(!is_thought(&assistant(
            "itm_1",
            "done",
            ItemStatus::Completed
        )));
    }

    #[test]
    fn a_run_is_the_thoughts_adjacent_before_the_one_asked_about() {
        let items = vec![
            thought("itm_1", "first", 1),
            thought("itm_2", "second", 2),
            thought("itm_3", "third", 3),
        ];
        assert!(run_before(&items, 0).is_empty());
        assert_eq!(run_before(&items, 1).len(), 1);
        assert_eq!(run_before(&items, 2).len(), 2);
        assert!(next_is_thought(&items, 0));
        assert!(!next_is_thought(&items, 2), "nothing follows the last");
    }

    /// Anything between two thoughts keeps them apart — an answer, and one of
    /// the agent's own calls, which wears a reasoning item and is not one.
    #[test]
    fn anything_between_two_thoughts_breaks_the_run() {
        for between in [
            assistant("itm_2", "done", ItemStatus::Completed),
            call("itm_2"),
        ] {
            let items = vec![
                thought("itm_1", "first", 1),
                between,
                thought("itm_3", "third", 3),
            ];
            assert!(run_before(&items, 2).is_empty(), "{:?}", items[1].body);
            assert!(!next_is_thought(&items, 0));
        }
    }

    #[test]
    fn a_run_says_its_texts_in_order_with_a_break_between_them() {
        let items = [
            thought("itm_1", "The manifest first.", 1),
            thought("itm_2", "", 1),
            thought("itm_3", "Then the crate map.", 1),
        ];
        let run: Vec<&Item> = items.iter().collect();
        assert_eq!(text(&run), "The manifest first.\n\nThen the crate map.");
        assert_eq!(text(&[]), "");
    }

    #[test]
    fn a_run_took_as_long_as_its_finished_thoughts_together() {
        let items = [
            thought("itm_1", "first", 70),
            thought("itm_2", "second", 131),
            thinking("itm_3", "third"),
        ];
        let run: Vec<&Item> = items.iter().collect();
        assert_eq!(took(&run), SignedDuration::from_secs(201));
        assert_eq!(took(&[]), SignedDuration::ZERO);
    }

    #[test]
    fn the_run_of_an_item_holds_every_thought_adjacent_to_it() {
        let state = stated(vec![
            assistant("itm_0", "starting", ItemStatus::Completed),
            thought("itm_1", "first", 1),
            thought("itm_2", "second", 2),
            thought("itm_3", "third", 3),
            assistant("itm_4", "done", ItemStatus::Completed),
        ]);
        let ids = |id: &str| -> Vec<String> {
            run_of(&state, &ItemId::from_raw(id))
                .iter()
                .map(|item| item.id.to_string())
                .collect()
        };
        assert_eq!(ids("itm_2"), vec!["itm_1", "itm_2", "itm_3"]);
        assert_eq!(ids("itm_1"), ids("itm_3"), "one run, whichever is asked");
        assert!(ids("itm_4").is_empty(), "an answer is in no run");
        assert!(ids("itm_9").is_empty(), "and neither is nothing");
    }

    #[test]
    fn only_the_last_thought_of_a_run_ends_it() {
        let state = stated(vec![
            thought("itm_1", "first", 1),
            thought("itm_2", "second", 2),
            assistant("itm_3", "done", ItemStatus::Completed),
        ]);
        let ended = |id: &str| {
            state
                .items
                .iter()
                .find(|item| item.id == ItemId::from_raw(id))
                .is_some_and(|item| ends_its_run(&state, item))
        };
        assert!(!ended("itm_1"));
        assert!(ended("itm_2"));
        assert!(
            ended("itm_3"),
            "an answer is the last of its own run of one"
        );
    }
}
