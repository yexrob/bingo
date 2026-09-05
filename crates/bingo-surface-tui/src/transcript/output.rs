//! What is under a finished or running tool row: the text as it arrived,
//! folded to the rows the transcript can spare, with a line that says how
//! much was left out and what opens it (design §5).

use bingo_sdk::{ContentPart, ToolOutput, View};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::{DIFF_ROWS, EXPAND, OUTPUT_ROWS};
use crate::fold::Fold;
use crate::{theme, views, wrap};

/// The last `keep` rows of something still arriving: what a running tool has
/// printed so far, or what a thought has been thinking. One tail, so the two
/// move the same way (§6); how many rows each spends is the one thing they
/// differ by, and each names its own.
///
/// Rows as the transcript will draw them, which is what `width` is for. A
/// paragraph is one logical line and any number of rows, so counting lines let
/// the block change height on every delta — and the transcript is anchored at
/// its foot, so that moved everything above it (2026-09-06). Only the last
/// `keep` logical lines are wrapped: each holds at least one row, so `keep` of
/// them always hold the `keep` rows the cut needs, and the work per delta stays
/// with the tail instead of the whole text.
pub(super) fn tail(arriving: &str, keep: usize, width: usize) -> Vec<Line<'static>> {
    let lines: Vec<&str> = arriving.trim_end().lines().collect();
    let last = lines[lines.len().saturating_sub(keep)..].join("\n");
    let mut rows = wrap::wrap_all(&plain(&last), width);
    rows.split_off(rows.len().saturating_sub(keep))
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

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    /// The paragraph the bug was reported on: one logical line, many rows.
    const PARAGRAPH: &str = "The manifest first, because the lockfile only says what \
                             the manifest already asked for, and then the crate map, \
                             which is the one place the layering is written down.";

    fn rows(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    #[test]
    fn one_long_paragraph_is_cut_to_the_rows_it_ends_on() {
        let cut = tail(PARAGRAPH, 2, 40);
        assert_eq!(cut.len(), 2, "{:?}", rows(&cut));
        let whole = wrap::wrap_all(&plain(PARAGRAPH), 40);
        assert_eq!(rows(&cut), rows(&whole[whole.len() - 2..]));
    }

    #[test]
    fn a_text_shorter_than_the_cut_is_kept_whole() {
        assert_eq!(rows(&tail("one\ntwo", 3, 40)), vec!["one", "two"]);
    }

    #[test]
    fn nothing_arrived_yet_is_no_rows_at_all() {
        assert!(tail("", 2, 40).is_empty());
    }

    /// A cell, not a character: a CJK row holds ten glyphs at twenty columns,
    /// so a cut counting characters would keep twice the block it promised.
    #[test]
    fn a_wide_paragraph_is_cut_by_cells() {
        let paragraph = "先立组件后立系统再立原语".repeat(3);
        let cut = tail(&paragraph, 2, 20);
        assert_eq!(cut.len(), 2);
        let drawn = rows(&cut);
        assert!(
            drawn
                .iter()
                .all(|row| UnicodeWidthStr::width(&row[..]) <= 20),
            "{drawn:?}"
        );
        assert!(paragraph.ends_with(&drawn.concat()), "{drawn:?}");
    }

    /// A blank line is a row like any other: crossing a paragraph break keeps
    /// the count where it was, which is the whole point of counting rows.
    #[test]
    fn a_paragraph_break_does_not_shrink_the_count() {
        let across = format!("{PARAGRAPH}\n\nThen the plan.");
        assert_eq!(tail(&across, 2, 40).len(), 2);
        assert_eq!(rows(&tail(&across, 2, 40))[1], "Then the plan.");
    }

    /// `transcript::under` wraps the tail again at the same width when it
    /// hangs it under the `⎿`, so that second pass has to be the identity —
    /// otherwise the count the cut promised is not the count on the screen.
    #[test]
    fn wrapping_the_tail_again_at_its_own_width_changes_nothing() {
        for text in [
            PARAGRAPH,
            "  an indented line that is long enough to wrap twice",
        ] {
            let cut = tail(text, 2, 40);
            assert_eq!(wrap::wrap_all(&cut, 40), cut, "{:?}", rows(&cut));
        }
    }
}
