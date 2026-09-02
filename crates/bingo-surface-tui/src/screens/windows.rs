//! What a list a cursor walks draws when it has more rows than room (§3): the
//! run around the row the keyboard is on, and a `…` at each end it cut. One
//! scene per picker with the cursor on the last row, and one with it in the
//! middle — the mark is on the screen in every one of them, which is the whole
//! of what the bug was about.

use super::*;

use crate::rewind::Rewind;
use crate::ui::Picker;

/// One picker at 80×24, the size every card must be readable at (§7).
fn shot(name: &str, tree: &Tree, ui: &Ui, now: Now) -> String {
    let screen = draw_tree(80, 24, tree, ui, now);
    insta::assert_snapshot!(name, screen.clone());
    screen
}

/// The one row the keyboard is on, as it was drawn.
fn marked(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().filter(|row| row.contains('❯')).collect();
    assert_eq!(rows.len(), 1, "one cursor on the screen: {rows:?}");
    rows[0].trim().to_string()
}

/// How many ends of the list said they were cut short. A drawn row comes
/// quoted, as the backend prints it.
fn cuts(screen: &str) -> usize {
    screen
        .lines()
        .filter(|row| row.replace('"', "").trim() == "…")
        .count()
}

/// Twenty sessions the root spawned in an earlier process, which is more rows
/// than the dropdown has.
fn crowded_switcher(selected: usize) -> (Tree, Ui, Now) {
    let tree = spawned_tree(busy_child("reviewer"));
    let (mut ui, now) = scene();
    let stored = (20..40)
        .map(|i| stored_summary(&format!("ses_{i}"), &format!("scout {i}")))
        .collect();
    shown(&mut ui, Open::Switcher(Switcher { selected, stored }), now);
    (tree, ui, now)
}

#[test]
fn the_switcher_scrolls_under_the_row_the_keyboard_is_on() {
    let (tree, ui, now) = crowded_switcher(21);
    let screen = shot("switcher_end", &tree, &ui, now);
    assert!(marked(&screen).contains("scout 39"), "{screen}");
    assert_eq!(
        cuts(&screen),
        1,
        "the list goes on above it and nowhere else"
    );

    let (tree, ui, now) = crowded_switcher(11);
    let screen = shot("switcher_middle", &tree, &ui, now);
    assert!(marked(&screen).contains("scout 29"), "{screen}");
    assert_eq!(cuts(&screen), 2, "and here it goes on at both ends");
}

fn spec(name: &str) -> bingo_sdk::CommandSpec {
    bingo_sdk::CommandSpec {
        name: name.into(),
        aliases: vec![],
        hint: "what it does".into(),
        args: bingo_sdk::ArgSpec::None,
        instant: true,
        family: "kernel".into(),
    }
}

/// A `/` with more commands behind it than the dropdown's eight rows.
fn crowded_menu(from_the_end: usize) -> (bingo_sdk::SessionState, Ui, Now) {
    let state = folded(answered());
    let (mut ui, now) = scene();
    ui.catalogs.commands = (1..=12).map(|i| spec(&format!("plugin-{i:02}"))).collect();
    write(&mut ui, &state, "/", now);
    let rows = ui.suggestions(&state.summary.cwd).len();
    ui.menu.selected = rows - 1 - from_the_end;
    (state, ui, now)
}

#[test]
fn the_command_dropdown_scrolls_under_the_row_the_keyboard_is_on() {
    let (state, ui, now) = crowded_menu(0);
    let screen = shot("dropdown_end", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("/plugin-12"), "{screen}");
    assert_eq!(cuts(&screen), 1);

    let (state, ui, now) = crowded_menu(6);
    let screen = shot("dropdown_middle", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("/plugin-06"), "{screen}");
    assert_eq!(cuts(&screen), 2);
}

/// Two dozen sessions to resume: more than the sheet has rows for.
fn crowded_picker(selected: usize) -> (bingo_sdk::SessionState, Ui, Now) {
    let (mut ui, now) = scene();
    let sessions = (10..34)
        .map(|i| stored_summary(&format!("ses_{i}"), &format!("session {i}")))
        .collect();
    shown(&mut ui, Open::Picker(Picker { sessions, selected }), now);
    (state(), ui, now)
}

#[test]
fn the_resume_picker_scrolls_under_the_row_the_keyboard_is_on() {
    let (state, ui, now) = crowded_picker(23);
    let screen = shot("resume_end", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("session 33"), "{screen}");
    assert_eq!(cuts(&screen), 1);

    let (state, ui, now) = crowded_picker(12);
    let screen = shot("resume_middle", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("session 22"), "{screen}");
    assert_eq!(cuts(&screen), 2);
}

/// A transcript of fourteen turns, so the rewind card has more of them to list
/// than its eight rows.
fn crowded_rewind(selected: usize) -> (bingo_sdk::SessionState, Ui, Now) {
    let mut state = state();
    state.items = (1..=14)
        .map(|i| {
            let mut asked = user(&format!("itm_{i}"), &format!("turn {i:02}: do the thing"));
            asked.turn = Some(TurnId::from_raw(format!("trn_{i}")));
            asked
        })
        .collect();
    let (mut ui, now) = scene();
    shown(&mut ui, Open::Rewind(Rewind { selected }), now);
    (state, ui, now)
}

#[test]
fn the_rewind_card_scrolls_under_the_row_the_keyboard_is_on() {
    let (state, ui, now) = crowded_rewind(13);
    let screen = shot("rewind_end", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("turn 01"), "{screen}");
    assert_eq!(cuts(&screen), 1);
    assert!(screen.contains("Rewind to"), "the title stays: {screen}");

    let (state, ui, now) = crowded_rewind(6);
    let screen = shot("rewind_middle", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("turn 08"), "{screen}");
    assert_eq!(cuts(&screen), 2);
}

/// Four plugins with a board each: more rows than the sheet can hold.
fn crowded_panel(cursor: usize) -> (bingo_sdk::SessionState, Ui, Now) {
    let frames = (1..=4)
        .map(|i| {
            frame(
                i,
                extended(
                    &format!("bingo.plugin-{i}"),
                    "board",
                    json!([
                        {"id": 1, "status": "pending", "subject": "write the plan"},
                        {"id": 2, "status": "in_progress", "subject": "ship it"},
                        {"id": 3, "status": "done", "subject": "read the diff"},
                    ]),
                ),
            )
        })
        .collect();
    let (mut ui, now) = scene();
    ui.panel = cursor;
    shown(&mut ui, Open::Panel, now);
    (folded(frames), ui, now)
}

/// The panel's cursor walks headings, and each heading carries its view under
/// it — so the sheet starts at the one it is on rather than around it.
#[test]
fn the_panel_sheet_starts_at_the_row_the_keyboard_is_on() {
    let (state, ui, now) = crowded_panel(3);
    let screen = shot("panel_end", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("bingo.plugin-4"), "{screen}");
    assert_eq!(cuts(&screen), 1);

    let (state, ui, now) = crowded_panel(1);
    let screen = shot("panel_middle", &solo(&state), &ui, now);
    assert!(marked(&screen).contains("bingo.plugin-2"), "{screen}");
    assert_eq!(cuts(&screen), 2);
}
