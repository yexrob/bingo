//! How a task reads, decided once: the model's listing, the reminder in the
//! prompt and the `/tasks` table all name a task the same way, so a person
//! and a model never compare two different lists.

use crate::task::{self, Task};

/// The columns a task list has wherever it is shown as a table.
pub const HEADERS: [&str; 4] = ["id", "status", "subject", "owner"];

/// What a model is told when the list is empty.
pub const NONE: &str = "No tasks. TaskCreate adds one.";

/// What an owner nobody here answers to reads as. It is said at read time and
/// never written: no machinery flips a crashed owner's tasks, the display just
/// tells the truth about who is here (ADR-0023 §3).
const GONE: &str = " (gone)";

const HEADING: &str = "# Tasks";

/// The names a listing may say are here. A session's own list says nothing
/// about its owners — a name there is a note the doer wrote itself. A board's
/// does: its room can see who is beside it, so an owner none of them answers
/// to is marked gone.
#[derive(Clone, Copy, Debug, Default)]
pub struct Present<'a>(Option<&'a [String]>);

impl<'a> Present<'a> {
    pub fn among(names: Option<&'a [String]>) -> Self {
        Self(names)
    }

    fn gone(&self, owner: &str) -> bool {
        self.0
            .is_some_and(|names| !names.iter().any(|name| name == owner))
    }

    /// The owner as it reads: the name written, marked when nobody here
    /// answers to it.
    fn owner(&self, task: &Task) -> Option<String> {
        let owner = task.owner.as_ref()?;
        Some(match self.gone(owner) {
            true => format!("{owner}{GONE}"),
            false => owner.clone(),
        })
    }
}

/// One task on one line: `#3 [in_progress] write the plan — reviewer (blocked by #1, #2)`.
pub fn line(task: &Task, present: Present<'_>) -> String {
    let mut line = format!("#{} [{}] {}", task.id, task.status.as_str(), task.subject);
    if let Some(owner) = present.owner(task) {
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
pub fn listing(tasks: &[Task], present: Present<'_>) -> String {
    if tasks.is_empty() {
        return NONE.to_string();
    }
    tasks
        .iter()
        .map(|task| line(task, present))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The block the prompt carries: what is still to do, or nothing at all when
/// there is nothing — an empty list is not worth a heading every request. The
/// session's own list and no other: a board in every prompt would be a context
/// tax nobody asked for (ADR-0023 §4).
pub fn reminder(tasks: &[Task]) -> Option<String> {
    let open = task::open_ones(tasks);
    if open.is_empty() {
        return None;
    }
    let lines: Vec<String> = open
        .iter()
        .map(|task| format!("- {}", line(task, Present::default())))
        .collect();
    Some(format!("{HEADING}\n{}", lines.join("\n")))
}

/// One task in the columns of `HEADERS`.
pub fn row(task: &Task, present: Present<'_>) -> Vec<String> {
    vec![
        task.id.to_string(),
        task.status.as_str().to_string(),
        task.subject.clone(),
        present.owner(task).unwrap_or_default(),
    ]
}

pub fn rows(tasks: &[Task], present: Present<'_>) -> Vec<Vec<String>> {
    tasks.iter().map(|task| row(task, present)).collect()
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

    /// A session's own list, which asserts nothing about who is here.
    fn own() -> Present<'static> {
        Present::default()
    }

    #[test]
    fn a_line_names_the_id_the_status_and_the_subject() {
        let tasks = list();
        assert_eq!(line(&tasks[0], own()), "#1 [pending] write the plan");
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
            line(&tasks[1], own()),
            "#2 [pending] ship it — reviewer (blocked by #1, #3)"
        );
    }

    #[test]
    fn an_empty_list_tells_the_model_how_to_start_one() {
        assert_eq!(listing(&[], own()), NONE);
        assert!(listing(&list(), own()).contains("#2 [pending] ship it"));
    }

    /// The board's one addition: an owner no name here answers to is marked,
    /// and the task itself is untouched by the marking.
    #[test]
    fn an_owner_nobody_here_answers_to_is_marked_gone() {
        let mut tasks = list();
        for (id, owner) in [(1, "reviewer"), (2, "scout")] {
            task::update(
                &mut tasks,
                Change {
                    id,
                    owner: Some(owner.into()),
                    ..Change::default()
                },
            );
        }
        let here = ["reviewer".to_string()];
        let present = Present::among(Some(&here));
        assert_eq!(
            line(&tasks[0], present),
            "#1 [pending] write the plan — reviewer"
        );
        assert_eq!(
            line(&tasks[1], present),
            "#2 [pending] ship it — scout (gone)"
        );
        assert_eq!(row(&tasks[1], present)[3], "scout (gone)");
        assert_eq!(
            tasks[1].owner.as_deref(),
            Some("scout"),
            "the mark is read, never written"
        );
    }

    /// A board nobody is beside marks every owner; a private list marks none.
    #[test]
    fn an_empty_board_marks_everyone_and_a_private_list_marks_no_one() {
        let mut tasks = list();
        task::update(
            &mut tasks,
            Change {
                id: 1,
                owner: Some("reviewer".into()),
                ..Change::default()
            },
        );
        assert_eq!(
            line(&tasks[0], Present::among(Some(&[]))),
            "#1 [pending] write the plan — reviewer (gone)"
        );
        assert_eq!(
            line(&tasks[0], Present::among(None)),
            "#1 [pending] write the plan — reviewer"
        );
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
        assert_eq!(rows(&tasks, own()).len(), 2);
        assert_eq!(row(&tasks[0], own()).len(), HEADERS.len());
        assert_eq!(
            row(&tasks[0], own()),
            ["1", "pending", "write the plan", "reviewer"]
        );
        assert_eq!(
            row(&tasks[1], own())[3],
            "",
            "an unowned task leaves the cell empty"
        );
    }
}
