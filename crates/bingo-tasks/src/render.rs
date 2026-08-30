//! How a task reads, decided once: the model's listing, the reminder in the
//! prompt and the `/tasks` table all name a task the same way, so a person
//! and a model never compare two different lists.

use crate::task::{self, Task};

/// The columns a task list has wherever it is shown as a table.
pub const HEADERS: [&str; 4] = ["id", "status", "subject", "owner"];

/// What a model is told when the list is empty.
pub const NONE: &str = "No tasks. TaskCreate adds one.";

const HEADING: &str = "# Tasks";

/// One task on one line: `#3 [in_progress] write the plan — reviewer (blocked by #1, #2)`.
pub fn line(task: &Task) -> String {
    let mut line = format!("#{} [{}] {}", task.id, task.status.as_str(), task.subject);
    if let Some(owner) = &task.owner {
        line.push_str(&format!(" — {owner}"));
    }
    if !task.blocked_by.is_empty() {
        line.push_str(&format!(" (blocked by {})", ids(&task.blocked_by)));
    }
    line
}

fn ids(ids: &[u64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every task the list holds, one per line.
pub fn listing(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return NONE.to_string();
    }
    tasks.iter().map(line).collect::<Vec<_>>().join("\n")
}

/// The block the prompt carries: what is still to do, or nothing at all when
/// there is nothing — an empty list is not worth a heading every request.
pub fn reminder(tasks: &[Task]) -> Option<String> {
    let open = task::open_ones(tasks);
    if open.is_empty() {
        return None;
    }
    let lines: Vec<String> = open
        .iter()
        .map(|task| format!("- {}", line(task)))
        .collect();
    Some(format!("{HEADING}\n{}", lines.join("\n")))
}

/// One task in the columns of `HEADERS`.
pub fn row(task: &Task) -> Vec<String> {
    vec![
        task.id.to_string(),
        task.status.as_str().to_string(),
        task.subject.clone(),
        task.owner.clone().unwrap_or_default(),
    ]
}

pub fn rows(tasks: &[Task]) -> Vec<Vec<String>> {
    tasks.iter().map(row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Change, Draft, Status};

    fn list() -> Vec<Task> {
        let mut tasks = Vec::new();
        task::create(
            &mut tasks,
            Draft {
                subject: "write the plan".into(),
                ..Draft::default()
            },
        );
        task::create(
            &mut tasks,
            Draft {
                subject: "ship it".into(),
                ..Draft::default()
            },
        );
        tasks
    }

    fn set_status(tasks: &mut [Task], id: u64, status: Status) {
        task::update(
            tasks,
            Change {
                id,
                status: Some(status),
                ..Change::default()
            },
        );
    }

    #[test]
    fn a_line_names_the_id_the_status_and_the_subject() {
        let tasks = list();
        assert_eq!(line(&tasks[0]), "#1 [pending] write the plan");
    }

    #[test]
    fn an_owner_and_what_holds_a_task_up_are_on_the_line_too() {
        let mut tasks = list();
        task::update(
            &mut tasks,
            Change {
                id: 2,
                owner: Some("reviewer".into()),
                add_blocked_by: vec![1, 3],
                ..Change::default()
            },
        );
        assert_eq!(
            line(&tasks[1]),
            "#2 [pending] ship it — reviewer (blocked by #1, #3)"
        );
    }

    #[test]
    fn an_empty_list_tells_the_model_how_to_start_one() {
        assert_eq!(listing(&[]), NONE);
        assert!(listing(&list()).contains("#2 [pending] ship it"));
    }

    #[test]
    fn the_reminder_carries_the_open_ones_under_a_heading() {
        let mut tasks = list();
        set_status(&mut tasks, 1, Status::InProgress);
        assert_eq!(
            reminder(&tasks).expect("two open tasks"),
            "# Tasks\n- #1 [in_progress] write the plan\n- #2 [pending] ship it"
        );
    }

    #[test]
    fn nothing_open_is_no_reminder_at_all() {
        let mut tasks = list();
        set_status(&mut tasks, 1, Status::Completed);
        set_status(&mut tasks, 2, Status::Completed);
        assert!(reminder(&tasks).is_none());
        assert!(reminder(&[]).is_none());
    }

    #[test]
    fn a_row_has_a_cell_for_every_header() {
        let mut tasks = list();
        task::update(
            &mut tasks,
            Change {
                id: 1,
                owner: Some("reviewer".into()),
                ..Change::default()
            },
        );
        assert_eq!(rows(&tasks).len(), 2);
        assert_eq!(row(&tasks[0]).len(), HEADERS.len());
        assert_eq!(
            row(&tasks[0]),
            ["1", "pending", "write the plan", "reviewer"]
        );
        assert_eq!(
            row(&tasks[1])[3],
            "",
            "an unowned task leaves the cell empty"
        );
    }
}
