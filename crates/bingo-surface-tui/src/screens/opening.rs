//! The welcome box with the opening playing in it (§11): twelve rows of a
//! ray-marched world while it runs, and the box the transcript has always had
//! when it lands.
//!
//! A frame is drawn off the loop's thread and handed to the box; these are
//! that frame on the screen, at the two sizes and in both looks — the record
//! of what a person actually sees when they start bingo.

use crate::painted::{daylight, in_look, truecolor};
use crate::test_support::*;

/// The screen the box is playing on, `t` seconds into the piece.
fn playing(width: u16, height: u16, t: f32) -> String {
    let state = state();
    let tree = solo(&state);
    let (mut ui, now) = scene();
    // The box is as wide as the transcript, which the frame itself measures.
    let _ = draw_tree(width, height, &tree, &ui, now);
    let across = ui.painted.borrow().regions.transcript.width;
    let boxed = crate::welcome::lines(&state, usize::from(across), None);
    let mut intro = crate::intro::Playing::from(now.instant);
    intro.landed(across, crate::intro::frame(t, across, &boxed));
    ui.intro = Some(intro);
    draw_tree(width, height, &tree, &ui, now)
}

#[test]
fn the_box_plays_the_world_and_then_becomes_itself() {
    for (name, t) in [("floor", 0.7f32), ("field", 2.6), ("handoff", 3.5)] {
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
fn the_light_palette_plays_the_same_piece_in_its_own_ink() {
    insta::assert_snapshot!(
        "opening_field_daylight",
        in_look(daylight(), || playing(80, 24, 2.6))
    );
}

/// Twelve rows while it plays, and the box's own six when it is over — and in
/// both cases the composer and the status line still have theirs.
#[test]
fn the_box_gives_the_rows_back_when_the_piece_is_over() {
    let rows = |t: f32| {
        in_look(truecolor(), || playing(80, 24, t))
            .lines()
            .filter(|row| row.contains('▀') || row.contains('▄'))
            .count()
    };
    assert!(rows(0.7) >= 10, "the world has the box's twelve rows");
    assert_eq!(rows(4.0), 0, "and gives every one of them back");
    let landed = in_look(truecolor(), || playing(80, 24, 4.0));
    assert!(landed.contains("✻ Welcome to bingo!"), "{landed}");
    assert!(landed.contains("? for shortcuts"), "{landed}");
}
