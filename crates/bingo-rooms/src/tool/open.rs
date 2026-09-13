//! `OpenRoom`: the door `/room` is, with an agent on the other side of it
//! (ADR-0021). A room is opened through `seat::open` — same name rules, same
//! membership frame — and this tool adds two questions: which session it hangs
//! under, which is the question of who will hear it, and what it is for, which
//! is the question of what belongs in it (ADR-0053 §1).
//!
//! A name that already stands is refused here, and the refusal names the verb
//! that does what was meant: a room is opened once, for one purpose, and a new
//! phase of the work is a new room.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    KernelError, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{refused, seated};
use crate::ear::{self, Listener, Seat};
use crate::placement::{self, Placement};
use crate::seat::Opening;
use crate::{door, name, seat};

pub const OPEN_ROOM: &str = "OpenRoom";

const DESCRIPTION: &str = "\
Open a room — a conversation whose every member reads what is posted into it — \
for one purpose, and say who is in it. The purpose is what the room is for: \
every member reads it at the head of every reading, and when the work moves on \
you open another room rather than reseating this one. A name that already \
stands is refused — `Seat` and `Unseat` change who is in a room that stands, \
`CloseRoom` ends it. Post into it with `SendMessage` to `#name`. \
By default the room hangs under you, so the agents you started are the ones \
who read it; with `shared: true` it hangs under the agent that started you \
instead, so your peers read it. Members are names, not sessions: a name \
nobody holds yet is kept and skipped until someone does. A seat is patient by \
default: it is woken when a post says `@name`, and once when the room has \
stood unread for 300 seconds. \
Name it in `listeners` with `patience_s` to say how long it may stand instead, \
or `patience_s: 0` for a seat every post wakes as it lands. Name `parent` among \
the members to read the room yourself, and to owe an answer to a post that says \
`@parent`.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenRoomArgs {
    /// What to call it: one word, no slashes. Members are told a post came
    /// from `#name`.
    pub name: String,
    /// The one thing the room is for, in a sentence. Every member reads it
    /// above everything the room says; work of another kind is another room.
    pub purpose: String,
    /// Who is in it, by name — the names `SpawnAgent` gave back, or the roles
    /// of the team. Nobody, by default.
    pub members: Option<Vec<String>>,
    /// The seats that hear the room otherwise than a bare name does: `{"name":
    /// "parent", "patience_s": 120}` may stand unread that long before it is
    /// woken, and `"patience_s": 0` is woken by every post as it lands. A name
    /// alone takes the default 300 seconds. A name here need not also be in
    /// `members`.
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
        let seats = args.seats().map_err(refused)?;
        let caller = door::own(&cx.host, &cx.session).await.map_err(refused)?;
        let parent = placement::under(&caller, args.placement()).map_err(refused)?;
        let by = name::signed_by(&caller);
        let opening = Opening {
            name: &args.name,
            purpose: Some(&args.purpose),
            by: &by,
        };
        seat::open(&cx.host, &parent, &cx.cwd, opening, &seats)
            .await
            .map_err(refused)?;
        let title = name::title(args.name.trim());
        let mut out = ToolOutput::text(seat::receipt(&title, &seats));
        out.display = Some(seated(&title, &seats));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post;
    use crate::room::Room;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::{
        Command as _, CommandOutcome, Driver, ParentLink, SessionId, Tone, ToolTraits, TreeNode,
        View,
    };
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

    /// A call, with the purpose every room is opened for filled in where the
    /// test is not about the purpose itself.
    async fn opened(
        fleet: &Fleet,
        caller: &SessionId,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        OpenRoomTool
            .call(for_something(input), &tool_context(caller, fleet))
            .await
    }

    fn for_something(mut input: Value) -> Value {
        if input.get("purpose").is_none() {
            input["purpose"] = json!("settle the storage layout");
        }
        input
    }

    fn node(label: &str, badge: Option<&str>, children: Vec<TreeNode>) -> TreeNode {
        TreeNode {
            label: label.into(),
            badge: badge.map(str::to_string),
            tone: Tone::Neutral,
            children,
        }
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
            json!({
                "name": "design",
                "members": ["helper"],
                "listeners": [{"name": "helper", "patience_s": 0}],
            }),
        )
        .await
        .expect("a room this crate can open");
        assert!(!out.is_error);
        assert_eq!(out.parts[0].as_text(), Some("#design: helper:0"));

        let (id, room) = room_of(&fleet, "#design");
        assert_eq!(room.parent, reviewer, "the caller's own tree");
        assert_eq!(fleet.summary(&id).driver, Driver::Log);
        assert_eq!(fleet.members(&id), ["helper"]);

        let room = room.seated(&fleet.state(&id));
        post::fan_out(&fleet.handle(), &room, "reviewer", "look again")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(delivered[0].0, helper, "the worker the caller started");
    }

    /// ADR-0053 §1: the room's journal says what it is for, and the name that
    /// opened it — the caller's own, which is what the roster verbs check.
    #[tokio::test]
    async fn a_room_says_what_it_was_opened_for_and_who_opened_it() {
        let (fleet, _, reviewer, _) = tree();
        opened(
            &fleet,
            &reviewer,
            json!({"name": "design", "purpose": "settle the storage layout"}),
        )
        .await
        .expect("a room");

        let (id, room) = room_of(&fleet, "#design");
        let state = fleet.state(&id);
        assert_eq!(
            room.seated(&state).purpose.as_deref(),
            Some("settle the storage layout")
        );
        assert_eq!(
            crate::room::Opened::of_state(&state).map(|opened| opened.by),
            Some("reviewer".into())
        );
    }

    #[tokio::test]
    async fn a_call_that_says_no_purpose_is_refused_with_the_schema() {
        let (fleet, _, reviewer, _) = tree();
        let error = OpenRoomTool
            .call(
                json!({"name": "design", "members": ["helper"]}),
                &tool_context(&reviewer, &fleet),
            )
            .await
            .expect_err("a room is opened for one purpose");
        assert!(
            matches!(error, ToolError::InvalidInput(_)),
            "the input it can correct: {error:?}"
        );
        assert!(fleet.created().is_empty(), "nothing was opened");
    }

    #[tokio::test]
    async fn a_shared_room_hangs_under_the_parent_and_a_peer_hears_it() {
        let (fleet, root, reviewer, _) = tree();
        let scout = fleet.child(&root, "scout");
        let out = opened(
            &fleet,
            &reviewer,
            json!({
                "name": "design",
                "members": ["reviewer", "scout"],
                "listeners": [{"name": "scout", "patience_s": 0}],
                "shared": true,
            }),
        )
        .await
        .expect("a room this crate can open");
        assert_eq!(out.parts[0].as_text(), Some("#design: reviewer, scout:0"));

        let (id, room) = room_of(&fleet, "#design");
        assert_eq!(
            room.parent, root,
            "a shared room hangs in the parent's tree"
        );

        let room = room.seated(&fleet.state(&id));
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
        assert_eq!(
            rows,
            [["#design", "settle the storage layout", "helper", ""]],
            "a new room owes nothing"
        );
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

    /// ADR-0053 §2: the name of a room that stands is not reopened here. The
    /// refusal carries what the room is for, who is in it, and the verb that
    /// does what the caller meant.
    #[tokio::test]
    async fn a_standing_name_is_refused_and_the_room_is_left_alone() {
        let (fleet, _, reviewer, _) = tree();
        opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["helper", "scout"] }),
        )
        .await
        .expect("a room");
        let error = opened(
            &fleet,
            &reviewer,
            json!({ "name": "design", "members": ["scout"] }),
        )
        .await
        .expect_err("the same name again");

        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        for said in [
            "#design already stands",
            "settle the storage layout",
            "helper, scout",
            "`Seat`",
            "`Unseat`",
            "`CloseRoom`",
        ] {
            assert!(message.contains(said), "{said} is unsaid: {message}");
        }
        assert_eq!(fleet.created().len(), 1, "the second call opened nothing");
        let id = fleet.titled("#design").expect("the one room");
        assert_eq!(
            fleet.members(&id),
            ["helper", "scout"],
            "and reseated nobody"
        );
    }

    #[tokio::test]
    async fn a_name_that_is_not_one_is_refused_by_the_same_rule_room_uses() {
        let (fleet, _, reviewer, _) = tree();
        for bad in ["two words", "de/sign", "  ", "close"] {
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
        assert_eq!(
            out.display,
            Some(View::Tree {
                nodes: vec![node(
                    "#design",
                    None,
                    vec![node("nobody yet", None, vec![])]
                )]
            }),
            "an empty roster says so in the block as it does in the words"
        );
        assert!(
            fleet
                .members(&fleet.titled("#design").expect("the room"))
                .is_empty()
        );
    }

    /// The block lane (ADR-0013 §2): the seats the call took, asserted as the
    /// value a surface draws.
    #[tokio::test]
    async fn the_block_draws_the_room_and_the_seats_it_took() {
        let (fleet, _, reviewer, _) = tree();
        let out = opened(
            &fleet,
            &reviewer,
            json!({
                "name": "design",
                "members": ["helper", "scout"],
                "listeners": [{"name": "watcher", "patience_s": 120}],
            }),
        )
        .await
        .expect("a room with a listening seat in it");
        assert_eq!(
            out.display,
            Some(View::Tree {
                nodes: vec![node(
                    "#design",
                    None,
                    vec![
                        node("helper", None, vec![]),
                        node("scout", None, vec![]),
                        node("watcher", Some("120s"), vec![]),
                    ]
                )]
            })
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
    }

    /// The one thing a person sees before approving. It is also the rule an
    /// "always" answer installs, so it is asserted whole.
    #[test]
    fn the_card_names_the_room_the_members_and_where_it_will_hang() {
        let shared = for_something(
            json!({ "name": "design", "members": ["reviewer", "scout"], "shared": true }),
        );
        assert_eq!(
            OpenRoomTool.subjects(&shared, Path::new("/work")),
            [Subject::Name {
                name: "#design under the caller's parent with reviewer, scout".into()
            }]
        );
        let own = for_something(json!({ "name": "design", "members": ["helper"] }));
        assert_eq!(
            OpenRoomTool.subjects(&own, Path::new("/work")),
            [Subject::Name {
                name: "#design under the caller with helper".into()
            }]
        );
        let empty = for_something(json!({ "name": "design" }));
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
        assert!(
            OpenRoomTool
                .subjects(&json!({ "name": "design" }), Path::new("/work"))
                .is_empty(),
            "a call with no purpose is a call that will be refused"
        );
    }

    /// The structured door onto the same dial `/room name:120` is: a name for
    /// the default ear, a number for its own (ADR-0029 §2, ADR-0034 §6).
    #[tokio::test]
    async fn listeners_seat_a_patient_ear_and_the_receipt_says_so() {
        let (fleet, _, reviewer, _) = tree();
        let out = opened(
            &fleet,
            &reviewer,
            json!({
                "name": "design",
                "members": ["helper"],
                "listeners": [
                    {"name": "helper", "patience_s": 0},
                    "parent",
                    {"name": "watcher", "patience_s": 120},
                ],
            }),
        )
        .await
        .expect("a room with listeners in it");
        assert_eq!(
            out.parts[0].as_text(),
            Some("#design: helper:0, parent, watcher:120")
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

    /// Where a model meets ADR-0028, ADR-0034 §6 and ADR-0053 §1–2: the
    /// pattern, the default and the four verbs are in the tool's own words.
    #[test]
    fn the_description_says_what_naming_the_holder_gets_you() {
        assert!(
            DESCRIPTION.contains("`parent` among the members"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("`@parent`"),
            "the debt is said: {DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("patient by default"),
            "the default is said: {DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("stood unread for 300 seconds"),
            "and what it costs to wait: {DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("`patience_s: 0`"),
            "the other end of the dial is offered: {DESCRIPTION}"
        );
    }

    /// The rule ADR-0053 was written for, in the words the model reads before
    /// it calls: one purpose, one room, and the verb for everything else.
    #[test]
    fn the_description_says_a_room_is_opened_once_for_one_purpose() {
        for said in [
            "for one purpose",
            "open another room rather than reseating this one",
            "A name that already stands is refused",
            "`Seat` and `Unseat`",
            "`CloseRoom` ends it",
        ] {
            assert!(
                DESCRIPTION.contains(said),
                "{said} is unsaid: {DESCRIPTION}"
            );
        }
    }

    #[test]
    fn the_spec_asks_for_a_name_and_a_purpose_and_leaves_the_rest_optional() {
        let spec = OpenRoomTool.spec();
        assert_eq!(spec.name, OPEN_ROOM);
        assert_eq!(spec.input_schema["required"], json!(["name", "purpose"]));
        let properties = &spec.input_schema["properties"];
        assert!(properties["members"].is_object(), "{properties}");
        assert!(properties["listeners"].is_object(), "{properties}");
        assert!(properties["shared"].is_object(), "{properties}");
    }
}
