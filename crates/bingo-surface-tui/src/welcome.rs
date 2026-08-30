//! The welcome box: the first block of a session's transcript, which scrolls
//! away like anything else once there is something above it. A sub-session and
//! a room are joined, not started, so neither is welcomed.

use bingo_sdk::{Driver, SessionState};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::{paths, theme, wrap};

const GREETING: &str = "Welcome to bingo!";
const HELP: &str = "/help for help · /login codex to use a subscription";

/// The box, or nothing at all for a session this surface did not open.
pub fn lines(state: &SessionState, width: usize) -> Vec<Line<'static>> {
    if state.summary.parent.is_some() || state.summary.driver != Driver::Model {
        return Vec::new();
    }
    boxed(
        vec![greeting(), Line::default(), help(), cwd(&state.summary.cwd)],
        width,
    )
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
        lines(state, width)
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
    fn a_session_this_surface_did_not_open_is_not_welcomed() {
        let mut child = state();
        child.summary = child_summary("reviewer");
        assert!(lines(&child, 60).is_empty(), "a sub-session joins");

        let mut room = folded(vec![]);
        room.summary = log_summary("#design");
        assert!(
            lines(&room, 60).is_empty(),
            "a room is not a session of ours"
        );
    }
}
