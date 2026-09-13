//! What one session says to another. One tool, one delivery: `SendMessage`
//! wakes an idle target and reaches a busy one mid-run (ADR-0024 §2). A post
//! into a room goes through the same door and is weighed there first: a closed
//! room takes none (ADR-0053 §4), and one written behind the room's head is
//! handed back with what it missed (ADR-0025) — the draft still in the call
//! that made it, so `again: true` posts those words as they stand
//! (ADR-0053 §6).

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Delivery, Driver, Input, IntentId, Origin, SessionId, SessionState, SessionSummary, Subject,
    Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, View, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{names, rooms, serial, watch};

/// The surface an agent's messages come from; a person's say `tui` or `print`.
pub const SURFACE: &str = "agent";

/// The tool a message is written through, as a journal records the name of the
/// call that wrote one.
pub const SEND_MESSAGE: &str = "SendMessage";

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
teammate's name, `parent`, or a room's `#name`. A room hands a post back when \
somebody spoke while you were writing: read what they said, then post the same \
words again with `again: true` and no `text` — the draft is already in your \
call, so repeating it costs nothing to write.";

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
        Driver::Log => "at each seat's next turn; an @ wakes now",
        Driver::Model => "as its next turn, or mid-run if it is working",
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MessageArgs {
    /// Who to write to: the name `SpawnAgent` gave back, a teammate's name,
    /// `parent` for the agent that started you, or `#name` for a room.
    pub to: String,
    /// What to say, in full. The recipient sees who wrote it. Leave it out
    /// when you are posting `again`.
    pub text: Option<String>,
    /// Post the draft this room last handed back to you, word for word — your
    /// own bounced call still holds it, so there is nothing to write again.
    /// Only for a `#room` that bounced you, and never beside `text`.
    pub again: Option<bool>,
}

/// What a call says to write: its own text, or the latest draft this room
/// bounced (ADR-0053 §6). One or the other — a call that says both says two
/// different things, and a call that says neither says nothing at all.
enum Said {
    Text(String),
    Again,
}

impl Said {
    fn of(text: Option<String>, again: Option<bool>) -> Result<Self, ToolError> {
        match (text, again.unwrap_or(false)) {
            (Some(_), true) => Err(refused("give `text` or `again`, not both")),
            (Some(text), false) => Ok(Said::Text(text)),
            (None, true) => Ok(Said::Again),
            (None, false) => Err(refused(
                "say what to write in `text`, or repeat what a room bounced with `again: true`",
            )),
        }
    }

    /// The words the call carries itself. An `again` carries none: what it
    /// means is the draft the room handed back.
    fn written(self) -> Option<String> {
        match self {
            Said::Text(text) => Some(text),
            Said::Again => None,
        }
    }
}

/// An input the caller is asked to correct and call again with.
fn refused(said: &str) -> ToolError {
    ToolError::InvalidInput(said.into())
}

/// What an `again` addressed to an agent is told: only a room bounces, so
/// only a room holds a draft to repeat.
const NOT_A_ROOM: &str = "`again` repeats a bounced room post; write to an agent with `text`";

/// A closed room takes no post (ADR-0053 §4), so the caller is told what is
/// true of the room rather than of its own call.
fn closed(room: &str) -> String {
    format!("{room} is closed; nothing lands there — open another room")
}

/// An `again` with no bounce of its own behind it.
fn nothing_to_repeat(room: &str) -> String {
    format!("nothing to post again in {room}: no post of yours bounced there")
}

/// A post into a room, weighed before anything lands: a closed room takes
/// none (ADR-0053 §4), an `again` means the draft this room last bounced, and
/// a post written behind the room's head is handed back with what it missed
/// (ADR-0025 §2). What comes back is the text that lands, or what the caller
/// is handed instead of a receipt.
async fn post(
    cx: &ToolContext,
    room: &SessionSummary,
    said: Said,
    from: &str,
) -> Result<String, Box<ToolOutput>> {
    let title = names::name_of(room);
    let Some((there, here)) = journals(cx, &room.id).await else {
        // A room this process cannot read judges nobody — and holds no draft
        // for it to repeat either.
        return said
            .written()
            .ok_or_else(|| Box::new(ToolOutput::error(nothing_to_repeat(title))));
    };
    if rooms::is_closed(&there) {
        return Err(Box::new(ToolOutput::error(closed(title))));
    }
    let text = match said {
        Said::Text(text) => text,
        Said::Again => serial::draft(&here, &cx.item, title)
            .ok_or_else(|| Box::new(ToolOutput::error(nothing_to_repeat(title))))?,
    };
    match serial::bounce(&there, &here, &cx.item, title, from) {
        Some(bounce) => Err(Box::new(bounce)),
        None => Ok(text),
    }
}

