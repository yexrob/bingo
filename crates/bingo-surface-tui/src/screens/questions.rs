//! A question put to the person (§2): one alone, and one with several
//! answers. The questions asked together are `forms`.

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
