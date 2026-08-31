//! The timer loop (ADR-0019 §3, §5): what fires, when, and on which
//! session.
//!
//! The loop itself owns no clock arithmetic and no policy. It asks
//! [`pass`] — pure — what is due and when to wake, opens or continues each
//! entry's own session, and delivers the text as a turn. Everything a fire
//! produces, including everything that goes wrong inside the turn, lands in
//! that session's transcript, which is the record `--resume` reads.
//!
//! Two rules keep it from spinning. A fire moves the entry's clock *before*
//! the turn is asked for, so a session that cannot be opened is one line in
//! the log and one missed occurrence, not a retry every pass. And a pass
//! that fired something recomputes at once rather than trusting a schedule
//! it has just changed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bingo_sdk::{
    Attachment, CancellationToken, ClientIdentity, Delivery, ErrorCode, HostHandle, Input,
    IntentId, KernelError, OpenOptions, Origin, SessionSelector, SessionSpec,
};
use jiff::Zoned;
use tokio::sync::Notify;

use crate::entry::Entry;
use crate::render;
use crate::store::Store;

/// The surface a scheduled turn comes from; a person's says `tui` or
/// `print`.
pub const SURFACE: &str = "schedule";

/// How long the loop will sleep without looking at the store again. The
/// tools ring the bell when they change it; a person editing a file by hand
/// has nobody to ring it for them, and this is how long they wait.
const RESCAN: Duration = Duration::from_secs(60);

/// What one pass of the loop has to do.
#[derive(Debug, Default, PartialEq)]
pub struct Pass<'a> {
    /// Every entry whose next fire has come or gone: one fire each,
    /// however many occurrences were missed (ADR-0019 §5).
    pub due: Vec<&'a Entry>,
    /// The soonest fire still ahead, of the entries that are not due.
    pub next: Option<Zoned>,
}

/// What is due at `now`, and when to wake for the rest. Pure: the loop's
/// whole decision, testable without a clock or a host.
pub fn pass<'a>(entries: &'a [Entry], now: &Zoned) -> Pass<'a> {
    let mut pass = Pass::default();
    for entry in entries {
        let Some(fire) = entry.next_fire(now.time_zone()) else {
            continue;
        };
        if fire <= *now {
            pass.due.push(entry);
        } else if pass.next.as_ref().is_none_or(|soonest| fire < *soonest) {
            pass.next = Some(fire);
        }
    }
    pass
}

/// How long to sleep for a fire at `next`: never past the rescan, never
/// into the past.
pub fn delay(next: Option<&Zoned>, now: &Zoned) -> Duration {
    let Some(next) = next else { return RESCAN };
    let ahead = next.timestamp().duration_since(now.timestamp());
    match ahead.is_positive() {
        true => ahead.unsigned_abs().min(RESCAN),
        false => Duration::ZERO,
    }
}

/// The one runner this process has, if it took the store's claim.
pub struct Runner {
    store: Arc<Store>,
    host: HostHandle,
    /// Rung by the tools when they write to the store, so a schedule made
    /// now is not waited for.
    changed: Arc<Notify>,
    /// Where a fire that never became a turn is left for a person to find.
    trouble: Arc<Mutex<Option<String>>>,
    cancel: CancellationToken,
}

impl Runner {
    pub fn new(
        store: Arc<Store>,
        host: HostHandle,
        changed: Arc<Notify>,
        trouble: Arc<Mutex<Option<String>>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            store,
            host,
            changed,
            trouble,
            cancel,
        }
    }

    /// Fire what is due, sleep to the next one, and start again — until the
    /// plugin stops.
    pub async fn run(self) {
        loop {
            let waited = self.tick().await;
            tokio::select! {
                _ = self.cancel.cancelled() => return,
                _ = tokio::time::sleep(waited) => {}
                _ = self.changed.notified() => {}
            }
        }
    }

    /// One pass over the store, and how long until the next.
    async fn tick(&self) -> Duration {
        let shelf = self.store.load();
        for bad in &shelf.unreadable {
            tracing::warn!(
                file = bad.file,
                "a schedule that cannot be read: {}",
                bad.why
            );
        }
        let now = Zoned::now();
        let pass = pass(&shelf.entries, &now);
        for entry in &pass.due {
            self.fire(entry, &now).await;
        }
        match pass.due.is_empty() {
            // A pass that changed the store reckons again before it sleeps.
            false => Duration::ZERO,
            true => delay(pass.next.as_ref(), &now),
        }
    }

    /// One occurrence: the clock first, then the turn.
    async fn fire(&self, entry: &Entry, now: &Zoned) {
        if let Err(e) = self.store.save(&entry.fired(now.timestamp())) {
            tracing::warn!(schedule = entry.id, "the fire was not written down: {e}");
        }
        if let Err(e) = self.turn(entry).await {
            let said = format!(
                "{} fired at {} and opened no turn on {}: {}",
                entry.id,
                render::when(Some(now)),
                entry.key(),
                e.message
            );
            tracing::warn!("{said}");
            *self.trouble.lock().unwrap_or_else(|held| held.into_inner()) = Some(said);
        }
    }

    /// The entry's own session, opened or continued, told what to do.
    async fn turn(&self, entry: &Entry) -> Result<(), KernelError> {
        let attachment = self.session(entry).await?;
        self.choose_mode(entry, &attachment);
        self.host
            .deliver(
                &attachment.session,
                IntentId::mint(),
                Input::text(entry.text.clone(), origin()),
                Delivery::Wake,
            )
            .await
    }

    /// The session keyed `schedule/<id>`: the one this entry has been
    /// firing on all along, else a new one at the entry's directory.
    async fn session(&self, entry: &Entry) -> Result<Attachment, KernelError> {
        let selector = SessionSelector::ByKey { key: entry.key() };
        match self
            .host
            .open(selector, who(), OpenOptions::default())
            .await
        {
            Err(e) if e.code == ErrorCode::SessionNotFound => {
                let spec = SessionSelector::Create { spec: spec(entry) };
                self.host.open(spec, who(), OpenOptions::default()).await
            }
            other => other,
        }
    }

    /// The entry's permission mode, asked for the way a person asks for it.
    ///
    /// There is no seam for one at `open`: `SessionSpec` carries no mode,
    /// and the mode a session runs in belongs to the permissions plugin,
    /// which this one may not import (ADR-0001). So the mode is set by
    /// submitting the `/permission` line a person would type, on the
    /// attachment this fire just opened, before the text is delivered. An
    /// unknown mode is refused there and the turn runs in the configured
    /// one — the safe direction, which is why this is a submit and not a
    /// precondition.
    fn choose_mode(&self, entry: &Entry, attachment: &Attachment) {
        let Some(mode) = &entry.permission_mode else {
            return;
        };
        attachment.handle.submit(
            IntentId::mint(),
            Input::text(format!("/permission {mode}"), origin()),
        );
    }
}

