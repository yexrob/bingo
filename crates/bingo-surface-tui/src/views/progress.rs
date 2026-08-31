//! `View::Progress`: `████████░░ 80 % · label` (design §5). The fill is
//! `presence` — the one colour that moves — and the track is dim. Without a
//! total there is no fraction to show, so the head of the track carries the
//! count instead; the sheen that walks it is M11c's.

use ratatui::text::{Line, Span};

use crate::theme;

/// How wide the track is, in cells.
const TRACK: usize = 10;
/// The lit run of an unbounded bar, which M11c walks along the track.
const SHEEN: usize = 3;
const FILLED: &str = "█";
const EMPTY: &str = "░";

pub fn lines(
    value: u64,
    total: Option<u64>,
    label: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let (lit, amount) = match total.filter(|total| *total > 0) {
        Some(total) => bounded(value, total),
        None => (SHEEN.min(TRACK), value.to_string()),
    };
    let mut spans = vec![
        Span::styled(FILLED.repeat(lit), theme::presence()),
        Span::styled(EMPTY.repeat(TRACK - lit), theme::dim()),
        Span::styled(format!(" {amount}"), theme::text()),
    ];
    if let Some(label) = label {
        spans.push(Span::styled(
            crate::views::clip(
                &format!(" · {label}"),
                width.saturating_sub(TRACK + 1 + amount.len()),
            ),
            theme::dim(),
        ));
    }
    vec![Line::from(spans)]
}

/// How much of the track is lit, and the percentage that says it in words.
fn bounded(value: u64, total: u64) -> (usize, String) {
    let done = value.min(total);
    let percent = done * 100 / total;
    let lit = (percent as usize * TRACK).div_ceil(100);
    (lit.min(TRACK), format!("{percent} %"))
}
