//! The `ctrl+t` panel: the viewed session's plugin-owned state, as the reducer
//! keeps it (ADR-0011 §2). The surface knows no plugin and no kind — a payload
//! is drawn for its shape alone, so a plugin that ships tomorrow shows up here
//! with nothing added. It is derived from `SessionState` at render time, so it
//! follows the view wherever it goes.

use bingo_sdk::{SessionState, View};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::{block, theme};

/// What a session with no plugin state says.
pub const NOTHING: &str = "nothing to show";
/// A value that is not there.
const MISSING: &str = "–";

pub fn lines(state: &SessionState) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (plugin, kinds) in &state.extensions {
        for (kind, payload) in kinds {
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.push(heading(plugin, kind));
            out.extend(block::lines(&view_of(payload)));
        }
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled(NOTHING.to_string(), theme::dim())));
    }
    out
}

fn heading(plugin: &str, kind: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("{plugin} · {kind}"),
        theme::text().patch(theme::bold()),
    ))
}

/// A payload as the shape it is: a list of records is a table over the union
/// of their keys, a record is its fields, anything else is its text.
pub fn view_of(payload: &Value) -> View {
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
        block::lines(view)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
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
            "id  meta\n1   {\"tags\":[\"a\"]}"
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
        let drawn = lines(&state());
        assert_eq!(drawn.len(), 1);
        assert_eq!(drawn[0].to_string(), NOTHING);
    }

    #[test]
    fn every_plugin_and_kind_is_a_heading_of_its_own() {
        let state = folded(vec![
            frame(1, extended("bingo.tasks", "tasks", json!([{"id": 1}]))),
            frame(
                2,
                extended("bingo.rooms", "members", json!({"members": ["scout"]})),
            ),
        ]);
        let drawn: Vec<String> = lines(&state).iter().map(ToString::to_string).collect();
        assert_eq!(
            drawn,
            vec![
                "bingo.rooms · members".to_string(),
                r#"members: ["scout"]"#.to_string(),
                String::new(),
                "bingo.tasks · tasks".to_string(),
                "id".to_string(),
                "1".to_string(),
            ]
        );
    }
}
