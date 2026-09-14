//! A transcript held a page back from its foot (§3, M95): the band's first
//! row carries the way back, centred on its bar, and nothing else
//! moves — the same frame as the tail's, with one row's words changed.

use super::*;

#[test]
fn held_back_from_the_foot() {
    let state = long_transcript(60);
    let (mut ui, now) = scene();
    let tree = solo(&state);
    draw_tree(80, 24, &tree, &ui, now);
    crate::input::on_key(&mut ui, &tree, key(KeyCode::PageUp), now);
    let settled = later(now, 100);
    both("held_back", &tree, &ui, settled);
}
