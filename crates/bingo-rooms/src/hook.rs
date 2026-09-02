//! The one hook. It watches two things and nothing else: a root session
//! opening, so a project's declared rooms are seated under it, and every frame
//! of every journal, so a post into a room reaches the room's members and what
//! the room owes for it is chased and shown (ADR-0022 §3–4).
//!
//! A journal it watches is also a seat's own: a queue that changed says what
//! that seat is holding, and a patient seat holding a room's posts too long is
//! woken for them (ADR-0029 §3).
//!
//! Nothing it does waits on the session it observes: what it reads is the
//! session tree, and what it writes to is another session's queue.

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, Event, Frame, Hook, HookContext, HookMatcher, HookPoint, Item, ItemBody,
    KernelError, Origin, Phase, SessionFilter, SessionId,
};
use jiff::Timestamp;

use crate::chase::Chaser;
use crate::deadline::Deadline;
use crate::name::PARENT;
use crate::room::Room;
use crate::roster::Roster;
use crate::{PLUGIN, mentions, owed, post, room, seat, team};

/// The rooms this hook has seen, and what it does about a post into one.
#[derive(Debug, Default)]
pub struct RoomsHook {
    rooms: Roster,
    chaser: Chaser,
    deadline: Deadline,
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
            Event::SessionUpdated { summary } => {
                if let Some(room) = self.rooms.register(summary) {
                    self.reckon(&frame.session, &room, cx).await;
                    self.backlog(&frame.session, &room, cx).await;
                }
            }
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == PLUGIN => self.rooms.extended(&frame.session, kind, payload),
            Event::ItemCompleted { item } => self.item(&frame.session, item, cx).await,
            // Any session's queue may be holding a room's posts (ADR-0029 §3).
            Event::QueueChanged { entries, .. } => {
                self.deadline
                    .queued(
                        &cx.host,
                        &self.rooms,
                        &frame.session,
                        entries,
                        Timestamp::now(),
                    )
                    .await
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
            if let Err(error) = declared_room(cx, &entry).await {
                tracing::warn!(room = %entry.name, %error, "a declared room was not seated");
            }
        }
    }

    /// A user item in a room is a post: it reaches every other member, and it
    /// changes what the room owes. Everywhere else it is somebody's own
    /// conversation and no business of this plugin's.
    async fn item(&self, session: &SessionId, item: &Item, cx: &HookContext) {
        let ItemBody::User { parts, origin } = &item.body else {
            return;
        };
        let Some(room) = self.rooms.get(session) else {
            return;
        };
        let Some(text) = parts.first().and_then(ContentPart::as_text) else {
            return;
        };
        self.fan_out(&room, origin, text, cx).await;
        self.reckon(session, &room, cx).await;
    }

    /// Everyone else in the room hears it.
    async fn fan_out(&self, room: &Room, origin: &Origin, text: &str, cx: &HookContext) {
        // A person's own session leaves no principal, and `parent` is what the
        // members of the room call it.
        let author = origin
            .principal
            .clone()
            .unwrap_or_else(|| PARENT.to_string());
        if let Err(error) = post::fan_out(&cx.host, room, &author, text).await {
            tracing::warn!(room = %room.title, %error, "a post did not reach every member");
        }
    }

    /// What the room owes now, and what to do about it: chase whoever has not
    /// answered, and show the parent what stands. Both read the one authority
    /// — the room's own journal — and nothing kept here.
    async fn reckon(&self, session: &SessionId, room: &Room, cx: &HookContext) {
        let open = mentions::of_room(&cx.host, session).await;
        self.chaser
            .reconcile(&cx.host, room, session, &open, Timestamp::now());
        self.show(&room.parent, cx).await;
    }

    /// A room this process had not seen before: its patient seats may have
    /// been holding its posts since before this process started, and a backlog
    /// found there is nudged once (ADR-0029 §3). The roster comes from the
    /// room's own snapshot, because the announce itself carries none.
    async fn backlog(&self, session: &SessionId, room: &Room, cx: &HookContext) {
        let Some(state) = room::read(&cx.host, session).await else {
            return;
        };
        self.deadline.overdue(&cx.host, &room.seated(&state)).await;
    }

    /// The card on a session: every debt in every room under it, or nothing at
    /// all once the last one closes.
    async fn show(&self, parent: &SessionId, cx: &HookContext) {
        let mut debts = Vec::new();
        for (id, room) in self.rooms.under(parent) {
            let open = mentions::of_room(&cx.host, &id).await;
            debts.extend(owed::debts(&room.title, &open));
        }
        owed::publish(&cx.host, parent, owed::view(debts)).await;
    }
}

