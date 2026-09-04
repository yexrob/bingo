//! The welcome box: the first block of a session's transcript, which scrolls
//! away like anything else once there is something above it. A sub-session and
//! a room are joined, not started, so neither is welcomed.

use bingo_sdk::{Driver, SessionState};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::{paths, theme, wrap};

const GREETING: &str = "Welcome to bingo!";
const HELP: &str = "/help for help · /login codex to use a subscription";
/// What a person types to become the release the check found (M63).
const BECOME: &str = "bingo update";

/// Whether this surface opened the session, and so has a box to say things
/// in. The start-up check asks the same question before it asks anything of
/// the network: a run with nowhere to put an answer does not go looking.
pub fn wanted(state: &SessionState) -> bool {
    state.summary.parent.is_none() && state.summary.driver == Driver::Model
}

/// The box, or nothing at all for a session this surface did not open.
pub fn lines(state: &SessionState, width: usize, update: Option<&str>) -> Vec<Line<'static>> {
    if !wanted(state) {
        return Vec::new();
    }
    let mut body = vec![greeting(), Line::default(), help(), cwd(&state.summary.cwd)];
    body.extend(update.map(newer));
    boxed(body, width)
}

fn greeting() -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", theme::spark()), theme::presence()),
        Span::styled(GREETING.to_string(), theme::text()),
    ])
}

/// Under the mark, as everything the box says after the greeting is.
fn help() -> Line<'static> {
    Line::from(Span::styled(format!("  {HELP}"), theme::dim()))
}

/// A release this build could become, under the rest: the version in the
/// spark's own colour, and the words that fetch it.
fn newer(version: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  ↑ ".to_string(), theme::dim()),
        Span::styled(format!("v{version}"), theme::presence()),
        Span::styled(format!(" is out · {BECOME}"), theme::dim()),
    ])
}

fn cwd(cwd: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  cwd: {}", paths::short(cwd, "", paths::home())),
        theme::dim(),
    ))
}

/// Rows inside a rounded border, padded by one cell — the only box the
/// transcript draws for itself.
fn boxed(body: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let border = theme::border();
    let inner = width.saturating_sub(4);
    let rule = border.horizontal_top.repeat(inner + 2);
    let mut out = vec![edge(format!(
        "{}{rule}{}",
        border.top_left, border.top_right
    ))];
    for line in wrap::wrap_all(&body, inner) {
        out.push(walled(line, inner, border.vertical_left));
    }
    out.push(edge(format!(
        "{}{rule}{}",
        border.bottom_left, border.bottom_right
    )));
    out
}

fn edge(text: String) -> Line<'static> {
    Line::from(Span::styled(text, theme::dim()))
}

fn walled(line: Line<'static>, inner: usize, wall: &'static str) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| s.content.width()).sum();
    let mut spans = vec![Span::styled(format!("{wall} "), theme::dim())];
    spans.extend(line.spans);
    spans.push(Span::raw(" ".repeat(inner.saturating_sub(used) + 1)));
    spans.push(Span::styled(wall.to_string(), theme::dim()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{child_summary, folded, log_summary, state};

    fn drawn(state: &SessionState, width: usize) -> Vec<String> {
        lines(state, width, None)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_fresh_session_is_welcomed_in_a_box_of_its_own_width() {
        let drawn = drawn(&state(), 60);
        assert_eq!(drawn.len(), 6, "border, greeting, blank, help, cwd, border");
        for row in &drawn {
            assert_eq!(row.width(), 60, "{row}");
        }
        assert!(drawn[1].contains("✻ Welcome to bingo!"), "{:?}", drawn[1]);
        assert!(drawn[4].contains("cwd: /tmp/project"), "{:?}", drawn[4]);
    }

    #[test]
    fn the_box_is_as_wide_as_the_transcript_it_opens() {
        let drawn = drawn(&state(), 120);
        assert!(drawn.iter().all(|row| row.width() == 120), "{drawn:#?}");
    }

    #[test]
    fn a_newer_release_is_one_more_row_under_the_help_line() {
        let plain = drawn(&state(), 60);
        let told = lines(&state(), 60, Some("0.5.0"))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(told.len(), plain.len() + 1, "one row, and no other change");
        assert!(
            told[5].contains("↑ v0.5.0 is out · bingo update"),
            "{:?}",
            told[5]
        );
        for row in &told {
            assert_eq!(row.width(), 60, "{row}");
        }
    }

    #[test]
    fn a_session_this_surface_did_not_open_is_not_welcomed() {
        let mut child = state();
        child.summary = child_summary("reviewer");
        assert!(!wanted(&child), "a sub-session joins");
        assert!(lines(&child, 60, None).is_empty());

        let mut room = folded(vec![]);
        room.summary = log_summary("#design");
        assert!(!wanted(&room), "a room is not a session of ours");
        assert!(lines(&room, 60, Some("0.5.0")).is_empty());
    }
}
