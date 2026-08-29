//! `WaitAgent`: hold this turn until a background agent is idle, and read
//! what it said.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    Attachment, Interrupt, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::handle::LateHost;
use crate::{names, watch};

const DESCRIPTION: &str = "\
Wait for a sub-agent you started in the background to finish, and read its \
reply. An agent that is already idle answers at once with what it last said. \
Use it when you cannot go on without the result; otherwise let the agent \
report back on its own and keep working. Waiting does not stop the agent: a \
timeout leaves it running.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitArgs {
    /// The agent to wait for, by the name `SpawnAgent` gave back.
    pub name: String,
    /// Give up after this many seconds. Without one, the wait lasts as long
    /// as the agent's turn does.
    pub timeout_s: Option<u64>,
}

/// Watching a session that is already running; it starts nothing.
#[derive(Debug)]
pub struct WaitAgentTool {
    host: Arc<LateHost>,
}

impl WaitAgentTool {
    pub fn new(host: Arc<LateHost>) -> Self {
        Self { host }
    }
}

/// The reply, or the wait cut short. A timeout is the agent's state, not a
/// failure of this call: it says the work is still going on.
async fn wait_for(
    attachment: &mut Attachment,
    cx: &ToolContext,
    args: &WaitArgs,
) -> Result<String, ToolError> {
    let Some(seconds) = args.timeout_s else {
        return watch::next_reply(attachment, &cx.cancel).await;
    };
    let wait = watch::next_reply(attachment, &cx.cancel);
    match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
        Ok(reply) => reply,
        Err(_) => Err(ToolError::Failed(format!(
            "{} was still working after {seconds}s; it is still running, so wait again or write to it",
            args.name
        ))),
    }
}

#[async_trait]
impl Tool for WaitAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "WaitAgent".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<WaitArgs>(),
            meta: Default::default(),
        }
    }

    /// Interrupting the turn drops the wait, never the agent.
    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits(Interrupt::Cancel)
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<WaitArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WaitArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let host = self
            .host
            .require()
            .map_err(|e| ToolError::Failed(e.message))?;
        let session = match names::child(host, &cx.session, args.name.trim()).await {
            Ok(session) => session,
            Err(e) => return Ok(ToolOutput::error(e.message)),
        };
        let mut attachment = watch::follow(host, &session)
            .await
            .map_err(|e| ToolError::Failed(e.message))?;
        // An idle agent has already said everything it is going to say.
        let reply = match attachment.snapshot.turn {
            None => watch::last_reply(&attachment.snapshot),
            Some(_) => wait_for(&mut attachment, cx, &args).await?,
        };
        Ok(ToolOutput::text(watch::replied(
            &args.name, &session, &reply,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, tool_context, turn_completed};
    use serde_json::json;

    async fn waited(fleet: &Fleet, caller: &bingo_sdk::SessionId, input: Value) -> ToolOutput {
        let host = Recorder::new(fleet);
        WaitAgentTool::new(fleet.late())
            .call(input, &tool_context(caller, host))
            .await
            .expect("a wait this crate can serve")
    }

    #[tokio::test]
    async fn an_agent_still_working_is_waited_for() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.set_busy(&child, true);
        fleet.script([assistant("done at last"), turn_completed()]);

        let out = waited(&fleet, &root, json!({ "name": "reviewer" })).await;
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("done at last"), "{text}");
    }

    #[tokio::test]
    async fn an_idle_agent_answers_with_what_it_last_said() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.said(&child, "the diff is fine");

        let out = waited(&fleet, &root, json!({ "name": "reviewer" })).await;
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("the diff is fine"), "{text}");
    }

    #[tokio::test]
    async fn an_agent_that_does_not_finish_in_time_is_still_running() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.set_busy(&child, true);
        fleet.script([assistant("still going")]);

        let host = Recorder::new(&fleet);
        let error = WaitAgentTool::new(fleet.late())
            .call(
                json!({ "name": "reviewer", "timeout_s": 0 }),
                &tool_context(&root, host),
            )
            .await
            .expect_err("the wait ran out");
        assert!(error.to_string().contains("still running"), "{error}");
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_an_error_result() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let out = waited(&fleet, &root, json!({ "name": "nobody" })).await;
        assert!(out.is_error);
    }

    #[test]
    fn it_reads_and_a_rule_may_name_the_agent_it_waits_for() {
        let tool = WaitAgentTool::new(Arc::new(LateHost::default()));
        assert_eq!(tool.spec().name, "WaitAgent");
        assert!(tool.spec().input_schema.get("$schema").is_none());
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(traits.interrupt, Interrupt::Cancel);
        assert_eq!(
            tool.subjects(&json!({ "name": "reviewer" }), Path::new("/")),
            [Subject::Name {
                name: "reviewer".into()
            }]
        );
    }
}
