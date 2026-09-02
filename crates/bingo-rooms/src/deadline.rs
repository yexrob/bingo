//! The patience deadline (ADR-0029 §3, ADR-0034 §3). A patient seat reads its
//! room at its next turn — but nothing promises there will be one, so a seat
//! left behind by a post is woken once when it has been behind for its whole
//! patience. The wake is a nudge, and the turn it opens reads the room.
//!
//! What it watches is the one fact "behind" is derived from: the seat's cursor
//! against the room's head. A seat that read the room meanwhile is level when
//! the timer fires, and is left alone.
//!
//! It keeps the chaser's discipline (ADR-0022 §3): one bounded timer per seat
//! per room, timers die with the process, and a seat this process first finds
//! already behind is nudged **once**.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bingo_sdk::{HostHandle, SessionId};

use crate::cursor::Unread;
use crate::ear::Ear;
use crate::post;
use crate::room::Room;

/// One seat's wait on one room.
type Waiting = (SessionId, String);

/// The waits this process is timing.
type Timing = Arc<Mutex<BTreeSet<Waiting>>>;

/// The seats this process is holding a deadline over. Machinery, never a
/// record: what a seat has read is its own journal's to say, and this only
/// remembers which waits are already being timed, so that a room that keeps
/// talking does not start the patience over.
#[derive(Debug, Default)]
pub struct Deadline {
    waiting: Timing,
}

impl Deadline {
    /// The seats a post left waiting. Each gets a timer at its own patience,
    /// unless one is already running for it: the deadline is on the oldest post
    /// a seat has not read, so a room that keeps talking is still bounded.
    pub async fn waiting(&self, host: &HostHandle, id: &SessionId, room: &Room, seats: &[&str]) {
        for member in seats {
            let Ear::Patient(patience) = room.ears.of(member) else {
                continue;
            };
            let Some(seat) = post::seat_of(host, room, member).await else {
                continue;
            };
            self.arm(host, id, room, member, seat, patience);
        }
    }

    /// A room this process had not seen before. Its patient seats may have been
    /// behind since before this process started; a seat found behind waited out
    /// a patience nobody was timing, so it is nudged once and at once, the rule
    /// for an occurrence missed while nobody was running.
    pub async fn overdue(&self, host: &HostHandle, id: &SessionId, room: &Room) {
        for member in &room.members {
            if room.ears.of(member).is_live() {
                continue;
            }
            let Some(seat) = post::seat_of(host, room, member).await else {
                continue;
            };
            wake_if_behind(host, id, room, member, &seat).await;
        }
    }

    /// One timer for one seat's wait on one room.
    fn arm(
        &self,
        host: &HostHandle,
        id: &SessionId,
        room: &Room,
        member: &str,
        seat: SessionId,
        patience: Duration,
    ) {
        let key = (seat.clone(), room.title.clone());
        if !self.timed().insert(key) {
            return;
        }
        tokio::spawn(wait(
            Wait {
                host: host.clone(),
                room: (id.clone(), room.clone()),
                seat: (seat, member.to_string()),
                timing: Arc::clone(&self.waiting),
            },
            patience,
        ));
    }

