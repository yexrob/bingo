//! What this session has running: `ListAgents` for the model, the same rows
//! for `/agents`. The roster is the session tree read at the moment it is
//! asked for — this plugin keeps no list of its own.

use async_trait::async_trait;
use bingo_sdk::{
    Interrupt, SessionSummary, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::names;

/// The columns a roster has, wherever it is shown.
pub const HEADERS: [&str; 3] = ["agent", "session", "state"];

const DESCRIPTION: &str = "\
List the sub-agents this session has started: their names, their sessions and \
whether each is working or idle. Use it before writing to one whose name you \
are unsure of, or to see whether the ones you started are still running.";

/// The arguments a listing takes, which is none. Named so the schema the
/// model reads is an object like every other tool's.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListArgs {}

/// One agent as a row: its name, its session and whether it is working.
pub fn row(child: &SessionSummary) -> Vec<String> {
    vec![
        names::name_of(child).to_string(),
        child.id.to_string(),
        state(child).to_string(),
    ]
}

pub fn rows(children: &[SessionSummary]) -> Vec<Vec<String>> {
    children.iter().map(row).collect()
}

/// What a session is doing, in the one word every roster shows.
pub fn state(child: &SessionSummary) -> &'static str {
    if child.busy { "busy" } else { "idle" }
}

/// The roster as the model reads it, one agent per line.
fn listing(children: &[SessionSummary]) -> String {
    if children.is_empty() {
        return "No agents are running. SpawnAgent starts one.".to_string();
    }
    children
        .iter()
        .map(|child| row(child).join("  "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reading the session tree; it starts nothing and changes nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListAgentsTool;

#[async_trait]
impl Tool for ListAgentsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ListAgents".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<ListArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits(Interrupt::Cancel)
    }

    /// The arguments are ignored: a listing has none, and a model that sends
    /// an empty object, a null or a stray key still gets its answer.
    async fn call(&self, _input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let children = names::agents(&cx.host, &cx.session)
            .await
            .map_err(|e| ToolError::Failed(e.message))?;
        Ok(ToolOutput::text(listing(&children)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, tool_context};
    use serde_json::json;

    async fn listed(fleet: &Fleet, session: &bingo_sdk::SessionId) -> String {
        let host = Recorder::new(fleet);
        let out = ListAgentsTool
            .call(json!({}), &tool_context(session, host))
            .await
            .expect("a listing");
        out.parts[0].as_text().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn every_child_of_this_session_with_its_state() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        fleet.set_busy(&reviewer, true);

        let text = listed(&fleet, &root).await;
        assert!(
            text.contains(&format!("reviewer  {reviewer}  busy")),
            "{text}"
        );
        assert!(text.contains("scout"), "{text}");
        assert!(text.contains("idle"), "{text}");
    }

    #[tokio::test]
    async fn a_session_that_started_nothing_is_told_how_to() {
        let fleet = Fleet::default();
        let root = fleet.root();
        assert!(listed(&fleet, &root).await.contains("SpawnAgent"));
    }

    #[tokio::test]
    async fn a_child_sees_its_own_children_and_not_its_siblings() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        assert!(listed(&fleet, &reviewer).await.contains("No agents"));
    }

    #[test]
    fn the_listing_and_the_table_have_the_same_columns() {
        let child = crate::tests::summary("ses_child", Some("reviewer"), None);
        assert_eq!(row(&child).len(), HEADERS.len());
        let tool = ListAgentsTool;
        assert!(tool.spec().input_schema.get("$schema").is_none());
        assert_eq!(tool.spec().input_schema["type"], "object");
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
    }
}
