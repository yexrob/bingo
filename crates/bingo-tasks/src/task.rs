//! The record and the operations over a list of them, as pure values: what a
//! task is, the two shapes a tool hands in, and what creating, changing,
//! finding and filtering mean. Nothing here reads or writes anything — where
//! a list comes from is the journal's business — so the rules of a list can
//! be read and tested on their own.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Claude Code's task shape, field for field, so a host that already knows
/// that one reads ours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: u64,
    pub subject: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The tasks this one holds up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<u64>,
    /// The tasks that must finish before this one can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<u64>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::InProgress => "in_progress",
            Status::Completed => "completed",
        }
    }
}

/// What `TaskCreate` takes: everything a task carries but its id and its
/// status, which the list decides.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    /// What is to be done, in the imperative: "write the plan".
    pub subject: String,
    /// Anything the doer would have to ask for otherwise.
    #[serde(default)]
    pub description: String,
    /// The same thing in the present continuous — "writing the plan" — shown
    /// while the task is in progress.
    #[serde(default)]
    pub active_form: Option<String>,
    /// Who is to do it.
    #[serde(default)]
    pub owner: Option<String>,
    /// Ids of the tasks that must finish before this one can start.
    #[serde(default)]
    pub blocked_by: Vec<u64>,
    /// Ids of the tasks this one holds up.
    #[serde(default)]
    pub blocks: Vec<u64>,
    /// Anything else worth keeping with the task, by key.
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// What `TaskUpdate` takes: the id, and only the fields it means to change.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// The task to change, by the id `TaskCreate` or `TaskList` reported.
    pub id: u64,
    /// A new subject, in the imperative.
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A new present-continuous form: "writing the plan".
    #[serde(default)]
    pub active_form: Option<String>,
    /// Move the task to `pending`, `in_progress` or `completed`.
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub owner: Option<String>,
    /// Ids to add to what this task waits for.
    #[serde(default)]
    pub add_blocked_by: Vec<u64>,
    /// Ids to add to what this task holds up.
    #[serde(default)]
    pub add_blocks: Vec<u64>,
    /// Keys to set on the task's metadata; keys not named here stay as they are.
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// One past the highest id in the list, so an id survives its task: a
/// completed task still holds its number, and the next one is never a repeat.
pub fn next_id(tasks: &[Task]) -> u64 {
    tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1
}

/// Appends the draft as a pending task and answers what was recorded.
pub fn create(tasks: &mut Vec<Task>, draft: Draft) -> Task {
    let task = Task {
        id: next_id(tasks),
        subject: draft.subject,
        description: draft.description,
        active_form: draft.active_form,
        status: Status::Pending,
        owner: draft.owner,
        blocks: draft.blocks,
        blocked_by: draft.blocked_by,
        metadata: draft.metadata,
    };
    tasks.push(task.clone());
    task
}

/// Applies the change to the task it names, answering the task as it now
/// stands; `None` when the list has no such id.
pub fn update(tasks: &mut [Task], change: Change) -> Option<Task> {
    let task = tasks.iter_mut().find(|task| task.id == change.id)?;
    if let Some(subject) = change.subject {
        task.subject = subject;
    }
    if let Some(description) = change.description {
        task.description = description;
    }
    if change.active_form.is_some() {
        task.active_form = change.active_form;
    }
    if let Some(status) = change.status {
        task.status = status;
    }
    if change.owner.is_some() {
        task.owner = change.owner;
    }
    add(&mut task.blocked_by, change.add_blocked_by);
    add(&mut task.blocks, change.add_blocks);
    task.metadata.extend(change.metadata);
    Some(task.clone())
}

