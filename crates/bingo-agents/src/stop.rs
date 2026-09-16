//! `StopAgent`: the running turn of a child the caller started, ended
//! (ADR-0060 §1). The verb is the kernel's `interrupt`, reached through an
//! attachment to the child; what this module adds is the reading — an idle
//! child refused in words, and a receipt that says how the turn ended once
//! it has, rather than that a stop was sent.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Attachment, ErrorCode, Event, HostHandle, IntentId, IntentOutcome, InterruptScope, KernelError,
    SessionId, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, TurnStatus,
    input_schema,
};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{names, watch};

pub const STOP_AGENT: &str = "StopAgent";

const DESCRIPTION: &str = "\
Stop a sub-agent you started: the turn it is running ends where it stands, \
every call in flight dropped, and the agent stays — idle, with its memory, \
so a message opens its next turn. The call returns once the turn has \
ended and says how. An agent that is already idle is refused: there is \
nothing to stop. An agent beside you is not yours to stop. To remove an \
agent altogether, `DismissAgent`.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StopArgs {
    /// The sub-agent, by the name `SpawnAgent` gave back.
    pub agent: String,
}

/// What the stop came to, read off the child's own frames.
#[derive(Clone, Debug, PartialEq)]
pub enum Stopped {
    /// The turn ended, with this status: interrupted when the stop landed,
    /// completed or failed when it ended on its own first.
    Ended(TurnStatus),
    /// The kernel refused the interrupt: no turn was running by the time it
    /// arrived.
    AlreadyIdle,
}

/// The child named, its turn ended, and the receipt. Refused before
/// anything is sent when the child is idle or not the caller's.
pub async fn stop(
    host: &HostHandle,
    caller: &SessionId,
    name: &str,
) -> Result<String, KernelError> {
    let child = names::mine(host, caller, name).await?;
    let name = names::name_of(&child).to_string();
    let mut attachment = watch::follow(host, &child.id).await?;
    if !attachment.snapshot.busy() {
        return Err(KernelError::new(
            ErrorCode::NotReady,
            format!("{name} is idle: no turn is running to stop. A message opens its next turn."),
        ));
    }
    let intent = IntentId::mint();
    attachment
        .handle
        .interrupt(intent.clone(), InterruptScope::Head);
    let stopped = ended(host, &mut attachment, &intent).await?;
    Ok(receipt(&name, &stopped))
}

/// The frames after the interrupt, read to the one that says what became
/// of the turn. A lag reopens the child, as a spawn's watcher does: the
/// fresh snapshot knows whether the turn ended inside the gap.
async fn ended(
    host: &HostHandle,
    attachment: &mut Attachment,
    intent: &IntentId,
) -> Result<Stopped, KernelError> {
    loop {
        let Some(frame) = attachment.events.next().await else {
            return Err(KernelError::new(
                ErrorCode::SessionClosed,
                "the agent's session ended before its turn did",
            ));
        };
        if matches!(frame.event, Event::Lagged { .. }) {
            let session = attachment.session.clone();
            *attachment = watch::follow(host, &session).await?;
            if let Some(status) = idle_status(attachment) {
                return Ok(Stopped::Ended(status));
            }
            continue;
        }
        attachment.snapshot.apply(&frame);
        match frame.event {
            Event::TurnCompleted { status, .. } => return Ok(Stopped::Ended(status)),
            Event::IntentAck {
                intent: acked,
                outcome: IntentOutcome::Rejected { .. },
            } if &acked == intent => return Ok(Stopped::AlreadyIdle),
            _ => {}
        }
    }
}

/// How the last turn ended, for a child that is idle again after a lag.
fn idle_status(attachment: &Attachment) -> Option<TurnStatus> {
    if attachment.snapshot.busy() {
        return None;
    }
    attachment.snapshot.last_status().cloned()
}

/// What the caller reads: the agent is idle either way, and how it came
/// to be.
fn receipt(name: &str, stopped: &Stopped) -> String {
    match stopped {
        Stopped::Ended(TurnStatus::Interrupted { .. }) => format!(
            "{name} stopped: its turn was interrupted where it stood. It is idle \
             now, with its memory; a message opens its next turn."
        ),
        Stopped::Ended(TurnStatus::Completed) => {
            format!("{name} finished before the stop landed: its turn completed, and it is idle.")
        }
        Stopped::Ended(TurnStatus::Failed { error }) => format!(
            "{name}'s turn failed before the stop landed: {}. It is idle.",
            error.message
        ),
        Stopped::AlreadyIdle => format!("{name} was already idle: no turn was running."),
    }
}

