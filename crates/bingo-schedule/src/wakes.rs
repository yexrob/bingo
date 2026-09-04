//! The wakes this process holds, and the loop that delivers them.
//!
//! A wake is the session's own (ADR-0019 §8, amended 2026-09-04): a turn of
//! that session sets it, in the process running the session, and the same
//! process delivers it. It is never written to the store. The store's runner
//! is one per store and a session is one per process, so a wake fired from
//! another process would find the session locked — and a file left in the
//! store is read by whichever binary holds the claim, however old that
//! binary is. Held here, in memory, a wake lives exactly as long as the
//! process that is running the session it wakes, which is what the ADR
//! says a wake may live.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bingo_sdk::{CancellationToken, Delivery, HostHandle, Input, IntentId, SessionId};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::Notify;

use crate::wake::{self, WAKE_MOST, Wake};

/// One wake per session, and the bell rung when the set of them changes.
#[derive(Debug, Default)]
pub struct Wakes {
    pending: Mutex<HashMap<SessionId, Wake>>,
    changed: Notify,
}

impl Wakes {
    /// Set the wake standing on `session`, in place of the one that stood.
    pub fn set(&self, session: &SessionId, wake: Wake) -> Option<Wake> {
        let had = self.held().insert(session.clone(), wake);
        self.changed.notify_one();
        had
    }

    /// End the wake standing on `session`, if one does.
    pub fn take(&self, session: &SessionId) -> Option<Wake> {
        let had = self.held().remove(session);
        self.changed.notify_one();
        had
    }

    /// The wake standing on `session`, if one does.
    pub fn pending(&self, session: &SessionId) -> Option<Wake> {
        self.held().get(session).cloned()
    }

    /// Every wake whose moment has come, soonest first, taken out as it
    /// goes: a wake is spent by being forgotten.
    pub fn due(&self, now: Timestamp) -> Vec<(SessionId, Wake)> {
        let mut held = self.held();
        let mut due: Vec<(SessionId, Wake)> = held
            .iter()
            .filter(|(_, wake)| wake.at <= now)
            .map(|(session, wake)| (session.clone(), wake.clone()))
            .collect();
        for (session, _) in &due {
            held.remove(session);
        }
        due.sort_by_key(|(_, wake)| wake.at);
        due
    }

    /// The soonest moment still ahead, if any wake stands.
    pub fn next(&self) -> Option<Timestamp> {
        self.held().values().map(|wake| wake.at).min()
    }

    fn held(&self) -> MutexGuard<'_, HashMap<SessionId, Wake>> {
        self.pending.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// How long to sleep for a wake at `next`: never into the past, and never
/// longer than the longest wake there is, so a clock that jumped is not
/// slept through.
pub fn until(next: Option<Timestamp>, now: Timestamp) -> Duration {
    let ahead = next.map_or(WAKE_MOST, |at| at.duration_since(now));
    ahead.clamp(SignedDuration::ZERO, WAKE_MOST).unsigned_abs()
}

/// Deliver each wake as its moment comes, sleep to the next, and start
/// again — until the plugin stops.
pub async fn run(wakes: Arc<Wakes>, host: HostHandle, cancel: CancellationToken) {
    loop {
        for (session, wake) in wakes.due(Timestamp::now()) {
            deliver(&host, &session, wake).await;
        }
        let waited = until(wakes.next(), Timestamp::now());
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(waited) => {}
            _ = wakes.changed.notified() => {}
        }
    }
}