/// Ids the list does not have yet. A dependency named twice is one dependency.
fn add(ids: &mut Vec<u64>, more: Vec<u64>) {
    for id in more {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
}

pub fn get(tasks: &[Task], id: u64) -> Option<&Task> {
    tasks.iter().find(|task| task.id == id)
}

/// The tasks still to do, in list order: what a reminder is about.
pub fn open_ones(tasks: &[Task]) -> Vec<&Task> {
    tasks
        .iter()
        .filter(|task| task.status != Status::Completed)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn draft(subject: &str) -> Draft {
        Draft {
            subject: subject.into(),
            ..Draft::default()
        }
    }

    fn change(id: u64) -> Change {
        Change {
            id,
            ..Change::default()
        }
    }

    #[test]
    fn a_new_task_is_pending_and_numbered_from_one() {
        let mut tasks = Vec::new();
        let first = create(&mut tasks, draft("write the plan"));
        let second = create(&mut tasks, draft("ship it"));
        assert_eq!((first.id, second.id), (1, 2));
        assert_eq!(first.status, Status::Pending);
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn an_id_is_never_reused_once_its_task_is_completed() {
        let mut tasks = Vec::new();
        create(&mut tasks, draft("write the plan"));
        update(
            &mut tasks,
            Change {
                status: Some(Status::Completed),
                ..change(1)
            },
        );
        assert_eq!(next_id(&tasks), 2);
        assert_eq!(create(&mut tasks, draft("ship it")).id, 2);
    }

    #[test]
    fn a_change_touches_only_the_fields_it_names() {
        let mut tasks = Vec::new();
        create(
            &mut tasks,
            Draft {
                owner: Some("reviewer".into()),
                active_form: Some("writing the plan".into()),
                ..draft("write the plan")
            },
        );
        let updated = update(
            &mut tasks,
            Change {
                status: Some(Status::InProgress),
                ..change(1)
            },
        )
        .expect("the task is there");
        assert_eq!(updated.status, Status::InProgress);
        assert_eq!(updated.subject, "write the plan");
        assert_eq!(updated.owner.as_deref(), Some("reviewer"));
        assert_eq!(updated.active_form.as_deref(), Some("writing the plan"));
    }

    #[test]
    fn dependencies_accumulate_and_a_repeat_is_not_a_second_one() {
        let mut tasks = Vec::new();
        create(
            &mut tasks,
            Draft {
                blocked_by: vec![1],
                ..draft("ship it")
            },
        );
        let updated = update(
            &mut tasks,
            Change {
                add_blocked_by: vec![1, 2],
                add_blocks: vec![3],
                ..change(1)
            },
        )
        .expect("the task is there");
        assert_eq!(updated.blocked_by, [1, 2]);
        assert_eq!(updated.blocks, [3]);
    }

    #[test]
    fn metadata_merges_by_key() {
        let mut tasks = Vec::new();
        let mut first = Map::new();
        first.insert("pr".into(), json!(7));
        first.insert("area".into(), json!("kernel"));
        create(
            &mut tasks,
            Draft {
                metadata: first,
                ..draft("write the plan")
            },
        );
        let mut second = Map::new();
        second.insert("area".into(), json!("tasks"));
        let updated = update(
            &mut tasks,
            Change {
                metadata: second,
                ..change(1)
            },
        )
        .expect("the task is there");
        assert_eq!(updated.metadata["pr"], json!(7));
        assert_eq!(updated.metadata["area"], json!("tasks"));
    }

    #[test]
    fn an_unknown_id_changes_nothing_and_finds_nothing() {
        let mut tasks = Vec::new();
        create(&mut tasks, draft("write the plan"));
        assert!(update(&mut tasks, change(9)).is_none());
        assert!(get(&tasks, 9).is_none());
        assert_eq!(get(&tasks, 1).map(|task| task.id), Some(1));
    }

    #[test]
    fn the_open_ones_are_everything_but_the_completed() {
        let mut tasks = Vec::new();
        create(&mut tasks, draft("write the plan"));
        create(&mut tasks, draft("ship it"));
        update(
            &mut tasks,
            Change {
                status: Some(Status::Completed),
                ..change(1)
            },
        );
        let open = open_ones(&tasks);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, 2);
    }

    /// The wire form is Claude Code's: camelCase keys and snake_case statuses.
    #[test]
    fn the_json_a_host_reads_is_the_shape_it_knows() {
        let mut tasks = Vec::new();
        create(
            &mut tasks,
            Draft {
                active_form: Some("writing the plan".into()),
                blocked_by: vec![2],
                ..draft("write the plan")
            },
        );
        update(
            &mut tasks,
            Change {
                status: Some(Status::InProgress),
                ..change(1)
            },
        );
        let value = serde_json::to_value(&tasks).expect("a list of tasks is json");
        assert_eq!(
            value,
            json!([{
                "id": 1,
                "subject": "write the plan",
                "activeForm": "writing the plan",
                "status": "in_progress",
                "blockedBy": [2],
            }])
        );
        let back: Vec<Task> = serde_json::from_value(value).expect("and reads back");
        assert_eq!(back, tasks);
    }
}
