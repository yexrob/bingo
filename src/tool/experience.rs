use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::experience::{
    delete_entry, format_index, load_entries, project_key, query as query_entries, save_entry,
    ExperienceEntry, ExperienceError, ExperienceStatus,
};
use crate::tool::{parse_input, schema_for, Tool, ToolContext, ToolError, ToolResult};

fn home(ctx: &ToolContext) -> &PathBuf {
    &ctx.home
}

/// 提交字段校验：trigger/summary/steps 非空，status 合法。
fn validate(trigger: &[String], summary: &str, steps: &[String], status: Option<&str>) -> Result<Option<ExperienceStatus>, ToolError> {
    if trigger.is_empty() {
        return Err(ToolError::failed("ExperienceCommit: trigger is required (at least one keyword)"));
    }
    if summary.trim().is_empty() {
        return Err(ToolError::failed("ExperienceCommit: summary is required"));
    }
    if steps.is_empty() {
        return Err(ToolError::failed("ExperienceCommit: steps is required"));
    }
    match status {
        Some("active") | None => Ok(Some(ExperienceStatus::Active)),
        Some("degraded") => Ok(Some(ExperienceStatus::Degraded)),
        Some("stale") => Ok(Some(ExperienceStatus::Stale)),
        Some(other) => Err(ToolError::failed(format!(
            "ExperienceCommit: invalid status {other:?} (expected active|degraded|stale)"
        ))),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceProposeInput {
    /// 触发关键词：之后场景命中时想起这条经验（至少一个）。
    pub trigger: Vec<String>,
    /// 一句话总结（呈现给用户）。
    pub summary: String,
    /// 执行步骤（可重跑的命令序列）。
    pub steps: Vec<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

pub struct ExperienceProposeTool;

/// ExperiencePropose 工具 prompt：生成候选，不落盘。
const EXPERIENCE_PROPOSE_PROMPT: &str = r#"Use this tool to propose a reusable experience entry for the current project.

## When to Use This Tool

- After completing a task that involved a non-obvious workflow, command sequence, or pitfall you had to discover — the kind of thing you (or the user) would otherwise rediscover next time
- When you notice a pattern that has appeared at least twice in this session (frequency signal)
- When a verified solution has clear steps that can be executed again

## What to Include

- **trigger**: keywords that should recall this experience later (e.g. "migration", "db", "build failure")
- **summary**: one sentence describing what this experience covers
- **steps**: the exact command sequence or step list, in order, as it should be re-executed
- **verify**: how to confirm the experience still works (optional)
- **evidence**: where this came from — which session, what was verified (optional)

This tool only generates a candidate with a stable id — it does NOT write anything.
After the user confirms, commit it with ExperienceCommit (which passes the permission gate)."#;

#[async_trait]
impl Tool for ExperienceProposeTool {
    fn name(&self) -> String {
        "ExperiencePropose".into()
    }

    fn description(&self) -> String {
        EXPERIENCE_PROPOSE_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<ExperienceProposeInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: ExperienceProposeInput = parse_input(&input)?;
        validate(&args.trigger, &args.summary, &args.steps, None)?;
        let key = project_key(&ctx.cwd);
        let entry = ExperienceEntry::new(
            &key,
            args.trigger,
            args.summary,
            args.steps,
            args.verify,
            args.evidence,
        );
        let short = entry.id.chars().take(4).collect::<String>();
        Ok(ToolResult {
            content: json!({
                "candidate": {
                    "id": entry.id,
                    "id_short": format!("E{short}"),
                    "project_key": key,
                    "summary": entry.summary,
                    "trigger": entry.trigger,
                    "steps": entry.steps,
                    "verify": entry.verify,
                    "evidence": entry.evidence,
                },
                "note": "candidate not written yet — present it to the user; on confirmation call ExperienceCommit with the same fields",
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceCommitInput {
    /// 触发关键词（至少一个）。
    pub trigger: Vec<String>,
    /// 一句话总结。
    pub summary: String,
    /// 执行步骤。
    pub steps: Vec<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
    /// 状态：active（默认）/ degraded / stale（失败现场标记失效）。
    #[serde(default)]
    pub status: Option<String>,
}

pub struct ExperienceCommitTool;

/// ExperienceCommit 工具 prompt：落盘（过权限门）。
const EXPERIENCE_COMMIT_PROMPT: &str = r#"Use this tool to commit (persist) an experience entry into the current project's experience store. Passes the permission gate — the user confirms the write.

## When to Use This Tool

- After the user has confirmed a proposed experience (see ExperiencePropose)
- To mark a previously committed experience as stale after it failed verification (set status: "stale")

## Behavior

- Same content maps to the same stable id: re-committing an existing entry updates it (hits +1, status change honored), it does not duplicate
- status: "stale" marks the entry as failed — it stops being injected into new sessions but stays queryable
- Storage is user-global (~/.config/bingo/experience/<project-key>/), never touches the project workspace
- Output includes the confirmation line shown to the user"#;

#[async_trait]
impl Tool for ExperienceCommitTool {
    fn name(&self) -> String {
        "ExperienceCommit".into()
    }

    fn description(&self) -> String {
        EXPERIENCE_COMMIT_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<ExperienceCommitInput>()
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
        let args: ExperienceCommitInput = parse_input(&input)?;
        let status = validate(&args.trigger, &args.summary, &args.steps, args.status.as_deref())?;
        let key = project_key(&ctx.cwd);
        let mut entry = ExperienceEntry::new(
            &key,
            args.trigger,
            args.summary,
            args.steps,
            args.verify,
            args.evidence,
        );
        // 同 id 已存在 → 更新：保留 created_at/verified_at，采用计数 +1（写 stale 不计）。
        let existing = load_entries(home(ctx), &key);
        let prior = existing.iter().find(|e| e.id == entry.id);
        if let Some(prior) = prior {
            entry.created_at = prior.created_at.clone();
            entry.verified_at = prior.verified_at.clone();
            entry.notes = prior.notes.clone();
            if status != Some(ExperienceStatus::Stale) {
                entry.hits = prior.hits.saturating_add(1);
            } else {
                entry.hits = prior.hits;
            }
        }
        entry.status = status.unwrap_or(ExperienceStatus::Active);
        let path = save_entry(home(ctx), &key, &entry).map_err(map_io)?;
        let short = entry.id.chars().take(4).collect::<String>();
        let status_str = entry.status.as_str();
        Ok(ToolResult {
            content: json!({
                "id": entry.id,
                "summary": entry.summary,
                "status": status_str,
                "hits": entry.hits,
                "path": path.to_string_lossy(),
                "confirmation": format!("已沉淀 E{short}: {}（{}）", entry.summary, status_str),
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceQueryInput {
    /// 检索文本：与 trigger 关键词做词元匹配（大小写不敏感）。
    pub query: String,
    /// 返回条数上限（默认 5）。
    #[serde(default)]
    pub limit: Option<usize>,
}

pub struct ExperienceQueryTool;

/// ExperienceQuery 工具 prompt：按需取全文。
const EXPERIENCE_QUERY_PROMPT: &str = r#"Use this tool to search the current project's committed experiences by trigger keyword.

## When to Use This Tool

- When a task begins and you suspect a past experience may apply (the session index only lists up to 10 summaries — query for full details)
- When the session-start index mentioned an E<id> that looks relevant

## Behavior

- Matches if the query text contains any trigger keyword (case-insensitive substring match)
- Active entries rank above stale/degraded, then by hit count
- Returns full content (summary, steps, verify, evidence) for matched entries"#;

#[async_trait]
impl Tool for ExperienceQueryTool {
    fn name(&self) -> String {
        "ExperienceQuery".into()
    }

    fn description(&self) -> String {
        EXPERIENCE_QUERY_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<ExperienceQueryInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: ExperienceQueryInput = parse_input(&input)?;
        let key = project_key(&ctx.cwd);
        let entries = load_entries(home(ctx), &key);
        let matched = query_entries(&entries, &args.query, args.limit.unwrap_or(5));
        Ok(ToolResult {
            content: json!({
                "project_key": key,
                "matches": matched
                    .iter()
                    .map(|e| {
                        json!({
                            "id": format!("E{}", e.id.chars().take(4).collect::<String>()),
                            "full_id": e.id,
                            "status": e.status.as_str(),
                            "hits": e.hits,
                            "summary": e.summary,
                            "trigger": e.trigger,
                            "steps": e.steps,
                            "verify": e.verify,
                            "evidence": e.evidence,
                            "verified_at": e.verified_at,
                            "created_at": e.created_at,
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceForgetInput {
    /// 条目 id（ExperienceQuery 返回的 full_id）。
    pub id: String,
}

pub struct ExperienceForgetTool;

/// ExperienceForget 工具 prompt：淘汰（须用户确认）。
const EXPERIENCE_FORGET_PROMPT: &str = r#"Use this tool to permanently delete an experience entry (eviction). Passes the permission gate — the user confirms deletion.

## When to Use This Tool

- When the user asks to remove an experience
- When an entry is stale and the user confirms it should be discarded rather than kept for reference

## Behavior

- Deletes the entry file; the id stops appearing in indexes and queries"#;

#[async_trait]
impl Tool for ExperienceForgetTool {
    fn name(&self) -> String {
        "ExperienceForget".into()
    }

    fn description(&self) -> String {
        EXPERIENCE_FORGET_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<ExperienceForgetInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: ExperienceForgetInput = parse_input(&input)?;
        if args.id.trim().is_empty() {
            return Err(ToolError::failed("ExperienceForget: id is required"));
        }
        let key = project_key(&ctx.cwd);
        let existed = load_entries(home(ctx), &key)
            .iter()
            .any(|e| e.id == args.id);
        delete_entry(home(ctx), &key, &args.id).map_err(map_io)?;
        Ok(ToolResult {
            content: json!({
                "id": args.id,
                "deleted": existed,
                "confirmation": if existed {
                    format!("已遗忘 E{}", args.id.chars().take(4).collect::<String>())
                } else {
                    format!("E{} 不存在，无需删除", args.id.chars().take(4).collect::<String>())
                },
            }),
            ..Default::default()
        })
    }
}

fn map_io(e: ExperienceError) -> ToolError {
    ToolError::failed(format!("[Experience] {e}"))
}

/// 会话开始注入的项目经验索引（仅 active 条目，≤10 行；空返回空串）。
pub fn session_index(home: &Path, cwd: &std::path::Path) -> String {
    let key = project_key(cwd);
    let entries = load_entries(home, &key);
    format_index(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::settings::Settings;

    fn ctx_at(home: &Path, cwd: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: cwd.to_path_buf(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(home, "test")),
            hooks: Settings::default().hooks,
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: Arc::new(|_t, _q, _o| Box::pin(async { None })),
            home: home.to_path_buf(),
        }
    }

    fn tmp(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("bingo-exp-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (root.join("home"), root)
    }

    fn propose_input() -> serde_json::Value {
        json!({
            "trigger": ["migration"],
            "summary": "迁移数据库三步",
            "steps": ["备份", "执行迁移", "验证"],
            "verify": "cargo test",
            "evidence": "会话 2026-08-04",
        })
    }

    #[tokio::test]
    async fn propose_does_not_write() {
        let (home, cwd) = tmp("propose");
        let ctx = ctx_at(&home, &cwd);
        let tool = ExperienceProposeTool;
        let result = tool.call(propose_input(), &ctx).await.unwrap();
        assert!(result.content["candidate"]["id_short"].as_str().unwrap().starts_with('E'));
        assert!(load_entries(&home, &project_key(&cwd)).is_empty(), "propose 不落盘");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn commit_writes_and_recommit_updates() {
        let (home, cwd) = tmp("commit");
        let ctx = ctx_at(&home, &cwd);
        let tool = ExperienceCommitTool;
        let first = tool.call(propose_input(), &ctx).await.unwrap();
        let id = first.content["id"].as_str().unwrap().to_string();
        assert!(first.content["confirmation"].as_str().unwrap().starts_with("已沉淀"));
        assert_eq!(first.content["hits"], 0);
        let path = first.content["path"].as_str().unwrap().to_string();
        assert!(path.contains("experience"), "{path}");

        // 同内容再提交 → 更新而非重复：hits 1。
        let second = tool.call(propose_input(), &ctx).await.unwrap();
        assert_eq!(second.content["id"].as_str().unwrap(), id);
        assert_eq!(second.content["hits"], 1);
        let entries = load_entries(&home, &project_key(&cwd));
        assert_eq!(entries.len(), 1, "同 id 覆盖不重复");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn commit_rejects_empty_fields() {
        let (home, cwd) = tmp("validate");
        let ctx = ctx_at(&home, &cwd);
        let tool = ExperienceCommitTool;
        let err = tool
            .call(json!({"trigger": [], "summary": "s", "steps": ["1"]}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("trigger"));
        let err = tool
            .call(json!({"trigger": ["t"], "summary": "", "steps": ["1"]}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("summary"));
        let err = tool
            .call(json!({"trigger": ["t"], "summary": "s", "steps": []}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("steps"));
        let err = tool
            .call(json!({"trigger": ["t"], "summary": "s", "steps": ["1"], "status": "bogus"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("status"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn stale_commit_keeps_hits_and_query_hides_from_index() {
        let (home, cwd) = tmp("stale");
        let ctx = ctx_at(&home, &cwd);
        let commit = ExperienceCommitTool;
        commit.call(propose_input(), &ctx).await.unwrap();
        // 标记失效：hits 不增。
        let stale = json!({
            "trigger": ["migration"],
            "summary": "迁移数据库三步",
            "steps": ["备份", "执行迁移", "验证"],
            "status": "stale",
        });
        let result = commit.call(stale, &ctx).await.unwrap();
        assert_eq!(result.content["status"], "stale");
        assert_eq!(result.content["hits"], 0, "写 stale 不采用计数");

        // 索引不含 stale。
        assert!(session_index(&home, &cwd).is_empty(), "stale 不入注入索引");

        // Query 仍可查到（供现场复核）。
        let query_tool = ExperienceQueryTool;
        let q = query_tool
            .call(json!({"query": "migration"}), &ctx)
            .await
            .unwrap();
        assert_eq!(q.content["matches"][0]["status"], "stale");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn forget_deletes_and_requires_id() {
        let (home, cwd) = tmp("forget");
        let ctx = ctx_at(&home, &cwd);
        let commit = ExperienceCommitTool;
        let result = commit.call(propose_input(), &ctx).await.unwrap();
        let id = result.content["id"].as_str().unwrap().to_string();
        let forget = ExperienceForgetTool;
        let out = forget
            .call(json!({"id": id}), &ctx)
            .await
            .unwrap();
        assert!(out.content["deleted"].as_bool().unwrap());
        assert!(load_entries(&home, &project_key(&cwd)).is_empty());
        // 再删：不存在但成功。
        let out = forget.call(json!({"id": id}), &ctx).await.unwrap();
        assert!(!out.content["deleted"].as_bool().unwrap());
        let err = forget.call(json!({"id": "  "}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("id"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn query_returns_full_content() {
        let (home, cwd) = tmp("query");
        let ctx = ctx_at(&home, &cwd);
        let commit = ExperienceCommitTool;
        commit.call(propose_input(), &ctx).await.unwrap();
        let tool = ExperienceQueryTool;
        let out = tool
            .call(json!({"query": "migrate now", "limit": 3}), &ctx)
            .await
            .unwrap();
        assert_eq!(out.content["matches"][0]["steps"][0], "备份");
        assert_eq!(out.content["matches"][0]["verify"], "cargo test");
        assert!(out.content["matches"][0]["summary"].as_str().unwrap().contains("迁移"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
