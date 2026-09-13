//! `CloseRoom`: the end of a room (ADR-0053 §4). A last line the caller signs,
//! and then the frame that says it is over. Nothing is deleted and no session
//! is ended: the journal is the record, and the frame is the whole of what
//! closing means.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, View, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{block, refused};
use crate::room::{CLOSED, Room};
use crate::{door, name, seat};

pub const CLOSE_ROOM: &str = "CloseRoom";

const DESCRIPTION: &str = "\
End a room whose purpose is done. Everyone in it reads one last line — yours, \
with `why` if you give one — and after that the room takes no post, wakes \
nobody, owes nothing and is never reopened under that name. Its members read \
what is left to read and then hear nothing of it again. Nothing is deleted: \
what was said stays where it was said. Close a room when the work it was opened \
for is finished, and open another for what follows. Only the session a room \
hangs under and whoever opened it may close it.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloseRoomArgs {
    /// The room, by name or `#name`: one under you, or one beside you.
    pub room: String,
    /// What to say in the last line, if anything: why it is done.
    pub why: Option<String>,
}

/// What a person approving the call is shown: which room is about to end.
fn card(room: &str) -> String {
    format!(
        "{} {CLOSED}",
        name::title(room.trim().trim_start_matches('#'))
    )
}

/// The room the call left, as a person reads it: the room badged closed, and
/// the seats that were in it when it ended.
fn ended(room: &Room) -> View {
    block(&room.title, Some(CLOSED), &room.seats())
}

/// Ending a room: it posts into the room and writes to its journal, so the
/// traits are the fail-closed defaults.
#[derive(Debug, Default, Clone, Copy)]
pub struct CloseRoomTool;

#[async_trait]
impl Tool for CloseRoomTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: CLOSE_ROOM.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<CloseRoomArgs>(),
            meta: Default::default(),
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<CloseRoomArgs>(input.clone())
            .map(|args| {
                vec![Subject::Name {
                    name: card(&args.room),
                }]
            })
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: CloseRoomArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let caller = door::own(&cx.host, &cx.session).await.map_err(refused)?;
        let (id, room) = door::entered(&cx.host, &caller, &args.room)
            .await
            .map_err(refused)?;
        let by = name::signed_by(&caller);
        seat::close(&cx.host, &id, &room, &by, args.why.as_deref())
            .await
            .map_err(refused)?;
        let mut out = ToolOutput::text(format!("{}: {CLOSED}", room.title));
        out.display = Some(ended(&room));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Seat;
    use crate::room;
    use crate::seat::Opening;
    use crate::tests::{Fleet, tool_context};
    use bingo_sdk::{SessionId, Tone, ToolTraits, TreeNode};
    use serde_json::json;

    /// A root holding `#design`, the reviewer that opened it, and a scout in it.
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
            &[Seat::named("scout")],
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
        CloseRoomTool
            .call(input, &tool_context(caller, fleet))
            .await
    }

    #[tokio::test]
    async fn a_room_is_closed_and_the_block_says_who_was_in_it() {
        let (fleet, _, reviewer) = tree().await;
        let out = called(
            &fleet,
            &reviewer,
            json!({"room": "design", "why": "it shipped"}),
        )
        .await
        .expect("the opener may close it");
        assert_eq!(out.parts[0].as_text(), Some("#design: closed"));
        assert_eq!(
            out.display,
            Some(View::Tree {
                nodes: vec![TreeNode {
                    label: "#design".into(),
                    badge: Some("closed".into()),
                    tone: Tone::Neutral,
                    children: vec![TreeNode {
                        label: "scout".into(),
                        badge: None,
                        tone: Tone::Neutral,
                        children: Vec::new(),
                    }],
                }]
            })
        );

        let id = fleet.titled("#design").expect("the room");
        assert!(room::closed_of(&fleet.state(&id)));
        assert_eq!(
            room::Closed::of_state(&fleet.state(&id)).map(|closed| closed.by),
            Some("reviewer".into())
        );
    }

    #[tokio::test]
    async fn a_room_that_has_ended_is_not_closed_twice() {
        let (fleet, _, reviewer) = tree().await;
        called(&fleet, &reviewer, json!({"room": "design"}))
            .await
            .expect("the first close");
        let error = called(&fleet, &reviewer, json!({"room": "design"}))
            .await
            .expect_err("and there is nothing left to close");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("#design is closed"), "{message}");
    }

    #[tokio::test]
    async fn a_caller_that_neither_holds_nor_opened_the_room_is_refused() {
        let (fleet, _, _) = tree().await;
        let scout = fleet.titled("scout").expect("the peer");
        let error = called(&fleet, &scout, json!({"room": "design"}))
            .await
            .expect_err("a seat does not close the room it sits in");
        let ToolError::InvalidInput(message) = error else {
            panic!("the wrong kind of refusal: {error:?}");
        };
        assert!(message.contains("not yours to change"), "{message}");
        assert!(
            !room::closed_of(&fleet.state(&fleet.titled("#design").expect("the room"))),
            "and the room still stands"
        );
    }

    #[test]
    fn the_traits_fail_closed() {
        let traits = CloseRoomTool.traits(&Value::Null);
        assert_eq!(traits, ToolTraits::default());
        assert!(!traits.read_only, "a room is ended, not read");
        assert!(!traits.concurrency_safe);
    }

    #[test]
    fn the_card_names_the_room_that_is_about_to_end() {
        assert_eq!(
            CloseRoomTool.subjects(&json!({"room": "design"}), Path::new("/work")),
            [Subject::Name {
                name: "#design closed".into()
            }]
        );
        assert!(
            CloseRoomTool
                .subjects(&json!({"why": "done"}), Path::new("/work"))
                .is_empty()
        );
    }

    #[test]
    fn the_spec_asks_for_a_room_and_says_what_closing_costs() {
        let spec = CloseRoomTool.spec();
        assert_eq!(spec.name, CLOSE_ROOM);
        assert_eq!(spec.input_schema["required"], json!(["room"]));
        assert!(DESCRIPTION.contains("never reopened"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("Nothing is deleted"), "{DESCRIPTION}");
    }
}
