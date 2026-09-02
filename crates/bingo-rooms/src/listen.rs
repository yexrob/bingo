//! `Listen`: the dial on your own seat (ADR-0029 §4). A member of a room says
//! how much patience it wants and the room's journal records it — its own ear
//! and nobody else's, under a kind of its own, so two seats retuning at once
//! settle by journal order rather than by writing over each other.
//!
//! Joining and leaving are still not verbs: the formation is the seater's, and
//! only the stance is the seat's.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Driver, ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary, Subject,
    Tool, ToolContext, ToolError, ToolOutput, ToolSpec, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::ear::Ear;
use crate::{PLUGIN, ear, name, room};

pub const LISTEN: &str = "Listen";

const DESCRIPTION: &str = "\
Set how much patience your own seat in a room has. `patience_s: 0` is a live \
ear: every post wakes you as it lands. Thirty seconds or more is a patient \
ear: posts wait and you read them whole at your next turn, and you are woken \
once when the oldest has waited that long. Either way a post that says your \
name reaches you at once and is owed an answer. Use a patient ear when you \
convene a room you want to be informed of rather than interrupted by; use a \
live one when the work arrives as posts. This changes your seat only — who is \
in the room is the seater's to say.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListenArgs {
    /// The room, by name or `#name`. It must be one you are seated in.
    pub room: String,
    /// Seconds of patience: 0 for a live ear, 30 or more for a patient one.
    pub patience_s: u64,
}

/// Retuning your own ear: it writes to the room's journal, so the traits are
/// the fail-closed defaults and the card says which room and which ear.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListenTool;

#[async_trait]
impl Tool for ListenTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: LISTEN.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<ListenArgs>(),
            meta: Default::default(),
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<ListenArgs>(input.clone())
            .ok()
            .and_then(|args| Some(card(&args, Ear::asked(args.patience_s).ok()?)))
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ListenArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let ear = Ear::asked(args.patience_s).map_err(refused)?;
        let caller = own(&cx.host, &cx.session).await.map_err(refused)?;
        let (id, title) = reachable(&cx.host, &caller, &args.room).await?;
        let seat = seated(&cx.host, &id, &caller, &title).await?;

        cx.host
            .extend(&id, PLUGIN, &ear::kind(&seat), ear::register(ear))
            .await
            .map_err(refused)?;
        Ok(ToolOutput::text(format!("{title}: {}", ear.said())))
    }
}

/// The one thing a person sees before approving: whose ear, and which way.
fn card(args: &ListenArgs, ear: Ear) -> String {
    let title = name::title(args.room.trim().trim_start_matches('#'));
    match ear {
        Ear::Live => format!("{title} with a live ear"),
        Ear::Patient(patience) => {
            format!("{title} with a patient ear ({}s)", patience.as_secs())
        }
    }
}

/// The room the caller means: one under it, else one beside it. A room reaches
/// the tree it hangs in, so those are the only two a caller can be seated in.
async fn reachable(
    host: &HostHandle,
    caller: &SessionSummary,
    asked: &str,
) -> Result<(SessionId, String), ToolError> {
    let name = name::check(asked.trim().trim_start_matches('#')).map_err(refused)?;
    let title = name::title(name);
    let mut trees = vec![caller.id.clone()];
    trees.extend(caller.parent.as_ref().map(|link| link.session.clone()));
    for tree in trees {
        if let Some(id) = room_under(host, &tree, &title).await {
            return Ok((id, title));
        }
    }
    Err(ToolError::InvalidInput(format!(
        "there is no {title} you can reach: a room is opened by `OpenRoom` or `/room`, and \
         reaches the session it hangs under and that session's other children"
    )))
}

async fn room_under(host: &HostHandle, tree: &SessionId, title: &str) -> Option<SessionId> {
    let children = host
        .sessions(SessionFilter {
            parent: Some(tree.clone()),
            ..SessionFilter::default()
        })
        .await
        .ok()?;
    children
        .into_iter()
        .find(|child| child.driver == Driver::Log && child.title.as_deref() == Some(title))
        .map(|child| child.id)
}

/// The caller's own seat on that roster, spelled as the roster spells it. A
/// caller that is not on it is refused: a seat is taken by the seater, and
/// this tool only says what an existing one hears.
async fn seated(
    host: &HostHandle,
    room: &SessionId,
    caller: &SessionSummary,
    title: &str,
) -> Result<String, ToolError> {
    let mine = caller.title.as_deref().unwrap_or_default();
    let state = room::read(host, room)
        .await
        .ok_or_else(|| ToolError::Failed(format!("{title} could not be read")))?;
    room::members_of(&state)
        .into_iter()
        .find(|member| !mine.is_empty() && name::same(member, mine))
        .ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "you are not seated in {title}, so there is no ear of yours to tune: ask \
                 whoever opened it to seat you"
            ))
        })
}

/// The caller's own summary. There is no filter for one id, so this is the
/// list the host has, read once.
async fn own(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    host.sessions(SessionFilter::default())
        .await?
        .into_iter()
        .find(|summary| &summary.id == session)
        .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
}

