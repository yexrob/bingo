//! Collection: at most once a day, drop the sessions nobody has touched for a
//! month and the oldest beyond the last hundred, never one that is open
//! (ADR-0005). The clock and the limits are values so a test needs no sleep.

use std::path::Path;

use bingo_sdk::{KernelError, SessionId};
use jiff::Timestamp;

use crate::storage;
use crate::{layout, lock};

const DAY: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug)]
pub struct Gc {
    pub now: Timestamp,
    pub keep_days: i64,
    pub keep_sessions: usize,
}

impl Gc {
    /// What the store runs on start.
    pub fn daily(now: Timestamp) -> Self {
        Self {
            now,
            keep_days: 30,
            keep_sessions: 100,
        }
    }

    /// Collect if a day has passed since the last run, and record this one.
    /// The ids of what it removed, in id order.
    pub fn run(&self, root: &Path) -> Result<Vec<SessionId>, KernelError> {
        if !self.due(root) {
            return Ok(Vec::new());
        }
        let removed = self.collect(root)?;
        self.stamp(root)?;
        Ok(removed)
    }

    /// A run that never happened is due; so is one a day old.
    pub fn due(&self, root: &Path) -> bool {
        match layout::modified(&layout::gc_stamp(root)) {
            Ok(last) => self.now.as_second() - last.as_second() >= DAY,
            Err(_) => true,
        }
    }

    /// Remove what the policy condemns, whatever the stamp says.
    pub fn collect(&self, root: &Path) -> Result<Vec<SessionId>, KernelError> {
        let mut sessions = layout::sessions(root)?;
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
        let mut removed = Vec::new();
        for session in self.condemned(&sessions) {
            if remove(session)? {
                removed.push(session.id.clone());
            }
        }
        removed.sort();
        Ok(removed)
    }

    /// From newest to oldest: everything past the keep count, and everything
    /// older than the keep age.
    fn condemned<'a>(&self, sessions: &'a [layout::Session]) -> Vec<&'a layout::Session> {
        let cutoff = self.now.as_second() - self.keep_days * DAY;
        sessions
            .iter()
            .enumerate()
            .filter(|(rank, session)| {
                *rank >= self.keep_sessions || session.updated_at.as_second() < cutoff
            })
            .map(|(_, session)| session)
            .collect()
    }

    /// The stamp carries the clock this run used, so an injected clock and the
    /// file agree about when the last run was.
    fn stamp(&self, root: &Path) -> Result<(), KernelError> {
        let path = layout::gc_stamp(root);
        std::fs::create_dir_all(root)
            .map_err(|e| storage(format!("create {}: {e}", root.display())))?;
        let file = std::fs::File::create(&path)
            .map_err(|e| storage(format!("write {}: {e}", path.display())))?;
        file.set_modified(std::time::SystemTime::from(self.now))
            .map_err(|e| storage(format!("stamp {}: {e}", path.display())))
    }
}

/// `false` when someone holds the session: an open session is never collected.
/// The lock is given back before the directory goes, because Windows will not
/// remove a directory whose file is open.
fn remove(session: &layout::Session) -> Result<bool, KernelError> {
    let Some(held) = lock::take(&layout::lock(&session.dir))? else {
        return Ok(false);
    };
    drop(held);
    std::fs::remove_dir_all(&session.dir)
        .map_err(|e| storage(format!("remove {}: {e}", session.dir.display())))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{aged_session, root_with};
    use bingo_sdk::SessionStore;

    fn now() -> Timestamp {
        Timestamp::from_second(1_800_000_000).expect("a timestamp")
    }

    fn days_ago(days: i64) -> Timestamp {
        Timestamp::from_second(now().as_second() - days * DAY).expect("a timestamp")
    }

    fn hours_ago(hours: i64) -> Timestamp {
        Timestamp::from_second(now().as_second() - hours * 60 * 60).expect("a timestamp")
    }

    fn gc() -> Gc {
        Gc::daily(now())
    }

    fn removed(ids: &[SessionId]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn kept(root: &Path) -> Vec<String> {
        layout::sessions(root)
            .expect("list")
            .iter()
            .map(|session| session.id.to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_session_nobody_touched_for_a_month_goes() {
        let (_root, store) = root_with();
        aged_session(&store, "ses_old", days_ago(31)).await;
        aged_session(&store, "ses_fresh", days_ago(2)).await;

        let gone = gc().run(store.root()).expect("collect");
        assert_eq!(removed(&gone), vec!["ses_old"]);
        assert_eq!(kept(store.root()), vec!["ses_fresh"]);
    }

    #[tokio::test]
    async fn the_oldest_beyond_the_keep_count_go() {
        let (_root, store) = root_with();
        for n in 0..5 {
            aged_session(&store, &format!("ses_{n}"), hours_ago(n)).await;
        }
        let gc = Gc {
            keep_sessions: 3,
            ..gc()
        };
        let gone = gc.collect(store.root()).expect("collect");
        assert_eq!(
            removed(&gone),
            vec!["ses_3", "ses_4"],
            "the two least recently updated"
        );
        assert_eq!(kept(store.root()), vec!["ses_0", "ses_1", "ses_2"]);
    }

    #[tokio::test]
    async fn an_open_session_is_never_collected() {
        let (_root, store) = root_with();
        aged_session(&store, "ses_open", days_ago(90)).await;
        aged_session(&store, "ses_shut", days_ago(90)).await;
        store
            .acquire(&SessionId::from_raw("ses_open"))
            .await
            .expect("acquire");

        let gone = gc().collect(store.root()).expect("collect");
        assert_eq!(removed(&gone), vec!["ses_shut"]);
        assert_eq!(kept(store.root()), vec!["ses_open"]);
    }

    #[tokio::test]
    async fn a_second_run_within_a_day_does_nothing() {
        let (_root, store) = root_with();
        aged_session(&store, "ses_first", days_ago(60)).await;
        assert_eq!(gc().run(store.root()).expect("first run").len(), 1);

        aged_session(&store, "ses_second", days_ago(60)).await;
        assert!(!gc().due(store.root()), "the stamp is fresh");
        assert!(gc().run(store.root()).expect("second run").is_empty());
        assert_eq!(kept(store.root()), vec!["ses_second"]);

        let tomorrow =
            Gc::daily(Timestamp::from_second(now().as_second() + DAY).expect("a day on"));
        assert!(tomorrow.due(store.root()));
        assert_eq!(tomorrow.run(store.root()).expect("a day later").len(), 1);
    }
}