    /// The waits already being timed. A panic in another task must not strand
    /// every seat after it.
    fn timed(&self) -> MutexGuard<'_, BTreeSet<Waiting>> {
        self.waiting.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// One wait, as the task that times it holds it: which room, which seat, and
/// the table to take itself out of when it is done.
struct Wait {
    host: HostHandle,
    room: (SessionId, Room),
    seat: (SessionId, String),
    timing: Timing,
}

/// The timer itself: it sleeps out the patience and wakes the seat if it is
/// still behind. What it re-reads is the live pair of journals, so a seat that
/// read the room meanwhile is left alone — and either way the wait is over, so
/// the next post left unread arms a new one.
async fn wait(waiting: Wait, patience: Duration) {
    tokio::time::sleep(patience).await;
    let (id, room) = &waiting.room;
    let (seat, member) = &waiting.seat;
    wake_if_behind(&waiting.host, id, room, member, seat).await;
    waiting
        .timing
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .remove(&(seat.clone(), room.title.clone()));
}

/// A seat behind its room, woken for it. Nothing is said of what it missed —
/// the turn this opens reads the room itself.
async fn wake_if_behind(
    host: &HostHandle,
    id: &SessionId,
    room: &Room,
    member: &str,
    seat: &SessionId,
) {
    let unread = Unread::of(host, id, &room.title, seat, member).await;
    if unread.is_empty() {
        return;
    }
    post::nudge(host, seat, &room.title, post::unread(&room.title)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor;
    use crate::ear::{Ears, Seat};
    use crate::room::{self, MEMBERS};
    use crate::roster::Roster;
    use crate::tests::{Fleet, ts};
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
    fn tree(roster: &[&str]) -> (Fleet, SessionId, SessionId, SessionId, Room) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let id = fleet.room(&root, "design");
        let rooms = Roster::default();
        rooms.register(&fleet.summary(&id));
        rooms.extended(&id, MEMBERS, &room::payload(&seats(roster)));
        let room = rooms.get(&id).expect("the room this process saw");
        (fleet, root, scout, id, room)
    }

    /// The room as a reader that never saw a frame holds it.
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

    /// A post into the room, and the seat's cursor moved to it.
    fn said(fleet: &Fleet, id: &SessionId, text: &str) {
        fleet.post(id, text, Some("reviewer"), ts());
    }

    async fn read_it_all(fleet: &Fleet, id: &SessionId, seat: &SessionId) {
        let head = room::read(&fleet.handle(), id)
            .await
            .expect("the room")
            .items
            .last()
            .expect("a post")
            .id
            .clone();
        cursor::advance(&fleet.handle(), seat, "#design", &head)
            .await
            .expect("a cursor this crate can write");
    }

    /// The whole of ADR-0029 §3 on the paused clock: a seat a post left behind
    /// is woken once when it has been behind its whole patience, and not again
    /// while it is behind on the same posts.
    #[tokio::test(start_paused = true)]
    async fn a_seat_left_behind_is_woken_once_when_its_patience_is_up() {
        let (fleet, _, scout, id, room) = tree(&["scout:120"]);
        let deadline = Deadline::default();
        said(&fleet, &id, "the build is green");

        deadline
            .waiting(&fleet.handle(), &id, &room, &["scout"])
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
    }

    /// A second post does not start the patience over: one timer per seat per
    /// room, so a room that keeps talking is still bounded.
    #[tokio::test(start_paused = true)]
    async fn a_later_post_does_not_move_the_deadline_the_first_one_set() {
        let (fleet, _, _, id, room) = tree(&["scout:120"]);
        let deadline = Deadline::default();
        let host = fleet.handle();
        said(&fleet, &id, "the build is green");
        deadline.waiting(&host, &id, &room, &["scout"]).await;

        after(Duration::from_secs(60)).await;
        said(&fleet, &id, "and the tests pass");
        deadline.waiting(&host, &id, &room, &["scout"]).await;

        after(Duration::from_secs(60)).await;
        assert_eq!(nudges(&fleet).len(), 1, "the first post's own deadline");
    }

    /// The seat read the room before the deadline came due: the timer wakes,
    /// finds its cursor at the head, and says nothing.
    #[tokio::test(start_paused = true)]
    async fn a_seat_that_reads_the_room_before_the_deadline_is_not_nudged() {
        let (fleet, _, scout, id, room) = tree(&["scout:120"]);
        let deadline = Deadline::default();
        let host = fleet.handle();
        said(&fleet, &id, "the build is green");
        deadline.waiting(&host, &id, &room, &["scout"]).await;

        after(Duration::from_secs(60)).await;
        read_it_all(&fleet, &id, &scout).await;

        after(Duration::from_secs(600)).await;
        assert!(nudges(&fleet).is_empty(), "{:?}", fleet.delivered());
    }

    /// A live seat was woken by the post itself; nothing here is armed for it.
    #[tokio::test(start_paused = true)]
    async fn a_live_seat_is_nobody_s_deadline() {
        let (fleet, _, _, id, room) = tree(&["scout:0"]);
        let deadline = Deadline::default();
        said(&fleet, &id, "the build is green");
        deadline
            .waiting(&fleet.handle(), &id, &room, &["scout"])
            .await;

        after(Duration::from_secs(3600)).await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// The holder is a seat like any other: its patience is read off the
    /// roster under the name the room calls it by.
    #[tokio::test(start_paused = true)]
    async fn the_session_the_room_hangs_under_is_a_patient_seat_too() {
        let (fleet, root, _, id, room) = tree(&["scout:0", "parent:120"]);
        let deadline = Deadline::default();
        said(&fleet, &id, "the build is green");
        deadline
            .waiting(&fleet.handle(), &id, &room, &["parent"])
            .await;

        after(Duration::from_secs(120)).await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert_eq!(nudged[0].0, root);
    }

    /// A room this process reads for the first time may find a seat already
    /// behind — the cursor the last process left. It waited out a patience
    /// nobody was timing, so it is nudged once, and at once.
    #[tokio::test(start_paused = true)]
    async fn a_seat_this_process_finds_already_behind_is_nudged_once() {
        let (fleet, root, scout, id, _) = tree(&["scout:120"]);
        said(&fleet, &id, "the build is green");
        let room = room_of(&root, &["scout:120"]);
        let deadline = Deadline::default();

        deadline.overdue(&fleet.handle(), &id, &room).await;
        settle().await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert_eq!(nudged[0].0, scout);

        read_it_all(&fleet, &id, &scout).await;
        deadline.overdue(&fleet.handle(), &id, &room).await;
        settle().await;
        assert_eq!(
            nudges(&fleet).len(),
            1,
            "a seat that has read the room is left alone"
        );
    }

    /// A live seat is never behind: whatever it has not read, it has not read
    /// because it is busy, and a busy seat reads at its next round.
    #[tokio::test(start_paused = true)]
    async fn a_live_seat_s_backlog_is_nobody_s_deadline() {
        let (fleet, root, _, id, _) = tree(&["scout:0"]);
        said(&fleet, &id, "the build is green");
        Deadline::default()
            .overdue(&fleet.handle(), &id, &room_of(&root, &["scout:0"]))
            .await;
        settle().await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// A seat that came into being after everything that was said is not behind
    /// on any of it (ADR-0025 §2), so nothing wakes it for that.
    #[tokio::test(start_paused = true)]
    async fn a_seat_younger_than_every_post_starts_level() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = fleet.room(&root, "design");
        said(&fleet, &id, "said before you were seated");
        let scout = fleet.child(&root, "scout");
        fleet.born(&scout, ts() + jiff::SignedDuration::from_secs(60));

        assert_eq!(
            Unread::of(&fleet.handle(), &id, "#design", &scout, "scout").await,
            Unread::default()
        );

        Deadline::default()
            .overdue(&fleet.handle(), &id, &room_of(&root, &["scout:120"]))
            .await;
        settle().await;
        assert!(fleet.delivered().is_empty(), "{:?}", fleet.delivered());
    }

    /// A cursor at the head is the whole of "read": no post of the room is
    /// unread, whoever wrote it.
    #[tokio::test]
    async fn a_cursor_at_the_head_leaves_nothing_unread() {
        let (fleet, _, scout, id, _) = tree(&["scout:120"]);
        said(&fleet, &id, "the build is green");
        let host = fleet.handle();
        assert!(
            !Unread::of(&host, &id, "#design", &scout, "scout")
                .await
                .is_empty()
        );

        read_it_all(&fleet, &id, &scout).await;
        let unread = Unread::of(&host, &id, "#design", &scout, "scout").await;
        assert!(unread.is_empty(), "{unread:?}");
        assert_eq!(unread.head, None, "and nothing left to move the cursor to");
        assert!(
            cursor::of_state(
                &room::read(&host, &scout).await.expect("the seat"),
                "#design"
            )
            .is_some(),
            "the cursor is on the seat's own session"
        );
    }
}
