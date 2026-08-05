use std::collections::BTreeMap;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::tasks::{Task, TaskPatch, TaskStatus, TaskStore};
use crate::tool::{parse_input, schema_for, Tool, ToolContext, ToolError, ToolResult};

fn store(ctx: &ToolContext) -> &std::sync::Arc<TaskStore> {
    &ctx.tasks
}

/// 修复层：模型常见近似字段名 → 规范字段（对标 Claude Code TaskCreate coerceInput）。
/// 返回 (修复后的输入, 本次修复的动作列表；无修复返回原值)。
fn coerce_create(input: serde_json::Value) -> (serde_json::Value, Vec<&'static str>) {
    let mut value = input;
    let mut fixed = Vec::new();
    let Some(map) = value.as_object_mut() else {
        return (value, fixed);
    };
    // task 包裹拆包（{task: {...}}）
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
    // 缺失 subject/description 时互相 backfill（对标 CC backfill_*）
    let has_subject = map.get("subject").is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
    let has_description =
        map.get("description").is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
    if has_subject && !has_description {
        map.insert(
            "description".to_string(),
            map.get("subject").cloned().unwrap_or_default(),
        );
        fixed.push("backfill_description");
    } else if has_description && !has_subject {
        map.insert("subject".to_string(), map.get("description").cloned().unwrap());
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

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> String {
        "TaskCreate".into()
    }

    fn description(&self) -> String {
        "Create a new task in the task list".into()
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

        // TaskCreated hooks：blocking → 撤掉刚建的任务并报错（对标 CC）。
        let blocking = crate::hooks::run_task_created(
            &ctx.hooks,
            &id,
            &task.subject,
            ctx.permission_mode.as_str(),
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

/// 修复层：TaskUpdate 近似的 status 键（state/status 均收）与 id 别名（对标 CC 输入修复）。
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

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> String {
        "TaskUpdate".into()
    }

    fn description(&self) -> String {
        "Update an existing task in the task list (status, subject, description, activeForm, owner, metadata, dependencies)".into()
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

        // deleted：永久删除（对标 CC status: deleted）
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
                return Err(ToolError::failed(format!("[Tasks] invalid status: {other}")));
            }
            None => None,
        };

        // completed：TaskCompleted hooks（blockingError 拒绝 completed，对标 CC）。
        if status == Some(TaskStatus::Completed) && task.status != TaskStatus::Completed {
            let blocking = crate::hooks::run_task_completed(
                &ctx.hooks,
                &args.task_id,
                &task.subject,
                ctx.permission_mode.as_str(),
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
    pub task_id: String,
}

pub struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> String {
        "TaskGet".into()
    }

    fn description(&self) -> String {
        "Get a task by ID from the task list".into()
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

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> String {
        "TaskList".into()
    }

    fn description(&self) -> String {
        "List all tasks in the task list".into()
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
        // 已完成的任务不算阻塞（对标 CC TaskList call）。
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
}
