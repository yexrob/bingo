//! `Unseat`: a name taken out of a room (ADR-0053 §3). The roster goes out
//! whole without it, the room reads that it happened, and the seat that left is
//! told — nothing in a tree is taken away in silence.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    KernelError, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{nobody, refused, seated};
use crate::{door, name, seat};

pub const UNSEAT: &str = "Unseat";

const DESCRIPTION: &str = "\
Take one or more names out of a room: the roster without them, published whole. \
The room reads that you unseated them, each of them is told once who unseated \
it from where, and the room reaches them no longer. A name the room is not \
seating is refused rather than passed over. Only the session a room hangs under \
and whoever opened it may unseat anyone. Use it when a seat's part in this \
room's purpose is done; to end the room for everybody, use `CloseRoom`.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UnseatArgs {
    /// The room, by name or `#name`: one under you, or one beside you.
    pub room: String,
    /// The names to take out, as the roster spells them.
    pub members: Vec<String>,
}

impl UnseatArgs {
    /// The names it asks for. A call that names nobody is refused: an empty
    /// roster move reads like one that worked.
    fn names(&self) -> Result<&[String], KernelError> {
        match self.members.is_empty() {
            true => Err(nobody("unseat")),
            false => Ok(&self.members),
        }
    }
}

/// What a person approving the call is shown: the room, and who is about to
/// leave it.
fn card(room: &str, members: &[String]) -> String {
    format!(
        "{} unseats {}",
        name::title(room.trim().trim_start_matches('#')),
        members.join(", ")
    )
}

/// Taking a seat out of a room: it writes to the room's journal and wakes the
/// seat that left, so the traits are the fail-closed defaults.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnseatTool;

#[async_trait]
impl Tool for UnseatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: UNSEAT.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<UnseatArgs>(),
            meta: Default::default(),
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<UnseatArgs>(input.clone())
            .ok()
            .and_then(|args| Some(card(&args.room, args.names().ok()?)))
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: UnseatArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let names = args.names().map_err(refused)?;
        let caller = door::own(&cx.host, &cx.session).await.map_err(refused)?;
        let (id, room) = door::entered(&cx.host, &caller, &args.room)
            .await
            .map_err(refused)?;
        let roster = seat::leave(&cx.host, &id, &room, names, &name::signed_by(&caller))
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
    use crate::ear::Seat;
    use crate::seat::Opening;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::{Input, SessionId, ToolTraits};
    use serde_json::json;

    /// A root holding `#design`, the reviewer that opened it, and the scout it
    /// seats beside them.
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
            &[Seat::named("reviewer"), Seat::named("scout")],
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
        UnseatTool.call(input, &tool_context(caller, fleet)).await
    }

    /// The seat that left is told, once, and by a nudge rather than a post.
    #[tokio::test]
    async fn a_name_leaves_the_roster_and_the_seat_that_left_is_told() {
        let (fleet, _, reviewer) = tree().await;
        let scout = fleet.titled("scout").expect("the seat");
        let out = called(
            &fleet,
            &reviewer,
            json!({"room": "design", "members": ["scout"]}),
        )
        .await
        .expect("the opener may unseat");
        assert_eq!(out.parts[0].as_text(), Some("#design: reviewer"));

        let id = fleet.titled("#design").expect("the room");
        assert_eq!(fleet.members(&id), ["reviewer"]);
        let told: Vec<String> = fleet
            .delivered()
            .into_iter()
            .filter(|(to, ..)| to == &scout)
            .filter_map(|(_, input, _)| match input {
                Input::Text { text, origin, .. } => origin.principal.is_none().then_some(text),
                _ => None,
            })
            .collect();
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(
            told[0].contains("unseated from #design by reviewer"),
            "{}",
            told[0]
        );
    }

    #[tokio::test]
    async fn a_name_the_room_is_not_seating_is_refused_and_the_roster_stands() {
        let (fleet, _, reviewer) = tree().await;
        let error = called(
            &fleet,
            &reviewer,
            json!({"room": "design", "members": ["ghost"]}),
        )
        .await
        .expect_err("a name the room is not seating");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("not seating ghost"), "{message}");

        let id = fleet.titled("#design").expect("the room");
        assert_eq!(fleet.members(&id), ["reviewer", "scout"]);
    }

    #[tokio::test]
    async fn a_caller_that_neither_holds_nor_opened_the_room_is_refused() {
        let (fleet, _, _) = tree().await;
        let scout = fleet.titled("scout").expect("the peer");
        let error = called(
            &fleet,
            &scout,
            json!({"room": "design", "members": ["reviewer"]}),
        )
        .await
        .expect_err("a peer does not unseat");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("not yours to change"), "{message}");
        assert_eq!(
            fleet.members(&fleet.titled("#design").expect("the room")),
            ["reviewer", "scout"]
        );
    }

    #[test]
    fn the_traits_fail_closed() {
        let traits = UnseatTool.traits(&Value::Null);
        assert_eq!(traits, ToolTraits::default());
        assert!(!traits.read_only, "a roster is written, not read");
        assert!(!traits.concurrency_safe);
    }

    #[test]
    fn the_card_names_the_room_and_who_leaves_it() {
        assert_eq!(
            UnseatTool.subjects(
                &json!({"room": "#design", "members": ["scout", "watcher"]}),
                Path::new("/work")
            ),
            [Subject::Name {
                name: "#design unseats scout, watcher".into()
            }]
        );
        assert!(
            UnseatTool
                .subjects(
                    &json!({"room": "design", "members": []}),
                    Path::new("/work")
                )
                .is_empty(),
            "a call that will be refused names nothing"
        );
    }

    #[test]
    fn the_spec_asks_for_a_room_and_the_names_to_take_out() {
        let spec = UnseatTool.spec();
        assert_eq!(spec.name, UNSEAT);
        assert_eq!(spec.input_schema["required"], json!(["room", "members"]));
        assert!(DESCRIPTION.contains("is told once"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("`CloseRoom`"), "{DESCRIPTION}");
    }
}
