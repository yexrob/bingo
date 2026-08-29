//! What one session says to another. Two tools, one noun: `SendMessage`
//! waits in the target's queue for whatever opens its next turn,
//! `FollowupTask` wakes it now (ADR-0010 §1).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Delivery, Input, IntentId, Origin, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec,
    ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::handle::LateHost;
use crate::names;

/// The surface an agent's messages come from; a person's say `tui` or `print`.
pub const SURFACE: &str = "agent";

/// Who a message is from. The kernel's fold turns a principal into a
/// `[from <name>]` line above the text, so a name is never written into the
/// text itself.
pub fn origin(principal: Option<String>) -> Origin {
    Origin {
        surface: SURFACE.into(),
        principal,
        conversation: None,
    }
}

/// Which of the two a call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Waits in the queue; the next turn, whatever opens it, carries it.
    Message,
    /// Opens a turn on an idle target now.
    Followup,
}

const MESSAGE: &str = "\
Write to an agent you started, or — from a sub-agent — to `parent`, the agent \
that started you. The message waits in the target's queue: it is read when the \
target's next turn begins, and it does not start one. Use it for something \
that should reach an agent that is already working, or for an answer to a \
question it asked. `to` is the name `SpawnAgent` gave back, or `parent`.";

const FOLLOWUP: &str = "\
Give an agent you started more work, now. The text reaches it the way the \
first prompt did and starts a turn on it if it is idle; if it is busy, it \
arrives at the end of the round it is running. Use it to continue a task with \
an agent that already has the context, rather than spawning a second one. \
`to` is the name `SpawnAgent` gave back.";

impl Kind {
    pub fn tool_name(self) -> &'static str {
        match self {
            Kind::Message => "SendMessage",
            Kind::Followup => "FollowupTask",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Kind::Message => MESSAGE,
            Kind::Followup => FOLLOWUP,
        }
    }

    fn delivery(self) -> Delivery {
        match self {
            Kind::Message => Delivery::Hold,
            Kind::Followup => Delivery::Wake,
        }
    }

    fn receipt(self, to: &str) -> String {
        match self {
            Kind::Message => format!("Sent to {to}; it will read it when its next turn starts."),
            Kind::Followup => format!("Sent to {to}; it takes it up as its next turn."),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MessageArgs {
    /// The agent to write to: the name `SpawnAgent` gave back, or `parent`
    /// for the agent that started you.
    pub to: String,
    /// What to say, in full. The recipient sees who wrote it.
    pub text: String,
}

/// Posting into another session's queue: this session's own transcript is
/// unchanged by it, and the target gates whatever it then does.
#[derive(Debug)]
pub struct MessageTool {
    kind: Kind,
    host: Arc<LateHost>,
}

impl MessageTool {
    pub fn new(kind: Kind, host: Arc<LateHost>) -> Self {
        Self { kind, host }
    }
}

#[async_trait]
impl Tool for MessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.kind.tool_name().into(),
            description: self.kind.description().into(),
            input_schema: input_schema::<MessageArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits(bingo_sdk::Interrupt::Block)
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<MessageArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.to }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: MessageArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let host = self
            .host
            .require()
            .map_err(|e| ToolError::Failed(e.message))?;
        let to = match names::resolve(host, &cx.session, &args.to).await {
            Ok(to) => to,
            Err(e) => return Ok(ToolOutput::error(e.message)),
        };
        let from = names::speaker(host, &cx.session).await;
        let input = Input::text(args.text, origin(Some(from)));
        cx.host
            .deliver(&to, IntentId::mint(), input, self.kind.delivery())
            .map_err(|e| ToolError::Failed(e.message))?;
        Ok(ToolOutput::text(self.kind.receipt(args.to.trim())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, tool_context};
    use serde_json::json;

    async fn send(kind: Kind, to: &str) -> (ToolOutput, Arc<Recorder>) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        let host = Recorder::new(&fleet);
        let tool = MessageTool::new(kind, fleet.late());
        let cx = tool_context(&root, host.clone());
        let out = tool
            .call(json!({ "to": to, "text": "look again" }), &cx)
            .await
            .expect("a message this crate can deliver");
        (out, host)
    }

    #[tokio::test]
    async fn a_message_waits_in_the_child_s_queue_and_says_who_wrote_it() {
        let (out, host) = send(Kind::Message, "reviewer").await;
        assert!(!out.is_error);
        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1);
        let (_, input, delivery) = &delivered[0];
        assert_eq!(*delivery, Delivery::Hold);
        let Input::Text { text, origin, .. } = input else {
            panic!("a peer delivers text");
        };
        assert_eq!(text, "look again");
        assert_eq!(origin.principal.as_deref(), Some(names::PARENT));
    }

    #[tokio::test]
    async fn a_follow_up_task_wakes_the_child() {
        let (_, host) = send(Kind::Followup, "reviewer").await;
        assert_eq!(host.delivered()[0].2, Delivery::Wake);
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_an_error_result_the_model_can_correct() {
        let (out, host) = send(Kind::Message, "nobody").await;
        assert!(out.is_error);
        assert!(host.delivered().is_empty());
    }

    #[test]
    fn both_tools_read_only_and_name_the_agent_a_rule_may_match() {
        let host = Arc::new(LateHost::default());
        for kind in [Kind::Message, Kind::Followup] {
            let tool = MessageTool::new(kind, Arc::clone(&host));
            assert_eq!(tool.spec().name, kind.tool_name());
            let traits = tool.traits(&Value::Null);
            assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
            assert_eq!(
                tool.subjects(&json!({ "to": "reviewer", "text": "x" }), Path::new("/")),
                [Subject::Name {
                    name: "reviewer".into()
                }]
            );
        }
    }
}
