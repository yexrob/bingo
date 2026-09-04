//! The screens a set of questions asked together is read through (M53).
//!
//! Its own module because the form is its own noun, and because `screens.rs`
//! had grown past the thousand lines a file may hold.

use super::*;

/// Three questions in one card (M53): the tab row names all of them, the
/// first is the one on screen, and the option under the cursor
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

/// The same card where nothing but the six characters of §7 may be drawn.
#[test]
fn form_in_ascii() {
    let (tree, ui, now) = asked(crate::test_support::form());
    without_glyphs("form_in_ascii", &tree, &ui, now);
}
