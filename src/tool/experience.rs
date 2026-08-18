use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::experience::{
    ExperienceEntry, ExperienceError, ExperienceOutcome, ExperienceStatus, delete_entry,
    format_index, load_entries, project_key, query as query_entries, record_outcome, save_entry,
};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input, schema_for};

fn home(ctx: &ToolContext) -> &PathBuf {
    &ctx.home
}

/// Commit field validation: trigger/summary/steps must be non-empty, status must be valid.
fn validate(
    trigger: &[String],
    summary: &str,
    steps: &[String],
    status: Option<&str>,
) -> Result<Option<ExperienceStatus>, ToolError> {
    if trigger.is_empty() {
        return Err(ToolError::failed(
            "ExperienceCommit: trigger is required (at least one keyword)",
        ));
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
    /// Trigger keywords: recall this experience when a later scenario matches (at least one).
    pub trigger: Vec<String>,
    /// One-sentence summary (shown to the user).
    pub summary: String,
    /// Execution steps (a re-runnable command sequence).
    pub steps: Vec<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

pub struct ExperienceProposeTool;

/// ExperiencePropose tool prompt: generate a candidate, does not persist.
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
    /// Trigger keywords (at least one).
    pub trigger: Vec<String>,
    /// One-sentence summary.
    pub summary: String,
    /// Execution steps.
    pub steps: Vec<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
    /// Status: active (default) / degraded / stale (marks the entry as failed).
    #[serde(default)]
    pub status: Option<String>,
}

pub struct ExperienceCommitTool;

/// ExperienceCommit tool prompt: persist (passes the permission gate).
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
        let status = validate(
            &args.trigger,
            &args.summary,
            &args.steps,
            args.status.as_deref(),
        )?;
        let key = project_key(&ctx.cwd);
        let mut entry = ExperienceEntry::new(
            &key,
            args.trigger,
            args.summary,
            args.steps,
            args.verify,
            args.evidence,
        );
        // Same id already exists → update: keep created_at/verified_at, bump the hit count
        // (+1; writing stale does not count).
        let existing = load_entries(home(ctx), &key);
        let prior = existing.iter().find(|e| e.id == entry.id);
        if let Some(prior) = prior {
            entry.created_at = prior.created_at.clone();
            entry.verified_at = prior.verified_at.clone();
            entry.helpful = prior.helpful;
            entry.harmful = prior.harmful;
            entry.outcome_history = prior.outcome_history.clone();
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
                "helpful": entry.helpful,
                "harmful": entry.harmful,
                "path": path.to_string_lossy(),
                "confirmation": format!("consolidated E{short}: {} ({})", entry.summary, status_str),
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceQueryInput {
    /// Search text: BM25-matched against triggers, summaries, steps and notes.
    pub query: String,
    /// Maximum number of results (default 5).
    #[serde(default)]
    pub limit: Option<usize>,
}

pub struct ExperienceQueryTool;

/// ExperienceQuery tool prompt: fetch full entries on demand.
const EXPERIENCE_QUERY_PROMPT: &str = r#"Use this tool to search the current project's committed experiences by trigger keyword.

## When to Use This Tool

- When a task begins and you suspect a past experience may apply (the session index only lists up to 10 summaries — query for full details)
- When the session-start index mentioned an E<id> that looks relevant
- If you actually apply a returned experience, record the externally observed result with ExperienceOutcome after verification

## Behavior

- BM25 relevance over trigger keywords, summary, steps and notes (English word stems and CJK bigrams both match)
- Equal relevance breaks ties the old way: active entries above stale/degraded, observed helpful/harmful outcomes before the legacy commit count
- Returns full content plus outcome counters and append-only outcome history"#;

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
                            "helpful": e.helpful,
                            "harmful": e.harmful,
                            "outcome_history": e.outcome_history,
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
#[serde(rename_all = "lowercase")]
pub enum ExperienceOutcomeValue {
    Helpful,
    Harmful,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ExperienceOutcomeInput {
    /// Full entry id returned by ExperienceQuery.
    pub id: String,
    /// Observed effect after applying the experience: helpful or harmful.
    pub outcome: ExperienceOutcomeValue,
    /// Concrete task, verification, or counterexample supporting this outcome.
    pub evidence: String,
}

pub struct ExperienceOutcomeTool;

const EXPERIENCE_OUTCOME_PROMPT: &str = r#"Use this tool to record the observed result of applying a committed project experience. The write passes the permission gate so the user confirms it.

## When to Use This Tool

- After ExperienceQuery returned an entry and you actually followed it in the current task
- Record `helpful` only when external evidence supports the result, such as passing verification or user acceptance
- Record `harmful` when following it caused a failure, regression, wasted path, or user correction

## Behavior

- Requires the exact full entry id, an outcome (`helpful` or `harmful`), and concrete evidence
- Appends an outcome history record and derives counters from that history
- Never changes lifecycle status or `verified_at` automatically
- This MVP performs a read-modify-write and is not concurrency-safe"#;

#[async_trait]
impl Tool for ExperienceOutcomeTool {
    fn name(&self) -> String {
        "ExperienceOutcome".into()
    }

    fn description(&self) -> String {
        EXPERIENCE_OUTCOME_PROMPT.to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<ExperienceOutcomeInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: ExperienceOutcomeInput = parse_input(&input)?;
        if args.id.trim().is_empty() {
            return Err(ToolError::failed("ExperienceOutcome: id is required"));
        }
        if args.evidence.trim().is_empty() {
            return Err(ToolError::failed("ExperienceOutcome: evidence is required"));
        }
        let outcome = match args.outcome {
            ExperienceOutcomeValue::Helpful => ExperienceOutcome::Helpful,
            ExperienceOutcomeValue::Harmful => ExperienceOutcome::Harmful,
        };
        let outcome_str = outcome.as_str();
        let key = project_key(&ctx.cwd);
        let Some(entry) =
            record_outcome(home(ctx), &key, &args.id, outcome, args.evidence).map_err(map_io)?
        else {
            return Err(ToolError::failed(format!(
                "ExperienceOutcome: entry {} not found",
                args.id
            )));
        };
        Ok(ToolResult {
            content: json!({
                "id": entry.id,
                "outcome": outcome_str,
                "helpful": entry.helpful,
                "harmful": entry.harmful,
                "outcome_history": entry.outcome_history,
                "status": entry.status.as_str(),
                "verified_at": entry.verified_at,
                "note": "Outcome recorded. Lifecycle status and verified_at were not changed automatically.",
            }),
            ..Default::default()
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExperienceForgetInput {
    /// Entry id (the full_id returned by ExperienceQuery).
    pub id: String,
}

pub struct ExperienceForgetTool;

/// ExperienceForget tool prompt: evict (requires user confirmation).
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
                    format!("forgotten E{}", args.id.chars().take(4).collect::<String>())
                } else {
                    format!("E{} does not exist, nothing to delete", args.id.chars().take(4).collect::<String>())
                },
            }),
            ..Default::default()
        })
    }
}

