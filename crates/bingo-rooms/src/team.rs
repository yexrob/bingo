//! `.bingo/team.json`, and of that file only its `rooms` key. A project says
//! who sits where; the file is shared with the plugins that own its other
//! nouns, and the plugin that owns `roles` owns the file — where it is, and
//! that it is JSON.
//!
//! So nothing here opens it. The key is asked for through the service that
//! plugin registers (ADR-0031): a key string, a method name and JSON, which
//! is the whole of the lane a plugin has onto another plugin's fact. A trait
//! would need one of them to import the other (ADR-0001) or `team` to become
//! a word the kernel knows, and it is neither.
//!
//! What the value *means* is still this plugin's: a roster is deserialized
//! here, from the value as a person wrote it, and refused here when it is not
//! one.

use std::path::{Path, PathBuf};

use bingo_sdk::{HostHandle, KernelError, ServiceError, ServiceHandle};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ear::{self, Listener, Seat};

/// The service the team file answers under, and the method it speaks. A key
/// and a method are the whole of the contract, so they are written down as
/// the two words they are.
pub(crate) const TEAM: &str = "agents.team";
const SECTION: &str = "section";

/// This plugin's key in that file.
const ROOMS: &str = "rooms";

/// One room a project declares: what it is for, who is in it, and which of
/// them hear it otherwise than a bare name does (ADR-0029 §2, ADR-0034 §6). A
/// name in `members` alone is a patient seat at 300 seconds; a `listeners`
/// entry of `{"name": "scout", "patience_s": 0}` is one every post wakes as it
/// lands. A file that says no purpose declares a room without one, as a
/// person's `/room` opens one (ADR-0053 §1).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub listeners: Vec<Listener>,
}

impl Entry {
    /// The roster it declares: its members at the default ear unless its
    /// listeners say otherwise. A patience nobody can hold is refused here, as
    /// at every other door.
    pub fn seats(&self) -> Result<Vec<Seat>, KernelError> {
        ear::seats(&self.members, &self.listeners)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    /// A directory JSON cannot name, so the file could not even be asked
    /// about: a path is bytes on unix and UTF-16 on Windows, and neither is
    /// a JSON string. It is refused here rather than at the `json!` that
    /// would otherwise panic on it.
    #[error("{0} is not a directory name this can ask about")]
    Unnameable(PathBuf),
    #[error("the team file: {0}")]
    Unreadable(#[from] ServiceError),
    #[error("the team file's `{ROOMS}`: {0}")]
    Malformed(#[from] serde_json::Error),
}

/// The rooms declared for a session working in `cwd`, as the plugin that owns
/// the file reads it: the nearest `.bingo/team.json` at or above it, and
/// nothing from the ones further up.
///
/// A build with nobody answering for the team file declares no rooms and says
/// nothing about it — the same fail-soft a missing service always has
/// (ADR-0031 §5). `/room` and `OpenRoom` are unaffected: a declared room is
/// the only thing that file was ever asked about.
pub async fn rooms(host: &HostHandle, cwd: &Path) -> Result<Vec<Entry>, TeamError> {
    let Some(file) = host.service::<ServiceHandle>(TEAM) else {
        return Ok(Vec::new());
    };
    let Some(cwd) = cwd.to_str() else {
        return Err(TeamError::Unnameable(cwd.to_path_buf()));
    };
    let answered = file
        .call(SECTION, json!({ "cwd": cwd, "key": ROOMS }))
        .await?;
    declared(&answered[SECTION])
}

/// The roster a section declares. A project that wrote nothing under `rooms`
/// declares none, which is what the service answers `null` for.
fn declared(section: &Value) -> Result<Vec<Entry>, TeamError> {
    match section {
        Value::Null => Ok(Vec::new()),
        declared => Ok(serde_json::from_value(declared.clone())?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Stub};
    use serde_json::json;

    /// What the service answers for a project that declared these rooms.
    fn section(source: Value) -> Result<Vec<Entry>, TeamError> {
        declared(&source)
    }

    #[test]
    fn a_declared_room_names_its_members() {
        assert_eq!(
            section(json!([{"name": "design", "members": ["reviewer", "scout"]}]))
                .expect("a roster"),
            [Entry {
                name: "design".into(),
                purpose: None,
                members: ["reviewer", "scout"].map(str::to_string).to_vec(),
                listeners: Vec::new(),
            }]
        );
    }

    /// The same door as `/room parent:120` and `OpenRoom`'s listeners, said in
    /// a file: a name alone takes the default ear, and a number asks for its
    /// own — zero for a seat every post wakes.
    #[test]
    fn a_declared_room_says_which_of_them_listen() {
        let declared = section(json!([{
            "name": "design",
            "members": ["scout"],
            "listeners": [{"name": "scout", "patience_s": 0}, "parent",
                          {"name": "reviewer", "patience_s": 120}]
        }]))
        .expect("a roster");
        assert_eq!(
            declared[0].seats().expect("a roster"),
            [
                Seat::live("scout"),
                Seat {
                    name: "parent".into(),
                    ear: crate::ear::Ear::Patient(std::time::Duration::from_secs(300)),
                },
                Seat {
                    name: "reviewer".into(),
                    ear: crate::ear::Ear::Patient(std::time::Duration::from_secs(120)),
                },
            ]
        );
    }

    #[test]
    fn a_patience_nobody_can_hold_is_refused_where_it_is_declared() {
        let declared = section(json!([{
            "name": "design",
            "listeners": [{"name": "parent", "patience_s": 15}]
        }]))
        .expect("a roster");
        let refused = declared[0].seats().expect_err("the dead band");
        assert!(
            refused.message.contains("under thirty seconds of patience"),
            "{refused}"
        );
    }

    /// A file that declares no rooms, and one that is not there at all, come
    /// back from the service the same way and declare none.
    #[test]
    fn nothing_declared_is_no_room() {
        assert!(section(Value::Null).expect("nothing declared").is_empty());
        assert!(section(json!([])).expect("an empty roster").is_empty());
    }

    /// A `rooms` key that is not a roster is this plugin's mistake to name:
    /// the file parsed, and what it said under this plugin's own name did not.
    #[test]
    fn a_rooms_key_that_is_not_a_roster_says_so() {
        let refused = section(json!({"design": ["scout"]})).expect_err("not a roster");
        assert!(matches!(refused, TeamError::Malformed(_)), "{refused}");
        assert!(refused.to_string().contains("rooms"), "{refused}");
    }

    /// A directory whose name is not UTF-8. Both spellings are written here
    /// because both platforms ship: a byte no UTF-8 sequence may start with
    /// on unix, an unpaired surrogate on Windows.
    #[cfg(unix)]
    fn unnameable() -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b"/work/\xff"))
    }

    #[cfg(windows)]
    fn unnameable() -> PathBuf {
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(&[0x005C, 0xD800]))
    }

