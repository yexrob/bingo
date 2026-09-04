//! What is under a finished or running tool row: the text as it arrived,
//! folded to the rows the transcript can spare, with a line that says how
//! much was left out and what opens it (design §5).

use bingo_sdk::{ContentPart, ToolOutput, View};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::{DIFF_ROWS, EXPAND, OUTPUT_ROWS};
use crate::fold::Fold;
use crate::{theme, views};

/// The last `keep` rows of something still arriving: what a running tool has
/// printed so far, or what a thought has been thinking. One tail, so the two
/// move the same way (§6); how many rows each spends is the one thing they
/// differ by, and each names its own.
pub(super) fn tail(arriving: &str, keep: usize) -> Vec<Line<'static>> {
    let all: Vec<&str> = arriving.trim_end().lines().collect();
    plain(&all[all.len().saturating_sub(keep)..].join("\n"))
}

/// What a person reads under a finished tool row: the display the tool drew
/// for them when there is one (ADR-0013 §2, the block lane), else the text the
/// model read, folded to what a row can spare either way.
pub(super) fn folded(output: &ToolOutput, fold: Fold, width: usize) -> Vec<Line<'static>> {
    let (rows, limit) = match &output.display {
        // A diff is the one display a person reads by the dozen rows.
        Some(view @ View::Diff { .. }) => (views::render(view, width), DIFF_ROWS),
        Some(view) => (views::render(view, width), OUTPUT_ROWS),
        None => (plain(&text_of(output)), OUTPUT_ROWS),
    };
    kept(rows, fold, limit, Some(EXPAND))
}

/// Everything a result says, with nothing folded away: what the pager opens
/// (design §5 — a long output opens in a sheet).
pub fn whole(output: &ToolOutput, width: usize) -> Vec<Line<'static>> {
    folded(output, Fold::Open, width)
}

pub(super) fn plain(text: &str) -> Vec<Line<'static>> {
    text.trim_end()
        .lines()
        .map(|line| Line::from(Span::styled(expand_tabs(line), theme::dim())))
        .collect()
}

/// A terminal cell has no tab in it: each one runs to the next stop of eight,
/// as the shell would have shown it.
pub(super) fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let stop = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', stop));
            column += stop;
        } else {
            out.push(c);
            column += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    out
}

pub(super) fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a block shows under its row, from the one fold it is in: nothing, the
/// first `limit` rows with how many were left out, or the whole of it. One map
/// answers for every fold, so a block is open in one way only.
pub(super) fn kept(
    rows: Vec<Line<'static>>,
    fold: Fold,
    limit: usize,
    opens: Option<&str>,
) -> Vec<Line<'static>> {
    match fold {
        Fold::Shut => Vec::new(),
        Fold::Peek => cut(rows, limit, opens),
        Fold::Open => rows,
    }
}

/// The first rows, then how many were left out and what opens them. `opens` is
/// `None` for what no key reaches: `ctrl+o` reaches a result, so a block that
/// is not one says how much it kept back and promises nothing.
pub(super) fn cut(
    rows: Vec<Line<'static>>,
    limit: usize,
    opens: Option<&str>,
) -> Vec<Line<'static>> {
    let hidden = rows.len().saturating_sub(limit);
    let mut out: Vec<Line<'static>> = rows.into_iter().take(limit).collect();
    if hidden > 0 {
        let key = opens.map(|key| format!(" ({key})")).unwrap_or_default();
        out.push(Line::from(Span::styled(
            format!("{} +{hidden} lines{key}", theme::ellipsis()),
            theme::dim(),
        )));
    }
    out
}
