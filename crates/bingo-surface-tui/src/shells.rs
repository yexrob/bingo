//! The shells the session has running in the background (ADR-0018), drawn
//! where Claude Code draws them (M75): a word under the row that started one,
//! a count on the status line, and no card anywhere.
//!
//! Everything here is read off what `bingo-tool-bash` already publishes — the
//! set it signals while any shell runs, and the display a call that went to
//! the background answers with (ADR-0038: a kind the plugin named and this
//! surface has learned). A surface may not import a plugin (ADR-0001), so the
//! names below are the whole of the contract between them, and every payload
//! is read as data: a row this does not recognise is left out, not guessed at.

use bingo_sdk::{Item, ItemBody, SessionState, ToolOutput, View};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::theme;

/// The plugin that runs the shell, and the kind it signals its running set
/// under (ADR-0018 §7).
const PLUGIN: &str = "bingo.tools.bash";
const KIND: &str = "jobs";
/// The custom kind a call that went to the background answers with, and the
/// field of it that names the job.
const STARTED: &str = "job";
const ID: &str = "id";
/// The column of the set's table the ids are in, by the header it is
/// published under.
const JOB: &str = "job";

/// What the row under a backgrounded call says: the shell is still going, or
/// it has been — read from the set, so the row is true after the item closed.
const RUNNING: &str = "Running in the background";
const RAN: &str = "Ran in the background";

/// The ids of the shells the session still has running, in the plugin's
/// order. Nothing while none do: the plugin takes the set away with `Null`.
pub fn running(state: &SessionState) -> Vec<String> {
    let Some(table) = state.signals.get(PLUGIN).and_then(|kinds| kinds.get(KIND)) else {
        return Vec::new();
    };
    let Some(column) = column(table, JOB) else {
        return Vec::new();
    };
    table
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get(column).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Where a header stands in the table's columns.
fn column(table: &Value, header: &str) -> Option<usize> {
    table
        .get("headers")
        .and_then(Value::as_array)?
        .iter()
        .position(|h| h.as_str() == Some(header))
}

/// Whether a plugin's kind is the running set: what the rail leaves out,
/// because the rows and the status line already say it, and a thing drawn
/// twice is two things to read.
pub fn is_set(plugin: &str, kind: &str) -> bool {
    plugin == PLUGIN && kind == KIND
}

/// The job a call's answer names, when the call went to the background.
pub fn started(output: &ToolOutput) -> Option<&str> {
    match output.display.as_ref()? {
        View::Custom { kind, data, .. } if kind == STARTED => data.get(ID)?.as_str(),
        _ => None,
    }
}

/// Whether the shell an item sent to the background is still running, when
/// it sent one: what a memo of the item's block is revised by (M75).
pub fn still(item: &Item, running: &[String]) -> Option<bool> {
    let ItemBody::ToolCall { output, .. } = &item.body else {
        return None;
    };
    let id = started(output.as_ref()?)?;
    Some(running.iter().any(|shell| shell == id))
}

/// The one row under a backgrounded call.
pub fn row(id: &str, running: &[String]) -> Line<'static> {
    let said = match running.iter().any(|shell| shell == id) {
        true => RUNNING,
        false => RAN,
    };
    Line::from(Span::styled(said, theme::dim()))
}

