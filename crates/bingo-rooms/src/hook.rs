//! The one hook. It watches two things and nothing else: a root session
//! opening, so a project's declared rooms are seated under it, and every frame
//! of every journal, so a post into a room reaches the room's members.
//!
//! Nothing it does waits on the session it observes: what it reads is the
//! session tree, and what it writes to is another session's queue.

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, Event, Frame, Hook, HookContext, HookMatcher, HookPoint, ItemBody, Origin, Phase,
    SessionFilter, SessionId,
};

use crate::name::PARENT;
use crate::roster::Roster;
use crate::{PLUGIN, post, room, seat, team};

/// The rooms this hook has seen, and what it does about a post into one.
#[derive(Debug, Default)]
pub struct RoomsHook {
    rooms: Roster,
}

#[async_trait]
impl Hook for RoomsHook {
    fn id(&self) -> &str {
        "rooms"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Session, HookPoint::Event],
            tool: None,
        }
    }

    /// A person's own session opening is when a project's rooms are seated;
    /// a child's opening is not, or every agent would seat them again.
    async fn on_session(&self, phase: Phase, cx: &HookContext) {
        if phase != Phase::Start || !is_root(cx).await {
            return;
        }
        self.seat_declared(cx).await;
    }

    async fn on_event(&self, frame: &Frame, cx: &HookContext) {
        match &frame.event {
            Event::SessionUpdated { summary } => self.rooms.register(summary),
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == PLUGIN && kind == room::MEMBERS => {
                self.rooms.set_members(&frame.session, payload)
            }
            Event::ItemCompleted { item } => {
                if let ItemBody::User { parts, origin } = &item.body {
                    self.posted(&frame.session, parts, origin, cx).await;
                }
            }
            _ => {}
        }
    }
}

impl RoomsHook {
    /// The rooms `.bingo/team.json` declares, opened under the session that
    /// just started, exactly as `/room <name> [member…]` would open them.
    async fn seat_declared(&self, cx: &HookContext) {
        let declared = match team::rooms(&cx.cwd) {
            Ok(declared) => declared,
            Err(error) => {
                tracing::warn!(%error, "the team file seats nobody this run");
                return;
            }
        };
        for entry in declared {
            let seated =
                seat::seat(&cx.host, &cx.session, &cx.cwd, &entry.name, &entry.members).await;
            if let Err(error) = seated {
                tracing::warn!(room = %entry.name, %error, "a declared room was not seated");
            }
        }
    }

    /// A user item in a room is a post. Everywhere else it is somebody's own
    /// conversation and no business of this plugin's.
    async fn posted(
        &self,
        session: &SessionId,
        parts: &[ContentPart],
        origin: &Origin,
        cx: &HookContext,
    ) {
        let Some(room) = self.rooms.get(session) else {
            return;
        };
        let Some(text) = parts.first().and_then(ContentPart::as_text) else {
            return;
        };
        // A person's own session leaves no principal, and `parent` is what the
        // members of the room call it.
        let author = origin
            .principal
            .clone()
            .unwrap_or_else(|| PARENT.to_string());
        if let Err(error) = post::fan_out(&cx.host, &room, &author, text).await {
            tracing::warn!(room = %room.title, %error, "a post did not reach every member");
        }
    }
}

