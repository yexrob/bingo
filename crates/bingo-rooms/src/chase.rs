//! The one chaser (ADR-0022 §3). A question nobody answered is a timer: after
//! five minutes the member is nudged, at most three times, and after that the
//! debt stays where a person can see it, which is the whole of the escalation.
//!
//! A nudge is a delivery and not a post: the room stays quiet, and nothing a
//! nudge says opens a debt of its own.
//!
//! The table below is machinery, never a record. What is owed is the fold's to
//! say; this only remembers which debts this process has already taken up, so
//! that re-deriving after every post does not arm a second timer for the same
//! one. Timers die with the process, and the next process's fold chases what it
//! finds already overdue **once** — the schedule plugin's rule for an
//! occurrence missed while nobody was running.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bingo_sdk::{
    CancellationToken, Delivery, HostHandle, Input, IntentId, ItemId, Origin, SessionFilter,
    SessionId,
};
use jiff::Timestamp;

use crate::SURFACE;
use crate::mentions::Mention;
use crate::post;
use crate::room::Room;

/// How long a member has to answer before the first nudge, and between them.
const PATIENCE: Duration = Duration::from_secs(300);

/// How many nudges a debt is worth in the process that heard it asked.
const NUDGES: u8 = 3;

/// What a debt already overdue when this process first read it is worth. A
/// fold cannot know what the process before it sent, so a restart's chase is
/// one — and after it, the debt is only a line in `owed`.
const OVERDUE: u8 = 1;

/// One debt, as the chaser tells two apart: the post that opened it and who
/// owes for it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Debt {
    room: SessionId,
    post: ItemId,
    member: String,
}

/// The debts this process has taken up, each with the timer chasing it.
#[derive(Debug, Default)]
pub struct Chaser {
    taken: Mutex<BTreeMap<Debt, CancellationToken>>,
}

impl Chaser {
    /// Bring the timers into line with what the room now owes: a debt nobody
    /// is chasing gets one, and a debt that has closed loses its. Called after
    /// every fold, so the fold — not this table — decides who is chased.
    pub fn reconcile(
        &self,
        host: &HostHandle,
        room: &Room,
        id: &SessionId,
        open: &[Mention],
        now: Timestamp,
    ) {
        let standing: Vec<Debt> = open.iter().filter_map(|m| debt(id, m)).collect();
        let mut taken = self.taken();
        // This fold speaks for one room, so only that room's debts are its to
        // close; another room's are still standing until its own fold says
        // otherwise.
        taken.retain(|debt, timer| {
            let stands = &debt.room != id || standing.contains(debt);
            if !stands {
                timer.cancel();
            }
            stands
        });
        for mention in open {
            let Some(debt) = debt(id, mention) else {
                continue;
            };
            if taken.contains_key(&debt) {
                continue;
            }
            taken.insert(debt, arm(host, room, mention, now));
        }
    }

