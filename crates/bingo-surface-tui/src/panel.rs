//! The `ctrl+t` sheet: the viewed session's plugin-owned state, as the reducer
//! keeps it (ADR-0011 §2). The surface knows no plugin and no kind — a payload
//! that parses as a `View` is drawn as one (ADR-0013 §2) and any other is
//! drawn for its shape alone, so a plugin that ships tomorrow shows up here
//! with nothing added. It is derived from `SessionState` at render time, so it
//! follows the view wherever it goes.
//!
//! It is also where a panel is pinned into the rail: `⏎` on a row pins it,
//! `⏎` again takes it back. Where a panel sits is the surface's answer, not
//! the plugin's (ADR-0013 §4).

use std::collections::BTreeSet;

use bingo_sdk::{SessionId, SessionState, View};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::clock::Now;
use crate::rail::{CardId, Pin};
use crate::views::{self, MISSING};
use crate::{theme, window};

/// What a session with no plugin state says.
pub const NOTHING: &str = "nothing to show";
/// What a row that is in the rail says of itself.
const PINNED: &str = "pinned";
/// How far a panel's view hangs under the row that names it.
const INDENT: usize = 2;

/// What the sheet is showing, in the order it draws them: one row per kind a
/// plugin published, whether or not anybody pinned it.
pub fn rows(state: &SessionState) -> Vec<CardId> {
    state
        .extensions
        .iter()
        .flat_map(|(plugin, kinds)| {
            kinds.keys().map(|kind| CardId {
                plugin: plugin.clone(),
                kind: kind.clone(),
            })
        })
        .collect()
}

/// The sheet: every kind, its view under it, and the cursor on the one `⏎`
/// would pin. A sheet with more in it than `room` starts at the row the cursor
/// is on, so what it names is read with the view that belongs to it.
pub fn lines(
    state: &SessionState,
    session: &SessionId,
    cursor: usize,
    pinned: &BTreeSet<Pin>,
    width: usize,
    room: usize,
    now: Now,
) -> Vec<Line<'static>> {
    let rows = rows(state);
    if rows.is_empty() {
        return vec![Line::from(Span::styled(NOTHING.to_string(), theme::dim()))];
    }
    let mut out = Vec::new();
    let mut at_cursor = 0;
    for (at, id) in rows.iter().enumerate() {
        if !out.is_empty() {
            out.push(Line::default());
        }
        if at == cursor {
            at_cursor = out.len();
        }
        out.push(heading(id, at == cursor, is_pinned(pinned, session, id)));
        out.extend(body(state, id, width, now));
    }
    window::onward(out, at_cursor, room)
}

fn body(state: &SessionState, id: &CardId, width: usize, now: Now) -> Vec<Line<'static>> {
    let payload = state
        .extensions
        .get(&id.plugin)
        .and_then(|kinds| kinds.get(&id.kind));
    let view = payload.map(view_of).unwrap_or_else(|| View::text(MISSING));
    views::marked(&view, width.saturating_sub(INDENT), &views::Marks::at(now))
        .into_iter()
        .map(|line| views::indent(line, INDENT))
        .collect()
}

fn is_pinned(pinned: &BTreeSet<Pin>, session: &SessionId, card: &CardId) -> bool {
    pinned.contains(&Pin {
        session: session.clone(),
        card: card.clone(),
    })
}

fn heading(id: &CardId, focused: bool, pinned: bool) -> Line<'static> {
    let mut spans = vec![
        theme::cursor_span(focused),
        Span::styled(
            format!("{} · {}", id.plugin, id.kind),
            theme::text().patch(theme::bold()),
        ),
    ];
    if pinned {
        spans.push(Span::styled(format!("  {PINNED}"), theme::presence()));
    }
    Line::from(spans)
}

