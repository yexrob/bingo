//! The rail (design §3): a plugin's pinned panels and its live signals as
//! small cards, down the right edge past 120 columns and under the running
//! rows below it. Which lane a view arrived in is the plugin's decision
//! (ADR-0013 §2); where the card sits, what it is called and how much of it
//! is shown are the surface's (§4).
//!
//! A card is derived from `SessionState` at render time and never kept, so it
//! follows the view from session to session and a signal that has gone leaves
//! nothing behind. What the surface does keep is which panels a person pinned.

use std::collections::BTreeSet;
use std::ops::Range;

use bingo_sdk::{SessionId, SessionState, View};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::views::{self, Marks};
use crate::{panel, theme};

/// How many rows one plugin may take before the rest of it folds away.
const ROWS: usize = 8;
/// What a card is separated from the next by.
const GAP: usize = 1;
/// The rail's left gutter: the card starts one cell in from the transcript.
const GUTTER: usize = 2;

/// Which card: the plugin that published it, and the kind it published under.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CardId {
    pub plugin: String,
    pub kind: String,
}

impl CardId {
    fn new(plugin: &str, kind: &str) -> Self {
        Self {
            plugin: plugin.to_string(),
            kind: kind.to_string(),
        }
    }
}

/// A panel a person pinned into the rail, in one session (§8: pinning is the
/// TUI's, not the plugin's).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pin {
    pub session: SessionId,
    pub card: CardId,
}

/// One card: what it is, what it is called, and the view it draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: CardId,
    pub title: String,
    pub body: View,
}

/// One card as it was drawn.
pub struct Drawn {
    pub id: CardId,
    pub lines: Vec<Line<'static>>,
}

/// What the rail holds for the session in view: the panels a person pinned,
/// then every live signal. A signal needs no pinning — it is on the screen
/// because it is happening.
pub fn cards(state: &SessionState, session: &SessionId, pinned: &BTreeSet<Pin>) -> Vec<Card> {
    let mut out: Vec<Card> = published(&state.extensions)
        .filter(|card| {
            pinned.contains(&Pin {
                session: session.clone(),
                card: card.id.clone(),
            })
        })
        .collect();
    out.extend(published(&state.signals));
    out
}

/// Every kind a plugin published in one lane, as a card.
fn published(
    lanes: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>>,
) -> impl Iterator<Item = Card> + '_ {
    lanes.iter().flat_map(|(plugin, kinds)| {
        kinds
            .iter()
            .map(|(kind, payload)| card(plugin, kind, payload))
    })
}

/// A payload as a card: a `Panel` lends the card its own title, anything else
/// is titled by the kind it was published under.
fn card(plugin: &str, kind: &str, payload: &Value) -> Card {
    let id = CardId::new(plugin, kind);
    match panel::view_of(payload) {
        View::Panel { title, child } => Card {
            id,
            title,
            body: *child,
        },
        body => Card {
            id,
            title: kind.to_string(),
            body,
        },
    }
}

/// How wide a card's content is: the rail's, less its gutter, or the whole
/// transcript when there is no rail to put the cards in (design §3).
pub fn width(rail: Option<Rect>, transcript: Rect) -> usize {
    match rail {
        Some(rail) => usize::from(rail.width).saturating_sub(GUTTER),
        None => usize::from(transcript.width),
    }
}

/// The cards as rows: a title the focus cursor marks, then the view, with at
/// most [`ROWS`] rows to a plugin and a fold saying what was left out.
pub fn render(cards: &[Card], width: usize, focus: Option<&CardId>, marks: &Marks) -> Vec<Drawn> {
    let mut spent: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    cards
        .iter()
        .map(|card| {
            let used = spent.entry(card.id.plugin.as_str()).or_default();
            let budget = ROWS.saturating_sub(*used);
            let lines = capped(rows(card, width, focus, marks), budget);
            *used += lines.len();
            Drawn {
                id: card.id.clone(),
                lines,
            }
        })
        .collect()
}

fn rows(card: &Card, width: usize, focus: Option<&CardId>, marks: &Marks) -> Vec<Line<'static>> {
    let focused = focus == Some(&card.id);
    let mut out = vec![Line::from(vec![
        theme::cursor_span(focused),
        Span::styled(card.title.clone(), theme::text().patch(theme::bold())),
    ])];
    out.extend(views::marked(&card.body, width, marks));
    out
}