/// The two journals a post is weighed against, each read once: the room's own,
/// and the caller's as the model that made this call saw it.
async fn journals(cx: &ToolContext, room: &SessionId) -> Option<(SessionState, SessionState)> {
    let there = watch::snapshot(&cx.host, room).await?;
    let here = watch::snapshot(&cx.host, &cx.session).await?;
    Some((there, here))
}

/// Posting into another session's queue: this session's own transcript is
/// unchanged by it, and the target gates whatever it then does.
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageTool;

#[async_trait]
impl Tool for MessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SEND_MESSAGE.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<MessageArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<MessageArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.to }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: MessageArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let said = Said::of(args.text, args.again)?;
        let to = match names::resolve(&cx.host, &cx.session, &args.to).await {
            Ok(to) => to,
            Err(e) => return Ok(ToolOutput::error(e.message)),
        };
        let from = names::speaker(&cx.host, &cx.session).await;
        let text = match (to.driver, said) {
            (Driver::Log, said) => match post(cx, &to, said, &from).await {
                Ok(text) => text,
                Err(handed_back) => return Ok(*handed_back),
            },
            (Driver::Model, Said::Text(text)) => text,
            (Driver::Model, Said::Again) => return Err(refused(NOT_A_ROOM)),
        };
        let input = Input::text(text, origin(Some(from.clone())));
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
    use bingo_sdk::ContentPart;
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
                    (
                        "read".into(),
                        "at each seat's next turn; an @ wakes now".into()
                    ),
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

    /// The rule of ADR-0024 §4, and the one word a bounce costs (ADR-0053
    /// §6), where the model reads them.
    #[test]
    fn the_description_says_a_direct_message_owes_nothing() {
        assert!(DESCRIPTION.contains("Beside you"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("@name"), "{DESCRIPTION}");
        assert!(
            DESCRIPTION.contains("a direct message is not"),
            "{DESCRIPTION}"
        );
        assert!(DESCRIPTION.contains("`again: true`"), "{DESCRIPTION}");
    }

    /// A room with one member beside the caller, holding a post of that
    /// member's already. The root is off the roster, so it keeps no cursor
    /// there and posts blind (ADR-0025, consequences): the first thing it
    /// writes bounces, which is where every scenario below starts.
    fn spoken_for() -> (Fleet, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let room = fleet.room(&root, "#design");
        fleet.child(&root, "scout");
        fleet.post(&room, "the build is green", Some("scout"));
        (fleet, root, room)
    }

    /// One call, journaled where a completed one lands: the caller's own
    /// history, which is where the next `again` looks for its draft.
    async fn called(
        fleet: &Fleet,
        caller: &SessionId,
        host: Arc<Recorder>,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        let out = MessageTool
            .call(input.clone(), &tool_context(caller, host))
            .await;
        if let Ok(out) = &out {
            fleet.called(caller, SEND_MESSAGE, input, out.clone());
        }
        out
    }

    fn said(out: &ToolOutput) -> String {
        out.parts.iter().filter_map(ContentPart::as_text).collect()
    }

    /// What one delivery carried, and whose name it arrived under.
    fn delivered(host: &Recorder) -> Vec<(String, Option<String>)> {
        host.delivered()
            .into_iter()
            .filter_map(|(_, input, _)| match input {
                Input::Text { text, origin, .. } => Some((text, origin.principal)),
                _ => None,
            })
            .collect()
    }

    /// The whole of ADR-0053 §6: the draft a bounce handed back is the
    /// caller's own bounced call, so one word posts it again, in the words it
    /// was written in and over the caller's own name.
    #[tokio::test]
    async fn a_bounced_post_is_posted_again_by_one_word() {
        let (fleet, root, _) = spoken_for();
        let host = Recorder::new(&fleet);
        let post = json!({ "to": "#design", "text": "stand-up in five" });

        let bounced = called(&fleet, &root, host.clone(), post)
            .await
            .expect("a post this crate can judge");
        assert!(bounced.is_error, "the root never heard the scout's post");
        assert!(delivered(&host).is_empty(), "a bounced post never landed");

        let posted = called(
            &fleet,
            &root,
            host.clone(),
            json!({ "to": "#design", "again": true }),
        )
        .await
        .expect("a post this crate can judge");
        assert!(!posted.is_error, "{}", said(&posted));
        assert_eq!(said(&posted), "Posted to #design.");
        assert_eq!(
            delivered(&host),
            [(
                "stand-up in five".to_string(),
                Some(names::PARENT.to_string())
            )]
        );
    }

    /// An `again` is judged like any other post (ADR-0053 §6): one that landed
    /// while the caller was repeating itself bounces the repeat, carrying only
    /// what is newly missed — and the next `again` posts the same draft.
    #[tokio::test]
    async fn a_post_that_landed_since_bounces_the_repeat_and_the_next_one_lands() {
        let (fleet, root, room) = spoken_for();
        let host = Recorder::new(&fleet);
        let post = json!({ "to": "#design", "text": "stand-up in five" });
        let again = json!({ "to": "#design", "again": true });

        called(&fleet, &root, host.clone(), post)
            .await
            .expect("a post this crate can judge");
        fleet.post(&room, "and the tests pass", Some("scout"));

        let bounced = called(&fleet, &root, host.clone(), again.clone())
            .await
            .expect("a post this crate can judge");
        assert!(bounced.is_error, "the scout spoke again first");
        let text = said(&bounced);
        assert!(text.contains("scout: and the tests pass"), "{text}");
        assert!(
            !text.contains("the build is green"),
            "the first bounce read that one out already: {text}"
        );
        assert!(delivered(&host).is_empty());

        let posted = called(&fleet, &root, host.clone(), again)
            .await
            .expect("a post this crate can judge");
        assert!(!posted.is_error, "{}", said(&posted));
        assert_eq!(
            delivered(&host),
            [(
                "stand-up in five".to_string(),
                Some(names::PARENT.to_string())
            )],
            "an `again` carries no text of its own, so the draft is still the first one"
        );
    }

    /// `text` and `again` are two different things to write, and a call that
    /// says neither says nothing: both are input the caller corrects.
    #[tokio::test]
    async fn a_call_that_says_both_or_neither_is_refused_as_input() {
        let (fleet, root, _) = spoken_for();
        let host = Recorder::new(&fleet);
        let cx = tool_context(&root, host.clone());

        let both = MessageTool
            .call(
                json!({ "to": "#design", "text": "stand-up", "again": true }),
                &cx,
            )
            .await;
        let Err(ToolError::InvalidInput(said)) = both else {
            panic!("two things to write is not one post: {both:?}");
        };
        assert_eq!(said, "give `text` or `again`, not both");

        let neither = MessageTool.call(json!({ "to": "#design" }), &cx).await;
        assert!(
            matches!(&neither, Err(ToolError::InvalidInput(_))),
            "{neither:?}"
        );
        assert!(delivered(&host).is_empty());
    }

    /// Only a room bounces, so only a room holds a draft to repeat.
    #[tokio::test]
    async fn again_to_an_agent_is_refused_as_input() {
        let (fleet, root, _) = spoken_for();
        let host = Recorder::new(&fleet);

        let out = MessageTool
            .call(
                json!({ "to": "scout", "again": true }),
                &tool_context(&root, host.clone()),
            )
            .await;
        let Err(ToolError::InvalidInput(said)) = out else {
            panic!("an agent hands nothing back to repeat: {out:?}");
        };
        assert_eq!(said, NOT_A_ROOM);
        assert!(delivered(&host).is_empty());
    }

    /// An `again` with no bounce behind it is the model's own mistake to
    /// correct in the turn it is in, so it is an error result and not a
    /// refusal of the input: what is missing is a fact of the journal.
    #[tokio::test]
    async fn again_with_nothing_to_repeat_delivers_nothing() {
        let (fleet, root, _) = spoken_for();
        let host = Recorder::new(&fleet);

        let out = called(
            &fleet,
            &root,
            host.clone(),
            json!({ "to": "#design", "again": true }),
        )
        .await
        .expect("a post this crate can judge");
        assert!(out.is_error);
        assert_eq!(
            said(&out),
            "nothing to post again in #design: no post of yours bounced there"
        );
        assert!(delivered(&host).is_empty());
    }

    /// ADR-0053 §4: a closed room takes no post, in either spelling, and the
    /// caller is told what to do instead.
    #[tokio::test]
    async fn nothing_lands_in_a_closed_room() {
        let (fleet, root, room) = spoken_for();
        fleet.extended(
            &room,
            rooms::ROOMS,
            rooms::CLOSED,
            json!({ "at": "2026-09-10T09:00:00Z", "by": "parent", "why": "it is settled" }),
        );
        let host = Recorder::new(&fleet);

        let out = called(
            &fleet,
            &root,
            host.clone(),
            json!({ "to": "#design", "text": "stand-up in five" }),
        )
        .await
        .expect("a post this crate can judge");
        assert!(out.is_error);
        assert_eq!(
            said(&out),
            "#design is closed; nothing lands there — open another room"
        );

        let repeated = called(
            &fleet,
            &root,
            host.clone(),
            json!({ "to": "#design", "again": true }),
        )
        .await
        .expect("a post this crate can judge");
        assert!(repeated.is_error, "{}", said(&repeated));
        assert!(delivered(&host).is_empty());
    }
}