/// A payload as a view: the vocabulary when it parses as one (ADR-0013 §2),
/// else the shape it is — a list of records is a table over the union of
/// their keys, a record is its fields, anything else is its text.
pub fn view_of(payload: &Value) -> View {
    if let Ok(view) = serde_json::from_value::<View>(payload.clone()) {
        return view;
    }
    match payload {
        Value::Array(items) if items.iter().any(Value::is_object) => table(items),
        Value::Object(fields) => View::Text {
            text: fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", cell(value)))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        other => View::Text { text: cell(other) },
    }
}

fn table(items: &[Value]) -> View {
    let headers = columns(items);
    View::Table {
        rows: items.iter().map(|item| row(item, &headers)).collect(),
        headers,
    }
}

/// Every key any record carries, in the order the records first name them.
fn columns(items: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for fields in items.iter().filter_map(Value::as_object) {
        for key in fields.keys() {
            if !out.iter().any(|seen| seen == key) {
                out.push(key.clone());
            }
        }
    }
    out
}

fn row(item: &Value, columns: &[String]) -> Vec<String> {
    let Some(fields) = item.as_object() else {
        // A record among scalars: the value is all there is to show.
        return vec![cell(item)];
    };
    columns
        .iter()
        .map(|key| fields.get(key).map(cell).unwrap_or_else(|| MISSING.into()))
        .collect()
}

/// One value as a person reads it: text as itself, a nested value as the
/// compact JSON it is, nothing as a dash.
fn cell(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => MISSING.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use serde_json::json;

    fn text_of(view: &View) -> String {
        views::render(view, 60)
            .iter()
            .map(|line| line.to_string().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The room a sheet at 80×24 has, which is more than these fixtures fill.
    const ROOM: usize = 20;

    fn sheet(state: &SessionState) -> Vec<String> {
        lines(
            state,
            &SessionId::from_raw("ses_1"),
            0,
            &BTreeSet::new(),
            60,
            ROOM,
            scene().1,
        )
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect()
    }

    #[test]
    fn a_payload_that_parses_as_a_view_is_drawn_as_one() {
        let payload = serde_json::to_value(View::Badge {
            text: "needs you".into(),
            tone: bingo_sdk::Tone::Attention,
        })
        .expect("a view");
        assert_eq!(text_of(&view_of(&payload)), "[ needs you ]");
    }

    /// A journaled payload whose word this binary predates is read as its
    /// fold (ADR-0038 §2) — the parse degrades where the unknown word is
    /// instead of failing, so the row is drawn rather than dropped.
    #[test]
    fn a_payload_whose_kind_this_surface_never_learned_is_its_fold() {
        let payload = json!({"kind": "chart.candles", "series": [1, 2], "fold": "AAPL 1 2"});
        assert_eq!(text_of(&view_of(&payload)), "AAPL 1 2");
        assert_eq!(
            text_of(&view_of(&json!({"kind": "chart.candles"}))),
            "[chart.candles]",
            "a node that forgot its fold still says which word it was"
        );
    }

    #[test]
    fn a_list_of_records_is_a_table_over_the_union_of_their_keys() {
        let view = view_of(&json!([
            {"id": 1, "status": "pending", "subject": "write the plan"},
            {"id": 2, "status": "in_progress", "subject": "ship it", "owner": "reviewer"},
        ]));
        assert_eq!(
            view,
            View::Table {
                headers: vec![
                    "id".into(),
                    "status".into(),
                    "subject".into(),
                    "owner".into()
                ],
                rows: vec![
                    vec![
                        "1".into(),
                        "pending".into(),
                        "write the plan".into(),
                        MISSING.into()
                    ],
                    vec![
                        "2".into(),
                        "in_progress".into(),
                        "ship it".into(),
                        "reviewer".into()
                    ],
                ],
            },
            "the key only the second record carries is a column all the same"
        );
    }

    #[test]
    fn a_record_is_its_fields_one_to_a_line() {
        assert_eq!(
            view_of(&json!({"name": "#design", "open": true})),
            View::Text {
                text: "name: #design\nopen: true".into()
            }
        );
    }

    #[test]
    fn a_nested_value_is_its_compact_json() {
        assert_eq!(
            view_of(&json!({"members": ["reviewer", "scout"]})),
            View::Text {
                text: r#"members: ["reviewer","scout"]"#.into()
            }
        );
        assert_eq!(
            text_of(&view_of(&json!([{"id": 1, "meta": {"tags": ["a"]}}]))),
            "id  meta\n──────────────────\n 1  {\"tags\":[\"a\"]}"
        );
    }

    #[test]
    fn anything_else_is_its_text() {
        assert_eq!(
            view_of(&json!("posted")),
            View::Text {
                text: "posted".into()
            }
        );
        assert_eq!(view_of(&json!(3)), View::Text { text: "3".into() });
        assert_eq!(
            view_of(&json!(null)),
            View::Text {
                text: MISSING.into()
            }
        );
        assert_eq!(
            view_of(&json!(["a", "b"])),
            View::Text {
                text: r#"["a","b"]"#.into()
            },
            "a list with no record in it has no columns to be a table over"
        );
    }

    #[test]
    fn a_session_no_plugin_has_written_to_says_so() {
        let drawn = sheet(&state());
        assert_eq!(drawn, vec![NOTHING.to_string()]);
    }

    #[test]
    fn every_plugin_and_kind_is_a_row_of_its_own() {
        let roster = json!({
            "members": ["scout"],
            "kind": "tree",
            "nodes": [{"label": "scout", "tone": "neutral"}],
        });
        let state = folded(vec![
            frame(1, extended("bingo.tasks", "tasks", json!([{"id": 1}]))),
            frame(2, extended("bingo.rooms", "members", roster)),
        ]);
        assert_eq!(
            sheet(&state),
            vec![
                "❯ bingo.rooms · members".to_string(),
                "  └─ scout".to_string(),
                String::new(),
                "  bingo.tasks · tasks".to_string(),
                "  id".to_string(),
                "  ──".to_string(),
                "   1".to_string(),
            ],
            "a payload that parses as a view is drawn as one, whatever else \
             the plugin put in it"
        );
    }

    #[test]
    fn a_pinned_row_says_so_and_only_in_the_session_it_was_pinned_in() {
        let state = folded(vec![frame(
            1,
            extended("bingo.tasks", "tasks", json!([{"id": 1}])),
        )]);
        let pinned = BTreeSet::from([Pin {
            session: SessionId::from_raw("ses_1"),
            card: CardId {
                plugin: "bingo.tasks".into(),
                kind: "tasks".into(),
            },
        }]);
        let here = lines(
            &state,
            &SessionId::from_raw("ses_1"),
            0,
            &pinned,
            60,
            ROOM,
            scene().1,
        );
        assert!(here[0].to_string().contains(PINNED), "{:?}", here[0]);
        let elsewhere = lines(&state, &child_id(), 0, &pinned, 60, ROOM, scene().1);
        assert!(!elsewhere[0].to_string().contains(PINNED));
    }
}
