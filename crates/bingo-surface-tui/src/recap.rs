//! The recap under the worked row (M84): `※ recap: Built the thing; tests
//! are green. Next: push.` — the model's own two sentences on the turn that
//! just ended, as Claude Code draws one after a long turn.
//!
//! The context plugin asks for it once the turn is over and publishes the
//! answer as live state — the signal `_bingo.context`/`recap`, `{ "turn":
//! <id>, "text": … }` — and this reads it by name, as [`crate::tasks`] reads
//! the list. Live state is the right lane: a recap is about the turn that
//! just ended, so it is never journaled, and the turn id keeps it from
//! outliving that turn on screen. The underscore keeps it out of the rail's
//! generic cards, as the baselines are kept out.

use bingo_sdk::SessionState;
use ratatui::text::{Line, Span};

use crate::{theme, wrap};

const PLUGIN: &str = "_bingo.context";
const KIND: &str = "recap";
const TURN: &str = "turn";
const TEXT: &str = "text";
/// Rows the recap may take: two sentences wrap to two or three, and the rest
/// is cut with the ellipsis — the recap is a glance, not the answer.
pub const ROWS: usize = 3;

/// The recap of the turn that last ended, and nothing for any other turn.
pub fn of(state: &SessionState) -> Option<String> {
    let last = state.last_turn.as_ref()?;
    let published = state.signals.get(PLUGIN)?.get(KIND)?;
    if published.get(TURN)?.as_str()? != last.id.as_str() {
        return None;
    }
    let text = published.get(TEXT)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `※ recap: …`, the mark and the word dim, the words plain, wrapped to the
/// width and cut at [`ROWS`].
pub fn rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let line = Line::from(vec![
        Span::styled(format!("{} recap: ", theme::recap_mark()), theme::dim()),
        Span::styled(text.to_string(), theme::text()),
    ]);
    let mut rows = wrap::wrap(&line, width);
    if rows.len() > ROWS {
        rows.truncate(ROWS);
        if let Some(last) = rows.last_mut() {
            last.spans
                .push(Span::styled(theme::ellipsis().to_string(), theme::dim()));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lanes::signalled;
    use crate::test_support::{completed, folded, frame, started};
    use serde_json::json;

    fn recapped(turn: &str, text: &str) -> SessionState {
        folded(vec![
            frame(1, started("trn_1")),
            frame(2, completed("trn_1", bingo_sdk::TurnStatus::Completed)),
            frame(
                3,
                signalled(PLUGIN, KIND, json!({"turn": turn, "text": text})),
            ),
        ])
    }

    #[test]
    fn the_recap_is_the_last_turns_and_no_other() {
        assert_eq!(
            of(&recapped("trn_1", " Built it. ")),
            Some("Built it.".to_string())
        );
        assert_eq!(
            of(&recapped("trn_0", "Built it.")),
            None,
            "an earlier turn's"
        );
        assert_eq!(of(&recapped("trn_1", "  ")), None, "nothing to say");
        let mut next = recapped("trn_1", "Built it.");
        next.apply(&frame(4, started("trn_2")));
        next.apply(&frame(
            5,
            completed("trn_2", bingo_sdk::TurnStatus::Completed),
        ));
        assert_eq!(of(&next), None, "the next turn leaves it behind");
    }

    #[test]
    fn the_rows_wrap_to_the_width_and_stop_at_three() {
        let short = rows("Built it.", 40);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0].to_string(), "※ recap: Built it.");
        let long = rows(&"word ".repeat(60), 20);
        assert_eq!(long.len(), ROWS);
        assert!(
            long[ROWS - 1].to_string().ends_with('…'),
            "{}",
            long[ROWS - 1]
        );
    }
}