    /// JSON carries no such name, so the ask is refused in words. Left to the
    /// `json!` it would have gone into, it would have been a panic.
    #[tokio::test]
    async fn a_directory_json_cannot_name_is_refused_rather_than_panicked_on() {
        let file = Stub::declaring(json!([]));
        let host = crate::tests::service_host(TEAM, file.clone());
        let refused = rooms(&host, &unnameable())
            .await
            .expect_err("a directory with no name JSON can carry");
        assert!(matches!(refused, TeamError::Unnameable(_)), "{refused}");
        assert!(file.asked().is_empty(), "and the owner was never asked");
    }

    /// Nobody answering for the team file is not a failure: a build without
    /// the plugin that owns it declares no rooms (ADR-0031 §5).
    #[tokio::test]
    async fn a_host_with_no_team_file_service_declares_no_rooms() {
        let fleet = Fleet::default();
        let declared = rooms(&fleet.handle(), Path::new("/work/project"))
            .await
            .expect("a host that answers for no team file");
        assert!(declared.is_empty());
    }

    /// The contract with `bingo-agents`, from this side: the key, the method
    /// and the params, asserted against a stand-in that records them. The
    /// other side pins the same three (`team::service`), so the two halves
    /// cannot drift apart in silence.
    #[tokio::test]
    async fn the_rooms_key_is_asked_for_by_key_method_and_directory() {
        let file = Stub::declaring(json!([{"name": "design"}]));
        let host = crate::tests::service_host(TEAM, file.clone());
        let declared = rooms(&host, Path::new("/work/project"))
            .await
            .expect("a roster");

        assert_eq!(
            file.asked(),
            [(
                SECTION.to_string(),
                json!({ "cwd": "/work/project", "key": "rooms" })
            )]
        );
        assert_eq!(
            declared,
            [Entry {
                name: "design".into(),
                ..Entry::default()
            }],
            "and the answer is the roster"
        );
    }

    /// A file the owner could not read is a refusal, in its words: this
    /// plugin has none of its own to add, because it never opened the file.
    #[tokio::test]
    async fn a_team_file_the_owner_refused_is_refused_here_in_its_own_words() {
        let host = crate::tests::service_host(TEAM, Stub::refusing("/w/team.json: { not json"));
        let refused = rooms(&host, Path::new("/w"))
            .await
            .expect_err("a file that will not parse");
        assert!(matches!(refused, TeamError::Unreadable(_)), "{refused}");
        assert!(refused.to_string().contains("team.json"), "{refused}");
    }
}