/// A refusal in the terms the model can act on: an input it can correct, or a
/// host that failed under it.
fn refused(error: KernelError) -> ToolError {
    match error.code {
        ErrorCode::InvalidInput => ToolError::InvalidInput(error.message),
        _ => ToolError::Failed(error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Seat;
    use crate::seat;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::ToolTraits;
    use serde_json::json;
    use std::time::Duration;

    /// A root, the scout it started, and a room beside the scout that seats it.
    async fn tree() -> (Fleet, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let room = seat::seat(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            "design",
            &[Seat::live("scout")],
        )
        .await
        .expect("a room this crate can open");
        (fleet, scout, room)
    }

    async fn listened(
        fleet: &Fleet,
        caller: &SessionId,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        ListenTool.call(input, &tool_context(caller, fleet)).await
    }

    #[tokio::test]
    async fn a_member_tunes_its_own_ear_and_is_told_what_it_now_wears() {
        let (fleet, scout, room) = tree().await;
        let out = listened(&fleet, &scout, json!({"room": "design", "patience_s": 120}))
            .await
            .expect("a seat may say how it listens");
        assert!(!out.is_error);
        assert_eq!(
            out.parts[0].as_text(),
            Some(
                "#design: you read the room at your next turn, and are woken once it has \
                 stood unread for 120s"
            )
        );
        assert_eq!(
            fleet.ears(&room).of("scout"),
            Ear::Patient(Duration::from_secs(120))
        );

        let live = listened(&fleet, &scout, json!({"room": "#design", "patience_s": 0}))
            .await
            .expect("and may say it the other way");
        assert_eq!(
            live.parts[0].as_text(),
            Some("#design: every post wakes you as it lands")
        );
        assert_eq!(fleet.ears(&room).of("scout"), Ear::Live);
    }

    /// The dial has a dead band, and it is refused in words rather than
    /// rounded into the nearest thing that works.
    #[tokio::test]
    async fn a_patience_under_the_floor_is_refused_and_writes_nothing() {
        let (fleet, scout, room) = tree().await;
        let error = listened(&fleet, &scout, json!({"room": "design", "patience_s": 15}))
            .await
            .expect_err("under the floor");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal");
        };
        assert!(
            message.contains("under thirty seconds of patience"),
            "{message}"
        );
        assert!(message.contains("live seat"), "{message}");
        assert!(
            fleet.ears(&room).retuned().is_empty(),
            "nothing was written"
        );
    }

    #[tokio::test]
    async fn a_caller_off_the_roster_is_refused_and_a_room_that_is_not_there_says_so() {
        let (fleet, _, room) = tree().await;
        let stranger = fleet.child(
            &fleet.summary(&room).parent.expect("a tree").session,
            "reviewer",
        );

        let off = listened(
            &fleet,
            &stranger,
            json!({"room": "design", "patience_s": 300}),
        )
        .await
        .expect_err("a seat is the seater's to give");
        let ToolError::InvalidInput(message) = off else {
            panic!("the wrong kind of refusal");
        };
        assert!(message.contains("not seated in #design"), "{message}");
        assert!(message.contains("ask whoever opened it"), "{message}");

        let missing = listened(
            &fleet,
            &stranger,
            json!({"room": "standup", "patience_s": 0}),
        )
        .await
        .expect_err("no such room");
        let ToolError::InvalidInput(message) = missing else {
            panic!("the wrong kind of refusal");
        };
        assert!(message.contains("no #standup you can reach"), "{message}");
        assert!(fleet.ears(&room).retuned().is_empty());
    }

    /// A room under the caller is found before one beside it, and both are
    /// reachable — the two placements `OpenRoom` offers (ADR-0021).
    #[tokio::test]
    async fn a_caller_reaches_a_room_under_it_and_a_room_beside_it() {
        let (fleet, scout, _) = tree().await;
        let below = seat::seat(
            &fleet.handle(),
            &scout,
            Path::new("/work/project"),
            "standup",
            &[Seat::live("scout")],
        )
        .await
        .expect("a room of the caller's own");

        listened(
            &fleet,
            &scout,
            json!({"room": "standup", "patience_s": 300}),
        )
        .await
        .expect("the caller's own room");
        assert_eq!(
            fleet.ears(&below).of("scout"),
            Ear::Patient(crate::chase::PATIENCE)
        );
    }

    #[test]
    fn the_traits_fail_closed() {
        let traits = ListenTool.traits(&Value::Null);
        assert_eq!(traits, ToolTraits::default());
        assert!(!traits.read_only, "an ear is retuned, not read");
        assert!(!traits.concurrency_safe);
    }

    #[test]
    fn the_card_names_the_room_and_the_ear() {
        let patient = json!({"room": "design", "patience_s": 300});
        assert_eq!(
            ListenTool.subjects(&patient, Path::new("/work")),
            [Subject::Name {
                name: "#design with a patient ear (300s)".into()
            }]
        );
        let live = json!({"room": "#design", "patience_s": 0});
        assert_eq!(
            ListenTool.subjects(&live, Path::new("/work")),
            [Subject::Name {
                name: "#design with a live ear".into()
            }]
        );
        assert!(
            ListenTool
                .subjects(
                    &json!({"room": "design", "patience_s": 15}),
                    Path::new("/work")
                )
                .is_empty(),
            "a call that will be refused names nothing"
        );
    }

    /// Where a model meets the dial: the tool's own words teach it.
    #[test]
    fn the_description_says_what_each_end_of_the_dial_does() {
        assert!(
            DESCRIPTION.contains("`patience_s: 0` is a live"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("Thirty seconds or more"),
            "{DESCRIPTION}"
        );
        assert!(DESCRIPTION.contains("says your name"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("your seat only"), "{DESCRIPTION}");
    }

    #[test]
    fn the_spec_asks_for_a_room_and_a_number() {
        let spec = ListenTool.spec();
        assert_eq!(spec.name, LISTEN);
        assert_eq!(spec.input_schema["required"], json!(["room", "patience_s"]));
    }
}
