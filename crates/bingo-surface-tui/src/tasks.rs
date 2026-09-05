//! The session's task list, where Claude Code keeps it (M74): under the
//! activity row while a turn runs, in that row's place between turns, and
//! never in a sheet of its own. The task being done lends the activity row
//! its verb — `✻ Writing the plan…` — and the four calls that move the list
//! draw no row in the transcript, because the list *is* their row.
//!
//! Nothing here is published and nothing is stored. The list is read at
//! render time out of what `bingo-tasks` already writes into the session's
//! journal — the extension `bingo.tasks`/`tasks`, in Claude Code's own task
//! shape (ADR-0011 §2) — so it follows the view wherever it goes: a room's
//! board is a room's own list, read by this same code from the room's view.
//!
//! A surface may not import a plugin (ADR-0001), so the names below are the
//! whole of the contract between them, and the payload is read as data: a
//! record this does not recognise is left out rather than guessed at, as
//! [`crate::seats`] reads a room's roster.

use bingo_sdk::{Item, ItemBody, ItemStatus, SessionState};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::{theme, views};

/// The plugin whose journal the list is read out of, and the kind the whole
/// list is published under.
const PLUGIN: &str = "bingo.tasks";
const KIND: &str = "tasks";
/// The four calls whose whole effect is the list on the screen.
const CALLS: [&str; 4] = ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"];
/// The argument that sends one of them to a room's board instead
/// (ADR-0023 §1): its effect is then on the room's screen, not this one.
const ELSEWHERE: &str = "in";
/// The fields of one task, as Claude Code spells them.
const SUBJECT: &str = "subject";
const ACTIVE_FORM: &str = "activeForm";
const STATUS: &str = "status";
const OWNER: &str = "owner";
/// How many rows the list may take before the rest is counted instead:
/// Claude Code's five, which is also a result's fold.
pub const ROWS: usize = 5;
/// Where the rows stand between turns: the transcript's text column.
const INDENT: usize = 2;

/// Where a task stands, in the order the marks are kept in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    Completed,
}

impl Status {
    fn of(word: &str) -> Option<Self> {
        match word {
            "pending" => Some(Status::Pending),
            "in_progress" => Some(Status::InProgress),
            "completed" => Some(Status::Completed),
            _ => None,
        }
    }

    fn glyph(self) -> &'static str {
        theme::tasks()[self as usize]
    }
}

/// One task as the list has it: what the row says, and nothing the row does
/// not say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub subject: String,
    pub active_form: Option<String>,
    pub status: Status,
    pub owner: Option<String>,
}

impl Task {
    fn open(&self) -> bool {
        self.status != Status::Completed
    }
}

/// The list as the session's journal has it, in the order it was written:
/// every record of the payload that reads as a task, and nothing at all where
/// there is no list.
pub fn of(state: &SessionState) -> Vec<Task> {
    state
        .extensions
        .get(PLUGIN)
        .and_then(|kinds| kinds.get(KIND))
        .and_then(Value::as_array)
        .map(|records| records.iter().filter_map(task).collect())
        .unwrap_or_default()
}

fn task(record: &Value) -> Option<Task> {
    let field = |name: &str| record.get(name).and_then(Value::as_str);
    Some(Task {
        subject: field(SUBJECT)?.to_string(),
        active_form: field(ACTIVE_FORM).map(str::to_string),
        status: Status::of(field(STATUS)?)?,
        owner: field(OWNER).map(str::to_string),
    })
}

/// Whether a plugin's kind is the list: what the panel sheet leaves out,
/// because it is drawn here and a thing drawn twice is two things to read.
pub fn is_list(plugin: &str, kind: &str) -> bool {
    plugin == PLUGIN && kind == KIND
}

/// Whether an item is a call this surface draws no row for: one of the four,
/// on the session's own list, that did not come back wrong. One sent to a
/// board did its work on another screen, and one that failed has something
/// to say the list cannot — both draw as any tool row.
pub fn quiet(item: &Item) -> bool {
    let ItemBody::ToolCall {
        name,
        input,
        output,
        ..
    } = &item.body
    else {
        return false;
    };
    let own = input.get(ELSEWHERE).is_none_or(Value::is_null);
    let wrong = item.status == ItemStatus::Failed || output.as_ref().is_some_and(|o| o.is_error);
    CALLS.contains(&name.as_str()) && own && !wrong
}

