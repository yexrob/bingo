//! `Seat`: a name added to a room that stands (ADR-0053 §3). The roster it has
//! plus the names asked for, published whole — the roster stays one frame — and
//! a post the caller signs so the room reads that it happened.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    KernelError, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{refused, seated, somebody};
use crate::ear::{self, Listener, Seat};
use crate::{door, name, seat};

pub const SEAT: &str = "Seat";

const DESCRIPTION: &str = "\
Seat one or more names in a room that already stands: the roster it has plus \
the names you give, published whole. The room reads that you seated them, and \
each new seat starts reading at the room's head, so what was said before it is \
not a backlog it owes. A name already seated keeps its place — give it a \
`listeners` entry to change the ear it wears. Only the session a room hangs \
under and whoever opened it may seat anyone in it; use it to bring a name into \
the purpose the room already has, and open a room of its own for work of \
another kind.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SeatArgs {
    /// The room, by name or `#name`: one under you, or one beside you.
    pub room: String,
    /// The names to seat, in the order they should take their places.
    pub members: Option<Vec<String>>,
    /// The seats that hear the room otherwise than a bare name does: `{"name":
    /// "scout", "patience_s": 0}` is woken by every post as it lands, and a
    /// number of seconds is how long it may stand unread instead. A name here
    /// need not also be in `members`.
    pub listeners: Option<Vec<Listener>>,
}

impl SeatArgs {
    /// The seats it asks for, ears and all. A call that names nobody is
    /// refused: an empty roster move reads like one that worked.
    fn seats(&self) -> Result<Vec<Seat>, KernelError> {
        somebody(
            ear::seats(
                &self.members.clone().unwrap_or_default(),
                &self.listeners.clone().unwrap_or_default(),
            )?,
            "seat",
        )
    }
}

/// What a person approving the call is shown: the room, and the seats it is
/// about to take. The room comes first, so an "always" answer written
/// `Seat(#design:*)` covers that room and no other.
fn card(room: &str, seats: &[Seat]) -> String {
    format!(
        "{} seats {}",
        name::title(room.trim().trim_start_matches('#')),
        seat::roster(seats)
    )
}

/// Seating somebody in a room: it writes to the room's journal and wakes the
/// seats in it, so the traits are the fail-closed defaults.
#[derive(Debug, Default, Clone, Copy)]
pub struct SeatTool;

#[async_trait]
impl Tool for SeatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SEAT.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SeatArgs>(),
            meta: Default::default(),
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<SeatArgs>(input.clone())
            .ok()
            .and_then(|args| Some(card(&args.room, &args.seats().ok()?)))
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SeatArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let seats = args.seats().map_err(refused)?;
        let caller = door::own(&cx.host, &cx.session).await.map_err(refused)?;
        let (id, room) = door::entered(&cx.host, &caller, &args.room)
            .await
            .map_err(refused)?;
        let roster = seat::join(&cx.host, &id, &room, &seats, &name::signed_by(&caller))
            .await
            .map_err(refused)?;
        let mut out = ToolOutput::text(seat::receipt(&room.title, &roster));
        out.display = Some(seated(&room.title, &roster));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seat::Opening;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::{SessionId, ToolTraits, View};
    use serde_json::json;

    /// A root that holds `#design`, the reviewer that opened it, and a scout
    /// beside them both.
    async fn tree() -> (Fleet, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        seat::open(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            Opening {
                name: "design",
                purpose: Some("settle the storage layout"),
                by: "reviewer",
            },
            &[Seat::named("reviewer")],
        )
        .await
        .expect("a room this crate can open");
        (fleet, root, reviewer)
    }

    async fn called(
        fleet: &Fleet,
        caller: &SessionId,
        input: Value,
    ) -> Result<ToolOutput, ToolError> {
        SeatTool.call(input, &tool_context(caller, fleet)).await
    }

    #[tokio::test]
    async fn a_name_is_seated_beside_the_roster_that_stands() {
        let (fleet, _, reviewer) = tree().await;
        let out = called(
            &fleet,
            &reviewer,
            json!({"room": "#design", "members": ["scout"]}),
        )
        .await
        .expect("the opener may seat");
        assert!(!out.is_error);
        assert_eq!(out.parts[0].as_text(), Some("#design: reviewer, scout"));
        assert!(matches!(out.display, Some(View::Tree { .. })));

        let id = fleet.titled("#design").expect("the room");
        assert_eq!(fleet.members(&id), ["reviewer", "scout"]);
    }

    /// ADR-0053 §5: a peer of a shared room posts in it and does not reseat it.
    #[tokio::test]
    async fn a_caller_that_neither_holds_nor_opened_the_room_is_refused() {
        let (fleet, _, _) = tree().await;
        let scout = fleet.titled("scout").expect("the peer");
        let error = called(
            &fleet,
            &scout,
            json!({"room": "design", "members": ["scout"]}),
        )
        .await
        .expect_err("a peer does not reseat what it did not open");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("not yours to change"), "{message}");

        let id = fleet.titled("#design").expect("the room");
        assert_eq!(fleet.members(&id), ["reviewer"], "and nothing moved");
    }

    #[tokio::test]
    async fn a_room_the_caller_cannot_reach_and_a_call_naming_nobody_are_refused() {
        let (fleet, _, reviewer) = tree().await;
        let missing = called(
            &fleet,
            &reviewer,
            json!({"room": "standup", "members": ["scout"]}),
        )
        .await
        .expect_err("no such room");
        let ToolError::InvalidInput(message) = missing else {
            panic!("the wrong kind of refusal");
        };
        assert!(message.contains("no #standup you can reach"), "{message}");

        let nobody = called(&fleet, &reviewer, json!({"room": "design"}))
            .await
            .expect_err("nobody to seat");
        let ToolError::InvalidInput(message) = nobody else {
            panic!("the wrong kind of refusal");
        };
        assert!(message.contains("at least one member"), "{message}");
    }

    #[test]
    fn the_traits_fail_closed() {
        let traits = SeatTool.traits(&Value::Null);
        assert_eq!(traits, ToolTraits::default());
        assert!(!traits.read_only, "a roster is written, not read");
        assert!(!traits.concurrency_safe);
    }

    #[test]
    fn the_card_names_the_room_and_the_seats_it_takes() {
        assert_eq!(
            SeatTool.subjects(
                &json!({"room": "design", "members": ["scout"]}),
                Path::new("/work")
            ),
            [Subject::Name {
                name: "#design seats scout".into()
            }]
        );
        assert_eq!(
            SeatTool.subjects(
                &json!({"room": "#design", "listeners": [{"name": "scout", "patience_s": 0}]}),
                Path::new("/work")
            ),
            [Subject::Name {
                name: "#design seats scout:0".into()
            }]
        );
        assert!(
            SeatTool
                .subjects(&json!({"room": "design"}), Path::new("/work"))
                .is_empty(),
            "a call that will be refused names nothing"
        );
    }

    #[test]
    fn the_spec_asks_for_a_room_and_says_what_seating_is_for() {
        let spec = SeatTool.spec();
        assert_eq!(spec.name, SEAT);
        assert_eq!(spec.input_schema["required"], json!(["room"]));
        assert!(DESCRIPTION.contains("already stands"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("the room's head"), "{DESCRIPTION}");
        assert!(
            DESCRIPTION.contains("whoever opened it"),
            "who may: {DESCRIPTION}"
        );
    }
}
