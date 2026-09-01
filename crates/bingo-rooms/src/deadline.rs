//! The patience deadline (ADR-0029 §3). A patient seat reads its room at its
//! next turn — but nothing promises there will be one, so a post held longer
//! than the seat's patience wakes it: once, by a nudge, and the woken turn
//! absorbs the backlog ahead of it because that is the order the queue is in.
//!
//! What it watches is the seat's own queue, and only the room mail in it: a
//! standby brief carries the agents' surface and must never trip this, or
//! ADR-0027's seat stops being free; a nudge carries no principal, so no nudge
//! can chase itself.
//!
//! It keeps the chaser's discipline (ADR-0022 §3): one bounded timer per seat,
//! timers die with the process, and what the next process finds already
//! waiting is nudged **once**.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bingo_sdk::{CancellationToken, HostHandle, IntentId, QueueEntry, SessionId};
use jiff::Timestamp;

use crate::ear::Ear;
use crate::name::{self, PARENT};
use crate::room::Room;
use crate::roster::Roster;
use crate::{SURFACE, chase, post};

/// One seat's queue as this process has watched it.
#[derive(Debug, Default)]
struct Watch {
    /// When each held post was first seen here. A process cannot know how long
    /// a post waited before it started, so what it finds already waiting has
    /// waited from now — the deadline is a bound on the wait it can see.
    since: BTreeMap<IntentId, Timestamp>,
    armed: Option<CancellationToken>,
}

/// The seats this process is holding a deadline over. Machinery, never a
/// record: what is held is the kernel's queue to say, and this only remembers
/// when it first saw it, so that folding the same queue again does not start
/// the patience over.
#[derive(Debug, Default)]
pub struct Deadline {
    seats: Mutex<BTreeMap<SessionId, Watch>>,
}

impl Deadline {
    /// A seat's queue changed. The room mail in it decides the timer: the
    /// earliest deadline wins, and a queue holding none of it disarms.
    pub async fn queued(
        &self,
        host: &HostHandle,
        rooms: &Roster,
        seat: &SessionId,
        entries: &[QueueEntry],
        now: Timestamp,
    ) {
        let held = self.held(seat, entries, now);
        let Some((room, left)) = soonest(host, rooms, seat, &held, now).await else {
            self.disarm(seat);
            return;
        };
        self.arm(host, seat, room, left);
    }

    /// A room this process had not seen before. Its patient seats may be
    /// holding posts from before this process started; a backlog found there
    /// waited out a patience nobody was timing, so it is nudged once and at
    /// once, the rule for an occurrence missed while nobody was running.
    pub async fn overdue(&self, host: &HostHandle, room: &Room) {
        for member in &room.members {
            if room.ears.of(member).is_live() {
                continue;
            }
            let Some(seat) = post::seat_of(host, room, member).await else {
                continue;
            };
            if holding(host, &seat, &room.title).await {
                post::nudge(host, &seat, &room.title, said(room)).await;
            }
        }
    }

    /// The room mail this seat is holding, and how long each piece has been
    /// held. Anything the queue no longer carries is forgotten here, so a seat
    /// that has drained is remembered by nothing.
    fn held(
        &self,
        seat: &SessionId,
        entries: &[QueueEntry],
        now: Timestamp,
    ) -> Vec<(Timestamp, String)> {
        let posts: Vec<(&IntentId, &str)> = entries.iter().filter_map(posted).collect();
        let mut seats = self.seats();
        let watch = seats.entry(seat.clone()).or_default();
        watch
            .since
            .retain(|intent, _| posts.iter().any(|(held, _)| *held == intent));
        posts
            .into_iter()
            .map(|(intent, room)| {
                let since = watch.since.entry(intent.clone()).or_insert(now);
                (*since, room.to_string())
            })
            .collect()
    }

    /// One timer, replacing whatever this seat had: the queue it was armed
    /// from is not the queue any more.
    fn arm(&self, host: &HostHandle, seat: &SessionId, room: Room, left: Duration) {
        let cancel = CancellationToken::new();
        let mut seats = self.seats();
        let watch = seats.entry(seat.clone()).or_default();
        if let Some(armed) = watch.armed.replace(cancel.clone()) {
            armed.cancel();
        }
        tokio::spawn(wait(host.clone(), seat.clone(), room, left, cancel));
    }

    /// A seat holding nothing of ours is watched by nothing.
    fn disarm(&self, seat: &SessionId) {
        if let Some(watch) = self.seats().remove(seat)
            && let Some(armed) = watch.armed
        {
            armed.cancel();
        }
    }

