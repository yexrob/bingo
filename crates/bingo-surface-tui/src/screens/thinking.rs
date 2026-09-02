//! The screens a thought is read through (§4's thinking row, §6's): being had,
//! over, opened, and the two it says nothing on — a thought the provider
//! summarised nothing of, and one so long the row folds it.

use super::*;

/// A thought that has finished, with what it thought and how long it took.
fn thought(id: &str, text: &str, seconds: i64) -> bingo_sdk::Item {
    let mut thought = crate::test_support::item(
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

/// The same thought as it is being had: still running, and only as much text
/// as the deltas have carried.
fn being_thought(id: &str, text: &str) -> bingo_sdk::Item {
    crate::test_support::item(
        id,
        ItemStatus::Running,
        ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: Default::default(),
        },
    )
}

#[test]
fn thinking_and_its_decay() {
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", "The manifest first.", 2)),
        item(
            3,
            assistant("itm_3", "One package, demo 0.1.0.", ItemStatus::Completed),
        ),
    ]);
    let (ui, now) = scene();
    both("thinking", &solo(&state), &ui, now);
}

/// A thought streams as it is thought (2026-09-02, user-directed): `✻
/// Thinking…` over the last three rows of what has arrived so far, dim under
/// the `⎿` a running tool's tail hangs from — and no comet on them, because
/// the glow is for words being said and thinking is where `dim` lives (§4).
#[test]
fn a_thought_being_had_streams_under_its_own_row() {
    let text = "The manifest first, because the lockfile only says what the manifest \
                already asked for.\n\
                Then the crate map, which is the one place the layering is written down.\n\
                The plan after that: it says which of the two is allowed to move.\n\
                Only then the code, and only the";
    let state = folded(vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "what is in this workspace?")),
        frame(
            3,
            Event::ItemStarted {
                item: being_thought("itm_2", text),
            },
        ),
    ]);
    let (ui, now) = mid_turn();
    both("reasoning_streaming", &solo(&state), &ui, now);
}

/// A thought is readable where it happened (M34): the row says how long it
/// took, what was thought hangs under it in dim, and the rest folds away
/// behind the key a result folds behind.
#[test]
fn a_long_thought_is_read_under_its_own_row() {
    let text = "The manifest first, because the lockfile only says what the manifest \
                already asked for.\n\
                Then the crate map, which is the one place the layering is written down.\n\
                The plan after that: it says which of the two is allowed to move.\n\
                Only then the code, and only the file the plan names.\n\
                Anything else is a second reading of the same three facts.\n\
                So: manifest, map, plan.";
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", text, 4)),
    ]);
    let (ui, now) = scene();
    both("reasoning_inline", &solo(&state), &ui, now);
}

/// Anthropic's redacted thinking, and an OpenAI turn the provider summarised
/// nothing of: the row alone. Nothing folds, so nothing is promised.
#[test]
fn an_empty_thought_draws_the_row_alone() {
    let state = folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", "", 2)),
    ]);
    let (ui, now) = scene();
    both("reasoning_redacted", &solo(&state), &ui, now);
}

/// `⏎` on a `✻ Thought for 2s` row: what was thought, whole.
#[test]
fn a_thought_opens_in_a_sheet() {
    let state = folded(vec![item(
        1,
        thought(
            "itm_1",
            "The manifest first, because the lockfile only says what the\n\
             manifest already asked for.\n\n\
             Then the crate map, which is the one place the layering is\n\
             written down.",
            2,
        ),
    )]);
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Pager(crate::pager::Pager::open(bingo_sdk::ItemId::from_raw(
            "itm_1",
        ))),
        now,
    );
    both("reasoning_sheet", &solo(&state), &ui, now);
}
