//! The screens a set of questions is read through: the band of §2, the tab row
//! with a box per question and the tab that sends, the options, the mockup in
//! its own frame beside them or above them, and the keys the card says.
//!
//! Its own module because the form is its own noun, and because these are the
//! screens M57 was compared against Claude Code's own row for row.

use super::*;

/// Three questions in one card (M53): the tab row names all of them and the tab
/// that sends, the first is the one on screen, and the option under the cursor
/// shows what picking it would mean — beside the options at 120 columns, above
/// them at 80, so what gives way on a short screen is never an answer (§2).
#[test]
fn form_asked() {
    let (tree, ui, now) = asked(crate::test_support::form());
    both("form_asked", &tree, &ui, now);
}

/// Two settled and the third on screen: a tab that has been answered wears
/// the mark of one, and the set is ticked with `space` before it is fixed.
#[test]
fn form_part_answered() {
    let (tree, mut ui, now) = asked(crate::test_support::form());
    for key in [key(KeyCode::Enter), key(KeyCode::Enter), typed(' ')] {
        crate::input::on_key(&mut ui, &tree, key, now);
    }
    both("form_part_answered", &tree, &ui, now);
}

/// The same card where nothing but the characters of §7 may be drawn. The tab
/// row's arrows and the key line's are the card's own words, as the `·` and the
/// help sheet's `↑↓` already are, so they stand in either look.
#[test]
fn form_in_ascii() {
    let (tree, ui, now) = asked(crate::test_support::form());
    without_glyphs("form_in_ascii", &tree, &ui, now);
}

/// The tab that sends, reached with a question still open (M57): it says how
/// many are left, and `⏎` there walks to the first of them rather than sending.
#[test]
fn form_submit_with_one_left() {
    let (tree, mut ui, now) = asked(crate::test_support::form());
    // Two questions fixed, then one more step: the walk carries on to `Submit`.
    for key in [key(KeyCode::Enter), key(KeyCode::Enter), key(KeyCode::Tab)] {
        crate::input::on_key(&mut ui, &tree, key, now);
    }
    both("form_submit_with_one_left", &tree, &ui, now);
}

/// The layout question of M57's capture: three options, compact because the
/// question carries mockups, and the mockup of the one under the cursor inside
/// its own dim frame — beside the options at 120 columns and above them at 80.
/// The frame is what keeps a mockup drawn out of box characters from reading as
/// the card's own edge.
#[test]
fn form_layout() {
    let (tree, ui, now) = asked(layout());
    both("form_layout", &tree, &ui, now);
}

/// That question, built off the fixture every other form screen uses so there
/// is one form in the catalogues and not two.
fn layout() -> bingo_sdk::Interaction {
    let mut open = crate::test_support::form();
    if let bingo_sdk::InteractionKind::Form { questions, .. } = &mut open.kind {
        questions[0].header = Some("Layout".into());
        questions[0].question = "Where should the sidebar sit?".into();
        questions[0].options = [
            ("Sidebar left", "Navigation beside the content", LEFT),
            ("Sidebar right", "Navigation on the far side", RIGHT),
            ("No sidebar", "One column, the full width", PLAIN),
        ]
        .into_iter()
        .enumerate()
        .map(
            |(index, (label, description, mockup))| bingo_sdk::QuestionOption {
                id: index.to_string(),
                label: label.into(),
                description: Some(description.into()),
                role: None,
                preview: Some(mockup.into()),
            },
        )
        .collect();
    }
    open
}

const LEFT: &str = "┌──────┬─────────────┐\n│ nav  │ content     │\n│      │             │\n└──────┴─────────────┘";
const RIGHT: &str = "┌─────────────┬──────┐\n│ content     │ nav  │\n│             │      │\n└─────────────┴──────┘";
const PLAIN: &str = "┌────────────────────┐\n│ content            │\n│                    │\n└────────────────────┘";

/// A set answered both ways at once (M59): two boxes ticked and the person's
/// own words in the row under them. Before M59 the card sent the words alone
/// and the ticks were lost; now the model reads one answer with both halves.
#[test]
fn form_set_ticked_and_typed_on() {
    let (tree, mut ui, now) = asked(crate::test_support::form());
    // To the set, tick two of its three, then open the words row and write.
    let walk = [
        key(KeyCode::Tab),
        key(KeyCode::Tab),
        typed(' '),
        key(KeyCode::Down),
        typed(' '),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
    ];
    let words = "and freebsd".chars().map(typed);
    for key in walk.into_iter().chain(words) {
        crate::input::on_key(&mut ui, &tree, key, now);
    }
    both("form_set_ticked_and_typed_on", &tree, &ui, now);
}

/// §3's "nothing jumps" is the card's too (M59): the band holds one height for
/// the whole form, so walking the tabs of three questions of different heights
/// moves no row of the screen. Three frames are compared row for row rather
/// than read off three snapshots — a snapshot says what one frame looks like,
/// not that two of them agree.
#[test]
fn the_band_holds_still_as_the_tabs_are_walked() {
    let (tree, mut ui, now) = asked(crate::test_support::form());
    let mut walked = Vec::new();
    for tab in 0..3 {
        if tab > 0 {
            crate::input::on_key(&mut ui, &tree, key(KeyCode::Right), now);
        }
        walked.push(landmarks(&draw_tree(80, 24, &tree, &ui, now), 80));
    }
    assert_eq!(walked[0], walked[1], "{walked:#?}");
    assert_eq!(walked[1], walked[2], "{walked:#?}");
}

/// Where the rows that may not move are in one frame: the rule the band opens
/// with — which is also what says nothing above it moved — the rule that closes
/// it, the way out under that, and the key line.
fn landmarks(screen: &str, width: usize) -> Vec<(&'static str, usize)> {
    let whole = crate::theme::rule().repeat(width);
    let rules: Vec<usize> = screen
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(&whole))
        .map(|(y, _)| y)
        .collect();
    let at = |needle: &str| crate::test_support::row_carrying(screen, needle) as usize;
    vec![
        ("the tab row", at("Submit")),
        ("the band opens", rules.first().copied().unwrap_or_default()),
        ("the band closes", rules.get(1).copied().unwrap_or_default()),
        ("the way out", at("Chat about this")),
        ("the key line", at("Enter to select")),
        ("the composer", at("ask anything")),
    ]
}
