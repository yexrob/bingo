//! One node of the `View` vocabulary, one renderer (ADR-0013 §1).
//!
//! [`render`] walks the tree and hands each node to the module that owns it,
//! so a `Table` looks the same under a tool row, in the `ctrl+p` sheet and in
//! a rail card — the TUI draws every node exactly once, and it is tested once.
//! Nothing here knows which lane the view came from or where it will be put:
//! a renderer is a pure function of a node and a width.

pub mod actions;
mod badge;
/// A fence in an answer is the same block a plugin publishes, so
/// [`crate::markdown`] hands its language and text straight to this renderer
/// (M11e).
pub mod code;
mod columns;
mod diff;
mod keyvalue;
mod list;
mod markdown;
mod panel;
mod progress;
mod stack;
/// A GFM table in an answer is the same table a plugin publishes, so
/// [`crate::markdown`] hands its rows straight to this renderer (M11e).
pub mod table;
mod text;
mod tree;

use bingo_sdk::{Action, ActionItem, View};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::theme;

/// A value that is not there: an empty cell, a column a row does not carry.
pub const MISSING: &str = "–";

/// What the frame knows that a node does not: which action a person has
/// fired and has not been answered about yet (ADR-0013 §3), and where the
/// free-running walk of a sheen has got to. It travels down the tree so an
/// `Actions` row nested in a `Panel` still shows the mark, and a bar nested
/// anywhere walks on the same beat as every other.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marks {
    pub pending: Option<Action>,
    /// Where an unbounded bar's sheen is in its turn: 0 at the head, which is
    /// where a frame that says nothing about it leaves it (§6's rest).
    pub beat: f32,
}

impl Marks {
    /// What the frame knows before it has been told anything: the clock.
    pub fn at(now: Now) -> Self {
        Self {
            pending: None,
            beat: progress::walk(now),
        }
    }
}

/// A view as styled lines at rest: a record — a finished output, a sheet of
/// one — knows no frame and moves in none.
pub fn render(view: &View, width: usize) -> Vec<Line<'static>> {
    marked(view, width, &Marks::default())
}

/// A view as styled lines.
pub fn marked(view: &View, width: usize, marks: &Marks) -> Vec<Line<'static>> {
    match view {
        View::Text { text } => text::lines(text),
        View::Markdown { text } => markdown::lines(text, width),
        View::Code { lang, text } => code::lines(lang.as_deref(), text, width),
        View::Diff { unified } => diff::lines(unified),
        View::List { items } => list::lines(items),
        View::Table { headers, rows } => table::lines(headers, rows, width),
        View::KeyValue { rows } => keyvalue::lines(rows, width),
        View::Progress {
            value,
            total,
            label,
        } => progress::lines(*value, *total, label.as_deref(), width, marks.beat),
        View::Badge { text, tone } => badge::lines(text, *tone),
        View::Tree { nodes } => tree::lines(nodes),
        View::Stack { children } => stack::lines(children, width, marks),
        View::Columns { children } => columns::lines(children, width, marks),
        View::Panel { title, child } => panel::lines(title, child, width, marks),
        View::Actions { items } => actions::lines(items, width, marks),
        // A word this surface has not learned is the text its author wrote
        // for exactly this (ADR-0038 §1). Learning a kind richly is a
        // surface's own affair, and no kind is learned here yet.
        View::Custom { fold, .. } => text::lines(fold),
    }
}

/// Every action a view offers, in the order it draws them: what a digit key
/// on a focused card fires.
pub fn actions_of(view: &View) -> Vec<&ActionItem> {
    match view {
        View::Actions { items } => items.iter().collect(),
        View::Stack { children } | View::Columns { children } => {
            children.iter().flat_map(actions_of).collect()
        }
        View::Panel { child, .. } => actions_of(child),
        _ => Vec::new(),
    }
}

/// Whether a column of values reads as numbers, which is what right-aligning
/// one is for. The mark for a missing value is not one.
pub fn numeric(value: &str) -> bool {
    value.parse::<f64>().is_ok()
}

/// Content that must not wrap — code, a table row — cut to the width, with
/// the ellipsis that says something was cut (design §7).
pub fn clip(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let ellipsis = theme::ellipsis();
    let room = width.saturating_sub(ellipsis.width());
    let mut out = String::new();
    for c in text.chars() {
        let next = out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if next > room {
            break;
        }
        out.push(c);
    }
    out.push_str(ellipsis);
    out
}

/// One line pushed `by` cells to the right: what a `Panel`'s child and a
/// sheet's entry hang under.
pub fn indent(line: Line<'static>, by: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(by))];
    spans.extend(line.spans);
    let mut out = Line::from(spans);
    out.style = line.style;
    out
}

/// One line cut and padded to exactly `width` cells: what a column of
/// [`columns`] and a rail card are laid out on.
pub fn fit(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        if used >= width {
            break;
        }
        let text = clip(&span.content, width - used);
        used += text.width();
        spans.push(Span::styled(text, span.style));
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    let mut out = Line::from(spans);
    out.style = line.style;
    out
}

#[cfg(test)]
mod tests;
