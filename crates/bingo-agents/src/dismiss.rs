//! `DismissAgent`: a child the caller started, deleted (ADR-0060 §2). The
//! verb is the kernel's `delete`, journal and all; what this module adds is
//! the guard — only an idle child of the caller's own — and the receipt
//! that says what the name means afterwards.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    ErrorCode, HostHandle, KernelError, SessionId, Subject, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::names;
use crate::stop::STOP_AGENT;

pub const DISMISS_AGENT: &str = "DismissAgent";

const DESCRIPTION: &str = "\
Dismiss a sub-agent you started: its session is deleted, journal and all, \
and its name is free again. Only an idle agent is dismissed — one still \
working is refused; `StopAgent` ends its turn first, or wait for it. An \
agent beside you is not yours to dismiss. A room that seated it keeps the \
name on its roster and skips it; a role `.bingo/team.json` declares is \
seated afresh, empty, the next time the project opens. To keep an agent \
and its memory for later, leave it: an idle agent costs nothing.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DismissArgs {
    /// The sub-agent, by the name `SpawnAgent` gave back.
    pub agent: String,
}

/// The child named, deleted, and the receipt. Refused before anything is
/// touched when the child is busy or not the caller's.
pub async fn dismiss(
    host: &HostHandle,
    caller: &SessionId,
    name: &str,
) -> Result<String, KernelError> {
    let child = names::mine(host, caller, name).await?;
    let name = names::name_of(&child).to_string();
    if child.busy {
        return Err(KernelError::new(
            ErrorCode::NotReady,
            format!(
                "{name} is busy: a session is not deleted under a running turn. \
                 `{STOP_AGENT}` ends its turn, or wait for it to finish."
            ),
        ));
    }
    host.delete(&child.id).await?;
    Ok(receipt(&name, &child.id))
}

fn receipt(name: &str, session: &SessionId) -> String {
    format!(
        "{name} dismissed: session {session} is deleted, with its journal, and \
         the name is free."
    )
}

/// Deleting a session of this process: the journal on disk goes with it,
/// which is the one thing no other tool here does. Destructive, and gated
/// as such: asked in `default`, refused in `plan`, named by an allow rule
/// like any tool.
#[derive(Debug, Default, Clone, Copy)]
pub struct DismissAgentTool;

#[async_trait]
impl Tool for DismissAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: DISMISS_AGENT.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<DismissArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            destructive: true,
            read_only: false,
            ..crate::traits()
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<DismissArgs>(input.clone())
            .ok()
            .map(|args| vec![Subject::Name { name: args.agent }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: DismissArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        // A busy child, a teammate, a name nobody has: each is something the
        // model reads and acts on.
        match dismiss(&cx.host, &cx.session, &args.agent).await {
            Ok(receipt) => Ok(ToolOutput::text(receipt)),
            Err(refused) => Ok(ToolOutput::error(refused.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, tool_context};
    use bingo_sdk::SessionFilter;
    use serde_json::json;

    async fn called(fleet: &Fleet, caller: &SessionId, agent: &str) -> ToolOutput {
        let host = Recorder::new(fleet);
        DismissAgentTool
            .call(json!({ "agent": agent }), &tool_context(caller, host))
            .await
            .expect("a dismissal this crate can serve")
    }

    #[tokio::test]
    async fn an_idle_child_is_deleted_and_gone_from_the_tree() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");

        let out = called(&fleet, &root, "reviewer").await;
        assert!(!out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.starts_with("reviewer dismissed"), "{text}");
        assert!(text.contains(reviewer.as_str()), "{text}");
        assert_eq!(fleet.deleted(), vec![reviewer]);

        let left = fleet
            .handle()
            .sessions(SessionFilter::default())
            .await
            .unwrap();
        assert_eq!(names::names_of(&left).len(), 2, "the root and the scout");
        assert!(!names::names_of(&left).contains(&"reviewer".to_string()));
    }

    #[tokio::test]
    async fn a_busy_child_is_refused_and_told_which_verb_ends_its_turn() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);

        let out = called(&fleet, &root, "reviewer").await;
        assert!(out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("reviewer is busy"), "{text}");
        assert!(text.contains(STOP_AGENT), "{text}");
        assert!(fleet.deleted().is_empty(), "nothing was deleted");
    }

    #[tokio::test]
    async fn a_teammate_and_a_stranger_are_refused_with_the_reason() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let builder = fleet.child(&root, "builder");
        fleet.child(&root, "reviewer");

        let out = called(&fleet, &builder, "reviewer").await;
        assert!(out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("beside you, not yours"), "{text}");

        let out = called(&fleet, &root, "nobody").await;
        assert!(out.is_error, "{out:?}");
        assert!(fleet.deleted().is_empty(), "nothing was deleted");
    }

    /// The one tool of this plugin the gate asks about (ADR-0060 §2).
    #[test]
    fn it_is_destructive_and_not_read_only_and_names_the_agent_as_its_subject() {
        let tool = DismissAgentTool;
        let traits = tool.traits(&Value::Null);
        assert!(traits.destructive && traits.trusted);
        assert!(!traits.read_only && !traits.concurrency_safe && !traits.edit);
        assert_eq!(tool.spec().name, DISMISS_AGENT);
        assert_eq!(tool.spec().input_schema["required"], json!(["agent"]));
        assert_eq!(
            tool.subjects(&json!({ "agent": "reviewer" }), Path::new("/")),
            vec![Subject::Name {
                name: "reviewer".into()
            }]
        );
    }
}
