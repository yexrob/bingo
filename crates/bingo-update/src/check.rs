//! The one question a start asks: is there a newer release than this build?
//!
//! It never fails outwards. Every way this can go wrong — no network, an
//! address that is rate limited, an answer that is not a release, a data
//! directory that cannot be written — is `None` and a line in the log, so a
//! run that cannot ask starts exactly as fast as one that has nothing to say.
//!
//! The fetch is handed in rather than made here (ADR-0043 §2): this crate
//! reaches no network of its own, which is what keeps it — and the rename
//! dance it carries — under a Windows compiler on any machine.

use std::future::Future;
use std::path::Path;

use jiff::Timestamp;

use crate::stamp::Stamp;
use crate::{api, release, stamp, version};

/// The newer version worth telling a person about, or nothing at all.
///
/// `ask` is given the address and answers with the body or with why not.
pub async fn check<F, Fut>(current: &str, data_dir: &Path, now: Timestamp, ask: F) -> Option<String>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let stamp = stamp::read(data_dir);
    let heard = stamp.as_ref().and_then(|stamp| stamp.latest.clone());
    if !stamp::due(stamp.as_ref(), now, stamp::EVERY) {
        return worth_saying(heard, current);
    }
    // Before the request, not after it: see `stamp`.
    remember(data_dir, now, heard.clone());
    let told = told(ask(api::latest_url()).await);
    if told.is_some() {
        remember(data_dir, now, told.clone());
    }
    worth_saying(told.or(heard), current)
}

/// What the answer says the newest release is; a failure is one debug line.
fn told(answer: Result<String, String>) -> Option<String> {
    let json = match answer {
        Ok(json) => json,
        Err(why) => {
            tracing::debug!("update check: {why}");
            return None;
        }
    };
    match release::latest(&json) {
        Ok(release) => Some(release.version),
        Err(e) => {
            tracing::debug!("update check: {e}");
            None
        }
    }
}

/// A version is only worth saying when it is ahead of this build.
fn worth_saying(latest: Option<String>, current: &str) -> Option<String> {
    latest.filter(|latest| version::newer(current, latest))
}

fn remember(data_dir: &Path, now: Timestamp, latest: Option<String>) {
    if let Err(e) = stamp::write(
        data_dir,
        &Stamp {
            checked_at: now,
            latest,
        },
    ) {
        tracing::debug!("update check: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a timestamp")
    }

    /// A release answer naming one version.
    fn answering(version: &str) -> String {
        format!(r#"{{"tag_name":"v{version}","assets":[]}}"#)
    }

    /// The check with a fetch that is counted, so "it did not ask" is a fact
    /// and not a guess.
    async fn checked(
        dir: &Path,
        current: &str,
        now: Timestamp,
        answer: Result<String, String>,
        asked: &Cell<usize>,
    ) -> Option<String> {
        check(current, dir, now, |_url| {
            asked.set(asked.get() + 1);
            std::future::ready(answer)
        })
        .await
    }

    #[tokio::test]
    async fn a_first_run_asks_and_says_what_it_was_told() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        let found = checked(
            dir.path(),
            "0.4.2",
            at(1_000_000),
            Ok(answering("0.5.0")),
            &asked,
        )
        .await;
        assert_eq!(found.as_deref(), Some("0.5.0"));
        assert_eq!(asked.get(), 1);
        let stamp = stamp::read(dir.path()).expect("a stamp");
        assert_eq!(stamp.checked_at, at(1_000_000));
        assert_eq!(stamp.latest.as_deref(), Some("0.5.0"));
    }

    #[tokio::test]
    async fn a_stamp_younger_than_a_day_is_answered_without_asking() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        checked(
            dir.path(),
            "0.4.2",
            at(1_000_000),
            Ok(answering("0.5.0")),
            &asked,
        )
        .await;

        let hours_later = at(1_000_000 + 6 * 60 * 60);
        let found = checked(dir.path(), "0.4.2", hours_later, Err("no".into()), &asked).await;
        assert_eq!(found.as_deref(), Some("0.5.0"), "from the stamp");
        assert_eq!(asked.get(), 1, "the second start asked nobody");
    }

    #[tokio::test]
    async fn a_day_later_it_asks_again() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        checked(dir.path(), "0.4.2", at(0), Ok(answering("0.4.2")), &asked).await;
        let found = checked(
            dir.path(),
            "0.4.2",
            at(stamp::EVERY.as_secs() as i64),
            Ok(answering("0.5.0")),
            &asked,
        )
        .await;
        assert_eq!(found.as_deref(), Some("0.5.0"));
        assert_eq!(asked.get(), 2);
    }

    #[tokio::test]
    async fn a_failed_ask_says_nothing_new_and_still_waits_a_day() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        let found = checked(
            dir.path(),
            "0.4.2",
            at(1_000_000),
            Err("connection refused".into()),
            &asked,
        )
        .await;
        assert_eq!(found, None);
        let stamp = stamp::read(dir.path()).expect("the stamp was written anyway");
        assert_eq!(stamp.checked_at, at(1_000_000), "the day starts now");
        assert_eq!(stamp.latest, None);

        // And the next start, minutes later, does not ask again.
        checked(
            dir.path(),
            "0.4.2",
            at(1_000_060),
            Ok(answering("0.5.0")),
            &asked,
        )
        .await;
        assert_eq!(asked.get(), 1);
    }

    #[tokio::test]
    async fn a_failed_ask_leaves_what_was_heard_before_where_it_was() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        checked(dir.path(), "0.4.2", at(0), Ok(answering("0.5.0")), &asked).await;

        let day = stamp::EVERY.as_secs() as i64;
        let found = checked(
            dir.path(),
            "0.4.2",
            at(day),
            Err("timed out".into()),
            &asked,
        )
        .await;
        assert_eq!(found.as_deref(), Some("0.5.0"), "it is still out");
        assert_eq!(
            stamp::read(dir.path()).and_then(|s| s.latest).as_deref(),
            Some("0.5.0"),
            "a failure never forgets what was heard"
        );
    }

    #[tokio::test]
    async fn the_newest_release_being_this_build_says_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        let found = checked(dir.path(), "0.5.0", at(0), Ok(answering("0.5.0")), &asked).await;
        assert_eq!(found, None);
        assert_eq!(
            stamp::read(dir.path()).and_then(|s| s.latest).as_deref(),
            Some("0.5.0"),
            "what was heard is kept whether or not it is news"
        );
    }

    #[tokio::test]
    async fn an_answer_that_is_not_a_release_is_silence() {
        let dir = tempfile::tempdir().expect("a directory");
        let asked = Cell::new(0);
        let rate_limited = r#"{"message":"API rate limit exceeded"}"#.to_string();
        let found = checked(dir.path(), "0.4.2", at(0), Ok(rate_limited), &asked).await;
        assert_eq!(found, None);
        assert_eq!(stamp::read(dir.path()).and_then(|s| s.latest), None);
    }

    #[tokio::test]
    async fn a_data_directory_that_cannot_be_written_costs_the_answer_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let blocked = dir.path().join("file").join("data");
        std::fs::write(dir.path().join("file"), b"not a directory").expect("a file");
        let asked = Cell::new(0);
        let found = checked(&blocked, "0.4.2", at(0), Ok(answering("0.5.0")), &asked).await;
        assert_eq!(found.as_deref(), Some("0.5.0"));
    }
}
