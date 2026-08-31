//! `View::Panel`: a bold title and its child indented under it — the same
//! shape a block of the transcript has, so a panel reads as one thing.

use bingo_sdk::View;
use ratatui::text::{Line, Span};

use crate::theme;
use crate::views::{Marks, indent, marked};

/// How far the child hangs under the title.
const INDENT: usize = 2;

pub fn lines(title: &str, child: &View, width: usize, marks: &Marks) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(
        title.to_string(),
        theme::text().patch(theme::bold()),
    ))];
    out.extend(
        marked(child, width.saturating_sub(INDENT), marks)
            .into_iter()
            .map(|line| indent(line, INDENT)),
    );
    out
}