fn spec(entry: &Entry) -> SessionSpec {
    SessionSpec {
        cwd: entry.cwd.clone(),
        key: Some(entry.key()),
        title: Some(render::head(&entry.text, 40)),
        ..SessionSpec::default()
    }
}

fn who() -> ClientIdentity {
    ClientIdentity {
        name: SURFACE.into(),
        surface: SURFACE.into(),
    }
}

/// Who a scheduled turn is from. Nobody is at the keyboard, and the
/// transcript says so.
fn origin() -> Origin {
    Origin {
        surface: SURFACE.into(),
        principal: None,
        conversation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    fn at(seconds: i64) -> Zoned {
        Timestamp::from_second(seconds)
            .expect("a timestamp")
            .to_zoned(TimeZone::UTC)
    }

    /// Every entry is written at the epoch and fires every 30 minutes, so
    /// `at(n)` is n seconds after it was created.
    fn shelf(ids: &[&str]) -> Vec<Entry> {
        ids.iter()
            .map(|id| Entry {
                id: (*id).to_string(),
                ..entry()
            })
            .collect()
    }

    #[test]
    fn nothing_is_due_before_its_time_and_the_soonest_is_what_to_wake_for() {
        let entries = shelf(&["aaaa"]);
        let pass = pass(&entries, &at(60));
        assert!(pass.due.is_empty());
        assert_eq!(pass.next, Some(at(1800)));
        assert_eq!(
            delay(pass.next.as_ref(), &at(1795)),
            Duration::from_secs(5),
            "the sleep ends when the fire is due"
        );
    }

    #[test]
    fn an_entry_whose_hour_has_come_is_due_and_one_that_is_late_is_due_once() {
        let entries = shelf(&["aaaa"]);
        for now in [at(1800), at(1801), at(86_400)] {
            let pass = pass(&entries, &now);
            assert_eq!(pass.due.len(), 1, "at {now}");
            assert_eq!(pass.due[0].id, "aaaa");
            assert_eq!(pass.next, None, "a due entry is not also waited for");
        }
    }

    #[test]
    fn a_disabled_entry_is_neither_due_nor_waited_for() {
        let entries = vec![Entry {
            enabled: false,
            ..entry()
        }];
        assert_eq!(pass(&entries, &at(86_400)), Pass::default());
    }

    #[test]
    fn the_soonest_of_many_is_the_one_to_wake_for() {
        let mut entries = shelf(&["aaaa", "bbbb", "cccc"]);
        entries[0].spec = "every 2h".parse().expect("a spec");
        entries[1].spec = "every 45s".parse().expect("a spec");
        entries[2].spec = "every 10m".parse().expect("a spec");
        let pass = pass(&entries, &at(10));
        assert!(pass.due.is_empty());
        assert_eq!(pass.next, Some(at(45)), "the 45 second one");
        assert_eq!(delay(pass.next.as_ref(), &at(10)), Duration::from_secs(35));
    }

    #[test]
    fn a_fire_already_past_is_waited_for_no_time_at_all() {
        assert_eq!(delay(Some(&at(0)), &at(60)), Duration::ZERO);
    }

    #[test]
    fn a_store_with_nothing_ahead_of_it_still_looks_again() {
        assert_eq!(delay(None, &at(0)), RESCAN);
        assert_eq!(
            delay(Some(&at(86_400)), &at(0)),
            RESCAN,
            "a fire tomorrow is not a day asleep: a file may be edited by hand"
        );
    }

    #[test]
    fn a_fire_opens_the_entry_s_own_session_at_the_entry_s_directory() {
        let spec = spec(&entry());
        assert_eq!(spec.key.as_deref(), Some("schedule/abcd1234"));
        assert_eq!(spec.cwd, std::path::PathBuf::from("/work/project"));
        assert_eq!(
            spec.title.as_deref(),
            Some("check whether the nightly build is green"),
            "a session list says what the schedule does"
        );
        assert_eq!(spec.provider, None, "a schedule runs on the host's model");
        assert_eq!(origin().surface, "schedule");
        assert_eq!(origin().principal, None, "nobody is at the keyboard");
    }
}
