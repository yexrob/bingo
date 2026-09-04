//! A shell line the person ran themselves (`!<command>`, ADR-0008 §5).
//!
//! It is drawn as what it is and as nothing else: their own block, on the
//! same raised bar their words are on, with the prompt they typed it at
//! standing where the `>` of a message stands. What came back hangs under a
//! `⎿`, folded to the rows a result is folded to, because that is what it is
//! — output, read by the line rather than as prose (design §5).
//!
//! The code the command came to is not part of that output and is never
//! folded away with it: a line that failed says so on a row of its own, in
//! `bad`, whatever else was cut. A clean exit says nothing — the output is
//! the whole of the answer.

use ratatui::text::{Line, Span};

use super::said;
use super::{EXPAND, OUTPUT_ROWS, Rows, kept, plain, returns, speaks_indent, under};
use crate::fold::Fold;
use crate::theme;

/// What stands where a message's `>` stands: the shell's own prompt.
const PROMPT: &str = "$";

/// One shell item's block.
pub(super) fn lines(
    command: &str,
    output: &str,
    exit: Option<i32>,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    let mut out = typed(command, rows);
    let came_back = came_back(output, exit, fold);
    if !came_back.is_empty() {
        out.extend(returns(came_back, rows));
    }
    out
}

/// The line as it was typed, on the bar the person's own words are drawn on.
fn typed(command: &str, rows: &Rows<'_>) -> Vec<Line<'static>> {
    let mark = Span::styled(format!("{PROMPT} "), theme::dim());
    let body = vec![Line::from(Span::styled(command.to_string(), theme::text()))];
    said::barred(
        under(mark, body, speaks_indent(), rows.measure()),
        rows.width,
    )
}

/// What the line wrote, cut to what the fold allows, and — under whatever is
/// left of it — the code it came to when that was not a clean exit.
fn came_back(output: &str, exit: Option<i32>, fold: Fold) -> Vec<Line<'static>> {
    if fold == Fold::Shut {
        return Vec::new();
    }
    let mut body = kept(plain(output), fold, OUTPUT_ROWS, Some(EXPAND));
    if let Some(code) = exit.filter(|code| *code != 0) {
        body.push(Line::from(Span::styled(
            format!("[exit {code}]"),
            theme::bad(),
        )));
    }
    body
}

/// Everything the line wrote, with nothing folded away: what the sheet opens
/// on (`crate::pager`).
pub fn whole(output: &str, exit: Option<i32>) -> Vec<Line<'static>> {
    came_back(output, exit, Fold::Open)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    /// The code is the point of a line that failed, so it survives the cut
    /// that takes the output down to its five rows.
    #[test]
    fn a_failing_line_keeps_its_code_however_much_output_was_folded_away() {
        let output = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let peeked = texts(&came_back(&output, Some(2), Fold::Peek));
        assert_eq!(peeked.len(), OUTPUT_ROWS + 2, "five rows, a cut, the code");
        assert_eq!(peeked.first().map(String::as_str), Some("line 1"));
        assert_eq!(peeked.last().map(String::as_str), Some("[exit 2]"));

        let open = texts(&came_back(&output, Some(2), Fold::Open));
        assert_eq!(open.len(), 21);
        assert_eq!(open.last().map(String::as_str), Some("[exit 2]"));
    }

    /// A line that simply worked says nothing about how it ended, and one
    /// shut away says nothing at all.
    #[test]
    fn a_clean_exit_adds_no_row_and_a_shut_block_has_none() {
        assert_eq!(texts(&came_back("hi\n", Some(0), Fold::Peek)), ["hi"]);
        assert!(came_back("hi\n", Some(1), Fold::Shut).is_empty());
        assert!(
            came_back("", Some(0), Fold::Peek).is_empty(),
            "a silent line is one row"
        );
    }

    /// A command killed before it could exit has no code to show: what
    /// stopped it is the last line of what it wrote (`bingo-tool-bash`).
    #[test]
    fn a_line_that_reached_no_code_shows_only_what_it_wrote() {
        assert_eq!(
            texts(&came_back("one\n[interrupted]", None, Fold::Peek)),
            ["one", "[interrupted]"]
        );
    }
}
