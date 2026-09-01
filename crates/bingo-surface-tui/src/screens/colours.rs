//! Where the colour lands (`docs/design/tui.md` §4), on the scenes §3 draws.
//! A snapshot pins the words; these pin where the eye is sent — a token is
//! spent on the one cell that earns it, and nowhere else.

use super::*;

#[test]
fn an_answer_row_is_white_after_its_bullet() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    let painted = painted(80, 24, &tree, &ui, now);
    assert_row_styled(
        &painted,
        "All 33 pass.",
        &[
            ("⏺ ", crate::theme::text().patch(crate::theme::bold())),
            ("All 33 pass.", crate::theme::text()),
        ],
    );
}

/// What a person reads off a batch: not one live row above a waiting one, but
/// two bullets in `presence` in the same frame, breathing side by side.
#[test]
fn every_row_of_a_batch_wears_the_live_mark_at_once() {
    let (ui, now) = mid_turn();
    let painted = painted(80, 24, &solo(&folded(running_together())), &ui, now);
    for command in ["cargo fmt --all -- --check", "cargo test --workspace"] {
        assert_row_styled(
            &painted,
            command,
            &[
                ("⏺ ", crate::theme::presence()),
                ("Bash", crate::theme::bold()),
                (&format!("({command})"), crate::theme::text()),
            ],
        );
    }
}

#[test]
fn a_tool_rows_bullet_is_the_only_cell_a_colour_is_spent_on() {
    let output = ToolOutput::text("Read 3 lines");
    let state = folded(vec![item(
        1,
        tool(
            "itm_1",
            "Read",
            json!({"file_path": "src/lib.rs"}),
            Some(output),
            ItemStatus::Completed,
        ),
    )]);
    let (ui, now) = scene();
    let painted = painted(80, 24, &solo(&state), &ui, now);
    assert_eq!(painted.coloured("Read(src/lib.rs)"), vec!["⏺".to_string()]);
    assert!(
        painted.coloured("Read 3 lines").is_empty(),
        "a result spends none at all"
    );
}

#[test]
fn a_card_spends_its_colour_on_the_row_the_keyboard_is_on() {
    let (tree, ui, now) = asked(permission(Some("Edit(src/)"), None));
    let painted = painted(80, 24, &tree, &ui, now);
    // The cursor, and the card's own border on either side of the row (§4:
    // a card has the only bright border on the screen).
    assert_eq!(
        painted.coloured("1. Yes"),
        vec!["│".to_string(), "❯".to_string(), "│".to_string()]
    );
    assert_eq!(
        painted.coloured("2. Yes, allow"),
        vec!["│".to_string(), "│".to_string()],
        "the rows it is not on carry the border and nothing else"
    );
    // Inside the border the question runs to the box's edge, in `text`.
    let question = format!(" {:<77}", "Do you want to edit src/lib.rs?");
    assert_row_styled(
        &painted,
        "Do you want to",
        &[
            ("│", crate::theme::presence()),
            (question.as_str(), crate::theme::text()),
            ("│", crate::theme::presence()),
        ],
    );
}

/// The quiet rule where the eye reads it: a notice wears the tool row's own
/// mark and spends no other colour, and a surface the closed set does not name
/// keeps the band a person's words are on.
#[test]
fn a_notice_is_marked_and_everybody_else_keeps_the_bar() {
    let (ui, now) = scene();
    crate::theme::with(truecolor(), || {
        let painted = painted(120, 40, &solo(&folded(reported())), &ui, now);
        assert_row_styled(
            &painted,
            "Background job",
            &[
                ("⏺ ", crate::theme::good()),
                (JOB.lines().next().unwrap_or_default(), crate::theme::text()),
            ],
        );
        assert!(
            painted.coloured("BashOutput").is_empty(),
            "what hangs under it is a result and spends nothing"
        );
        let bar = painted.row("look at the deploy");
        let width: usize = bar.iter().map(|(text, _)| text.width()).sum();
        assert_eq!(width, 120, "a channel is somebody: {bar:#?}");
        let ground = crate::theme::as_drawn(crate::theme::raised()).bg;
        assert!(
            bar.iter().all(|(_, style)| style.bg == ground),
            "and somebody's line is a band: {bar:#?}"
        );
    });
}

/// The one place `raised` is spent, and the only rule 24 bits can carry that
/// the eight cannot: what you said is a band, not a sentence.
#[test]
fn your_own_line_sits_on_a_bar_the_width_of_the_transcript() {
    let (ui, now) = scene();
    let tree = solo(&folded(answered()));
    crate::theme::with(truecolor(), || {
        let row = painted(80, 24, &tree, &ui, now).row("run the tests");
        let width: usize = row.iter().map(|(text, _)| text.width()).sum();
        assert_eq!(width, 80, "the bar runs to the edge: {row:#?}");
        let ground = crate::theme::as_drawn(crate::theme::raised()).bg;
        assert!(
            row.iter().all(|(_, style)| style.bg == ground),
            "every cell of it is on the raised ground: {row:#?}"
        );
    });
}

#[test]
fn the_status_line_spends_no_colour_but_the_mode() {
    let state = with_permission_mode("acceptEdits");
    let (ui, now) = scene();
    let painted = painted(80, 24, &solo(&state), &ui, now);
    assert_eq!(
        painted.coloured("? for shortcuts"),
        vec!["⏵⏵ acceptEdits".to_string()],
        "the mode is the one thing the line is allowed to colour"
    );
}