    fn taken(&self) -> MutexGuard<'_, BTreeMap<Debt, CancellationToken>> {
        self.taken.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// The debt a mention is, or nothing for one nobody is chased for: `@all`
/// picked no member, so no timer can pick one either.
fn debt(room: &SessionId, mention: &Mention) -> Option<Debt> {
    Some(Debt {
        room: room.clone(),
        post: mention.post.clone(),
        member: mention.owed_by.chased()?.to_string(),
    })
}

/// One timer for one debt.
fn arm(host: &HostHandle, room: &Room, mention: &Mention, now: Timestamp) -> CancellationToken {
    let cancel = CancellationToken::new();
    let (first, nudges) = budget(mention.at, now);
    tokio::spawn(chase(
        host.clone(),
        room.clone(),
        mention.clone(),
        first,
        nudges,
        cancel.clone(),
    ));
    cancel
}

/// How long until the first nudge, and how many there are: a question this
/// process heard asked waits the full patience and is worth three; one already
/// overdue when the fold first read it is nudged now, and once.
fn budget(at: Timestamp, now: Timestamp) -> (Duration, u8) {
    let waited = Duration::from_secs(u64::try_from(now.duration_since(at).as_secs()).unwrap_or(0));
    match PATIENCE.checked_sub(waited) {
        Some(left) if !left.is_zero() => (left, NUDGES),
        _ => (Duration::ZERO, OVERDUE),
    }
}

/// The timer itself. It sleeps, nudges, and sleeps again until its nudges are
/// spent — or until the debt closes, which cancels it mid-sleep.
async fn chase(
    host: HostHandle,
    room: Room,
    mention: Mention,
    first: Duration,
    nudges: u8,
    cancel: CancellationToken,
) {
    let mut delay = first;
    for _ in 0..nudges {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
        nudge(&host, &room, &mention).await;
        delay = PATIENCE;
    }
}

/// One nudge, into the member's own queue. A member whose seat is gone is
/// skipped the way a post skips one: a room reaches as far as the tree it sits
/// in, and no further.
async fn nudge(host: &HostHandle, room: &Room, mention: &Mention) {
    let Some(member) = mention.owed_by.chased() else {
        return;
    };
    let Some(seat) = seat(host, room, member).await else {
        tracing::debug!(room = %room.title, member, "a nudge found nobody of that name");
        return;
    };
    let input = Input::text(said(room, mention), origin(&room.title));
    let sent = host
        .deliver(&seat, IntentId::mint(), input, Delivery::Wake)
        .await;
    if let Err(error) = sent {
        tracing::debug!(room = %room.title, member, %error, "a nudge did not arrive");
    }
}

/// The session a member name means now. A nudge looks a member up the way a
/// post does, so the two agree on who is there to hear it.
async fn seat(host: &HostHandle, room: &Room, member: &str) -> Option<SessionId> {
    let siblings = host
        .sessions(SessionFilter {
            parent: Some(room.parent.clone()),
            ..SessionFilter::default()
        })
        .await
        .ok()?;
    post::seat_of(&siblings, member).map(|summary| summary.id.clone())
}

/// What a nudge says: where it was asked, who asked, and what they asked.
fn said(room: &Room, mention: &Mention) -> String {
    format!(
        "{} asked you in {} and you have not answered: \"{}\". Post in {} to answer.",
        mention.asker, room.title, mention.head, room.title
    )
}

/// Where a nudge comes from: the room, and nobody in it. The fold reads it as
/// `[in #design]`, which a post — always signed — never reads as.
fn origin(room: &str) -> Origin {
    Origin {
        surface: SURFACE.into(),
        principal: None,
        conversation: Some(room.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mentions::Owed;
    use crate::tests::Fleet;
    use bingo_sdk::Input;

    /// A room with a scout in it, and the fold's word that the scout owes.
    fn asked(members: &[&str]) -> (Fleet, SessionId, Room, Mention) {
        let fleet = Fleet::default();
        let root = fleet.root();
        for member in members {
            fleet.child(&root, member);
        }
        let id = fleet.room(&root, "design");
        let room = Room {
            title: "#design".into(),
            parent: root,
            members: members.iter().map(|m| m.to_string()).collect(),
        };
        (fleet, id, room, mention(Owed::Member("scout".into())))
    }

    fn mention(owed_by: Owed) -> Mention {
        Mention {
            owed_by,
            asker: "parent".into(),
            post: ItemId::from_raw("itm_ask"),
            at: Timestamp::UNIX_EPOCH,
            head: "look at the build".into(),
        }
    }

    /// Now, as the room's own clock has it: the question was asked this second.
    fn fresh() -> Timestamp {
        Timestamp::UNIX_EPOCH
    }

    /// One patience at a time. The timers are given their turn before the
    /// clock moves — a task that has not run yet has not asked for its sleep,
    /// and would take the whole patience again from wherever the clock landed.
    async fn wait(rounds: u32) {
        for _ in 0..rounds {
            settle().await;
            tokio::time::advance(PATIENCE).await;
            settle().await;
        }
    }

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
                Input::Text { text, origin, .. } => origin.principal.is_none().then_some(text),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_question_is_nudged_three_times_and_then_no_more() {
        let (fleet, id, room, mention) = asked(&["scout"]);
        let chaser = Chaser::default();
        chaser.reconcile(&fleet.handle(), &room, &id, &[mention], fresh());

        wait(1).await;
        assert_eq!(nudges(&fleet).len(), 1, "nothing before the patience is up");
        wait(2).await;
        assert_eq!(nudges(&fleet).len(), 3);
        wait(4).await;
        assert_eq!(
            nudges(&fleet).len(),
            3,
            "after the third the debt is only a line in `owed`"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_nudge_says_where_who_and_what_and_is_not_a_post() {
        let (fleet, id, room, mention) = asked(&["scout"]);
        Chaser::default().reconcile(&fleet.handle(), &room, &id, &[mention], fresh());
        wait(1).await;

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        let (to, input, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Wake);
        let Input::Text { text, origin, .. } = input else {
            panic!("a nudge is text");
        };
        assert_eq!(
            text,
            "parent asked you in #design and you have not answered: \
             \"look at the build\". Post in #design to answer."
        );
        assert_eq!(origin.surface, SURFACE);
        assert_eq!(origin.principal, None, "nobody wrote a nudge");
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    #[tokio::test(start_paused = true)]
    async fn answering_cancels_the_timer_mid_sleep() {
        let (fleet, id, room, mention) = asked(&["scout"]);
        let chaser = Chaser::default();
        let host = fleet.handle();
        chaser.reconcile(&host, &room, &id, &[mention], fresh());
        chaser.reconcile(&host, &room, &id, &[], fresh());

        wait(4).await;
        assert!(
            nudges(&fleet).is_empty(),
            "the debt closed before the nudge"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn one_room_answering_leaves_another_room_s_question_standing() {
        let (fleet, design, room, mention) = asked(&["scout"]);
        let standup = fleet.room(&room.parent, "standup");
        let other = Room {
            title: "#standup".into(),
            ..room.clone()
        };
        let chaser = Chaser::default();
        let host = fleet.handle();
        chaser.reconcile(
            &host,
            &room,
            &design,
            std::slice::from_ref(&mention),
            fresh(),
        );
        chaser.reconcile(&host, &other, &standup, &[mention], fresh());

        // The design room's fold speaks for the design room alone.
        chaser.reconcile(&host, &room, &design, &[], fresh());
        wait(1).await;
        let nudged = nudges(&fleet);
        assert_eq!(nudged.len(), 1, "{nudged:?}");
        assert!(nudged[0].contains("#standup"), "{}", nudged[0]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_debt_already_chased_is_not_chased_twice_over() {
        let (fleet, id, room, mention) = asked(&["scout"]);
        let chaser = Chaser::default();
        let host = fleet.handle();
        for _ in 0..5 {
            chaser.reconcile(&host, &room, &id, std::slice::from_ref(&mention), fresh());
        }
        wait(3).await;
        assert_eq!(nudges(&fleet).len(), 3, "one timer per debt, however often");
    }

    #[tokio::test(start_paused = true)]
    async fn a_debt_found_overdue_is_nudged_once_and_at_once() {
        let (fleet, id, room, mention) = asked(&["scout"]);
        let later = Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(3600);
        Chaser::default().reconcile(&fleet.handle(), &room, &id, &[mention], later);

        wait(1).await;
        assert_eq!(nudges(&fleet).len(), 1, "a restart's chase is once");
        wait(4).await;
        assert_eq!(nudges(&fleet).len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_room_s_own_debt_is_never_chased() {
        let (fleet, id, room, _) = asked(&["scout"]);
        Chaser::default().reconcile(&fleet.handle(), &room, &id, &[mention(Owed::Room)], fresh());
        wait(4).await;
        assert!(
            fleet.delivered().is_empty(),
            "the sigil named nobody, so nobody is chased"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_member_whose_seat_is_gone_is_skipped_and_nothing_wedges() {
        let (fleet, id, room, mention) = asked(&[]);
        Chaser::default().reconcile(&fleet.handle(), &room, &id, &[mention], fresh());
        wait(4).await;
        assert!(fleet.delivered().is_empty(), "nobody answers to that name");
    }

    #[test]
    fn a_fresh_question_waits_and_an_overdue_one_does_not() {
        let asked_at = Timestamp::UNIX_EPOCH;
        let at = |seconds: i64| asked_at + jiff::SignedDuration::from_secs(seconds);
        assert_eq!(budget(asked_at, at(0)), (PATIENCE, NUDGES));
        assert_eq!(
            budget(asked_at, at(60)),
            (Duration::from_secs(240), NUDGES),
            "a process that heard the question keeps the rest of the patience"
        );
        assert_eq!(budget(asked_at, at(300)), (Duration::ZERO, OVERDUE));
        assert_eq!(budget(asked_at, at(86_400)), (Duration::ZERO, OVERDUE));
        assert_eq!(
            budget(asked_at, at(-60)),
            (PATIENCE, NUDGES),
            "a clock that went backwards is not an overdue debt"
        );
    }
}