/// Ending a turn in another session of this process: nothing outside it
/// changes, nothing is deleted, and the child's own journal records the
/// interruption as it records one a person made.
#[derive(Debug, Default, Clone, Copy)]
pub struct StopAgentTool;

#[async_trait]
impl Tool for StopAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: STOP_AGENT.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<StopArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<StopArgs>(input.clone())
            .ok()
            .map(|args| vec![Subject::Name { name: args.agent }])
            .unwrap_or_default()
    }

    /// Cancelling the call stops the wait for the turn's end, never the
    /// interrupt already sent: the child ends its turn either way.
    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: StopArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let stopped = tokio::select! {
            () = cx.cancel.cancelled() => return Err(ToolError::Cancelled),
            stopped = stop(&cx.host, &cx.session, &args.agent) => stopped,
        };
        // An idle child, a teammate, a name nobody has: each is something
        // the model reads and acts on.
        match stopped {
            Ok(receipt) => Ok(ToolOutput::text(receipt)),
            Err(refused) => Ok(ToolOutput::error(refused.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{
        Fleet, Recorder, interrupt_rejected, tool_context, turn_completed, turn_interrupted,
    };
    use serde_json::json;

    async fn called(fleet: &Fleet, caller: &SessionId, agent: &str) -> ToolOutput {
        let host = Recorder::new(fleet);
        StopAgentTool
            .call(json!({ "agent": agent }), &tool_context(caller, host))
            .await
            .expect("a stop this crate can serve")
    }

    #[tokio::test]
    async fn a_busy_child_is_interrupted_and_the_receipt_says_how_its_turn_ended() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);
        fleet.script([turn_interrupted()]);

        let out = called(&fleet, &root, "reviewer").await;
        assert!(!out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.starts_with("reviewer stopped"), "{text}");
        assert!(text.contains("idle"), "{text}");
        assert_eq!(
            fleet.interrupted(),
            vec![(reviewer, InterruptScope::Head)],
            "whatever is running now, in the child and nowhere else"
        );
    }

    #[tokio::test]
    async fn a_turn_that_ended_on_its_own_first_is_said_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);
        fleet.script([turn_completed()]);

        let out = called(&fleet, &root, "reviewer").await;
        assert!(!out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("finished before the stop landed"), "{text}");
    }

    #[tokio::test]
    async fn a_refused_interrupt_reads_as_already_idle_and_waits_for_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);
        // The call mints its intent, so the reading is tested below the
        // call with an intent this test holds: the ack names it, and
        // nothing else on the stream is waited for.
        let intent = IntentId::mint();
        fleet.script([interrupt_rejected(&intent)]);
        let host = fleet.handle();
        let mut attachment = watch::follow(&host, &reviewer).await.unwrap();
        let stopped = ended(&host, &mut attachment, &intent).await.unwrap();
        assert_eq!(stopped, Stopped::AlreadyIdle);
        assert_eq!(
            receipt("reviewer", &stopped),
            "reviewer was already idle: no turn was running."
        );
    }

    #[tokio::test]
    async fn an_idle_child_is_refused_before_anything_is_sent() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");

        let out = called(&fleet, &root, "reviewer").await;
        assert!(out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("reviewer is idle"), "{text}");
        assert!(fleet.interrupted().is_empty(), "nothing was sent");
    }

    #[tokio::test]
    async fn a_teammate_and_a_stranger_are_refused_with_the_reason() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let builder = fleet.child(&root, "builder");
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);

        let out = called(&fleet, &builder, "reviewer").await;
        assert!(out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("beside you, not yours"), "{text}");

        let out = called(&fleet, &root, "nobody").await;
        assert!(out.is_error, "{out:?}");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("you started: builder, reviewer"), "{text}");
        assert!(fleet.interrupted().is_empty(), "nothing was sent");
    }

    #[test]
    fn it_is_the_plugin_s_kind_of_tool_and_names_the_agent_as_its_subject() {
        let tool = StopAgentTool;
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.destructive);
        assert_eq!(tool.spec().name, STOP_AGENT);
        assert_eq!(tool.spec().input_schema["required"], json!(["agent"]));
        assert_eq!(
            tool.subjects(&json!({ "agent": "reviewer" }), Path::new("/")),
            vec![Subject::Name {
                name: "reviewer".into()
            }]
        );
    }
}
