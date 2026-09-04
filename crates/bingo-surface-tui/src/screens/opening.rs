//! The welcome box with the opening playing in it (§11): the box drawing
//! itself, and the box the transcript has always had when it lands.
//!
//! These are that box on the screen, at the two sizes and in both looks — the
//! record of what a person actually sees when they start bingo, with the
//! composer and the status line where they always are, because the piece plays
//! at the box's resting height and moves nothing under it.

use crate::painted::{daylight, in_look, truecolor};
use crate::test_support::*;

/// The screen the box is playing on, `t` seconds into the piece.
fn playing(width: u16, height: u16, t: f32) -> String {
    let state = state();
    let tree = solo(&state);
    let (mut ui, now) = scene();
    ui.intro = Some(crate::opening::Playing::from(
        now.instant
            .checked_sub(std::time::Duration::from_secs_f32(t))
            .expect("a clock with the piece behind it"),
    ));
    draw_tree(width, height, &tree, &ui, now)
}

#[test]
fn the_box_draws_itself_and_then_becomes_itself() {
    for (name, t) in [("line", 0.45f32), ("words", 1.7), ("rest", 2.4)] {
        insta::assert_snapshot!(
            format!("opening_{name}_80x24"),
            in_look(truecolor(), || playing(80, 24, t))
        );
        insta::assert_snapshot!(
            format!("opening_{name}_120x40"),
            in_look(truecolor(), || playing(120, 40, t))
        );
    }
}

#[test]
fn the_light_palette_draws_the_same_box_in_its_own_ink() {
    insta::assert_snapshot!(
        "opening_words_daylight",
        in_look(daylight(), || playing(80, 24, 1.7))
    );
}

/// Nothing under the box moves while it plays: the composer and the status line
/// are on the same rows in the first frame and the last, and the box is the
/// same height throughout.
#[test]
fn the_piece_moves_nothing_but_the_boxs_own_cells() {
    let rows = |t: f32| {
        in_look(truecolor(), || playing(80, 24, t))
            .lines()
            .map(|row| row.contains("? for shortcuts") || row.contains("> "))
            .collect::<Vec<bool>>()
    };
    assert_eq!(rows(0.0), rows(2.4), "the furniture never moved");
    let landed = in_look(truecolor(), || playing(80, 24, 2.4));
    assert!(landed.contains("✻ Welcome to bingo!"), "{landed}");
    assert!(landed.contains("? for shortcuts"), "{landed}");
}
