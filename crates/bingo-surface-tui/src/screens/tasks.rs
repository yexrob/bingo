//! The screens the task list is read through (M74): Claude Code's own shape,
//! read off its screen and drawn in bingo's grammar.
//!
//! Four things are being decided here and nowhere else: that the list hangs
//! under the activity row while a turn runs and stands under a summary in
//! that row's place between turns; that the task being done lends the row
//! its verb; that a long list is five rows and a count; and that the calls
//! which moved the list draw no row of their own.

use super::*;

fn listed(seq: u64, records: serde_json::Value) -> bingo_sdk::Frame {
    frame(seq, extended("bingo.tasks", "tasks", records))
}

/// One done, one being done, one to do.
fn three() -> serde_json::Value {
    json!([
        {"id": 1, "status": "completed", "subject": "Write the plan", "activeForm": "Writing the plan"},
        {"id": 2, "status": "in_progress", "subject": "Ship it", "activeForm": "Shipping it"},
        {"id": 3, "status": "pending", "subject": "Celebrate"},
    ])
}

/// Twelve, as Claude Code was given them: the first done, the second under
/// way, ten to do.
fn twelve() -> serde_json::Value {
    let mut records = vec![
        json!({"id": 1, "status": "completed", "subject": "Write the plan"}),
        json!({"id": 2, "status": "in_progress", "subject": "Ship it", "activeForm": "Shipping it"}),
    ];
    let names = [
        "Celebrate",
        "Alpha",
        "Bravo",
        "Charlie",
        "Delta",
        "Echo",
        "Foxtrot",
        "Golf",
        "Hotel",
        "India",
    ];
    records.extend(
        names
            .iter()
            .enumerate()
            .map(|(at, name)| json!({"id": at + 3, "status": "pending", "subject": name})),
    );
    serde_json::Value::Array(records)
}

/// The two calls that moved the list, already come back — and drawing no row.
fn moved(seq: u64) -> Vec<bingo_sdk::Frame> {
    vec![
        item(
            seq,
            tool(
                "itm_2",
                "TaskCreate",
                json!({"subject": "Write the plan", "activeForm": "Writing the plan"}),
                Some(ToolOutput::text("Created #1: Write the plan")),
                ItemStatus::Completed,
            ),
        ),
        item(
            seq + 1,
            tool(
                "itm_3",
                "TaskUpdate",
                json!({"id": 2, "status": "in_progress"}),
                Some(ToolOutput::text("Updated #2 (in_progress): Ship it")),
                ItemStatus::Completed,
            ),
        ),
    ]
}

/// A turn at work on the second of three tasks: a call still running, the
/// list under the row that says what the turn is doing.
fn at_work(records: serde_json::Value) -> bingo_sdk::SessionState {
    let mut frames = vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "plan it, ship it, and celebrate")),
    ];
    frames.extend(moved(3));
    frames.push(listed(5, records));
    frames.push(frame(
        6,
        Event::ItemStarted {
            item: running_tool("itm_4", "Bash", "   Compiling bingo v0.1.0\n"),
        },
    ));
    folded(frames)
}

/// The same session between turns: the model has spoken, and the list stands
/// under its summary where the verb row was.
fn between_turns(records: serde_json::Value) -> bingo_sdk::SessionState {
    let mut frames = vec![item(1, user("itm_1", "plan it, ship it, and celebrate"))];
    frames.extend(moved(2));
    frames.push(listed(4, records));
    frames.push(item(
        5,
        assistant(
            "itm_4",
            "The plan is written; shipping next.",
            ItemStatus::Completed,
        ),
    ));
    folded(frames)
}

/// `✻ Shipping it…` over the three rows hung from a `⎿`; the two calls that
/// moved the list are nowhere in the transcript.
#[test]
fn tasks_at_work() {
    let (ui, now) = mid_turn();
    let state = at_work(three());
    let screen = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(
        screen.contains("✻ Shipping it… (esc to interrupt"),
        "{screen}"
    );
    assert!(screen.contains("⎿  ✔ Write the plan"), "{screen}");
    assert!(!screen.contains("TaskCreate"), "{screen}");
    both("tasks_at_work", &solo(&state), &ui, now);
}

/// `3 tasks (1 done, 1 in progress, 1 open)` where the verb row stood, the
/// rows standing under it with no mark to hang from.
#[test]
fn tasks_between_turns() {
    let (ui, now) = scene();
    let state = between_turns(three());
    let screen = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(
        screen.contains("3 tasks (1 done, 1 in progress, 1 open)"),
        "{screen}"
    );
    assert!(screen.contains("  ◼ Ship it"), "{screen}");
    both("tasks_between_turns", &solo(&state), &ui, now);
}

/// Twelve tasks are five rows of what is open and a count of the rest —
/// Claude Code's own cut, read off its screen.
#[test]
fn tasks_cut_to_five() {
    let (ui, now) = scene();
    let state = between_turns(twelve());
    let screen = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(
        screen.contains("12 tasks (1 done, 1 in progress, 10 open)"),
        "{screen}"
    );
    assert!(screen.contains("… +6 pending, 1 completed"), "{screen}");
    assert!(
        !screen.contains("Write the plan"),
        "the done one is counted, not drawn: {screen}"
    );
    both("tasks_cut_to_five", &solo(&state), &ui, now);
}

/// `ctrl+t` takes the rows and the summary off the band; the verb row keeps
/// the task being done, because that is what the turn is doing.
#[test]
fn tasks_hidden() {
    let (mut ui, now) = mid_turn();
    ui.tasks_hidden = true;
    let state = at_work(three());
    let screen = draw_tree(80, 24, &solo(&state), &ui, now);
    assert!(screen.contains("✻ Shipping it…"), "{screen}");
    assert!(!screen.contains("Write the plan"), "{screen}");
    insta::assert_snapshot!("tasks_hidden_80x24", screen);
}

/// The look a terminal that can draw no glyphs gets (§7): a dash, the bullet
/// and a cross, out of the six characters the ASCII table has.
#[test]
fn tasks_in_ascii() {
    let (ui, now) = scene();
    without_glyphs(
        "tasks_between_turns_ascii",
        &solo(&between_turns(three())),
        &ui,
        now,
    );
}
