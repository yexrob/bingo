//! `OpenRoom`: the door `/room` is, with an agent on the other side of it
//! (ADR-0021). A room is opened through the same `seat::seat` a person's
//! `/room` calls — same name rules, same reset of a room that already stands,
//! same membership frame — and the only question this tool adds is which
//! session it hangs under, which is the question of who will hear it.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary, Subject, Tool,
    ToolContext, ToolError, ToolOutput, ToolSpec, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::ear::{self, Listener, Seat};
use crate::placement::{self, Placement};
use crate::{name, seat};

pub const OPEN_ROOM: &str = "OpenRoom";

const DESCRIPTION: &str = "\
Open a room — a conversation whose every member reads what is posted into it — \
and say who is in it. Post into it afterwards with `SendMessage` to `#name`. \
By default the room hangs under you, so the agents you started are the ones \
who hear it; with `shared: true` it hangs under the agent that started you \
instead, so your peers hear it. Members are names, not sessions: a name \
nobody holds yet is kept and skipped at delivery until someone does. Name \
`parent` among the members to hear the room yourself: every post reaches you as \
it lands — read at your next stop while you work, or opening a turn of its own \
when you are idle — and one that says `@parent` is owed an answer. A chatty \
room spends your attention that way, so leave `parent` off one that should not, \
or name it in `listeners` instead: a listening seat is handed the posts without \
being woken by them, reads them whole at its next turn, and is woken once when \
they have waited its patience. Opening a room that already stands replaces who \
is in it rather than opening a second one.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenRoomArgs {
    /// What to call it: one word, no slashes. Members are told a post came
    /// from `#name`.
    pub name: String,
    /// Who is in it, by name — the names `SpawnAgent` gave back, or the roles
    /// of the team. Nobody, by default.
    pub members: Option<Vec<String>>,
    /// Which of them listen rather than answer: a name for the default
    /// patience of 300 seconds, or `{"name": "parent", "patience_s": 120}` for
    /// its own. A name here need not also be in `members`.
    pub listeners: Option<Vec<Listener>>,
    /// Hang the room under the agent that started you, so your peers hear it,
    /// instead of under you. `false` by default.
    pub shared: Option<bool>,
}

impl OpenRoomArgs {
    /// The roster it asks for, ears and all.
    fn seats(&self) -> Result<Vec<Seat>, KernelError> {
        ear::seats(
            &self.members.clone().unwrap_or_default(),
            &self.listeners.clone().unwrap_or_default(),
        )
    }

    fn placement(&self) -> Placement {
        Placement::of(self.shared.unwrap_or(false))
    }
}

/// What a person approving the call is shown: the room, the tree it will hang
/// in, and who will be in it. The gate makes this the card's summary and the
/// rule an "always" answer would install, so the room comes first: a rule
/// written `OpenRoom(#design:*)` then covers that room whoever is in it, and
/// `OpenRoom(#design under the caller:*)` covers only the unprivileged
/// placement.
fn card(name: &str, placement: Placement, seats: &[Seat]) -> String {
    format!(
        "{} {} with {}",
        name::title(name.trim()),
        placement.phrase(),
        seat::roster(seats)
    )
}

/// Opening a room in a tree: the session it hangs under is the audience, so
/// this tool's traits are the fail-closed defaults and its card says where.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenRoomTool;

#[async_trait]
impl Tool for OpenRoomTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: OPEN_ROOM.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<OpenRoomArgs>(),
            meta: Default::default(),
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<OpenRoomArgs>(input.clone())
            .ok()
            .and_then(|args| Some(card(&args.name, args.placement(), &args.seats().ok()?)))
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: OpenRoomArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let name = name::check(&args.name).map_err(refused)?;
        let seats = args.seats().map_err(refused)?;
        let caller = own(&cx.host, &cx.session).await.map_err(refused)?;
        let parent = placement::under(&caller, args.placement()).map_err(refused)?;
        seat::seat(&cx.host, &parent, &cx.cwd, name, &seats)
            .await
            .map_err(refused)?;
        Ok(ToolOutput::text(seat::receipt(&name::title(name), &seats)))
    }
}

