//! What one session says to another. One tool, one delivery: `SendMessage`
//! wakes an idle target and reaches a busy one mid-run (ADR-0024 §2). A post
//! into a room goes through the same door and is checked against the room's
//! head first (ADR-0025).

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Delivery, Driver, Input, IntentId, Origin, Subject, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec, ToolTraits, View, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{names, serial};

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

const DESCRIPTION: &str = "\
Write to another session: an agent you started, a teammate beside you — one \
the same agent started, which `ListAgents` names under `Beside you` — \
`parent`, the agent that started you, or `#room`, a conversation every member \
of which reads it. The message arrives whatever the target is doing: an idle \
one takes it up as its next turn, one that is working reads it mid-run. A \
direct message asks for nothing back: say what you have and go on. When you \
need an answer from someone, ask in a room with `@name` — a mention is owed an \
answer, a direct message is not. `to` is the name `SpawnAgent` gave back, a \
teammate's name, `parent`, or a room's `#name`.";

/// What the caller is told. A log session has no turns (ADR-0011 §1), so a
/// receipt about one is not true of a room: the post is the whole of what
/// happened.
fn receipt(to: &str, driver: Driver) -> String {
    match driver {
        Driver::Log => format!("Posted to {to}."),
        Driver::Model => {
            format!(
                "Sent to {to}; it takes it up as its next turn, or reads it mid-run if it is already working."
            )
        }
    }
}

/// The same delivery a person reads (ADR-0013, the block lane): where it
/// went, whose name it arrives under, and when it will be read. The model has
/// the sentence above; this is the receipt beside it.
fn card(to: &str, from: &str, driver: Driver) -> View {
    View::KeyValue {
        rows: vec![
            ("to".into(), to.to_string()),
            ("from".into(), from.to_string()),
            ("read".into(), read(driver).to_string()),
        ],
    }
}

/// When it will be read, in the few cells a card row has for it.
fn read(driver: Driver) -> &'static str {
    match driver {
        Driver::Log => "by every member, as it lands",
        Driver::Model => "as its next turn, or mid-run if it is working",
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MessageArgs {
    /// Who to write to: the name `SpawnAgent` gave back, a teammate's name,
    /// `parent` for the agent that started you, or `#name` for a room.
    pub to: String,
    /// What to say, in full. The recipient sees who wrote it.
    pub text: String,
}

/// Posting into another session's queue: this session's own transcript is
/// unchanged by it, and the target gates whatever it then does.
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageTool;

#[async_trait]
impl Tool for MessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "SendMessage".into(),
            description: DESCRIPTION.into(),
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
        let to = match names::resolve(&cx.host, &cx.session, &args.to).await {
            Ok(to) => to,
            Err(e) => return Ok(ToolOutput::error(e.message)),
        };
        let from = names::speaker(&cx.host, &cx.session).await;
        // A room is serial: a post written behind its head is handed back
        // with what it missed, and nothing lands (ADR-0025 §2).
        if to.driver == Driver::Log
            && let Some(bounce) = serial::bounce(cx, &to, &from).await
        {
            return Ok(bounce);
        }
        let input = Input::text(args.text, origin(Some(from.clone())));
        cx.host
            .deliver(&to.id, IntentId::mint(), input, Delivery::Wake)
            .await
            .map_err(|e| ToolError::Failed(e.message))?;
        let addressed = args.to.trim();
        let mut out = ToolOutput::text(receipt(addressed, to.driver));
        out.display = Some(card(addressed, &from, to.driver));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, tool_context};
    use serde_json::json;
    use std::sync::Arc;

    /// A root with one child and one room under it, as a person's session
    /// holds both.
    fn fleet() -> (Fleet, bingo_sdk::SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        fleet.room(&root, "#design");
        (fleet, root)
    }

    async fn send(to: &str) -> (ToolOutput, Arc<Recorder>) {
        let (fleet, root) = fleet();
        let host = Recorder::new(&fleet);
        let cx = tool_context(&root, host.clone());
        let out = MessageTool
            .call(json!({ "to": to, "text": "look again" }), &cx)
            .await
            .expect("a message this crate can deliver");
        (out, host)
    }

    #[tokio::test]
    async fn a_message_wakes_the_child_and_says_who_wrote_it() {
        let (out, host) = send("reviewer").await;
        assert!(!out.is_error);
        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1);
        let (_, input, delivery) = &delivered[0];
        assert_eq!(*delivery, Delivery::Wake, "an idle target starts a turn");
        let Input::Text { text, origin, .. } = input else {
            panic!("a peer delivers text");
        };
        assert_eq!(text, "look again");
        assert_eq!(origin.principal.as_deref(), Some(names::PARENT));
    }

    /// The address space of ADR-0024 §1, through the tool the model calls.
    #[tokio::test]
    async fn a_teammate_is_written_to_by_name() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let builder = fleet.child(&root, "builder");
        let reviewer = fleet.child(&root, "reviewer");
        let host = Recorder::new(&fleet);

        let out = MessageTool
            .call(
                json!({ "to": "reviewer", "text": "look again" }),
                &tool_context(&builder, host.clone()),
            )
            .await
            .expect("a message this crate can deliver");
        assert!(!out.is_error);
        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, reviewer);
        let Input::Text { origin, .. } = &delivered[0].1 else {
            panic!("a peer delivers text");
        };
        assert_eq!(origin.principal.as_deref(), Some("builder"));
    }

    /// The block lane (ADR-0013 §2): the same delivery a person reads, asserted
    /// as the value it is.
    #[tokio::test]
    async fn the_card_says_where_it_went_and_when_it_will_be_read() {
        let (sent, _) = send("reviewer").await;
        assert_eq!(
            sent.display,
            Some(View::KeyValue {
                rows: vec![
                    ("to".into(), "reviewer".into()),
                    ("from".into(), "parent".into()),
                    (
                        "read".into(),
                        "as its next turn, or mid-run if it is working".into()
                    ),
                ]
            })
        );

        let (posted, _) = send("#design").await;
        assert_eq!(
            posted.display,
            Some(View::KeyValue {
                rows: vec![
                    ("to".into(), "#design".into()),
                    ("from".into(), "parent".into()),
                    ("read".into(), "by every member, as it lands".into()),
                ]
            }),
            "a room is read by everyone in it, not taken up as a turn"
        );
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_an_error_result_the_model_can_correct() {
        let (out, host) = send("nobody").await;
        assert!(out.is_error);
        assert!(host.delivered().is_empty());
    }

    /// A room has no turns to promise anything about, so the receipt says
    /// what did happen and nothing more.
    #[tokio::test]
    async fn a_message_to_a_room_is_a_post() {
        let (out, host) = send("#design").await;
        assert!(!out.is_error);
        assert_eq!(out.parts[0].as_text(), Some("Posted to #design."));
        assert_eq!(host.delivered().len(), 1);
    }

    #[test]
    fn it_reads_only_and_names_the_agent_a_rule_may_match() {
        assert_eq!(MessageTool.spec().name, "SendMessage");
        let traits = MessageTool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(
            MessageTool.subjects(&json!({ "to": "reviewer", "text": "x" }), Path::new("/")),
            [Subject::Name {
                name: "reviewer".into()
            }]
        );
    }

    /// The rule of ADR-0024 §4, where the model reads it.
    #[test]
    fn the_description_says_a_direct_message_owes_nothing() {
        assert!(DESCRIPTION.contains("Beside you"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("@name"), "{DESCRIPTION}");
        assert!(
            DESCRIPTION.contains("a direct message is not"),
            "{DESCRIPTION}"
        );
    }
}
