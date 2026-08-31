//! What a permission prompt is about, drawn: a diff, a command, or a url.
//! Bounded by default so the question and its options stay on the screen;
//! ctrl+e lifts the bound.
//!
//! [`diff`] is the one unified-diff renderer the whole surface shares — a
//! card's preview, a plugin's `View::Diff`, a fenced ```diff in an answer —
//! so a patch is coloured by column and emphasised by word wherever it is read.

use bingo_sdk::Preview;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};

use crate::theme;

/// A long diff would push the options off the screen.
const DIFF_ROWS: usize = 12;
/// Heredocs and `&&` chains run long.
const COMMAND_ROWS: usize = 6;

/// The preview's rows, and how many were left out.
pub fn lines(preview: &Preview, expanded: bool) -> (Vec<Line<'static>>, usize) {
    match preview {
        Preview::Diff { unified } => bound(diff(unified), DIFF_ROWS, expanded),
        Preview::Command { command, cwd } => {
            let mut rows = vec![Line::from(Span::styled(cwd.clone(), theme::dim()))];
            rows.extend(command.lines().map(command_line));
            bound(rows, COMMAND_ROWS + 1, expanded)
        }
        Preview::Url { url } => (
            vec![Line::from(Span::styled(url.clone(), theme::link()))],
            0,
        ),
    }
}

/// A unified diff, coloured by what each line does to the file, with the words
/// that actually moved picked out inside a pair of rows that replace one
/// another (design §5).
pub fn diff(unified: &str) -> Vec<Line<'static>> {
    let rows: Vec<&str> = unified.lines().collect();
    let mut out: Vec<Line<'static>> = Vec::with_capacity(rows.len());
    let mut at = 0;
    while at < rows.len() {
        let removed = run(&rows[at..], Column::Removed);
        let added = run(&rows[at + removed..], Column::Added);
        if removed == 0 || added == 0 {
            out.push(diff_line(rows[at]));
            at += 1;
            continue;
        }
        out.extend(replaced(
            &rows[at..at + removed],
            &rows[at + removed..at + removed + added],
        ));
        at += removed + added;
    }
    out
}

/// What a row does to the file: what its colour is read from, and what says
/// whether it is half of a replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    /// A file name or a hunk header — about the diff, not in it.
    Header,
    Removed,
    Added,
    Context,
}

fn column(line: &str) -> Column {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
        return Column::Header;
    }
    match line.as_bytes().first() {
        Some(b'+') => Column::Added,
        Some(b'-') => Column::Removed,
        _ => Column::Context,
    }
}

fn style(column: Column) -> Style {
    match column {
        Column::Added => theme::added(),
        Column::Removed => theme::removed(),
        Column::Header | Column::Context => theme::dim(),
    }
}

/// How many rows at the front of `rows` are in that column.
fn run(rows: &[&str], of: Column) -> usize {
    rows.iter().take_while(|row| column(row) == of).count()
}

/// A run of removed rows and the run of added ones under it: one replaced the
/// other, so the `n`th of each is a pair and its words are compared. A run
/// longer on one side than the other keeps its odd rows plain — nothing
/// replaced them.
fn replaced(before: &[&str], after: &[&str]) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(before.len() + after.len());
    out.extend(paired(before, after, Column::Removed));
    out.extend(paired(after, before, Column::Added));
    out
}

fn paired(rows: &[&str], against: &[&str], column: Column) -> Vec<Line<'static>> {
    rows.iter()
        .enumerate()
        .map(|(n, row)| match against.get(n) {
            Some(other) => emphasised(row, other, style(column)),
            None => diff_line(row),
        })
        .collect()
}

/// One side of a replacement, with the words the other side has not in bold.
/// The colour still comes from the column: emphasis says *what* changed inside
/// a row that already says it changed.
fn emphasised(row: &str, other: &str, style: Style) -> Line<'static> {
    let (mark, body) = row.split_at(1);
    let against = other.get(1..).unwrap_or_default();
    let mut spans = vec![Span::styled(mark.to_string(), style)];
    for change in TextDiff::from_words(against, body).iter_all_changes() {
        let words = change.value().to_string();
        match change.tag() {
            // What only the other side has belongs to the other side's row.
            ChangeTag::Delete => continue,
            ChangeTag::Equal => push(&mut spans, words, style),
            ChangeTag::Insert => push(&mut spans, words, style.patch(theme::bold())),
        }
    }
    Line::from(spans)
}

