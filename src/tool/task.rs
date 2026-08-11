use std::collections::BTreeMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::tasks::{Task, TaskPatch, TaskStatus, TaskStore};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input, schema_for};

fn store(ctx: &ToolContext) -> &std::sync::Arc<TaskStore> {
    &ctx.tasks
}

/// Coercion layer: common approximate field names from the model → canonical fields.
/// Returns (coerced input, list of fixes applied; the original value if no fixes).
fn coerce_create(input: serde_json::Value) -> (serde_json::Value, Vec<&'static str>) {
    let mut value = input;
    let mut fixed = Vec::new();
    let Some(map) = value.as_object_mut() else {
        return (value, fixed);
    };
    // Unwrap the task wrapper ({task: {...}})
    if !map.contains_key("subject")
        && !map.contains_key("description")
        && let Some(wrapped) = map.remove("task")
        && let serde_json::Value::Object(inner) = wrapped
    {
        for (k, v) in inner {
            map.insert(k, v);
        }
        fixed.push("task_wrapper_unwrapped");
    }
    let aliases: &[(&[&str], &str)] = &[
        (&["title", "name"], "subject"),
        (&["content"], "description"),
        (&["active_form"], "activeForm"),
    ];
    for (froms, to) in aliases {
        for from in *froms {
            if let Some(v) = map.remove(*from)
                && !map.contains_key(*to)
                && v.as_str().is_some_and(|s| !s.is_empty())
            {
                map.insert(to.to_string(), v);
                fixed.push("alias");
                break;
            }
        }
    }
    if let Some(s) = map.get("subject").and_then(|v| v.as_str())
        && s.is_empty()
        && !map.contains_key("description")
    {
        return (value, fixed);
    }
    // Backfill subject/description from each other when one is missing
    let has_subject = map
        .get("subject")
        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
    let has_description = map
        .get("description")
        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
    if has_subject && !has_description {
        map.insert(
            "description".to_string(),
            map.get("subject").cloned().unwrap_or_default(),
        );
        fixed.push("backfill_description");
    } else if has_description && !has_subject
        && let Some(description) = map.get("description").cloned()
    {
        map.insert("subject".to_string(), description);
        fixed.push("backfill_subject");
    }
    (value, fixed)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskCreateInput {
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "activeForm", default)]
    pub active_form: Option<String>,
    #[serde(default)]
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}

pub struct TaskCreateTool;

/// TaskCreate tool prompt (without the swarm/owner surface).
const TASK_CREATE_PROMPT: &str = r#"Use this tool to create a structured task list for your current coding session. This helps you track progress, organize complex tasks, and demonstrate thoroughness to the user.
It also helps the user understand the progress of the task and overall progress of their requests.

## When to Use This Tool

Use this tool proactively in these scenarios:

- Complex multi-step tasks - When a task requires 3 or more distinct steps or actions
- Non-trivial and complex tasks - Tasks that require careful planning or multiple operations
- Plan mode - When using plan mode, create a task list to track the work
- User explicitly requests todo list - When the user directly asks you to use the todo list
- User provides multiple tasks - When users provide a list of things to be done (numbered or comma-separated)
- After receiving new instructions - Immediately capture user requirements as tasks
- When you start working on a task - Mark it as in_progress BEFORE beginning work
- After completing a task - Mark it as completed and add any new follow-up tasks discovered during implementation

## When NOT to Use This Tool

Skip using this tool when:
- There is only a single, straightforward task
- The task is trivial and tracking it provides no organizational benefit
- The task can be completed in less than 3 trivial steps
- The task is purely conversational or informational

NOTE that you should not use this tool if there is only one trivial task to do. In this case you are better off just doing the task directly.

## Task Fields

- **subject**: A brief, actionable title in imperative form (e.g., "Fix authentication bug in login flow")
- **description**: What needs to be done
- **activeForm** (optional): Present continuous form shown in the spinner when the task is in_progress (e.g., "Fixing authentication bug"). If omitted, the spinner shows the subject instead.

All tasks are created with status `pending`.

## Tips

- Create tasks with clear, specific subjects that describe the outcome
- The task list is your live plan: keep it current as the session progresses —
  if the direction changes, update or delete stale tasks promptly with TaskUpdate