/// Whether this session is a person's own: the one at the top of a tree, and
/// the one a project's declared rooms hang under.
async fn is_root(cx: &HookContext) -> bool {
    match cx.host.sessions(SessionFilter::default()).await {
        Ok(sessions) => sessions
            .iter()
            .find(|s| s.id == cx.session)
            .is_some_and(|s| s.parent.is_none()),
        Err(error) => {
            tracing::debug!(%error, "the session tree could not be read");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::payload;
    use crate::tests::{Fleet, extension, hook_context, posted, stamped, updated};
    use bingo_sdk::Delivery;
    use std::path::{Path, PathBuf};

    /// A root with a reviewer and a scout under it, and a room the hook has
    /// been told about through the frames it would have seen.
    async fn opened(members: &[&str]) -> (Fleet, SessionId, SessionId, RoomsHook) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        let room = fleet.room(&root, "design");
        let hook = RoomsHook::default();
        let cx = hook_context(&room, &fleet, Path::new("/work/project"));
        hook.on_event(&stamped(1, updated(&fleet.summary(&room)), &room), &cx)
            .await;
        let names: Vec<String> = members.iter().map(|m| m.to_string()).collect();
        hook.on_event(&stamped(2, extension(payload(&names)), &room), &cx)
            .await;
        (fleet, root, room, hook)
    }

    async fn post_into(fleet: &Fleet, hook: &RoomsHook, room: &SessionId, who: Option<&str>) {
        let cx = hook_context(room, fleet, Path::new("/work/project"));
        hook.on_event(&stamped(3, posted("hello team", who), room), &cx)
            .await;
    }

    #[tokio::test]
    async fn a_post_reaches_every_member_but_the_one_who_wrote_it() {
        let (fleet, _, room, hook) = opened(&["reviewer", "scout"]).await;
        post_into(&fleet, &hook, &room, Some("reviewer")).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        let (to, _, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Wake);
    }

    #[tokio::test]
    async fn a_post_nobody_signed_came_from_the_session_the_room_hangs_under() {
        let (fleet, _, room, hook) = opened(&["reviewer"]).await;
        post_into(&fleet, &hook, &room, None).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        let bingo_sdk::Input::Text { origin, .. } = &delivered[0].1 else {
            panic!("a post is text");
        };
        assert_eq!(origin.principal.as_deref(), Some(PARENT));
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    #[tokio::test]
    async fn a_user_item_outside_a_room_is_nobody_s_business_here() {
        let (fleet, root, _, hook) = opened(&["reviewer"]).await;
        let cx = hook_context(&root, &fleet, Path::new("/work/project"));
        hook.on_event(&stamped(9, posted("just me", None), &root), &cx)
            .await;
        assert!(fleet.delivered().is_empty());
    }

    #[tokio::test]
    async fn a_reopened_room_is_the_same_room_and_keeps_its_members() {
        let (fleet, _, room, hook) = opened(&["reviewer"]).await;
        let cx = hook_context(&room, &fleet, Path::new("/work/project"));
        hook.on_event(&stamped(8, updated(&fleet.summary(&room)), &room), &cx)
            .await;
        post_into(&fleet, &hook, &room, None).await;
        assert_eq!(
            fleet.delivered().len(),
            1,
            "a second announcement is the same room, still seating one member"
        );
    }

    /// A project whose team file declares one room, and the directory a
    /// session in it works from.
    fn project(source: &str) -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().expect("a temporary home");
        let file = home.path().join(".bingo").join("team.json");
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("a directory");
        std::fs::write(&file, source).expect("a file");
        let cwd = home.path().to_path_buf();
        (home, cwd)
    }

    const DECLARED: &str = r#"{
        "roles": [{"name": "reviewer"}],
        "rooms": [{"name": "design", "members": ["reviewer", "scout"]}]
    }"#;

    #[tokio::test]
    async fn a_project_s_rooms_are_seated_when_a_person_s_session_opens() {
        let (_home, cwd) = project(DECLARED);
        let fleet = Fleet::default();
        let root = fleet.root();
        let hook = RoomsHook::default();
        hook.on_session(Phase::Start, &hook_context(&root, &fleet, &cwd))
            .await;

        let created = fleet.created();
        assert_eq!(created.len(), 1, "{created:?}");
        assert_eq!(created[0].title.as_deref(), Some("#design"));
        assert_eq!(created[0].cwd, cwd);
        let room = fleet.titled("#design").expect("the room was opened");
        assert_eq!(fleet.members(&room), ["reviewer", "scout"]);
    }

    #[tokio::test]
    async fn an_agent_s_session_opening_seats_nothing() {
        let (_home, cwd) = project(DECLARED);
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let hook = RoomsHook::default();
        hook.on_session(Phase::Start, &hook_context(&child, &fleet, &cwd))
            .await;
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn a_session_ending_seats_nothing() {
        let (_home, cwd) = project(DECLARED);
        let fleet = Fleet::default();
        let root = fleet.root();
        RoomsHook::default()
            .on_session(Phase::End, &hook_context(&root, &fleet, &cwd))
            .await;
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn a_project_that_declares_none_seats_none() {
        let (_home, cwd) = project(r#"{"roles": [{"name": "reviewer"}]}"#);
        let fleet = Fleet::default();
        let root = fleet.root();
        RoomsHook::default()
            .on_session(Phase::Start, &hook_context(&root, &fleet, &cwd))
            .await;
        assert!(fleet.created().is_empty());
    }

    #[test]
    fn it_asks_for_the_two_points_it_uses() {
        let hook = RoomsHook::default();
        assert_eq!(hook.id(), "rooms");
        assert_eq!(
            hook.matcher().points,
            [HookPoint::Session, HookPoint::Event]
        );
        assert!(hook.matcher().tool.is_none());
    }
}