/// Join a run to the one before it when the two are drawn the same, so a row
/// is as few spans as it has changes.
fn push(spans: &mut Vec<Span<'static>>, words: String, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(&words),
        _ => spans.push(Span::styled(words, style)),
    }
}

fn diff_line(line: &str) -> Line<'static> {
    Line::from(Span::styled(line.to_string(), style(column(line))))
}

fn command_line(line: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("$ ", theme::dim()),
        Span::styled(line.to_string(), theme::text()),
    ])
}

fn bound(rows: Vec<Line<'static>>, limit: usize, expanded: bool) -> (Vec<Line<'static>>, usize) {
    if expanded || rows.len() <= limit {
        return (rows, 0);
    }
    let hidden = rows.len() - limit;
    (rows.into_iter().take(limit).collect(), hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unified(rows: usize) -> String {
        (0..rows)
            .map(|i| format!("+line {i}\n"))
            .collect::<String>()
    }

    #[test]
    fn a_long_diff_is_bounded_until_it_is_expanded() {
        let preview = Preview::Diff {
            unified: unified(20),
        };
        let (rows, hidden) = lines(&preview, false);
        assert_eq!((rows.len(), hidden), (DIFF_ROWS, 8));
        let (rows, hidden) = lines(&preview, true);
        assert_eq!((rows.len(), hidden), (20, 0));
    }

    #[test]
    fn diff_rows_take_their_colour_from_the_first_column() {
        let rows = diff("@@ -1 +1 @@\n-old\n+new\n context");
        let styles: Vec<_> = rows.iter().map(|r| r.spans[0].style).collect();
        assert_eq!(
            styles,
            vec![theme::dim(), theme::removed(), theme::added(), theme::dim()]
        );
    }

    /// One run of a row, as its text and whether it is emphasised.
    fn marked(line: &Line<'static>) -> Vec<(String, bool)> {
        line.spans
            .iter()
            .map(|span| {
                (
                    span.content.to_string(),
                    span.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::BOLD),
                )
            })
            .collect()
    }

    #[test]
    fn only_the_words_that_moved_are_emphasised_in_a_replaced_pair() {
        let rows = diff("@@ -1 +1 @@\n-let ready = false;\n+let ready = true;\n");
        assert_eq!(
            marked(&rows[1]),
            vec![
                ("-let ready = ".to_string(), false),
                ("false;".to_string(), true),
            ],
        );
        assert_eq!(
            marked(&rows[2]),
            vec![
                ("+let ready = ".to_string(), false),
                ("true;".to_string(), true),
            ],
        );
    }

    #[test]
    fn the_colour_still_comes_from_the_column_and_not_from_the_words() {
        let rows = diff("-old line\n+new line\n");
        assert!(
            rows[0]
                .spans
                .iter()
                .all(|s| s.style.fg == theme::removed().fg)
        );
        assert!(
            rows[1]
                .spans
                .iter()
                .all(|s| s.style.fg == theme::added().fg)
        );
    }

    #[test]
    fn a_row_nothing_replaced_is_left_plain() {
        let rows = diff("@@ -1,2 +1,1 @@\n-first\n-second\n+first\n");
        assert_eq!(rows.len(), 4);
        assert_eq!(
            marked(&rows[2]),
            vec![("-second".to_string(), false)],
            "the second removal was replaced by nothing"
        );
        assert!(
            marked(&rows[1]).iter().all(|(_, bold)| !*bold),
            "and the row that survived unchanged has nothing to point at"
        );
    }

    #[test]
    fn a_file_header_is_never_read_as_a_removal() {
        let rows = diff("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(rows[0].spans[0].style, theme::dim());
        assert_eq!(rows[1].spans[0].style, theme::dim());
        assert_eq!(rows[3].spans[0].style, theme::removed());
    }

    #[test]
    fn a_command_preview_shows_its_directory_and_the_line() {
        let (rows, _) = lines(
            &Preview::Command {
                command: "ls -la".into(),
                cwd: "/tmp".into(),
            },
            false,
        );
        assert_eq!(
            rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            vec!["/tmp", "$ ls -la"]
        );
    }
}
