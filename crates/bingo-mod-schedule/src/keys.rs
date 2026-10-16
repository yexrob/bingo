use crate::rows::{self, Row};
use crate::{Confirmation, Cursor, ID, REQUEST, Shared};
use bingo_sdk::*;
use bingo_services::schedule::Row as ScheduleRow;
use serde_json::{Value, json};
pub struct Handler(pub Shared);
fn press(
    cursor: &mut Cursor,
    rows: &[Row],
    shown: Option<&Row>,
    confirming: Option<&Confirmation>,
    key: &str,
) -> Value {
    if key == "esc" && cursor.confirming.take().is_some() {
        return json!({});
    }
    if matches!(key, "esc" | "ctrl+c") {
        return json!({"close":true});
    }
    let at = rows::selected(rows, cursor.selected.as_ref());
    if matches!(key, "up" | "down") {
        let at = at
            .saturating_add_signed(if key == "up" { -1 } else { 1 })
            .min(rows.len().saturating_sub(1));
        cursor.selected = rows.get(at).map(Row::selection);
        cursor.confirming = None;
        return json!({});
    }
    let Some(row) = rows.get(at).filter(|row| Some(*row) == shown) else {
        cursor.confirming = None;
        return json!({"message":"the list changed; look again"});
    };
    if cursor.confirming.is_some() {
        if key == "enter"
            && confirming.is_some_and(|shown| Some(shown.row()) == row.schedule())
            && cursor.confirming.as_ref() == confirming
        {
            let Some(confirmation) = cursor.confirming.take() else {
                return json!({});
            };
            return confirmed(&confirmation);
        }
        return json!({});
    }
    match row {
        Row::Schedule(row) => operation(cursor, row, key),
        Row::Wake(_) if key == "d" => json!({"run":"/wake off"}),
        Row::Wake(_) => json!({}),
    }
}
fn confirmed(confirmation: &Confirmation) -> Value {
    let row = confirmation.row();
    let mut operation = run(confirmation.verb(), &row.id);
    if matches!(confirmation, Confirmation::Confirm(_))
        && let Some(line) = operation["run"].as_str()
    {
        let digest = bingo_services::schedule::confirmation_digest(&row.text, row.mode.as_deref());
        operation["run"] = json!(format!("{line} {digest}"));
    }
    operation
}

fn operation(cursor: &mut Cursor, row: &ScheduleRow, key: &str) -> Value {
    match key {
        "space" | " " => run(if row.enabled { "pause" } else { "resume" }, &row.id),
        "r" => run("run", &row.id),
        "d" => {
            cursor.confirming = Some(Confirmation::Forget(row.clone()));
            json!({})
        }
        "c" if row.by != "you" => {
            cursor.confirming = Some(Confirmation::Confirm(row.clone()));
            json!({})
        }
        "right" => match &row.session {
            Some(session) => json!({"view":session}),
            None => json!({"message":"has not fired yet"}),
        },
        _ => json!({}),
    }
}
/// The command line for `verb` on `id`, if `id` can ride on one. An id is a
/// file's name, and a file a person named by hand could otherwise put words
/// of its own on the line the pane runs.
fn run(verb: &str, id: &str) -> Value {
    match carried(id) {
        true => json!({"run":format!("/schedule {verb} {id}")}),
        false => {
            json!({"message":format!("{id:?} is not an id a command line can carry; rename its file")})
        }
    }
}
/// What a minted id is made of, with room for a person's own name.
fn carried(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}
#[async_trait::async_trait]
impl Command for Handler {
    fn spec(&self) -> CommandSpec {
        CommandSpec::ui(ID)
    }
    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let args: Value = serde_json::from_str(args).map_err(crate::error)?;
        if !render::vouched() || args["handler"] != REQUEST {
            return Ok(CommandOutcome::Handled { handled: true });
        }
        if args["event"] == "close" {
            crate::panel::close(cx, &self.0).await?;
            return Ok(CommandOutcome::Handled { handled: true });
        }
        let shown: Option<Row> =
            serde_json::from_value(args["row"].clone()).map_err(crate::error)?;
        let confirming: Option<Confirmation> =
            serde_json::from_value(args["confirming"].clone()).map_err(crate::error)?;
        let (listing, wake) = crate::listing(&cx.host, &cx.session).await?;
        let rows = rows::rows(&listing, wake.as_ref());
        let operation = {
            let mut shared = self.0.lock().map_err(crate::error)?;
            let Some(cursor) = shared.get_mut(&cx.session) else {
                return Ok(CommandOutcome::Handled { handled: true });
            };
            press(
                cursor,
                &rows,
                shown.as_ref(),
                confirming.as_ref(),
                args["value"]["key"].as_str().unwrap_or_default(),
            )
        };
        if operation["close"] == true || operation.get("view").is_some() {
            crate::panel::close(cx, &self.0).await?;
        }
        cx.host
            .invalidate(&cx.session, vec![REQUEST.into()])
            .await?;
        Ok(CommandOutcome::Surface {
            action: Action {
                name: "surface:pane".into(),
                args: json!({"plugin":ID,"id":REQUEST,"value":operation}),
            },
            fallback: None,
        })
    }
}
#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