/// What the turn is doing, in the task's own words: the `activeForm` of the
/// first task in progress, else its subject — Claude Code's rule — and
/// nothing where nothing is in progress, so bingo's own verb stands.
pub fn doing(tasks: &[Task]) -> Option<&str> {
    let task = tasks
        .iter()
        .find(|task| task.status == Status::InProgress)?;
    Some(task.active_form.as_deref().unwrap_or(&task.subject))
}

/// `3 tasks (1 done, 1 in progress, 1 open)`: the row that stands where the
/// verb row stood, once the turn is over. Dim, the counts bold, so the
/// numbers are what a glance finds.
pub fn summary(tasks: &[Task]) -> Line<'static> {
    let count = |status| tasks.iter().filter(|task| task.status == status).count();
    Line::from(vec![
        bold(tasks.len()),
        dim(format!(" {} (", plural(tasks.len(), "task"))),
        bold(count(Status::Completed)),
        dim(" done, ".into()),
        bold(count(Status::InProgress)),
        dim(" in progress, ".into()),
        bold(count(Status::Pending)),
        dim(" open)".into()),
    ])
}

fn bold(count: usize) -> Span<'static> {
    Span::styled(count.to_string(), theme::dim().patch(theme::bold()))
}

fn dim(text: String) -> Span<'static> {
    Span::styled(text, theme::dim())
}

fn plural(count: usize, word: &str) -> String {
    match count {
        1 => word.to_string(),
        _ => format!("{word}s"),
    }
}

/// The rows of the list: every task in list order when they fit in [`ROWS`],
/// else the open ones in list order to that many and one dim line counting
/// what was left out — the done ones first, because what is left to do is
/// what the rows are for.
pub fn rows(tasks: &[Task], width: usize) -> Vec<Line<'static>> {
    let shown: Vec<&Task> = match tasks.len() <= ROWS {
        true => tasks.iter().collect(),
        false => tasks.iter().filter(|task| task.open()).take(ROWS).collect(),
    };
    let mut out: Vec<Line<'static>> = shown.iter().map(|task| row(task, width)).collect();
    out.extend(cut(tasks, &shown));
    out
}

/// `◼ Ship it — reviewer`: the mark says where the task stands, the words wear
/// the same fact — bold while it is being done, struck through once it is
/// done — so `NO_COLOR` still reads the list.
fn row(task: &Task, width: usize) -> Line<'static> {
    let (mark, words) = match task.status {
        Status::Pending => (theme::text(), theme::text()),
        Status::InProgress => (theme::presence(), theme::text().patch(theme::bold())),
        Status::Completed => (theme::good(), theme::dim().patch(theme::struck())),
    };
    let glyph = task.status.glyph();
    let room = width.saturating_sub(glyph.width() + 1);
    let mut spans = vec![
        Span::styled(format!("{glyph} "), mark),
        Span::styled(views::clip(&task.subject, room), words),
    ];
    if let Some(owner) = &task.owner {
        spans.push(dim(format!(" — {owner}")));
    }
    Line::from(spans)
}

/// `… +6 pending, 1 completed`: what the rows did not show, by where it
/// stands. Nothing at all when every task is a row.
fn cut(tasks: &[Task], shown: &[&Task]) -> Option<Line<'static>> {
    let hidden = tasks.len() - shown.len();
    if hidden == 0 {
        return None;
    }
    let done = tasks.iter().filter(|task| !task.open()).count();
    let mut parts = Vec::new();
    if hidden > done {
        parts.push(format!("+{} pending", hidden - done));
    }
    if done > 0 {
        parts.push(format!("{done} completed"));
    }
    Some(Line::from(dim(format!(
        "{} {}",
        theme::ellipsis(),
        parts.join(", ")
    ))))
}

/// The rows under the verb row while the turn runs: hung from a `⎿`, as
/// what a call has printed so far hangs under its row.
pub fn hung(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let connector = format!("  {}  ", theme::connector());
    let indent = " ".repeat(connector.chars().count());
    rows.into_iter()
        .enumerate()
        .map(|(at, row)| match at {
            0 => led(dim(connector.clone()), row),
            _ => led(Span::raw(indent.clone()), row),
        })
        .collect()
}

/// The summary and the rows under it between turns: standing in the
/// transcript's text column, with no mark — nothing is running for them to
/// hang from.
pub fn standing(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|row| led(Span::raw(" ".repeat(INDENT)), row))
        .collect()
}