/// One room a project declares, seated exactly as `/room <name> [member…]`
/// would seat it — ears and all.
async fn declared_room(cx: &HookContext, entry: &team::Entry) -> Result<(), KernelError> {
    let seats = entry.seats()?;
    seat::seat(&cx.host, &cx.session, &cx.cwd, &entry.name, &seats).await?;
    Ok(())
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
    use crate::ear::Seat;
    use crate::room::payload;
    use crate::tests::{Fleet, extension, hook_context, posted, stamped, ts, updated};
    use bingo_sdk::Delivery;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// The seats a roster line asks for: a bare name is live, `~name` listens.
    fn seats(roster: &[&str]) -> Vec<Seat> {
        roster
            .iter()
            .map(|word| Seat::read(word).expect("a roster word"))
            .collect()
    }

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
        hook.on_event(&stamped(2, extension(payload(&seats(members))), &room), &cx)
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

    /// The ear on the roster decides how the fan-out reaches a seat, end to
    /// end through the hook (ADR-0029 §1).
    #[tokio::test]
    async fn a_patient_seat_is_handed_the_post_without_being_woken() {
        let (fleet, _, room, hook) = opened(&["reviewer", "~scout"]).await;
        post_into(&fleet, &hook, &room, Some("reviewer")).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        let (to, _, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Hold);
    }

    /// The whole of the deadline through the hook: the seat's queue says what
    /// it is holding, and the patience says when it is woken for it.
    #[tokio::test(start_paused = true)]
    async fn a_seat_holding_a_post_past_its_patience_is_woken_by_the_hook() {
        let (fleet, root, _, hook) = opened(&["~scout:120"]).await;
        let scout = fleet.titled("scout").expect("the seat");
        let cx = hook_context(&scout, &fleet, Path::new("/work/project"));
        let waiting =
            crate::tests::queued(&fleet, &scout, &[crate::tests::held("req_1", "#design")]);

        hook.on_event(
            &stamped(
                3,
                Event::QueueChanged {
                    revision: 1,
                    entries: waiting,
                },
                &scout,
            ),
            &cx,
        )
        .await;
        settle().await;
        assert!(
            nudges(&fleet).is_empty(),
            "nothing before the patience is up"
        );

        tokio::time::advance(std::time::Duration::from_secs(120)).await;
        settle().await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert!(nudged[0].contains("#design"), "{}", nudged[0]);
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "the holder is not on this roster"
        );
    }

    /// A room this process reads for the first time may find a seat already
    /// holding its posts. It is nudged once — and a reopen is the same room,
    /// so it is not nudged again for the same backlog.
    #[tokio::test(start_paused = true)]
    async fn a_backlog_found_at_the_announce_is_nudged_once() {
        let (fleet, _, room) = standing(&["~scout:120"]).await;
        let scout = fleet.titled("scout").expect("the seat");
        crate::tests::queued(&fleet, &scout, &[crate::tests::held("req_1", "#design")]);
        let hook = RoomsHook::default();

        announce(&fleet, &hook, &room).await;
        settle().await;
        assert_eq!(nudges(&fleet).len(), 1, "{:?}", fleet.delivered());

        announce(&fleet, &hook, &room).await;
        settle().await;
        assert_eq!(nudges(&fleet).len(), 1, "a reopen is the same room");
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

    /// A room already standing with a membership, as another process left it.
    async fn standing(members: &[&str]) -> (Fleet, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        for seat in seats(members) {
            fleet.child(&root, &seat.name);
        }
        let room = seat::seat(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            "design",
            &seats(members),
        )
        .await
        .expect("a room this crate can open");
        (fleet, root, room)
    }

    /// The room announcing itself, which is where this process first reads it.
    async fn announce(fleet: &Fleet, hook: &RoomsHook, room: &SessionId) {
        let cx = hook_context(room, fleet, Path::new("/work/project"));
        hook.on_event(&stamped(1, updated(&fleet.summary(room)), room), &cx)
            .await;
    }

    /// A post, as the room's journal takes it and the hook sees it.
    async fn says(
        fleet: &Fleet,
        hook: &RoomsHook,
        room: &SessionId,
        who: Option<&str>,
        text: &str,
    ) {
        let cx = hook_context(room, fleet, Path::new("/work/project"));
        let event = fleet.post(room, text, who, Timestamp::now());
        hook.on_event(&stamped(9, event, room), &cx).await;
    }

    /// Let a chase that is due now — an overdue debt waits no time at all —
    /// run to its nudge. Nothing here moves the clock, so a debt still inside
    /// its patience stays unchased.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    fn nudges(fleet: &Fleet) -> Vec<String> {
        fleet
            .delivered()
            .into_iter()
            .filter_map(|(_, input, _)| match input {
                bingo_sdk::Input::Text { text, origin, .. } => {
                    origin.principal.is_none().then_some(text)
                }
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_this_process_finds_already_overdue_is_chased_once() {
        let (fleet, _, room) = standing(&["scout"]).await;
        fleet.post(&room, "@scout what does the log say?", None, ts());
        let hook = RoomsHook::default();

        announce(&fleet, &hook, &room).await;
        settle().await;

        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert!(nudged[0].contains("#design"), "{}", nudged[0]);
        assert!(
            nudged[0].contains("what does the log say?"),
            "{}",
            nudged[0]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_nobody_asked_of_a_member_chases_nobody() {
        let (fleet, _, room) = standing(&["scout"]).await;
        fleet.post(&room, "@all stand-up in five", None, ts());
        fleet.post(&room, "mail@scout is not a call", None, ts());
        let hook = RoomsHook::default();

        announce(&fleet, &hook, &room).await;
        settle().await;
        assert!(nudges(&fleet).is_empty(), "{:?}", fleet.delivered());
    }

    #[tokio::test(start_paused = true)]
    async fn the_card_says_what_stands_and_goes_when_the_last_debt_closes() {
        let (fleet, root, room) = standing(&["scout"]).await;
        let hook = RoomsHook::default();
        announce(&fleet, &hook, &room).await;

        says(&fleet, &hook, &room, None, "@scout look at the build").await;
        says(&fleet, &hook, &room, Some("scout"), "looking").await;
        settle().await;

        let cards = fleet.signalled(&root, owed::KIND);
        let [opened, standing, closed] = cards.as_slice() else {
            panic!("one card per fold: {cards:?}");
        };
        assert_eq!(*opened, Value::Null, "a room owing nothing carries no card");
        assert_eq!(standing["kind"], "table");
        assert_eq!(standing["rows"][0][0], "#design");
        assert_eq!(standing["rows"][0][1], "scout");
        assert_eq!(standing["debts"][0]["room"], "#design");
        assert_eq!(standing["debts"][0]["who"], "scout");
        assert!(
            standing["debts"][0]["at"].is_string(),
            "and the moment it was asked, for whoever wants an age: {standing}"
        );
        assert_eq!(*closed, Value::Null, "answered, and the card goes");
        assert!(nudges(&fleet).is_empty(), "nobody was chased for it");
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