fn map_io(e: ExperienceError) -> ToolError {
    ToolError::failed(format!("[Experience] {e}"))
}

/// Project experience index injected at session start (active entries only, ≤10 lines;
/// empty string if none).
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
            watch: crate::app::AppCore::start(Default::default()).watch(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(home, "test")),
            hooks: Settings::default().hooks,
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: Arc::new(|_t, _q, _o| Box::pin(async { None })),
            home: home.to_path_buf(),
            instance: None,
            rewind: Default::default(),
        }
    }

    fn tmp(tag: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bingo-exp-tool-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        (root.join("home"), root)
    }

    fn propose_input() -> serde_json::Value {
        json!({
            "trigger": ["migration"],
            "summary": "migrate the database in three steps",
            "steps": ["back up", "run the migration", "verify"],
            "verify": "cargo test",
            "evidence": "session 2026-08-04",
        })
    }

    #[tokio::test]
    async fn propose_does_not_write() {
        let (home, cwd) = tmp("propose");
        let ctx = ctx_at(&home, &cwd);
        let tool = ExperienceProposeTool;
        let result = tool.call(propose_input(), &ctx).await.unwrap();
        assert!(
            result.content["candidate"]["id_short"]
                .as_str()
                .unwrap()
                .starts_with('E')
        );
        assert!(
            load_entries(&home, &project_key(&cwd)).is_empty(),
            "propose does not persist"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn commit_writes_and_recommit_updates() {
        let (home, cwd) = tmp("commit");
        let ctx = ctx_at(&home, &cwd);
        let tool = ExperienceCommitTool;
        let first = tool.call(propose_input(), &ctx).await.unwrap();
        let id = first.content["id"].as_str().unwrap().to_string();
        assert!(
            first.content["confirmation"]
                .as_str()
                .unwrap()
                .starts_with("consolidated")
        );
        assert_eq!(first.content["hits"], 0);
        let path = first.content["path"].as_str().unwrap().to_string();
        assert!(path.contains("experience"), "{path}");

        // Re-committing the same content → update, not duplicate: hits 1.
        let second = tool.call(propose_input(), &ctx).await.unwrap();
        assert_eq!(second.content["id"].as_str().unwrap(), id);
        assert_eq!(second.content["hits"], 1);
        let entries = load_entries(&home, &project_key(&cwd));
        assert_eq!(entries.len(), 1, "same id overwrites without duplicates");
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
            .call(
                json!({"trigger": ["t"], "summary": "", "steps": ["1"]}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("summary"));
        let err = tool
            .call(json!({"trigger": ["t"], "summary": "s", "steps": []}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("steps"));
        let err = tool
            .call(
                json!({"trigger": ["t"], "summary": "s", "steps": ["1"], "status": "bogus"}),
                &ctx,
            )
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
        // Mark stale: hits do not increase.
        let stale = json!({
            "trigger": ["migration"],
            "summary": "migrate the database in three steps",
            "steps": ["back up", "run the migration", "verify"],
            "status": "stale",
        });
        let result = commit.call(stale, &ctx).await.unwrap();
        assert_eq!(result.content["status"], "stale");
        assert_eq!(
            result.content["hits"], 0,
            "writing stale does not count hits"
        );

        // The index excludes stale entries.
        assert!(
            session_index(&home, &cwd).is_empty(),
            "stale entries are excluded from the injection index"
        );

        // Query can still find it (for on-site review).
        let query_tool = ExperienceQueryTool;
        let q = query_tool
            .call(json!({"query": "migration"}), &ctx)
            .await
            .unwrap();
        assert_eq!(q.content["matches"][0]["status"], "stale");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn outcome_schema_matches_mvp_contract() {
        let tool = ExperienceOutcomeTool;
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        for field in ["id", "outcome", "evidence"] {
            assert!(required.contains(&json!(field)));
        }
        assert_eq!(required.len(), 3);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["id"]["type"], "string");
        assert!(
            schema["properties"]["outcome"]["$ref"]
                .as_str()
                .is_some_and(|reference| reference.ends_with("/ExperienceOutcomeValue"))
        );
        assert_eq!(
            schema["definitions"]["ExperienceOutcomeValue"]["enum"],
            json!(["helpful", "harmful"])
        );
        assert_eq!(schema["properties"]["evidence"]["type"], "string");
        assert!(!tool.is_read_only(&json!({})));
        assert!(!tool.is_destructive(&json!({})));
        assert!(!tool.is_concurrency_safe(&json!({})));
    }

    #[tokio::test]
    async fn outcome_roundtrips_history_without_changing_policy_fields() {
        let (home, cwd) = tmp("outcome");
        let ctx = ctx_at(&home, &cwd);
        let key = project_key(&cwd);
        let mut entry = ExperienceEntry::new(
            &key,
            vec!["migration".into()],
            "migrate the database in three steps".into(),
            vec![
                "back up".into(),
                "run the migration".into(),
                "verify".into(),
            ],
            Some("cargo test".into()),
            None,
        );
        entry.status = ExperienceStatus::Degraded;
        entry.verified_at = Some("2024-06-01".into());
        save_entry(&home, &key, &entry).unwrap();
        let outcome = ExperienceOutcomeTool;

        let helpful = outcome
            .call(
                json!({
                    "id": entry.id,
                    "outcome": "helpful",
                    "evidence": "focused verification passed"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            (
                helpful.content["helpful"].as_u64(),
                helpful.content["harmful"].as_u64()
            ),
            (Some(1), Some(0))
        );
        assert_eq!(helpful.content["status"], "degraded");
        assert_eq!(helpful.content["verified_at"], "2024-06-01");
        assert_eq!(helpful.content["outcome_history"][0]["outcome"], "helpful");
        assert_eq!(
            helpful.content["outcome_history"][0]["evidence"],
            "focused verification passed"
        );

        let harmful = outcome
            .call(
                json!({
                    "id": helpful.content["id"].as_str().unwrap(),
                    "outcome": "harmful",
                    "evidence": "user found a counterexample"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            (
                harmful.content["helpful"].as_u64(),
                harmful.content["harmful"].as_u64()
            ),
            (Some(1), Some(1))
        );
        assert_eq!(
            harmful.content["outcome_history"].as_array().unwrap().len(),
            2
        );

        let queried = ExperienceQueryTool
            .call(json!({"query": "migration"}), &ctx)
            .await
            .unwrap();
        let found = &queried.content["matches"][0];
        assert_eq!(
            (found["helpful"].as_u64(), found["harmful"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(found["outcome_history"], harmful.content["outcome_history"]);
        assert_eq!(found["status"], "degraded");
        assert_eq!(found["verified_at"], harmful.content["verified_at"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn recommit_preserves_outcome_counters_and_history() {
        let (home, cwd) = tmp("outcome-recommit");
        let ctx = ctx_at(&home, &cwd);
        let commit = ExperienceCommitTool;
        let first = commit.call(propose_input(), &ctx).await.unwrap();
        ExperienceOutcomeTool
            .call(
                json!({
                    "id": first.content["id"],
                    "outcome": "helpful",
                    "evidence": "cargo test passed"
                }),
                &ctx,
            )
            .await
            .unwrap();

        let recommitted = commit.call(propose_input(), &ctx).await.unwrap();
        assert_eq!(recommitted.content["helpful"], 1);
        assert_eq!(recommitted.content["harmful"], 0);
        let entries = load_entries(&home, &project_key(&cwd));
        assert_eq!(entries[0].outcome_history.len(), 1);
        assert_eq!(entries[0].helpful, 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn outcome_validates_fields_and_missing_id_does_not_mutate() {
        let (home, cwd) = tmp("outcome-validation");
        let ctx = ctx_at(&home, &cwd);
        let commit = ExperienceCommitTool;
        let committed = commit.call(propose_input(), &ctx).await.unwrap();
        let path = committed.content["path"].as_str().unwrap();
        let before = std::fs::read(path).unwrap();
        let tool = ExperienceOutcomeTool;

        for input in [
            json!({"id": "", "outcome": "helpful", "evidence": "observed"}),
            json!({"id": "missing", "outcome": "neutral", "evidence": "observed"}),
            json!({"id": "missing", "outcome": "harmful", "evidence": "counterexample"}),
            json!({"id": committed.content["id"], "outcome": "helpful", "evidence": ""}),
            json!({"id": committed.content["id"], "outcome": "helpful", "evidence": "observed", "unexpected": true}),
            json!({"id": committed.content["id"].as_str().unwrap().chars().take(4).collect::<String>(), "outcome": "helpful", "evidence": "observed"}),
        ] {
            assert!(tool.call(input, &ctx).await.is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
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
        let out = forget.call(json!({"id": id}), &ctx).await.unwrap();
        assert!(out.content["deleted"].as_bool().unwrap());
        assert!(load_entries(&home, &project_key(&cwd)).is_empty());
        // Delete again: not found but succeeds.
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
        assert_eq!(out.content["matches"][0]["steps"][0], "back up");
        assert_eq!(out.content["matches"][0]["verify"], "cargo test");
        assert!(
            out.content["matches"][0]["summary"]
                .as_str()
                .unwrap()
                .contains("migrate")
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
