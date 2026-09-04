//! One schedule: what to say, where, when, and when it last said it.
//!
//! The id is the file name and is written nowhere inside the file, so
//! renaming the file renames the entry (the experience store's discipline,
//! ADR-0019 §1). Everything else is the record, and the record is the whole
//! of what a fire needs.

use std::path::PathBuf;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::spec::Spec;

/// The first segment of the session key a fire opens (ADR-0019 §3). The
/// kernel mints no keys of its own: the first segment is the plugin's, and
/// this is ours.
const OWNER: &str = "schedule";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// The file's name, not its contents.
    #[serde(skip)]
    pub id: String,
    pub spec: Spec,
    /// A prompt or a `/command` line, delivered as the turn's text.
    pub text: String,
    pub cwd: PathBuf,
    /// `default | acceptEdits | plan | bypassPermissions | dontAsk`; the
    /// configured mode when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    pub enabled: bool,
    pub created: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired: Option<Timestamp>,
}

impl Entry {
    /// The session a fire opens or continues (ADR-0019 §3). One key per
    /// entry, so every one of its turns lands in one transcript.
    pub fn key(&self) -> String {
        format!("{OWNER}/{}", self.id)
    }

    /// What the next fire is reckoned from: the last one, else the day the
    /// entry was written. An `every` counts from here, which is why a
    /// process that was down for three intervals owes one fire and not
    /// three (ADR-0019 §5).
    pub fn anchor(&self) -> Timestamp {
        self.last_fired.unwrap_or(self.created)
    }

    /// When this fires next, in `tz`. `None` when it is disabled, or when
    /// its spec has nothing left to give.
    pub fn next_fire(&self, tz: &TimeZone) -> Option<Zoned> {
        if !self.enabled {
            return None;
        }
        self.spec.next_fire(&self.anchor().to_zoned(tz.clone()))
    }

    /// This entry, fired at `now`: the clock moves, and a `once at` is spent.
    pub fn fired(&self, now: Timestamp) -> Entry {
        Entry {
            last_fired: Some(now),
            enabled: !self.spec.is_once(),
            ..self.clone()
        }
    }

    /// The file, as it is written and as the creation card shows it: one
    /// rendering, so what a person approved is what lands on disk.
    pub fn document(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|json| format!("{json}\n"))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn entry() -> Entry {
        Entry {
            id: "abcd1234".into(),
            spec: "every 30m".parse().expect("a spec"),
            text: "check whether the nightly build is green".into(),
            cwd: PathBuf::from("/work/project"),
            permission_mode: None,
            enabled: true,
            created: Timestamp::UNIX_EPOCH,
            last_fired: None,
        }
    }

    /// The fixture the file format is: what is written, and what is not.
    #[test]
    fn an_entry_is_the_json_a_person_can_hand_edit() {
        let written = Entry {
            permission_mode: Some("acceptEdits".into()),
            last_fired: Some(Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_hours(1)),
            ..entry()
        }
        .document()
        .expect("json");
        assert_eq!(
            written,
            r#"{
  "spec": "every 30m",
  "text": "check whether the nightly build is green",
  "cwd": "/work/project",
  "permissionMode": "acceptEdits",
  "enabled": true,
  "created": "1970-01-01T00:00:00Z",
  "lastFired": "1970-01-01T01:00:00Z"
}
"#
        );
        let read: Entry = serde_json::from_str(&written).expect("it reads back");
        assert_eq!(read.id, "", "the id is the file name, never the file");
        assert_eq!(read.spec, "every 30m".parse().expect("a spec"));
    }

    #[test]
    fn what_was_never_set_is_not_written_and_reads_back_unset() {
        let written = entry().document().expect("json");
        assert!(!written.contains("permissionMode"), "{written}");
        assert!(!written.contains("lastFired"), "{written}");
        let read: Entry = serde_json::from_str(&written).expect("it reads back");
        assert_eq!(read.permission_mode, None);
        assert_eq!(read.last_fired, None);
    }

    /// A wake was a key of this format for one day (ADR-0019 §8, 2026-09-04)
    /// and is the session's own now; a file from that day still reads, and
    /// the key means nothing.
    #[test]
    fn a_file_that_names_a_session_still_reads_and_the_key_means_nothing() {
        let dated = r#"{
  "spec": "once at 1970-01-01T00:10:00Z",
  "text": "look at the build again",
  "cwd": "/work/project",
  "session": "ses_01j0",
  "enabled": false,
  "created": "1970-01-01T00:00:00Z"
}
"#;
        let read: Entry = serde_json::from_str(dated).expect("it still reads");
        assert_eq!(read.text, "look at the build again");
        assert!(
            !read.document().expect("json").contains("session"),
            "and is not written back"
        );
    }

    #[test]
    fn an_entry_that_will_not_parse_is_an_error_and_not_a_default() {
        for wrong in [
            r#"{"spec":"* * * * *","text":"t","cwd":"/","enabled":true,"created":"1970-01-01T00:00:00Z"}"#,
            r#"{"text":"t","cwd":"/","enabled":true,"created":"1970-01-01T00:00:00Z"}"#,
            r#"{"spec":"every 1h","text":"t","cwd":"/","enabled":true}"#,
        ] {
            assert!(serde_json::from_str::<Entry>(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_fire_is_reckoned_from_the_last_one_and_the_first_from_the_day_it_was_written() {
        let tz = TimeZone::UTC;
        let entry = entry();
        assert_eq!(entry.anchor(), Timestamp::UNIX_EPOCH);
        let first = entry.next_fire(&tz).expect("a first fire");
        assert_eq!(first.timestamp().to_string(), "1970-01-01T00:30:00Z");

        let again = entry.fired(first.timestamp());
        assert_eq!(again.anchor(), first.timestamp());
        assert_eq!(
            again.next_fire(&tz).expect("a second fire").timestamp(),
            first.timestamp() + jiff::SignedDuration::from_mins(30),
        );
        assert!(again.enabled, "an interval goes on");
    }

    #[test]
    fn a_once_spends_itself_and_a_disabled_entry_has_no_next_fire() {
        let once = Entry {
            spec: "once at 1970-01-02T00:00:00Z".parse().expect("a spec"),
            ..entry()
        };
        let fire = once.next_fire(&TimeZone::UTC).expect("it is still to come");
        let spent = once.fired(fire.timestamp());
        assert!(!spent.enabled, "a once at disables itself (ADR-0019 §3)");
        assert_eq!(spent.next_fire(&TimeZone::UTC), None);
        assert_eq!(
            Entry {
                enabled: false,
                ..entry()
            }
            .next_fire(&TimeZone::UTC),
            None,
            "a disabled interval is not a schedule"
        );
    }

    #[test]
    fn an_entry_names_one_session_of_its_own() {
        assert_eq!(entry().key(), "schedule/abcd1234");
    }
}