/// `1 shell` / `2 shells` for the status line, while any runs.
pub fn counted(state: &SessionState) -> Option<Span<'static>> {
    let shells = running(state).len();
    let said = match shells {
        0 => return None,
        1 => "1 shell".to_string(),
        n => format!("{n} shells"),
    };
    Some(Span::styled(said, theme::dim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{folded, frame, signalled};
    use serde_json::json;

    fn set(rows: &[(&str, &str)]) -> Value {
        json!({
            "kind": "table",
            "headers": ["job", "command", "since"],
            "rows": rows.iter().map(|(id, command)| json!([id, command, "10:22:07"])).collect::<Vec<_>>(),
        })
    }

    fn with(payload: Value) -> SessionState {
        folded(vec![frame(1, signalled(PLUGIN, KIND, payload))])
    }

    fn shown(id: &str) -> ToolOutput {
        ToolOutput {
            parts: vec![bingo_sdk::ContentPart::text(
                "Started `sleep 45` in the background",
            )],
            is_error: false,
            display: Some(View::Custom {
                kind: STARTED.into(),
                data: json!({ "id": id, "command": "sleep 45" }),
                fold: format!("Started in the background as {id}"),
            }),
        }
    }

    #[test]
    fn the_running_set_is_read_by_its_headers_and_not_by_its_column_order() {
        let state = with(set(&[
            ("job_ab12cd34", "sleep 45"),
            ("job_ff000000", "cargo test"),
        ]));
        assert_eq!(running(&state), vec!["job_ab12cd34", "job_ff000000"]);

        let reordered = with(json!({
            "kind": "table",
            "headers": ["since", "job"],
            "rows": [["10:22:07", "job_ab12cd34"]],
        }));
        assert_eq!(running(&reordered), vec!["job_ab12cd34"]);
    }

    #[test]
    fn a_set_this_does_not_recognise_is_nothing_running() {
        assert!(running(&folded(Vec::new())).is_empty(), "no signal at all");
        assert!(running(&with(json!("running"))).is_empty(), "not a table");
        let headless = with(json!({ "kind": "table", "headers": ["command"], "rows": [["x"]] }));
        assert!(running(&headless).is_empty(), "no job column");
        let short =
            with(json!({ "kind": "table", "headers": ["command", "job"], "rows": [["x"]] }));
        assert!(running(&short).is_empty(), "a row without the column");
    }

    #[test]
    fn the_answer_that_went_to_the_background_names_its_job() {
        assert_eq!(started(&shown("job_ab12cd34")), Some("job_ab12cd34"));
        assert_eq!(started(&ToolOutput::text("Started")), None, "no display");
        let other = ToolOutput {
            display: Some(View::Custom {
                kind: "chart".into(),
                data: json!({ "id": "job_ab12cd34" }),
                fold: "[chart]".into(),
            }),
            ..ToolOutput::text("x")
        };
        assert_eq!(started(&other), None, "another plugin's kind");
    }

    #[test]
    fn a_block_is_revised_by_whether_its_shell_still_runs() {
        use crate::test_support::tool;
        let call = tool(
            "itm_1",
            "Bash",
            json!({ "command": "sleep 45", "background": true }),
            Some(shown("job_ab12cd34")),
            bingo_sdk::ItemStatus::Completed,
        );
        let running = vec!["job_ab12cd34".to_string()];
        assert_eq!(still(&call, &running), Some(true));
        assert_eq!(still(&call, &[]), Some(false));
        let plain = tool(
            "itm_2",
            "Bash",
            json!({ "command": "ls" }),
            Some(ToolOutput::text("a\nb")),
            bingo_sdk::ItemStatus::Completed,
        );
        assert_eq!(still(&plain, &running), None, "no shell was sent anywhere");
    }

    #[test]
    fn the_row_says_running_while_the_set_lists_it_and_ran_after() {
        let running = vec!["job_ab12cd34".to_string()];
        assert_eq!(row("job_ab12cd34", &running).to_string(), RUNNING);
        assert_eq!(row("job_ab12cd34", &[]).to_string(), RAN);
        assert_eq!(row("job_ff000000", &running).to_string(), RAN);
    }

    #[test]
    fn the_count_is_worded_by_number_and_is_nothing_at_none() {
        assert!(counted(&folded(Vec::new())).is_none());
        let one = with(set(&[("job_ab12cd34", "sleep 45")]));
        assert_eq!(
            counted(&one).map(|s| s.content.to_string()),
            Some("1 shell".into())
        );
        let two = with(set(&[
            ("job_ab12cd34", "sleep 45"),
            ("job_ff000000", "cargo test"),
        ]));
        assert_eq!(
            counted(&two).map(|s| s.content.to_string()),
            Some("2 shells".into())
        );
    }

    #[test]
    fn only_the_running_set_is_the_set() {
        assert!(is_set(PLUGIN, KIND));
        assert!(!is_set(PLUGIN, "progress"));
        assert!(!is_set("bingo.demo.ui", KIND));
    }
}
