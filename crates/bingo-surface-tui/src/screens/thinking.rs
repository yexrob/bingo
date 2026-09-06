//! The screens a thought is read through (§4's thinking row, §6's): being had,
//! over, and each of the three states a click walks it through — plus the one
//! it says nothing on, a thought the provider summarised nothing of.

use super::*;

use crate::fold::Fold;

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

/// A thought in the flow of a turn: asked, thought, answered. Short enough to
/// hold one row, and it holds it — the answer under it sits where it sat while
/// the thinking was still going on.
#[test]
fn a_thought_between_a_question_and_its_answer() {
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
/// Thinking…` over the newest **two** rows of what has arrived so far, dim
/// under the `⎿` a running tool's tail hangs from — and no comet on them,
/// because the glow is for words being said and thinking is where `dim` lives
/// (§4).
#[test]
fn a_thought_being_had_streams_under_its_own_row() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "what is in this workspace?")),
        frame(
            3,
            Event::ItemStarted {
                item: being_thought("itm_2", AS_FAR_AS_IT_GOT),
            },
        ),
    ]);
    let (ui, now) = mid_turn();
    both("reasoning_streaming", &solo(&state), &ui, now);
}

/// A thought as the deltas have left it, cut off mid-sentence where the model
/// happened to be. The two scenes above and below share it, so the one thing
/// their snapshots may differ by is what the close is allowed to change.
const AS_FAR_AS_IT_GOT: &str = "The manifest first, because the lockfile only says what \
                                the manifest already asked for.\n\
                                Then the crate map, which is the one place the layering \
                                is written down.\n\
                                The plan after that: it says which of the two is allowed \
                                to move.\n\
                                Only then the code, and only the";

/// The same thought one frame later (2026-09-06, user-directed: 默认思考完不要
/// 闭合吧 就保持住 不然还是会在闭合的时候布局变化). The thinking is over, the
/// heading says how long it took, and the two rows under it are the two rows
/// it was already wearing — so the block keeps its height and the conversation
/// above it does not move. Read against `reasoning_streaming_*`: the heading
/// is the only row that differs.
#[test]
fn a_finished_thought_holds_the_rows_it_ended_on() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "what is in this workspace?")),
        item(3, thought("itm_2", AS_FAR_AS_IT_GOT, 1)),
    ]);
    let (ui, now) = mid_turn();
    both("reasoning_held", &solo(&state), &ui, now);
}

/// The same thought written as models write them: one paragraph, no newline
/// in it at all (2026-09-06, user-reported — "看得晃眼睛"). The cut counts the
/// rows the paragraph wraps to, so the block is the row and its two at eighty
/// columns and at a hundred and twenty alike, and the two rows hold the end of
/// what has arrived.
#[test]
fn a_thought_of_one_paragraph_is_still_two_rows() {
    let text = "The manifest first, because the lockfile only says what the manifest \
                already asked for, and then the crate map, which is the one place the \
                whole of the layering is written down, and only after both of them the \
                plan, which says which of the two is allowed to";
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
    both("reasoning_paragraph", &solo(&state), &ui, now);
}

/// A question, and a thought long enough to have something to keep back.
fn a_thought_worth_reading() -> bingo_sdk::SessionState {
    let text = "The manifest first, because the lockfile only says what the manifest \
                already asked for.\n\
                Then the crate map, which is the one place the layering is written down.\n\
                The plan after that: it says which of the two is allowed to move.\n\
                Only then the code, and only the file the plan names.\n\
                Anything else is a second reading of the same three facts.\n\
                So: manifest, map, plan.";
    folded(vec![
        item(1, user("itm_1", "what is in this workspace?")),
        item(2, thought("itm_2", text, 4)),
    ])
}

/// The thought's own item, which the two states below are set on.
fn the_thought() -> bingo_sdk::ItemId {
    bingo_sdk::ItemId::from_raw("itm_2")
}

/// One click past the whole of it, which is where the ring reaches the shut
/// (§7): the row alone. A thought is the one block worth putting away — it is
/// working, not an answer — and putting it away is asked for rather than met
/// on arrival, because arriving at it is what moved the layout.
#[test]
fn a_finished_thought_shuts_where_the_ring_comes_round() {
    let (mut ui, now) = scene();
    ui.folds.insert(the_thought(), Fold::Shut);
    both(
        "reasoning_shut",
        &solo(&a_thought_worth_reading()),
        &ui,
        now,
    );
}

/// Once more: the whole of it where it happened, still dim and still under the
/// `⎿`, because it is still working and not an answer.
#[test]
fn a_finished_thought_opens_whole_where_it_sits() {
    let (mut ui, now) = scene();
    ui.folds.insert(the_thought(), Fold::Open);
    both(
        "reasoning_open",
        &solo(&a_thought_worth_reading()),
        &ui,
        now,
    );
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