fn led(lead: Span<'static>, row: Line<'static>) -> Line<'static> {
    let mut spans = vec![lead];
    spans.extend(row.spans);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::ToolOutput;
    use serde_json::json;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn listed(records: Value) -> SessionState {
        folded(vec![frame(1, extended("bingo.tasks", "tasks", records))])
    }

    fn three() -> Vec<Task> {
        of(&listed(json!([
            {"id": 1, "status": "completed", "subject": "write the plan", "activeForm": "writing the plan"},
            {"id": 2, "status": "in_progress", "subject": "ship it", "activeForm": "shipping it", "owner": "reviewer"},
            {"id": 3, "status": "pending", "subject": "celebrate"},
        ])))
    }

    /// Twelve tasks, one done and one being done, the rest to do.
    fn twelve() -> Vec<Task> {
        let mut records = vec![
            json!({"id": 1, "status": "completed", "subject": "write the plan"}),
            json!({"id": 2, "status": "in_progress", "subject": "ship it"}),
        ];
        records.extend(
            (3..=12).map(|n| json!({"id": n, "status": "pending", "subject": format!("task {n}")})),
        );
        of(&listed(Value::Array(records)))
    }

    #[test]
    fn the_list_is_read_out_of_the_journal_in_the_order_it_was_written() {
        let tasks = three();
        assert_eq!(
            tasks.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(),
            ["write the plan", "ship it", "celebrate"]
        );
        assert_eq!(tasks[1].status, Status::InProgress);
        assert_eq!(tasks[1].owner.as_deref(), Some("reviewer"));
        assert_eq!(tasks[2].active_form, None);
    }

    /// A record that is not a task is left out; a payload that is not a list
    /// is no list; another plugin's list is not this one.
    #[test]
    fn what_is_not_a_task_is_not_on_the_list() {
        assert!(of(&state()).is_empty());
        assert!(of(&listed(json!({"tasks": "later"}))).is_empty());
        let odd = of(&listed(json!([
            {"id": 1, "subject": "no status"},
            {"id": 2, "status": "done", "subject": "a status this surface never learned"},
            {"id": 3, "status": "pending"},
            {"id": 4, "status": "pending", "subject": "kept"},
        ])));
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].subject, "kept");
        let other = folded(vec![frame(
            1,
            extended(
                "bingo.rooms",
                "tasks",
                json!([{"status": "pending", "subject": "x"}]),
            ),
        )]);
        assert!(of(&other).is_empty());
        assert!(is_list("bingo.tasks", "tasks"));
        assert!(!is_list("bingo.tasks", "board"));
    }

    #[test]
    fn the_verb_is_the_task_being_done_in_its_own_words() {
        assert_eq!(doing(&three()), Some("shipping it"));
        let bare = of(&listed(json!([
            {"status": "completed", "subject": "write the plan"},
            {"status": "in_progress", "subject": "ship it"},
        ])));
        assert_eq!(
            doing(&bare),
            Some("ship it"),
            "without an activeForm, the subject"
        );
        let idle = of(&listed(
            json!([{"status": "pending", "subject": "ship it"}]),
        ));
        assert_eq!(
            doing(&idle),
            None,
            "nothing in progress is bingo's own verb"
        );
    }

    #[test]
    fn the_summary_counts_by_where_each_task_stands() {
        assert_eq!(
            super::summary(&three()).to_string(),
            "3 tasks (1 done, 1 in progress, 1 open)"
        );
        assert_eq!(
            super::summary(&twelve()).to_string(),
            "12 tasks (1 done, 1 in progress, 10 open)"
        );
        let one = of(&listed(
            json!([{"status": "pending", "subject": "ship it"}]),
        ));
        assert_eq!(
            super::summary(&one).to_string(),
            "1 task (0 done, 0 in progress, 1 open)"
        );
    }

    #[test]
    fn a_list_that_fits_is_every_row_in_list_order() {
        assert_eq!(
            texts(&rows(&three(), 80)),
            ["✔ write the plan", "◼ ship it — reviewer", "◻ celebrate"]
        );
    }

    /// Claude Code's own cut, read off its screen: five rows of what is open,
    /// the done ones counted rather than drawn.
    #[test]
    fn a_long_list_is_five_open_rows_and_a_count_of_the_rest() {
        let drawn = texts(&rows(&twelve(), 80));
        assert_eq!(drawn.len(), ROWS + 1);
        assert_eq!(drawn[0], "◼ ship it");
        assert_eq!(drawn[4], "◻ task 6");
        assert_eq!(drawn[5], "… +6 pending, 1 completed");
    }

    /// When what is open fits, every open task is a row and only the done
    /// ones are counted — the end of a piece of work reads as what is left.
    #[test]
    fn when_only_the_done_ones_are_cut_the_count_says_so() {
        let mut records: Vec<Value> = (1..=6)
            .map(|n| json!({"status": "completed", "subject": format!("done {n}")}))
            .collect();
        records.push(json!({"status": "pending", "subject": "last"}));
        let drawn = texts(&rows(&of(&listed(Value::Array(records))), 80));
        assert_eq!(drawn, ["◻ last", "… 6 completed"]);
    }

    #[test]
    fn a_subject_wider_than_the_band_is_cut_with_the_ellipsis() {
        let long = of(&listed(
            json!([{"status": "pending", "subject": "a".repeat(40)}]),
        ));
        let drawn = texts(&rows(&long, 20));
        assert_eq!(drawn[0].chars().count(), 20, "{}", drawn[0]);
        assert!(drawn[0].ends_with('…'), "{}", drawn[0]);
    }

    /// Where the rows stand is the caller's: under a `⎿` while the turn runs,
    /// at the transcript's indent between turns, and the same rows either way.
    #[test]
    fn the_rows_hang_from_the_connector_while_a_turn_runs_and_stand_between_turns() {
        let three = three();
        assert_eq!(
            texts(&hung(rows(&three, 80))),
            [
                "  ⎿  ✔ write the plan",
                "     ◼ ship it — reviewer",
                "     ◻ celebrate"
            ]
        );
        assert_eq!(
            texts(&standing(rows(&three, 80))),
            [
                "  ✔ write the plan",
                "  ◼ ship it — reviewer",
                "  ◻ celebrate"
            ]
        );
    }

    /// The marks carry the fact in ASCII too, out of the six characters §7
    /// allows.
    #[test]
    fn in_ascii_the_marks_are_a_dash_a_bullet_and_a_cross() {
        let drawn = crate::painted::in_look(crate::painted::ascii(), || {
            let mut lines = texts(&rows(&three(), 80));
            lines.push(texts(&rows(&twelve(), 80))[5].clone());
            lines.join("\n")
        });
        assert_eq!(
            drawn,
            "x write the plan\n* ship it — reviewer\n- celebrate\n... +6 pending, 1 completed"
        );
    }

    fn call(name: &str, input: Value, output: Option<ToolOutput>, status: ItemStatus) -> Item {
        tool("itm_1", name, input, output, status)
    }

    /// One of the four, on the session's own list, that worked: the list is
    /// its row. Anything else is drawn.
    #[test]
    fn a_task_call_on_the_own_list_that_worked_is_quiet_and_nothing_else_is() {
        let ok = Some(ToolOutput::text("Created #1: write the plan"));
        for name in CALLS {
            assert!(
                quiet(&call(name, json!({}), ok.clone(), ItemStatus::Completed)),
                "{name}"
            );
        }
        assert!(
            quiet(&call(
                "TaskCreate",
                json!({"subject": "x", "in": null}),
                ok.clone(),
                ItemStatus::Completed
            )),
            "an `in` that names nothing is the own list"
        );
        assert!(
            quiet(&call("TaskCreate", json!({}), None, ItemStatus::Running)),
            "and so is one still on its way"
        );
        assert!(!quiet(&call(
            "Read",
            json!({}),
            ok.clone(),
            ItemStatus::Completed
        )));
        assert!(
            !quiet(&call(
                "TaskCreate",
                json!({"subject": "x", "in": "#design"}),
                ok.clone(),
                ItemStatus::Completed
            )),
            "a board is another screen"
        );
        assert!(
            !quiet(&call(
                "TaskUpdate",
                json!({"id": 9}),
                Some(ToolOutput::error("No task #9")),
                ItemStatus::Completed
            )),
            "an error is something the list cannot say"
        );
        assert!(!quiet(&call(
            "TaskList",
            json!({}),
            None,
            ItemStatus::Failed
        )));
        assert!(!quiet(&user("itm_1", "TaskCreate")));
    }
}
