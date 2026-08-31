//! `View::Stack`: its children, one under the next, and nothing between them.
//! A plugin that wants air puts a `Text` with nothing in it there — the same
//! rule the fold follows, so the two never disagree.

use bingo_sdk::View;
use ratatui::text::Line;

use crate::views::{Marks, marked};

pub fn lines(children: &[View], width: usize, marks: &Marks) -> Vec<Line<'static>> {
    children
        .iter()
        .flat_map(|child| marked(child, width, marks))
        .collect()
}