/// What is left of a card once its plugin's rows have run out.
fn capped(lines: Vec<Line<'static>>, budget: usize) -> Vec<Line<'static>> {
    if lines.len() <= budget {
        return lines;
    }
    let kept = budget.saturating_sub(1);
    let hidden = lines.len() - kept;
    let mut out: Vec<Line<'static>> = lines.into_iter().take(kept).collect();
    out.push(Line::from(Span::styled(
        format!("{} +{hidden} lines", theme::ellipsis()),
        theme::dim(),
    )));
    out
}

/// The rail's rows, on the raised tint that gives the frame its depth, and
/// where each card landed — what a click on the rail is answered against.
pub fn painted(drawn: &[Drawn], width: usize) -> (Vec<Line<'static>>, Vec<(CardId, Range<usize>)>) {
    let inner = width.saturating_sub(GUTTER);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut where_ = Vec::new();
    for card in drawn {
        if !lines.is_empty() {
            lines.extend(std::iter::repeat_n(Line::default(), GAP));
        }
        let from = lines.len();
        lines.extend(card.lines.iter().map(|line| tinted(line.clone(), inner)));
        where_.push((card.id.clone(), from..lines.len()));
    }
    (lines, where_)
}

/// The same cards under the running rows, where there is no rail to put them
/// in: no tint, because the transcript's ground is the terminal's own (§4).
pub fn inline(drawn: &[Drawn]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for card in drawn {
        if !lines.is_empty() {
            lines.extend(std::iter::repeat_n(Line::default(), GAP));
        }
        lines.extend(card.lines.iter().cloned());
    }
    lines
}

/// One row of a card: the gutter, then the row, on the raised tint for the
/// whole of the card's width so it reads as a surface and not as a sentence.
fn tinted(line: Line<'static>, width: usize) -> Line<'static> {
    let mut out = views::fit(views::indent(line, GUTTER), width + GUTTER);
    out.style = theme::raised();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{child_id, extended, folded, frame, progress_view, signalled};
    use serde_json::json;

    fn ids(cards: &[Card]) -> Vec<String> {
        cards
            .iter()
            .map(|card| format!("{}/{}", card.id.plugin, card.id.kind))
            .collect()
    }

    fn session() -> SessionId {
        SessionId::from_raw("ses_1")
    }

    fn pin(plugin: &str, kind: &str) -> Pin {
        Pin {
            session: session(),
            card: CardId::new(plugin, kind),
        }
    }

    #[test]
    fn a_signal_is_a_card_and_an_extension_is_one_only_once_it_is_pinned() {
        let state = folded(vec![
            frame(1, extended("bingo.demo.ui", "board", json!([{"id": 1}]))),
            frame(
                2,
                signalled("bingo.demo.ui", "progress", progress_view(3, 10)),
            ),
        ]);
        assert_eq!(
            ids(&cards(&state, &session(), &BTreeSet::new())),
            vec!["bingo.demo.ui/progress".to_string()],
            "nothing is pinned yet, and a signal never needs to be"
        );
        let pinned = BTreeSet::from([pin("bingo.demo.ui", "board")]);
        assert_eq!(
            ids(&cards(&state, &session(), &pinned)),
            vec![
                "bingo.demo.ui/board".to_string(),
                "bingo.demo.ui/progress".to_string(),
            ],
            "the pinned panel comes first; the live cards follow"
        );
    }

    #[test]
    fn a_pin_belongs_to_the_session_it_was_made_in() {
        let state = folded(vec![frame(
            1,
            extended("bingo.demo.ui", "board", json!([{"id": 1}])),
        )]);
        let elsewhere = BTreeSet::from([Pin {
            session: child_id(),
            card: CardId::new("bingo.demo.ui", "board"),
        }]);
        assert!(cards(&state, &session(), &elsewhere).is_empty());
    }

    #[test]
    fn a_panel_lends_the_card_its_title_and_anything_else_is_titled_by_kind() {
        let state = folded(vec![
            frame(
                1,
                signalled(
                    "bingo.demo.ui",
                    "board",
                    serde_json::to_value(View::Panel {
                        title: "Board".into(),
                        child: Box::new(View::text("empty")),
                    })
                    .expect("a view"),
                ),
            ),
            frame(2, signalled("bingo.demo.ui", "progress", json!("running"))),
        ]);
        let cards = cards(&state, &session(), &BTreeSet::new());
        assert_eq!(cards[0].title, "Board");
        assert_eq!(cards[0].body, View::text("empty"));
        assert_eq!(cards[1].title, "progress");
    }

    #[test]
    fn one_plugin_takes_eight_rows_and_folds_the_rest() {
        let long = View::List {
            items: (1..=20).map(|i| format!("item {i}")).collect(),
        };
        let cards = vec![Card {
            id: CardId::new("bingo.demo.ui", "list"),
            title: "list".into(),
            body: long,
        }];
        let drawn = render(&cards, 22, None, &Marks::default());
        assert_eq!(drawn[0].lines.len(), ROWS);
        assert_eq!(
            drawn[0].lines[ROWS - 1].to_string(),
            "… +14 lines",
            "the title and seven items are shown; the rest says how many"
        );
    }

    #[test]
    fn two_plugins_each_get_their_own_eight_rows() {
        let cards: Vec<Card> = ["a", "b"]
            .into_iter()
            .map(|plugin| Card {
                id: CardId::new(plugin, "list"),
                title: "list".into(),
                body: View::List {
                    items: (1..=20).map(|i| format!("item {i}")).collect(),
                },
            })
            .collect();
        let drawn = render(&cards, 22, None, &Marks::default());
        assert_eq!(
            drawn.iter().map(|d| d.lines.len()).collect::<Vec<_>>(),
            [ROWS, ROWS]
        );
    }

    #[test]
    fn the_focused_card_is_the_one_the_cursor_marks() {
        let cards = vec![
            Card {
                id: CardId::new("a", "one"),
                title: "one".into(),
                body: View::text("x"),
            },
            Card {
                id: CardId::new("a", "two"),
                title: "two".into(),
                body: View::text("y"),
            },
        ];
        let focus = CardId::new("a", "two");
        let drawn = render(&cards, 22, Some(&focus), &Marks::default());
        assert!(!drawn[0].lines[0].to_string().contains('❯'));
        assert!(drawn[1].lines[0].to_string().contains('❯'));
    }

    #[test]
    fn every_row_of_the_rail_is_exactly_as_wide_as_the_rail() {
        let cards = vec![Card {
            id: CardId::new("a", "one"),
            title: "one".into(),
            body: View::text("a line that is much longer than the rail is wide"),
        }];
        let (lines, where_) = painted(&render(&cards, 22, None, &Marks::default()), 24);
        for line in &lines {
            assert_eq!(line.width(), 24, "{line:?}");
        }
        assert_eq!(where_[0].1, 0..2);
    }
}