/// A refusal in the terms the model can act on: an input it can correct, or a
/// host that failed under it.
fn refused(error: KernelError) -> ToolError {
    match error.code {
        ErrorCode::InvalidInput => ToolError::InvalidInput(error.message),
        _ => ToolError::Failed(error.message),
    }
}

/// The caller's own summary, for the parent it hangs under. There is no filter
/// for one id, so this is the list the host has, read once.
async fn own(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    host.sessions(SessionFilter::default())
        .await?
        .into_iter()
        .find(|summary| &summary.id == session)
        .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post;
    use crate::room::Room;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::{Command as _, CommandOutcome, Driver, ParentLink, ToolTraits, View};
    use serde_json::json;

    /// A root, the agent it started, and that agent's own worker: three
    /// storeys, so a placement that slipped by one is visible.
    fn tree() -> (Fleet, SessionId, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        let helper = fleet.child(&reviewer, "helper");
        (fleet, root, reviewer, helper)
    }

    async fn opened(
        fleet: &Fleet,
        caller: &SessionId,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        OpenRoomTool.call(input, &tool_context(caller, fleet)).await
    }

    /// The room the call left, read back as any client reads one.
    fn room_of(fleet: &Fleet, title: &str) -> (SessionId, Room) {
        let id = fleet.titled(title).expect("the room was opened");
        let summary = fleet.summary(&id);
        let room = Room::of(&summary).expect("a room the crate can read");
        (id, room)
    }

    #[tokio::test]
    async fn a_room_hangs_under_the_caller_and_its_own_workers_hear_it() {
        let (fleet, _, reviewer, helper) = tree();
        let out = opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["helper"] }),
        )
        .await
        .expect("a room this crate can open");
        assert!(!out.is_error);
        assert_eq!(out.parts[0].as_text(), Some("#design: helper"));

        let (id, mut room) = room_of(&fleet, "#design");
        assert_eq!(room.parent, reviewer, "the caller's own tree");
        assert_eq!(fleet.summary(&id).driver, Driver::Log);
        assert_eq!(fleet.members(&id), ["helper"]);

        room.members = fleet.members(&id);
        post::fan_out(&fleet.handle(), &room, "reviewer", "look again")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(delivered[0].0, helper, "the worker the caller started");
    }

    #[tokio::test]
    async fn a_shared_room_hangs_under_the_parent_and_a_peer_hears_it() {
        let (fleet, root, reviewer, _) = tree();
        let scout = fleet.child(&root, "scout");
        let out = opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["reviewer", "scout"], "shared": true }),
        )
        .await
        .expect("a room this crate can open");
        assert_eq!(out.parts[0].as_text(), Some("#design: reviewer, scout"));

        let (id, mut room) = room_of(&fleet, "#design");
        assert_eq!(
            room.parent, root,
            "a shared room hangs in the parent's tree"
        );

        room.members = fleet.members(&id);
        post::fan_out(&fleet.handle(), &room, "reviewer", "stand-up in five")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "the author is not written to");
        assert_eq!(delivered[0].0, scout, "the caller's peer");
    }

    /// One door, so one listing: a room an agent opened is a room `/room`
    /// names, with no second mechanism to teach it about (ADR-0021 §3).
    #[tokio::test]
    async fn room_lists_what_the_tool_opened() {
        let (fleet, _, reviewer, _) = tree();
        opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["helper"] }),
        )
        .await
        .expect("a room");

        let listed = crate::RoomCommand
            .run("", &crate::tests::command_context(&reviewer, &fleet))
            .await
            .expect("a listing");
        let CommandOutcome::View {
            view: View::Table { rows, .. },
        } = listed
        else {
            panic!("a roster is a table");
        };
        assert_eq!(rows, [["#design", "helper", ""]], "a new room owes nothing");
    }

    #[tokio::test]
    async fn a_root_asking_to_share_is_refused_with_the_reason_and_opens_nothing() {
        let (fleet, root, ..) = tree();
        let error = opened(&fleet, &root, json!({ "name": "design", "shared": true }))
            .await
            .expect_err("a root has no peers to convene");
        let ToolError::InvalidInput(message) = error else {
            panic!("a root asking to share gave the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("root"), "{message}");
        assert!(message.contains("without `shared`"), "{message}");
        assert!(fleet.created().is_empty(), "nothing was opened");
    }

    /// The same room twice is one room whose membership is replaced — `/room`'s
    /// own rule, reached through the tool (ADR-0021 §3).
    #[tokio::test]
    async fn a_standing_room_is_reset_not_opened_twice() {
        let (fleet, _, reviewer, _) = tree();
        opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["helper", "scout"] }),
        )
        .await
        .expect("a room");
        opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["scout"] }),
        )
        .await
        .expect("the same room");

        assert_eq!(fleet.created().len(), 1, "the second call opened nothing");
        let id = fleet.titled("#design").expect("the one room");
        assert_eq!(fleet.members(&id), ["scout"], "the membership is replaced");
    }

    #[tokio::test]
    async fn a_name_that_is_not_one_is_refused_by_the_same_rule_room_uses() {
        let (fleet, _, reviewer, _) = tree();
        for bad in ["two words", "de/sign", "  "] {
            let error = opened(&fleet, &reviewer, json!({ "name": bad }))
                .await
                .expect_err("a room name is one word, no slashes");
            assert!(
                matches!(error, ToolError::InvalidInput(_)),
                "{bad:?}: {error:?}"
            );
        }
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn a_room_with_nobody_in_it_is_opened_and_says_so() {
        let (fleet, _, reviewer, _) = tree();
        let out = opened(&fleet, &reviewer, json!({ "name": "design" }))
            .await
            .expect("a room a caller may fill later");
        assert_eq!(out.parts[0].as_text(), Some("#design: nobody yet"));
        assert!(
            fleet
                .members(&fleet.titled("#design").expect("the room"))
                .is_empty()
        );
    }

    /// A room a session opened before is not reused across trees: the key
    /// carries the parent, and so the caller's own room and its shared one are
    /// two rooms of one name.
    #[tokio::test]
    async fn the_two_placements_are_two_rooms() {
        let (fleet, root, reviewer, _) = tree();
        opened(&fleet, &reviewer, json!({ "name": "design" }))
            .await
            .expect("the caller's own");
        opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "shared": true }),
        )
        .await
        .expect("the peers'");

        let created = fleet.created();
        assert_eq!(created.len(), 2);
        let parents: Vec<Option<&ParentLink>> =
            created.iter().map(|spec| spec.parent.as_ref()).collect();
        assert_eq!(parents[0].map(|p| &p.session), Some(&reviewer));
        assert_eq!(parents[1].map(|p| &p.session), Some(&root));
        assert!(
            created
                .iter()
                .all(|spec| spec.parent.as_ref().is_some_and(|p| p.item.is_none())),
            "a room is not linked to the call that opened it (ADR-0011 §3)"
        );
    }

    #[test]
    fn the_traits_fail_closed() {
        let traits = OpenRoomTool.traits(&Value::Null);
        assert_eq!(traits, ToolTraits::default());
        assert!(!traits.read_only, "a room is opened, not read");
        assert!(!traits.concurrency_safe);
        assert_eq!(traits.interrupt, bingo_sdk::Interrupt::Block);
    }

    /// The one thing a person sees before approving. It is also the rule an
    /// "always" answer installs, so it is asserted whole.
    #[test]
    fn the_card_names_the_room_the_members_and_where_it_will_hang() {
        let shared = json!({ "name": "design", "members": ["reviewer", "scout"], "shared": true });
        assert_eq!(
            OpenRoomTool.subjects(&shared, Path::new("/work")),
            [Subject::Name {
                name: "#design under the caller's parent with reviewer, scout".into()
            }]
        );
        let own = json!({ "name": "design", "members": ["helper"] });
        assert_eq!(
            OpenRoomTool.subjects(&own, Path::new("/work")),
            [Subject::Name {
                name: "#design under the caller with helper".into()
            }]
        );
        let empty = json!({ "name": "design" });
        assert_eq!(
            OpenRoomTool.subjects(&empty, Path::new("/work")),
            [Subject::Name {
                name: "#design under the caller with nobody yet".into()
            }]
        );
    }

    /// Nothing to name is nothing to show; the gate falls back to the input.
    #[test]
    fn a_call_that_will_not_parse_names_nothing() {
        assert!(
            OpenRoomTool
                .subjects(&json!({ "members": [] }), Path::new("/work"))
                .is_empty()
        );
    }

    /// The structured door onto the same dial `/room ~name` is: a name for the
    /// default patience, a number for its own (ADR-0029 §2).
    #[tokio::test]
    async fn listeners_seat_a_patient_ear_and_the_receipt_says_so() {
        let (fleet, _, reviewer, _) = tree();
        let out = opened(
            &fleet,
            &reviewer,
            json!({
                "name": "design",
                "members": ["helper"],
                "listeners": ["parent", {"name": "watcher", "patience_s": 120}],
            }),
        )
        .await
        .expect("a room with listeners in it");
        assert_eq!(
            out.parts[0].as_text(),
            Some("#design: helper, ~parent(300s), ~watcher(120s)")
        );

        let id = fleet.titled("#design").expect("the room");
        assert_eq!(fleet.members(&id), ["helper", "parent", "watcher"]);
        let ears = fleet.ears(&id);
        assert_eq!(ears.of("helper"), crate::ear::Ear::Live);
        assert_eq!(
            ears.of("parent"),
            crate::ear::Ear::Patient(crate::chase::PATIENCE)
        );
    }

    #[tokio::test]
    async fn a_patience_under_the_floor_is_refused_and_opens_nothing() {
        let (fleet, _, reviewer, _) = tree();
        let error = opened(
            &fleet,
            &reviewer,
            json!({"name": "design", "listeners": [{"name": "parent", "patience_s": 15}]}),
        )
        .await
        .expect_err("the dead band");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal");
        };
        assert!(
            message.contains("under thirty seconds of patience"),
            "{message}"
        );
        assert!(fleet.created().is_empty(), "nothing was opened");
    }

    /// Where a model meets ADR-0028: the pattern is in the tool's own words.
    #[test]
    fn the_description_says_what_naming_the_holder_gets_you() {
        assert!(
            DESCRIPTION.contains("`parent` among the members"),
            "{DESCRIPTION}"
        );
        assert!(DESCRIPTION.contains("as it lands"), "{DESCRIPTION}");
        assert!(
            DESCRIPTION.contains("`@parent` is owed an answer"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("spends your attention"),
            "the cost is said, not hidden: {DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("name it in `listeners` instead"),
            "the other end of the dial is offered where the cost is: {DESCRIPTION}"
        );
    }

    #[test]
    fn the_spec_asks_for_a_name_and_leaves_the_rest_optional() {
        let spec = OpenRoomTool.spec();
        assert_eq!(spec.name, OPEN_ROOM);
        assert_eq!(spec.input_schema["required"], json!(["name"]));
        let properties = &spec.input_schema["properties"];
        assert!(properties["members"].is_object(), "{properties}");
        assert!(properties["listeners"].is_object(), "{properties}");
        assert!(properties["shared"].is_object(), "{properties}");
    }
}
