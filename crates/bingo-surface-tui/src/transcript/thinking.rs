//! The rows a thought draws (§4's thinking row, §6's): `✻ Thinking…` while it
//! is being had, `✻ Thought for 2s` once it is over, and the newest rows of
//! what was thought under either — or of the whole run of thoughts it ends,
//! because consecutive thoughts are one thought ([`crate::thoughts`], M79).
//!
//! Its own module because the thought is a noun of its own: it is the one
//! block whose body is the same on either side of the close, the one with
//! three fold states, and the one that stands for more than its own item.

use bingo_sdk::{Item, ItemBody};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{Place, Rows, THOUGHT_ROWS, plain, returns, tail};
use crate::fold::Fold;
use crate::{theme, thoughts};

/// A thought is readable where it happened, and while it is being had: `✻
/// Thinking…` over the newest [`THOUGHT_ROWS`] rows of what has arrived so
/// far, and once it is over `✻ Thought for 2s` over the very same rows.
///
/// The heading is the whole of what the close changes (2026-09-06,
/// user-directed). The transcript is anchored at its foot, so a thought that
/// gave its rows back when it ended dropped the conversation above it by two
/// at the one moment a person was reading it. One body, then, and one match —
/// on the one fact the two halves differ by.
///
/// A run of thoughts is one thought (2026-09-06, later still, user-directed).
/// A provider that opens a reasoning item per output item hands the transcript
/// ten of them in a row; they draw as one block, hung on the **last** of them,
/// over their texts joined and the sum of their times. A thought a newer one
/// joined draws nothing at all — its rows are the newer one's.
pub(super) fn lines(
    item: &Item,
    place: &Place<'_>,
    fold: Fold,
    rows: &Rows<'_>,
) -> Vec<Line<'static>> {
    if place.joined {
        return Vec::new();
    }
    let run = place.thoughts(item);
    let mut out = vec![match item.completed_at {
        None => still_thinking(),
        Some(_) => thought_for(thoughts::took(&run)),
    }];
    let text = thoughts::text(&run);
    if !text.is_empty() {
        out.extend(returns(
            thought_rows(&text, fold, rows.result_width()),
            rows,
        ));
    }
    out
}

/// The row of a thought as it is being had: dim italic, and the ellipsis
/// breathing with the rest of the surface.
fn still_thinking() -> Line<'static> {
    sparkled(
        format!("Thinking{}", theme::ellipsis()),
        theme::dim().patch(theme::italic()),
    )
}

/// The row of a thought that is over: how long it took — the whole run of
/// them, where it is the last of a run.
fn thought_for(span: jiff::SignedDuration) -> Line<'static> {
    sparkled(format!("Thought for {}", took(span)), theme::dim())
}

/// What hangs under a thought's row, dim under the same `⎿` a running tool's
/// tail hangs from (§6): the newest rows of what has been thought, which is
/// the only cut that can follow something growing from the bottom; the whole
/// of it where a person opened it; nothing where a click has gone round to its
/// shut.
///
/// The same rows on either side of the close, which is what holding where it
/// ended means. They wear no comet — the comet is `presence`'s glow on words
/// being *said* (§6 "streaming"), and thinking is where `dim` lives (§4) — and
/// no `… +N lines`: while the thought streams the count would change under the
/// reader on every delta, and at the close the mark would spend the very row
/// the hold is there to keep. What was cut is one click away, which is what
/// the ring is for.
///
/// `width` is the measure the `⎿` body is wrapped at, so the tail is cut at
/// the width it is drawn at and the block is [`THOUGHT_ROWS`] rows tall
/// whatever the prose does.
fn thought_rows(text: &str, fold: Fold, width: usize) -> Vec<Line<'static>> {
    match fold {
        Fold::Shut => Vec::new(),
        Fold::Peek => tail(text, THOUGHT_ROWS, width),
        Fold::Open => plain(text),
    }
}

/// The `✻` and what it says beside it.
fn sparkled(text: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", theme::spark()), theme::dim()),
        Span::styled(text, style),
    ])
}

/// How long a thought took, as a person reads a clock: something that happened
/// took some time, so under a second is `<1s` and never `0s`, and from a
/// minute it is `3m 21s` — a run of ten thoughts is minutes, and nobody
/// divides `201s` in their head.
fn took(span: jiff::SignedDuration) -> String {
    match span.as_secs() {
        seconds if seconds < 1 => "<1s".to_string(),
        seconds if seconds < 60 => format!("{seconds}s"),
        seconds => format!("{}m {}s", seconds / 60, seconds % 60),
    }
}

/// What a thought has under it: what was thought. `None` for one that came
/// back empty — Anthropic's redacted thinking, an OpenAI turn the provider
/// summarised nothing of — which draws the row alone, folds nothing, opens
/// nothing and so promises nothing.
pub fn thought(item: &Item) -> Option<&str> {
    match &item.body {
        ItemBody::Reasoning { text, .. } if !text.trim().is_empty() => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;

    /// The clock a heading reads: under a second is a moment and not no time
    /// at all, and from a minute it is minutes — a run of ten thoughts is
    /// three and a half of them (M79).
    #[test]
    fn how_long_it_took_reads_as_a_person_reads_a_clock() {
        assert_eq!(took(SignedDuration::from_millis(400)), "<1s");
        assert_eq!(took(SignedDuration::from_secs(-1)), "<1s");
        assert_eq!(took(SignedDuration::from_secs(1)), "1s");
        assert_eq!(took(SignedDuration::from_secs(59)), "59s");
        assert_eq!(took(SignedDuration::from_secs(60)), "1m 0s");
        assert_eq!(took(SignedDuration::from_secs(201)), "3m 21s");
    }
}