- After creating tasks, use TaskUpdate to set up dependencies (blocks/blockedBy) if needed
- Check TaskList first to avoid creating duplicate tasks"#;

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> String {
        "TaskCreate".into()
    }

    fn description(&self) -> String {
        TASK_CREATE_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<TaskCreateInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let (coerced, _) = coerce_create(input);
        let args: TaskCreateInput = parse_input(&coerced)?;
        let task = Task {
            id: String::new(),
            subject: args.subject,
            description: args.description,
            active_form: args.active_form,
            status: TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: args.metadata.unwrap_or_default(),
        };
        let id = store(ctx)
            .create(&task)
            .await
            .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?;

        // TaskCreated hooks: blocking → undo the just-created task and report the error.
        let blocking = crate::hooks::run_task_created(
            &ctx.hooks,
            &id,
            &task.subject,
            ctx.permission_mode.as_str(),
            &ctx.cwd,
        )
        .await;
        if !blocking.is_empty() {
            let _ = store(ctx).delete(&id).await;
            return Err(ToolError::failed(blocking.join("\n")));
        }
        ctx.set_expanded_view_tasks();
        Ok(ToolResult {
            content: json!({ "task": { "id": id, "subject": task.subject } }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskUpdateInput {
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "activeForm", default)]
    pub active_form: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(rename = "addBlocks", default)]
    pub add_blocks: Option<Vec<String>>,
    #[serde(rename = "addBlockedBy", default)]
    pub add_blocked_by: Option<Vec<String>>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}

/// Coercion layer: TaskUpdate accepts approximate status keys (state/status) and id aliases.
fn coerce_update(input: serde_json::Value) -> serde_json::Value {
    let mut value = input;
    let Some(map) = value.as_object_mut() else {
        return value;
    };
    for from in ["id", "task_id", "taskid"] {
        if let Some(v) = map.remove(from)
            && !map.contains_key("taskId")
        {
            map.insert("taskId".to_string(), v);
        }
    }
    for from in ["state", "task_status"] {
        if let Some(v) = map.remove(from)
            && !map.contains_key("status")
        {
            map.insert("status".to_string(), v);
        }
    }
    if let Some(active_form) = map.remove("active_form")
        && !map.contains_key("activeForm")
    {
        map.insert("activeForm".to_string(), active_form);
    }
    value
}

pub struct TaskUpdateTool;

/// TaskUpdate tool prompt (without the swarm/owner assignment section).
const TASK_UPDATE_PROMPT: &str = r#"Use this tool to update a task in the task list.

## When to Use This Tool

**Mark tasks as resolved:**
- When you have completed the work described in a task
- When a task is no longer needed or has been superseded
- IMPORTANT: Always mark your assigned tasks as resolved when you finish them
- After resolving, call TaskList to find your next task

- ONLY mark a task as completed when you have FULLY accomplished it
- If you encounter errors, blockers, or cannot finish, keep the task as in_progress
- When blocked, create a new task describing what needs to be resolved
- Never mark a task as completed if:
  - Tests are failing
  - Implementation is partial
  - You encountered unresolved errors
  - You couldn't find necessary files or dependencies

**Delete tasks:**
- When a task is no longer relevant or was created in error
- Setting status to `deleted` permanently removes the task

**Update task details:**
- When requirements change or become clearer
- When establishing dependencies between tasks

## Keep the List Current

The task list is your live plan of record. Whenever the direction of the session
changes — requirements shift, an approach is abandoned, scope grows or shrinks —
update the list promptly instead of leaving it stale:

- Reword tasks whose scope changed (subject/description)
- Mark superseded or abandoned items as `completed` or `deleted`
- Add tasks for newly discovered work
- Do not keep outdated tasks around "for later": a stale list misleads both you
  and the user about what is actually being done

## Fields You Can Update

- **status**: The task status (see Status Workflow below)
- **subject**: Change the task title (imperative form, e.g., "Run tests")
- **description**: Change the task description
- **activeForm**: Present continuous form shown in spinner when in_progress (e.g., "Running tests")
- **owner**: Change the task owner (agent name)
- **metadata**: Merge metadata keys into the task (set a key to null to delete it)
- **addBlocks**: Mark tasks that cannot start until this one completes
- **addBlockedBy**: Mark tasks that must complete before this one can start

## Status Workflow

Status progresses: `pending` → `in_progress` → `completed`

Use `deleted` to permanently remove a task.

## Staleness

Make sure to read a task's latest state using `TaskGet` before updating it.

## Examples

Mark task as in progress when starting work:
```json
{"taskId": "1", "status": "in_progress"}
```

Mark task as completed after finishing work:
```json
{"taskId": "1", "status": "completed"}
```

Delete a task:
```json
{"taskId": "1", "status": "deleted"}
```

Set up task dependencies:
```json
{"taskId": "2", "addBlockedBy": ["1"]}
```
"#;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> String {
        "TaskUpdate".into()
    }

    fn description(&self) -> String {
        TASK_UPDATE_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<TaskUpdateInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: TaskUpdateInput = parse_input(&coerce_update(input))?;
        let store = store(ctx);
        let Some(task) = store
            .get(&args.task_id)
            .await
            .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?
        else {
            return Ok(ToolResult {
                content: json!({
                    "success": false,
                    "taskId": args.task_id,
                    "updatedFields": [],
                    "error": "Task not found",
                }),
                ..Default::default()
            });
        };
        let mut updated_fields: Vec<String> = Vec::new();

        // deleted: permanently remove
        if args.status.as_deref() == Some("deleted") {
            let ok = store
                .delete(&args.task_id)
                .await
                .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?;
            return Ok(ToolResult {
                content: json!({
                    "success": ok,
                    "taskId": args.task_id,
                    "updatedFields": if ok { vec!["deleted"] } else { Vec::new() },
                    "error": if ok { serde_json::Value::Null } else { json!("Failed to delete task") },
                    "statusChange": if ok { json!({"from": task.status, "to": "deleted"}) } else { serde_json::Value::Null },
                }),
                ..Default::default()
            });
        }

        let status = match args.status.as_deref() {
            Some("pending") => Some(TaskStatus::Pending),
            Some("in_progress") => Some(TaskStatus::InProgress),
            Some("completed") => Some(TaskStatus::Completed),
            Some(other) => {
                return Err(ToolError::failed(format!(
                    "[Tasks] invalid status: {other}"
                )));
            }
            None => None,
        };

        // completed: TaskCompleted hooks (blockingError refuses completed).
        if status == Some(TaskStatus::Completed) && task.status != TaskStatus::Completed {
            let blocking = crate::hooks::run_task_completed(
                &ctx.hooks,
                &args.task_id,
                &task.subject,
                ctx.permission_mode.as_str(),
                &ctx.cwd,
            )
            .await;
            if !blocking.is_empty() {
                return Ok(ToolResult {
                    content: json!({
                        "success": false,
                        "taskId": args.task_id,
                        "updatedFields": [],
                        "error": blocking.join("\n"),
                    }),
                    ..Default::default()
                });
            }
        }

        let patch = TaskPatch {
            subject: args.subject.clone(),
            description: args.description.clone(),
            active_form: args.active_form.clone(),
            owner: args.owner.clone(),
            status,
            metadata: args.metadata.clone(),
        };
        let fields: &[(&str, bool)] = &[
            ("subject", args.subject.is_some()),
            ("description", args.description.is_some()),
            ("activeForm", args.active_form.is_some()),
            ("owner", args.owner.is_some()),
            ("status", status.is_some()),
            ("metadata", args.metadata.is_some()),
        ];
        for (name, set) in fields {
            if *set {
                updated_fields.push((*name).to_string());
            }
        }
        let Some(old) = store
            .update(&args.task_id, &patch)
            .await
            .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?
        else {
            return Ok(ToolResult {
                content: json!({
                    "success": false,
                    "taskId": args.task_id,
                    "updatedFields": [],
                    "error": "Task not found",
                }),
                ..Default::default()
            });
        };

        if let Some(blocks) = &args.add_blocks {
            for b in blocks.iter().filter(|b| !old.blocks.contains(b)) {
                store
                    .link_block(&args.task_id, b)
                    .await
                    .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?;
            }
            if !blocks.is_empty() {
                updated_fields.push("blocks".to_string());
            }
        }
        if let Some(blocked_by) = &args.add_blocked_by {
            for b in blocked_by.iter().filter(|b| !old.blocked_by.contains(b)) {
                store
                    .link_block(b, &args.task_id)
                    .await
                    .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?;
            }
            if !blocked_by.is_empty() {
                updated_fields.push("blockedBy".to_string());
            }
        }

        let status_change = status
            .map(|s| json!({ "from": old.status, "to": s }))
            .unwrap_or(serde_json::Value::Null);
        Ok(ToolResult {
            content: json!({
                "success": true,
                "taskId": args.task_id,
                "updatedFields": updated_fields,
                "statusChange": status_change,
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskGetInput {
    #[serde(rename = "taskId")]
    pub task_id: String,
}

pub struct TaskGetTool;

/// TaskGet tool prompt.
const TASK_GET_PROMPT: &str = r#"Use this tool to retrieve a task by its ID from the task list.

## When to Use This Tool

- When you need the full description and context before starting work on a task
- To understand task dependencies (what it blocks, what blocks it)
- After being assigned a task, to get complete requirements

## Output

Returns full task details:
- **subject**: Task title
- **description**: Detailed requirements and context
- **status**: 'pending', 'in_progress', or 'completed'
- **blocks**: Tasks waiting on this one to complete
- **blockedBy**: Tasks that must complete before this one can start

## Tips

- After fetching a task, verify its blockedBy list is empty before beginning work.
- Use TaskList to see all tasks in summary form.
"#;

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> String {
        "TaskGet".into()
    }

    fn description(&self) -> String {
        TASK_GET_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<TaskGetInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: TaskGetInput = parse_input(&input)?;
        let Some(task) = store(ctx)
            .get(&args.task_id)
            .await
            .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?
        else {
            return Ok(ToolResult {
                content: json!({ "task": serde_json::Value::Null }),
                ..Default::default()
            });
        };
        Ok(ToolResult {
            content: json!({
                "task": {
                    "id": task.id,
                    "subject": task.subject,
                    "description": task.description,
                    "status": task.status,
                    "owner": task.owner,
                    "blocks": task.blocks,
                    "blockedBy": task.blocked_by,
                }
            }),
            ..Default::default()
        })
    }
}

pub struct TaskListTool;

/// TaskList tool prompt (without the teammate section).
const TASK_LIST_PROMPT: &str = r#"Use this tool to list all tasks in the task list.

## When to Use This Tool

- To see what tasks are available to work on (status: 'pending', no owner, not blocked)
- To check overall progress on the project
- To find tasks that are blocked and need dependencies resolved
- After completing a task, to check for newly unblocked work or claim the next available task
- **Prefer working on tasks in ID order** (lowest ID first) when multiple tasks are available, as earlier tasks often set up context for later ones

## Output

Returns a summary of each task:
- **id**: Task identifier (use with TaskGet, TaskUpdate)
- **subject**: Brief description of the task
- **status**: 'pending', 'in_progress', or 'completed'
- **owner**: Agent the task is assigned to, if any
- **blockedBy**: IDs of tasks that must complete before this one can start

## Tips

- Look for tasks with status 'pending', no owner, and empty blockedBy when choosing work
- Check TaskList before creating new tasks to avoid duplicates
"#;

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> String {
        "TaskList".into()
    }

    fn description(&self) -> String {
        TASK_LIST_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let tasks = store(ctx)
            .list()
            .await
            .map_err(|e| ToolError::failed(format!("[Tasks] {e}")))?;
        // Completed tasks do not count as blocking
        let completed: std::collections::HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.id.clone())
            .collect();
        let list: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "subject": t.subject,
                    "status": t.status,
                    "owner": t.owner,
                    "blockedBy": t.blocked_by.iter().filter(|b| !completed.contains(*b)).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(ToolResult {
            content: json!({ "tasks": list }),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_task_wrapper() {
        let (v, fixed) = coerce_create(json!({"task": {"subject": "s", "description": "d"}}));
        assert_eq!(v["subject"], "s");
        assert_eq!(v["description"], "d");
        assert!(fixed.contains(&"task_wrapper_unwrapped"));
    }

    #[test]
    fn coerce_aliases() {
        let (v, fixed) = coerce_create(json!({"title": "t", "content": "c"}));
        assert_eq!(v["subject"], "t");
        assert_eq!(v["description"], "c");
        assert!(fixed.contains(&"alias"));
        let (v, _) = coerce_create(json!({"name": "n", "active_form": "doing"}));
        assert_eq!(v["subject"], "n");
        assert_eq!(v["activeForm"], "doing");
    }

    #[test]
    fn coerce_backfills() {
        let (v, fixed) = coerce_create(json!({"subject": "only"}));
        assert_eq!(v["description"], "only");
        assert!(fixed.contains(&"backfill_description"));
        let (v, fixed) = coerce_create(json!({"description": "only"}));
        assert_eq!(v["subject"], "only");
        assert!(fixed.contains(&"backfill_subject"));
    }

    #[test]
    fn coerce_update_id_aliases() {
        let v = coerce_update(json!({"task_id": "3", "state": "in_progress"}));
        assert_eq!(v["taskId"], "3");
        assert_eq!(v["status"], "in_progress");
    }

    #[test]
    fn task_update_parses_canonical_and_aliased_ids() {
        // The schema key is taskId (camelCase); the model passes taskId per the schema.
        let canonical: TaskUpdateInput =
            parse_input(&json!({"taskId": "3", "status": "in_progress"})).unwrap();
        assert_eq!(canonical.task_id, "3");
        // Historical/dialect callers pass task_id: still parseable after coerce fixes it.
        let aliased: TaskUpdateInput = parse_input(&coerce_update(
            json!({"task_id": "3", "status": "in_progress"}),
        ))
        .unwrap();
        assert_eq!(aliased.task_id, "3");
        let by_id: TaskUpdateInput =
            parse_input(&coerce_update(json!({"id": "3", "status": "completed"}))).unwrap();
        assert_eq!(by_id.task_id, "3");
    }

    #[test]
    fn task_get_schema_uses_camel_case_task_id() {
        let schema = TaskGetTool.input_schema();
        assert_eq!(schema["properties"]["taskId"]["type"], "string");
        assert_eq!(schema["required"], json!(["taskId"]));
        // TaskUpdate schema and implementation share a source: taskId is required.
        let update_schema = TaskUpdateTool.input_schema();
        assert_eq!(update_schema["properties"]["taskId"]["type"], "string");
        assert_eq!(update_schema["required"], json!(["taskId"]));
    }
}
