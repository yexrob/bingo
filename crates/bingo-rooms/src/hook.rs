//! The one hook. It watches two things and nothing else: a root session
//! opening, so a project's declared rooms are seated under it, and every frame
//! of every journal, so a post into a room reaches the room's members and what
//! the room owes for it is chased and shown (ADR-0022 §3–4).
//!
//! A post it sees fan out leaves some seats waiting, and a seat left behind
//! its room for its whole patience is woken for it (ADR-0029 §3, ADR-0034 §3).
//!
//! Nothing it does waits on the session it observes: what it reads is the
//! session tree, and what it writes to is another session's queue.

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, Event, Frame, Hook, HookContext, HookMatcher, HookPoint, HostHandle, Item,
    ItemBody, KernelError, Origin, Phase, SessionFilter, SessionId,
};
use jiff::Timestamp;
use serde_json::Value;

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
                    self.first_read(&frame.session, &room, cx).await;
                }
            }
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == PLUGIN => self.extended(&frame.session, kind, payload, cx).await,
            Event::ItemCompleted { item } => self.item(&frame.session, item, cx).await,
            _ => {}
        }
    }
}

impl RoomsHook {
    /// The rooms `.bingo/team.json` declares, opened under the session that
    /// just started, exactly as `/room <name> [member…]` would open them.
    async fn seat_declared(&self, cx: &HookContext) {
        let declared = match team::rooms(&cx.host, &cx.cwd).await {
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
        self.fan_out(session, &room, origin, text, cx).await;
        self.reckon(session, &room, cx).await;
    }

    /// Who the post wakes is woken, and who it leaves behind starts waiting:
    /// the patience of a seat it did not wake is the bound on when that seat
    /// reads it (ADR-0034 §3).
    async fn fan_out(
        &self,
        session: &SessionId,
        room: &Room,
        origin: &Origin,
        text: &str,
        cx: &HookContext,
    ) {
        // A person's own session leaves no principal, and `parent` is what the
        // members of the room call it.
        let author = origin
            .principal
            .clone()
            .unwrap_or_else(|| PARENT.to_string());
        match post::fan_out(&cx.host, room, &author, text).await {
            // A closed room wakes nobody later: its last post is the last
            // thing there is to read, and no patience is timed for it.
            Ok(waiting) if !room.closed => {
                self.deadline
                    .waiting(&cx.host, session, room, &waiting)
                    .await
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(room = %room.title, %error, "a post did not wake every member")
            }
        }
    }

    /// What the room owes now, and what to do about it: chase whoever has not
    /// answered, and show the parent what stands. Both read the one authority
    /// — the room's own journal — and nothing kept here. A closed room owes
    /// nothing whatever its posts say (ADR-0053 §4), so the fold it is chased
    /// against is empty and every timer it had is cancelled.
    async fn reckon(&self, session: &SessionId, room: &Room, cx: &HookContext) {
        let open = match room.closed {
            true => Vec::new(),
            false => mentions::of_room(&cx.host, session).await,
        };
        self.chaser
            .reconcile(&cx.host, room, session, &open, Timestamp::now());
        self.show(&room.parent, cx).await;
    }

    /// A room this process had not seen before, read for the first time: what
    /// it owes now, and whose patience ran out while nobody was timing it
    /// (ADR-0029 §3). Both come from the room's own snapshot, because the
    /// announce carries neither — and a room that closed before this process
    /// started asks nothing of anybody.
    async fn first_read(&self, session: &SessionId, room: &Room, cx: &HookContext) {
        let Some(state) = room::read(&cx.host, session).await else {
            return;
        };
        let room = room.seated(&state);
        self.reckon(session, &room, cx).await;
        if !room.closed {
            self.deadline.overdue(&cx.host, session, &room).await;
        }
    }

    /// One of this plugin's frames in a room's journal. A closing is the one
    /// that changes what the room owes without anything being said in it, so
    /// it is reckoned again here: the debts it carried are dropped, the timers
    /// chasing them cancelled, and the card redrawn without them.
    async fn extended(&self, session: &SessionId, kind: &str, payload: &Value, cx: &HookContext) {
        self.rooms.extended(session, kind, payload);
        if kind != room::CLOSED {
            return;
        }
        if let Some(room) = self.rooms.get(session) {
            self.reckon(session, &room, cx).await;
        }
    }

    /// The card on a session: every debt in every room under it, or nothing at
    /// all once the last one closes.
    async fn show(&self, parent: &SessionId, cx: &HookContext) {
        let mut debts = Vec::new();
        for (id, room) in self.rooms.under(parent) {
            debts.extend(stands(&cx.host, &id, &room).await);
        }
        owed::publish(&cx.host, parent, owed::view(debts)).await;
    }
}

/// What one room owes, as its own journal says: nothing at all once it has
/// closed (ADR-0053 §4). Both answers come from one read of the room, and from
/// the room rather than from what this process has folded so far — a room read
/// for the first time may have closed before this process started.
async fn stands(host: &HostHandle, id: &SessionId, room: &Room) -> Vec<owed::Debt> {
    let Some(state) = room::read(host, id).await else {
        return Vec::new();
    };
    match room::closed_of(&state) {
        true => Vec::new(),
        false => owed::debts(&room.title, &mentions::of_state(&state)),
    }
}

/// One room a project declares, seated exactly as `/room <name> [member…]`
/// would seat it — ears and all, and the purpose the file gives it.
async fn declared_room(cx: &HookContext, entry: &team::Entry) -> Result<(), KernelError> {
    let seats = entry.seats()?;
    let opening = seat::Opening::person(&entry.name, entry.purpose.as_deref());
    seat::seat(&cx.host, &cx.session, &cx.cwd, opening, &seats).await?;
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
    use crate::tests::{
        Fleet, Stub, extended, extension, hook_context, posted, stamped, ts, updated,
    };
    use bingo_sdk::Delivery;
    use serde_json::json;
    use std::path::Path;

    /// The seats a roster line asks for: a bare name is patient, `name:0` live.
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
    async fn a_post_wakes_every_live_member_but_the_one_who_wrote_it() {
        let (fleet, _, room, hook) = opened(&["reviewer:0", "scout:0"]).await;
        post_into(&fleet, &hook, &room, Some("reviewer")).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        let (to, _, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Wake);
    }

    /// A post nobody signed came from the session the room hangs under, so a
    /// rostered holder is not woken for its own — and what the seats that are
    /// woken get is a nudge, signed by nobody (ADR-0034 §3).
    #[tokio::test]
    async fn a_post_nobody_signed_came_from_the_session_the_room_hangs_under() {
        let (fleet, root, room, hook) = opened(&["reviewer:0", "parent:0"]).await;
        post_into(&fleet, &hook, &room, None).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_ne!(delivered[0].0, root, "the holder wrote it");
        let bingo_sdk::Input::Text { text, origin, .. } = &delivered[0].1 else {
            panic!("a nudge is text");
        };
        assert!(!text.contains("hello team"), "{text}");
        assert_eq!(origin.principal, None);
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    /// The ear on the roster decides whether the post wakes a seat at all, end
    /// to end through the hook (ADR-0029 §1, ADR-0034 §3).
    #[tokio::test]
    async fn a_patient_seat_the_post_does_not_name_is_written_to_not_at_all() {
        let (fleet, _, room, hook) = opened(&["reviewer:0", "scout"]).await;
        post_into(&fleet, &hook, &room, None).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(
            fleet.summary(&delivered[0].0).title.as_deref(),
            Some("reviewer"),
            "the live seat, and nobody else"
        );
    }

    /// The other half of ADR-0029 §5 end to end: `@all` pierces every patient
    /// ear the room seats, and the only seat it leaves alone is the one that
    /// wrote it.
    #[tokio::test]
    async fn a_post_that_says_at_all_wakes_every_patient_seat_but_its_author() {
        let (fleet, _, room, hook) = opened(&["reviewer", "scout"]).await;
        says(&fleet, &hook, &room, Some("reviewer"), "@all stand-up").await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(
            fleet.summary(&delivered[0].0).title.as_deref(),
            Some("scout"),
            "the reviewer wrote it"
        );
        assert_eq!(delivered[0].2, Delivery::Wake);
    }

    /// The whole of the deadline through the hook: the post leaves the seat
    /// behind its room, and the patience says when it is woken to read it.
    #[tokio::test(start_paused = true)]
    async fn a_seat_a_post_left_behind_is_woken_by_the_hook_at_its_patience() {
        let (fleet, root, room, hook) = opened(&["scout:120"]).await;
        let scout = fleet.titled("scout").expect("the seat");

        says(&fleet, &hook, &room, Some("reviewer"), "the build is green").await;
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
        assert_eq!(fleet.delivered()[0].0, scout);
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "the holder is not on this roster"
        );
    }

    /// A room this process reads for the first time may find a seat whose
    /// cursor the last process left behind the head. It is nudged once — and a
    /// reopen is the same room, so it is not nudged again for the same posts.
    #[tokio::test(start_paused = true)]
    async fn a_backlog_found_at_the_announce_is_nudged_once() {
        let (fleet, _, room) = standing(&["scout:120"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
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
        let (fleet, _, room, hook) = opened(&["reviewer:0"]).await;
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

    /// The directory a session in a project works from. Where the team file
    /// is and what it says are the owning plugin's (ADR-0031); what reaches
    /// this hook is the `rooms` its service answers with.
    fn cwd() -> &'static Path {
        Path::new("/work/project")
    }

    /// A fleet whose team file declares these rooms.
    fn declaring(section: Value) -> Fleet {
        Fleet::default().serving(crate::team::TEAM, Stub::declaring(section))
    }

    fn declared() -> Value {
        json!([{"name": "design", "members": ["reviewer", "scout"]}])
    }

    #[tokio::test]
    async fn a_project_s_rooms_are_seated_when_a_person_s_session_opens() {
        let fleet = declaring(declared());
        let root = fleet.root();
        let hook = RoomsHook::default();
        hook.on_session(Phase::Start, &hook_context(&root, &fleet, cwd()))
            .await;

        let created = fleet.created();
        assert_eq!(created.len(), 1, "{created:?}");
        assert_eq!(created[0].title.as_deref(), Some("#design"));
        assert_eq!(created[0].cwd, cwd());
        let room = fleet.titled("#design").expect("the room was opened");
        assert_eq!(fleet.members(&room), ["reviewer", "scout"]);
    }

    #[tokio::test]
    async fn an_agent_s_session_opening_seats_nothing() {
        let fleet = declaring(declared());
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let hook = RoomsHook::default();
        hook.on_session(Phase::Start, &hook_context(&child, &fleet, cwd()))
            .await;
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn a_session_ending_seats_nothing() {
        let fleet = declaring(declared());
        let root = fleet.root();
        RoomsHook::default()
            .on_session(Phase::End, &hook_context(&root, &fleet, cwd()))
            .await;
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn a_project_that_declares_none_seats_none() {
        let fleet = declaring(Value::Null);
        let root = fleet.root();
        RoomsHook::default()
            .on_session(Phase::Start, &hook_context(&root, &fleet, cwd()))
            .await;
        assert!(fleet.created().is_empty());
    }

    /// A team file the plugin that owns it could not read seats nobody this
    /// run, and stops nothing: the session opens either way.
    #[tokio::test]
    async fn a_team_file_that_will_not_parse_seats_nobody_and_fails_nothing() {
        let fleet = Fleet::default().serving(
            crate::team::TEAM,
            Stub::refusing("/work/project/.bingo/team.json: { not json"),
        );
        let root = fleet.root();
        RoomsHook::default()
            .on_session(Phase::Start, &hook_context(&root, &fleet, cwd()))
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
            seat::Opening::person("design", None),
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

    /// The nudges the chaser sent, which are the ones that quote a debt: the
    /// deadline's own says only that the room stands unread.
    fn chases(fleet: &Fleet) -> Vec<String> {
        nudges(fleet)
            .into_iter()
            .filter(|said| said != &post::unread("#design"))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_question_this_process_finds_already_overdue_is_chased_once() {
        let (fleet, _, room) = standing(&["scout"]).await;
        fleet.post(&room, "@scout what does the log say?", None, ts());
        let hook = RoomsHook::default();

        announce(&fleet, &hook, &room).await;
        settle().await;

        let chased = chases(&fleet);
        assert_eq!(chased.len(), 1, "{chased:?}");
        assert!(chased[0].contains("#design"), "{}", chased[0]);
        assert!(
            chased[0].contains("what does the log say?"),
            "{}",
            chased[0]
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
        assert!(chases(&fleet).is_empty(), "{:?}", fleet.delivered());
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
        assert!(chases(&fleet).is_empty(), "nobody was chased for it");
    }

    /// A room closed as `CloseRoom` closes one, and the frame that left in its
    /// journal — which is what the hook sees of it.
    async fn closed(fleet: &Fleet, room: &SessionId) -> Event {
        seat::close(
            &fleet.handle(),
            room,
            &Room::of(&fleet.summary(room)).expect("a room"),
            PARENT,
            None,
        )
        .await
        .expect("a room this crate can close");
        let closing =
            crate::room::Closed::of_state(&fleet.state(room)).expect("the frame the close left");
        extended(room::CLOSED, closing.payload())
    }

    /// ADR-0053 §4: a closed room owes nothing. A debt this process would have
    /// chased the moment it read the room is dropped with the room, and the
    /// card carries no row for it.
    #[tokio::test(start_paused = true)]
    async fn a_room_found_closed_is_chased_for_nothing_and_carries_no_card() {
        let (fleet, root, room) = standing(&["scout"]).await;
        fleet.post(&room, "@scout what does the log say?", None, ts());
        closed(&fleet, &room).await;

        announce(&fleet, &RoomsHook::default(), &room).await;
        settle().await;
        assert!(chases(&fleet).is_empty(), "{:?}", fleet.delivered());
        assert!(
            fleet
                .signalled(&root, owed::KIND)
                .iter()
                .all(Value::is_null),
            "a closed room owes nothing: {:?}",
            fleet.signalled(&root, owed::KIND)
        );
    }

    /// And a room that closes while this process is watching drops what it
    /// owed: the card goes, and the timer chasing the debt is cancelled.
    #[tokio::test(start_paused = true)]
    async fn a_closing_takes_the_room_s_debts_off_the_card_and_cancels_their_chase() {
        let (fleet, root, room) = standing(&["scout"]).await;
        let hook = RoomsHook::default();
        announce(&fleet, &hook, &room).await;
        says(&fleet, &hook, &room, None, "@scout look at the build").await;
        settle().await;
        let owing = fleet.signalled(&root, owed::KIND);
        assert_eq!(
            owing.last().expect("a card")["rows"][0][1],
            "scout",
            "{owing:?}"
        );

        let closing = closed(&fleet, &room).await;
        let cx = hook_context(&room, &fleet, Path::new("/work/project"));
        hook.on_event(&stamped(10, closing, &room), &cx).await;
        settle().await;
        assert_eq!(
            fleet.signalled(&root, owed::KIND).last(),
            Some(&Value::Null),
            "the card went with the room"
        );

        tokio::time::advance(crate::chase::PATIENCE).await;
        settle().await;
        assert!(
            chases(&fleet).is_empty(),
            "nobody is chased for a closed room: {:?}",
            chases(&fleet)
        );
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