/// The note, on the conversation that asked for it: a delivery like any
/// other, so a session still in a turn takes it at the barrier and one that
/// is idle opens a turn on it. The pending wake is taken back first — it is
/// already forgotten here, and a screen that still counted one down would be
/// the only thing left saying so.
async fn deliver(host: &HostHandle, session: &SessionId, wake: Wake) {
    wake::publish(host, session, None).await;
    let input = Input::text(wake.note, wake::origin());
    if let Err(error) = host
        .deliver(session, IntentId::mint(), input, Delivery::Wake)
        .await
    {
        tracing::warn!(%session, "a wake reached no turn: {}", error.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fixture;
    use serde_json::Value;

    fn session(name: &str) -> SessionId {
        SessionId::from_raw(name)
    }

    fn at(seconds: i64) -> Timestamp {
        Timestamp::UNIX_EPOCH + SignedDuration::from_secs(seconds)
    }

    fn wake(seconds: i64) -> Wake {
        Wake {
            at: at(seconds),
            note: format!("at {seconds}"),
        }
    }

    #[test]
    fn one_wake_stands_per_session_and_the_next_takes_its_place() {
        let wakes = Wakes::default();
        assert_eq!(wakes.set(&session("a"), wake(10)), None);
        assert_eq!(wakes.set(&session("a"), wake(20)), Some(wake(10)));
        assert_eq!(wakes.pending(&session("a")), Some(wake(20)));
        assert_eq!(wakes.pending(&session("b")), None);
        assert_eq!(wakes.take(&session("a")), Some(wake(20)));
        assert_eq!(wakes.take(&session("a")), None, "none stood");
    }

    #[test]
    fn what_is_due_is_taken_out_soonest_first_and_the_rest_is_waited_for() {
        let wakes = Wakes::default();
        wakes.set(&session("late"), wake(30));
        wakes.set(&session("soon"), wake(10));
        wakes.set(&session("later"), wake(50));
        assert_eq!(wakes.next(), Some(at(10)));
        assert_eq!(
            wakes.due(at(30)),
            vec![(session("soon"), wake(10)), (session("late"), wake(30))]
        );
        assert_eq!(wakes.next(), Some(at(50)), "the spent ones are gone");
        assert!(wakes.due(at(30)).is_empty(), "and are not due twice");
    }

    #[test]
    fn the_sleep_ends_when_the_wake_is_due_and_never_runs_past_the_longest_wake() {
        assert_eq!(until(Some(at(45)), at(10)), Duration::from_secs(35));
        assert_eq!(until(Some(at(0)), at(60)), Duration::ZERO);
        assert_eq!(until(None, at(0)), WAKE_MOST.unsigned_abs());
        assert_eq!(
            until(Some(at(86_400)), at(0)),
            WAKE_MOST.unsigned_abs(),
            "a clock that jumped is looked at again within the hour"
        );
    }

    /// A wake fires on the conversation it names and is spent by being
    /// forgotten: the delivery is `Wake` like any other, so a session still
    /// in a turn takes it at the barrier.
    #[tokio::test]
    async fn a_due_wake_lands_on_its_session_and_is_taken_back_from_the_screen_first() {
        let fixture = Fixture::new();
        let woken = session("ses_woken");
        let wakes = Wakes::default();
        wakes.set(&woken, wake(0));
        for (session, wake) in wakes.due(Timestamp::now()) {
            deliver(&fixture.handle(), &session, wake).await;
        }
        assert_eq!(wakes.pending(&woken), None, "spent");
        assert_eq!(
            fixture.host.delivered(),
            vec![(
                woken.clone(),
                Input::text("at 0", wake::origin()),
                Delivery::Wake
            )]
        );
        assert_eq!(
            fixture.host.extended(),
            vec![(
                woken,
                wake::PLUGIN.to_string(),
                wake::KIND.to_string(),
                Value::Null
            )],
            "the pending wake is taken back before the note lands"
        );
    }

    /// The loop itself, on a real clock: a wake set for now is delivered
    /// without waiting for a sleep to end, because setting one rings the bell.
    #[tokio::test]
    async fn the_loop_delivers_a_wake_the_moment_it_is_set_for() {
        let fixture = Fixture::new();
        let wakes = Arc::new(Wakes::default());
        let cancel = CancellationToken::new();
        let loop_ = tokio::spawn(run(Arc::clone(&wakes), fixture.handle(), cancel.clone()));
        wakes.set(&session("ses_now"), wake(0));
        let started = std::time::Instant::now();
        while fixture.host.delivered().is_empty() && started.elapsed() < Duration::from_secs(10) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cancel.cancel();
        loop_.await.expect("the loop ends when told");
        assert_eq!(fixture.host.delivered().len(), 1, "delivered once");
    }
}