    fn seats(&self) -> MutexGuard<'_, BTreeMap<SessionId, Watch>> {
        self.seats.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// The room a queued input is a post from, or nothing for everything else a
/// queue holds. The surface is the whole discriminator (ADR-0029 §3): a
/// standby brief is the agents' surface and never arms this, and a nudge — a
/// delivery nobody signed — is not a post to be chased.
fn posted(entry: &QueueEntry) -> Option<(&IntentId, &str)> {
    if entry.origin.surface != SURFACE || entry.origin.principal.is_none() {
        return None;
    }
    Some((&entry.intent, entry.origin.conversation.as_deref()?))
}

/// The first of this seat's held posts to fall due, and how long there is
/// until it does. A post from a room this process cannot place, or one whose
/// seat is live there, is nobody's deadline.
async fn soonest(
    host: &HostHandle,
    rooms: &Roster,
    seat: &SessionId,
    held: &[(Timestamp, String)],
    now: Timestamp,
) -> Option<(Room, Duration)> {
    let titles: BTreeSet<&str> = held.iter().map(|(_, title)| title.as_str()).collect();
    let mut patient: BTreeMap<&str, (Room, Duration)> = BTreeMap::new();
    for title in titles {
        if let Some(found) = patience(host, rooms, seat, title).await {
            patient.insert(title, found);
        }
    }
    held.iter()
        .filter_map(|(since, title)| {
            let (room, patience) = patient.get(title.as_str())?;
            Some((
                room.clone(),
                patience.saturating_sub(chase::waited(*since, now)),
            ))
        })
        .min_by_key(|(_, left)| *left)
}

/// The room a held post came from and how long this seat may hold it: the
/// rooms of that title this process has seen, and the name this session sits
/// under on each of them.
async fn patience(
    host: &HostHandle,
    rooms: &Roster,
    seat: &SessionId,
    title: &str,
) -> Option<(Room, Duration)> {
    for room in rooms.titled(title) {
        // A room of live seats places no deadline, and asking the tree who
        // this session is there would be a round-trip for nothing: today's
        // room costs this module exactly what it cost before there were ears.
        if !room.ears.patient() {
            continue;
        }
        let Some(member) = seated_as(host, &room, seat).await else {
            continue;
        };
        if let Ear::Patient(patience) = room.ears.of(&member) {
            return Some((room, patience));
        }
    }
    None
}

/// The name a session sits under on a room's roster: `parent` for the session
/// the room hangs under, and its own title for a member beside it.
async fn seated_as(host: &HostHandle, room: &Room, seat: &SessionId) -> Option<String> {
    let title = match seat == &room.parent {
        true => PARENT.to_string(),
        false => titled(host, room, seat).await?,
    };
    room.members
        .iter()
        .find(|member| name::same(member, &title))
        .cloned()
}

async fn titled(host: &HostHandle, room: &Room, seat: &SessionId) -> Option<String> {
    post::siblings_of(host, room)
        .await
        .ok()?
        .into_iter()
        .find(|summary| &summary.id == seat)?
        .title
}

/// The timer itself. It sleeps out what is left of the patience and, if the
/// seat is still holding the room's mail when it wakes, nudges it once — the
/// queue it re-reads is the live one, so a seat that read the room meanwhile
/// is left alone.
async fn wait(
    host: HostHandle,
    seat: SessionId,
    room: Room,
    left: Duration,
    cancel: CancellationToken,
) {
    tokio::select! {
        () = cancel.cancelled() => return,
        () = tokio::time::sleep(left) => {}
    }
    if holding(&host, &seat, &room.title).await {
        post::nudge(&host, &seat, &room.title, said(&room)).await;
    }
}

/// Whether a seat's queue still holds a post from this room.
async fn holding(host: &HostHandle, seat: &SessionId, title: &str) -> bool {
    let Some(state) = crate::room::read(host, seat).await else {
        return false;
    };
    state
        .queue
        .iter()
        .filter_map(posted)
        .any(|(_, room)| room == title)
}

/// What the nudge says. The posts it is about are already above it — a woken
/// turn takes the queue in order and this delivery is the last of it — so it
/// points at them rather than repeating them.
fn said(room: &Room) -> String {
    format!(
        "{} has posts you have not read; they are above this note. \
         Post in {} if any of it falls to you.",
        room.title, room.title
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::{Ears, Seat};
    use crate::room::{self, MEMBERS};
    use crate::tests::{Fleet, briefed, held, nudged, queued};
    use bingo_sdk::{Delivery, Input};

    /// The seats a roster line asks for.
    fn seats(roster: &[&str]) -> Vec<Seat> {
        roster
            .iter()
            .map(|word| Seat::read(word).expect("a roster word"))
            .collect()
    }

    /// A root with a scout under it and a room the roster seats them both in,
    /// as the frames this process saw would have left it.
    fn tree(roster: &[&str]) -> (Fleet, SessionId, SessionId, Roster) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let room = fleet.room(&root, "design");
        let rooms = Roster::default();
        rooms.register(&fleet.summary(&room));
        rooms.extended(&room, MEMBERS, &room::payload(&seats(roster)));
        (fleet, root, scout, rooms)
    }

    /// The room as a reader that never saw a frame holds it: the roster, and
    /// the ears it declared.
    fn room_of(parent: &SessionId, roster: &[&str]) -> Room {
        let seated = seats(roster);
        let mut ears = Ears::default();
        ears.declare(&room::payload(&seated));
        Room {
            title: "#design".into(),
            parent: parent.clone(),
            members: seated.into_iter().map(|seat| seat.name).collect(),
            ears,
        }
    }

    fn nudges(fleet: &Fleet) -> Vec<(SessionId, String)> {
        fleet
            .delivered()
            .into_iter()
            .filter_map(|(to, input, delivery)| match input {
                Input::Text { text, origin, .. } if origin.principal.is_none() => {
                    assert_eq!(delivery, Delivery::Wake, "a nudge wakes the seat");
                    Some((to, text))
                }
                _ => None,
            })
            .collect()
    }

    /// One patience at a time, the chaser's own wait: a task that has not run
    /// yet has not asked for its sleep.
    async fn after(patience: Duration) {
        settle().await;
        tokio::time::advance(patience).await;
        settle().await;
    }

    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    /// The whole of ADR-0029 §3 on the paused clock: a patient seat holding a
    /// post is woken once when the post has waited its patience, and not again
    /// while it holds the same backlog.
    #[tokio::test(start_paused = true)]
    async fn a_held_post_wakes_the_seat_once_when_it_has_waited_its_patience() {
        let (fleet, _, scout, rooms) = tree(&["~scout:120"]);
        let patience = Duration::from_secs(120);
        let deadline = Deadline::default();
        let host = fleet.handle();
        let waiting = queued(&fleet, &scout, &[held("req_1", "#design")]);

        deadline
            .queued(&host, &rooms, &scout, &waiting, now())
            .await;
        after(Duration::from_secs(119)).await;
        assert!(
            nudges(&fleet).is_empty(),
            "nothing before the patience is up"
        );

        after(Duration::from_secs(1)).await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert_eq!(nudged[0].0, scout);
        assert!(nudged[0].1.contains("#design"), "{}", nudged[0].1);
        assert!(nudged[0].1.contains("above this note"), "{}", nudged[0].1);

        after(patience).await;
        after(patience).await;
        assert_eq!(nudges(&fleet).len(), 1, "one nudge per backlog");
    }

    /// A second post does not start the patience over: the deadline is on the
    /// oldest thing held, so a room that keeps talking is still bounded.
    #[tokio::test(start_paused = true)]
    async fn the_deadline_is_the_oldest_held_post_s_and_a_later_one_does_not_move_it() {
        let (fleet, _, scout, rooms) = tree(&["~scout:120"]);
        let deadline = Deadline::default();
        let host = fleet.handle();
        let first = queued(&fleet, &scout, &[held("req_1", "#design")]);
        deadline.queued(&host, &rooms, &scout, &first, now()).await;

        after(Duration::from_secs(60)).await;
        let both = queued(
            &fleet,
            &scout,
            &[held("req_1", "#design"), held("req_2", "#design")],
        );
        let later = now() + jiff::SignedDuration::from_secs(60);
        deadline.queued(&host, &rooms, &scout, &both, later).await;

        after(Duration::from_secs(60)).await;
        assert_eq!(nudges(&fleet).len(), 1, "the first post's own deadline");
    }

    /// The seat read the room before the deadline came due: the timer wakes,
    /// sees a queue that no longer holds the post, and says nothing.
    #[tokio::test(start_paused = true)]
    async fn a_seat_that_drains_before_the_deadline_is_not_nudged() {
        let (fleet, _, scout, rooms) = tree(&["~scout:120"]);
        let deadline = Deadline::default();
        let host = fleet.handle();
        let waiting = queued(&fleet, &scout, &[held("req_1", "#design")]);
        deadline
            .queued(&host, &rooms, &scout, &waiting, now())
            .await;

        after(Duration::from_secs(60)).await;
        let drained = queued(&fleet, &scout, &[]);
        deadline
            .queued(&host, &rooms, &scout, &drained, now())
            .await;

        after(Duration::from_secs(600)).await;
        assert!(nudges(&fleet).is_empty(), "{:?}", fleet.delivered());
    }

    /// ADR-0027 lives or dies here: a standby brief waits in the same queue,
    /// under the agents' surface, and nothing in this module may wake it.
    #[tokio::test(start_paused = true)]
    async fn a_standby_brief_arms_nothing_however_long_it_waits() {
        let (fleet, _, scout, rooms) = tree(&["~scout:120"]);
        let deadline = Deadline::default();
        let brief = queued(&fleet, &scout, &[briefed("req_1")]);
        deadline
            .queued(&fleet.handle(), &rooms, &scout, &brief, now())
            .await;

        after(Duration::from_secs(3600)).await;
        assert!(
            fleet.delivered().is_empty(),
            "a standby seat was woken by its own briefing: {:?}",
            fleet.delivered()
        );
    }

    /// A live seat holds a post only because it is busy, and a busy seat reads
    /// its queue at its next barrier. Nothing to bound, nothing armed.
    #[tokio::test(start_paused = true)]
    async fn a_live_seat_s_queue_arms_nothing() {
        let (fleet, _, scout, rooms) = tree(&["scout"]);
        let deadline = Deadline::default();
        let waiting = queued(&fleet, &scout, &[held("req_1", "#design")]);
        deadline
            .queued(&fleet.handle(), &rooms, &scout, &waiting, now())
            .await;

        after(Duration::from_secs(3600)).await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// A nudge is not a post: it carries no principal, so the queue it waits
    /// in never arms a deadline of its own.
    #[tokio::test(start_paused = true)]
    async fn a_nudge_in_the_queue_never_chases_itself() {
        let (fleet, _, scout, rooms) = tree(&["~scout:120"]);
        let deadline = Deadline::default();
        let waiting = queued(&fleet, &scout, &[nudged("req_1", "#design")]);
        deadline
            .queued(&fleet.handle(), &rooms, &scout, &waiting, now())
            .await;

        after(Duration::from_secs(3600)).await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// The holder is a seat like any other: its patience is read off the
    /// roster under the name the room calls it by.
    #[tokio::test(start_paused = true)]
    async fn the_session_the_room_hangs_under_is_a_patient_seat_too() {
        let (fleet, root, _, rooms) = tree(&["scout", "~parent:120"]);
        let deadline = Deadline::default();
        let waiting = queued(&fleet, &root, &[held("req_1", "#design")]);
        deadline
            .queued(&fleet.handle(), &rooms, &root, &waiting, now())
            .await;

        after(Duration::from_secs(120)).await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert_eq!(nudged[0].0, root);
    }

    /// A room this process reads for the first time may find a seat already
    /// holding its posts — the queue the last process left. It waited out a
    /// patience nobody was timing, so it is nudged once, and at once.
    #[tokio::test(start_paused = true)]
    async fn a_backlog_this_process_finds_already_waiting_is_nudged_once() {
        let (fleet, root, scout, _) = tree(&["~scout:120"]);
        queued(&fleet, &scout, &[held("req_1", "#design")]);
        let room = room_of(&root, &["~scout:120"]);
        let deadline = Deadline::default();

        deadline.overdue(&fleet.handle(), &room).await;
        settle().await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert_eq!(nudged[0].0, scout);

        queued(&fleet, &scout, &[]);
        deadline.overdue(&fleet.handle(), &room).await;
        settle().await;
        assert_eq!(
            nudges(&fleet).len(),
            1,
            "a seat that has read the room is left alone"
        );
    }

    /// A live seat is never behind: whatever it holds, it holds because it is
    /// busy, and a busy seat reads its queue at the next barrier.
    #[tokio::test(start_paused = true)]
    async fn a_live_seat_s_backlog_is_nobody_s_deadline() {
        let (fleet, root, scout, _) = tree(&["scout"]);
        queued(&fleet, &scout, &[held("req_1", "#design")]);
        Deadline::default()
            .overdue(&fleet.handle(), &room_of(&root, &["scout"]))
            .await;
        settle().await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// A room nobody has seen a frame of places nothing: the post is held by a
    /// seat this process cannot name, so nothing is armed for it.
    #[tokio::test(start_paused = true)]
    async fn a_post_from_a_room_this_process_has_not_seen_arms_nothing() {
        let (fleet, _, scout, _) = tree(&["~scout:120"]);
        let deadline = Deadline::default();
        let waiting = queued(&fleet, &scout, &[held("req_1", "#elsewhere")]);
        deadline
            .queued(&fleet.handle(), &Roster::default(), &scout, &waiting, now())
            .await;

        after(Duration::from_secs(3600)).await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    #[test]
    fn only_a_room_s_own_post_is_a_deadline_s_business() {
        assert_eq!(
            posted(&held("req_1", "#design")).map(|(_, room)| room),
            Some("#design")
        );
        assert_eq!(posted(&briefed("req_2")), None, "the agents' surface");
        assert_eq!(
            posted(&nudged("req_3", "#design")),
            None,
            "nobody signed it"
        );
    }
}
