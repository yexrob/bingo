//! A question put to the person (§2): one alone, one with several answers,
//! and the questions asked together as one card (M53) — with its tabs, its
//! preview beside or above the options, and its ASCII look.

use super::*;

#[test]
fn question_single() {
    let (tree, ui, now) = asked(question(false, false));
    both("question_single", &tree, &ui, now);
}

#[test]
fn question_multi() {
    let (tree, mut ui, now) = asked(question(true, true));
    crate::input::on_key(&mut ui, &tree, typed(' '), now);
    both("question_multi", &tree, &ui, now);
}

/// Three questions in one card (M53): the tab row names all of them, the
/// first is the one on screen, and the option under the cursor shows what
/// picking it would mean — beside the options at 120 columns, above them at
/// 80, so what gives way on a short screen is never an answer (§2).
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
