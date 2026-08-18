//! The room sidecar: what a session's rooms were, written down.
//!
//! Room history has never survived a restart. The transcript records what the
//! model saw; a room is a log between agents that main only *relays*, so a
//! resumed session came back with its rooms empty and every unread mark gone.
//! Amendment #6 puts both in 1.0, and this is the file they live in.
//!
//! **Append-only, one JSON object per line.** Nothing is ever rewritten: a room
//! is opened, posts are appended, cursors move forward. Replay is a fold over the
//! lines in order, so a truncated last line costs the last fact and nothing else
//! — which is the property that matters for a file a crash can interrupt.
//!
//! ```text
//! {"v":1,"at":1760000000000,"type":"room","room":"build","mode":"free","members":["main","scout"],"frozen":false}
//! {"v":1,"at":1760000000100,"type":"post","room":"build","seq":1,"from":"scout","text":"the suite is green","atUnix":1760000000,"said":true}
//! {"v":1,"at":1760000000200,"type":"member","room":"build","member":"scout","seen":1,"sent":1}
//! {"v":1,"at":1760000005000,"type":"read","room":"build","seq":1}
//! ```
//!
//! What is **not** here: mentions, which are re-derived by replaying the posts
//! through the same rule that opened them, so there is one authority for what a
//! `@` owes; and an agent conversation's history, which is that instance's own
//! transcript and is walked rather than duplicated.
//!
//! The format is the wire's neighbour, not the wire itself, so it uses the
//! domain's own vocabulary (`free`/`serial`, room sequence numbers) rather than
//! the protocol's. A room log written by one version is read by the next: `v` is
//! the record version, and an unknown one is skipped rather than fatal.

use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::ids::{UnixMillis, now_millis};

/// The record version this build writes. A reader skips anything newer.
pub const VERSION: u32 = 1;

/// One line of the sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Record {
    /// A room exists, with this roster and these settings. Written on creation
    /// and again whenever the roster or the settings move, so the last one wins.
    #[serde(rename_all = "camelCase")]
    Room {
        room: String,
        /// `free` or `serial`, the domain's own words.
        mode: String,
        members: Vec<String>,
        frozen: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_limit: Option<u64>,
    },
    /// One entry of a room's log — a post or a roster change, both of which take
    /// a sequence number and both of which a reader sees.
    #[serde(rename_all = "camelCase")]
    Post {
        room: String,
        seq: u64,
        from: String,
        text: String,
        /// Unix *seconds*, the unit a room message has always carried.
        at_unix: u64,
        /// Somebody spoke, rather than joined or left.
        said: bool,
    },
    /// How far one member has read, and how much of its budget it has spent.
    #[serde(rename_all = "camelCase")]
    Member {
        room: String,
        member: String,
        seen: u64,
        sent: u64,
    },
    /// How far the *user* has read, in the room's own sequence. The one durable
    /// unit attention has: an item identifier dies with its epoch.
    #[serde(rename_all = "camelCase")]
    Read { room: String, seq: u64 },
}

/// One line, with its version and the instant it was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Line {
    v: u32,
    at: UnixMillis,
    #[serde(flatten)]
    record: Record,
}

/// Where a session's room sidecar lives.
pub fn path(home: &Path, stem: &str) -> PathBuf {
    // Its own directory rather than beside the transcript: the transcript sweep
    // selects on `*.jsonl`, and a sidecar in that folder would be collected as a
    // session.
    crate::storage::rooms_dir(home).join(format!("{stem}.rooms.jsonl"))
}

/// The sidecar, replayed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Replay {
    /// Rooms in the order they were first written.
    pub rooms: Vec<RoomState>,
}

impl Replay {
    /// How far the user had read each room, in the room's own sequence.
    pub fn read_cursors(&self) -> Vec<(String, u64)> {
        self.rooms
            .iter()
            .filter(|room| room.read > 0)
            .map(|room| (room.name.clone(), room.read))
            .collect()
    }
}

/// One room, as the sidecar remembers it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RoomState {
    pub name: String,
    pub mode: String,
    pub members: Vec<String>,
    pub frozen: bool,
    pub message_limit: Option<u64>,
    /// The log, in sequence order.
    pub log: Vec<Entry>,
    /// Per-member read cursor and spend.
    pub members_seen: Vec<(String, u64, u64)>,
    /// How far the user had read.
    pub read: u64,
}

/// One entry of a replayed room log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub from: String,
    pub text: String,
    pub at_unix: u64,
    pub said: bool,
}

/// Read a sidecar back.
///
/// A line that does not parse is skipped, not fatal: the file is append-only and
/// the only line a crash can damage is the last one. A missing file replays as
/// nothing, which is what a session that never had a room looks like.
pub fn replay(path: &Path) -> Replay {
    let Ok(file) = std::fs::File::open(path) else {
        return Replay::default();
    };
    let mut replay = Replay::default();
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(line) = serde_json::from_str::<Line>(&line) else {
            continue;
        };
        if line.v > VERSION {
            continue;
        }
        apply(&mut replay, line.record);
    }
    replay
}

fn apply(replay: &mut Replay, record: Record) {
    let name = match &record {
        Record::Room { room, .. }
        | Record::Post { room, .. }
        | Record::Member { room, .. }
        | Record::Read { room, .. } => room.clone(),
    };
    if !replay.rooms.iter().any(|held| held.name == name) {
        replay.rooms.push(RoomState {
            name: name.clone(),
            mode: "free".to_string(),
            ..RoomState::default()
        });
    }
    let Some(room) = replay.rooms.iter_mut().find(|held| held.name == name) else {
        return;
    };
    match record {
        Record::Room {
            mode,
            members,
            frozen,
            message_limit,
            ..
        } => {
            room.mode = mode;
            room.members = members;
            room.frozen = frozen;
            room.message_limit = message_limit;
        }
        Record::Post {
            seq,
            from,
            text,
            at_unix,
            said,
            ..
        } => {
            // Append-only means a repeat is a rewrite of the same fact, not a
            // second one: the sequence number is the identity.
            if room.log.iter().any(|entry| entry.seq == seq) {
                return;
            }
            room.log.push(Entry {
                seq,
                from,
                text,
                at_unix,
                said,
            });
        }
        Record::Member {
            member, seen, sent, ..
        } => match room
            .members_seen
            .iter_mut()
            .find(|(held, ..)| held == &member)
        {
            Some(entry) => {
                entry.1 = entry.1.max(seen);
                entry.2 = entry.2.max(sent);
            }
            None => room.members_seen.push((member, seen, sent)),
        },
        Record::Read { seq, .. } => room.read = room.read.max(seq),
    }
}

/// The thread that writes the sidecar.
///
/// The actor never touches the disk: it is the process's one ordering point, and
/// a write that blocked it would stall every conversation. Records are handed
/// over and appended here, in the order they were handed over.
pub struct Writer {
    records: std::sync::mpsc::Sender<Message>,
}

enum Message {
    Append(Vec<Record>),
    /// Answered once everything already sent has reached the file.
    Flush(std::sync::mpsc::Sender<()>),
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("roomlog::Writer")
    }
}

impl Writer {
    /// Start writing to this path, creating it and its directory if need be.
    pub fn open(path: PathBuf) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let (records, inbox) = std::sync::mpsc::channel::<Message>();
        std::thread::Builder::new()
            .name("bingo-roomlog".to_string())
            .spawn(move || {
                let mut file = BufWriter::new(file);
                while let Ok(message) = inbox.recv() {
                    match message {
                        Message::Append(batch) => {
                            for record in batch {
                                let line = Line {
                                    v: VERSION,
                                    at: now_millis(),
                                    record,
                                };
                                if let Ok(text) = serde_json::to_string(&line) {
                                    let _ = writeln!(file, "{text}");
                                }
                            }
                            let _ = file.flush();
                        }
                        Message::Flush(reply) => {
                            let _ = file.flush();
                            let _ = reply.send(());
                        }
                    }
                }
                let _ = file.flush();
            })?;
        Ok(Self { records })
    }

    pub fn append(&self, records: Vec<Record>) {
        if records.is_empty() {
            return;
        }
        let _ = self.records.send(Message::Append(records));
    }

    /// Wait until everything already handed over is on disk.
    pub fn flush(&self) {
        let (reply, done) = std::sync::mpsc::channel();
        if self.records.send(Message::Flush(reply)).is_ok() {
            let _ = done.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path nothing else in this run uses: two tests in the same millisecond
    /// would otherwise share a file and read each other's lines.
    fn temp() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let ordinal = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "bingo-roomlog-{}-{}-{ordinal}",
            std::process::id(),
            now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{error}"));
        dir.join("session.rooms.jsonl")
    }

    /// The golden shape: one line per fact, in the order the facts happened, and
    /// the exact keys a reader of another version will look for.
    #[test]
    fn the_sidecar_is_one_json_object_per_fact() {
        let path = temp();
        let writer = Writer::open(path.clone()).unwrap_or_else(|error| panic!("{error}"));
        writer.append(vec![
            Record::Room {
                room: "build".to_string(),
                mode: "free".to_string(),
                members: vec!["main".to_string(), "scout".to_string()],
                frozen: false,
                message_limit: None,
            },
            Record::Post {
                room: "build".to_string(),
                seq: 1,
                from: "scout".to_string(),
                text: "the suite is green".to_string(),
                at_unix: 1_760_000_000,
                said: true,
            },
            Record::Member {
                room: "build".to_string(),
                member: "scout".to_string(),
                seen: 1,
                sent: 1,
            },
            Record::Read {
                room: "build".to_string(),
                seq: 1,
            },
        ]);
        writer.flush();

        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{error}"));
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}")))
            .collect();
        assert_eq!(lines.len(), 4, "one line per fact");
        for line in &lines {
            assert_eq!(line.get("v").and_then(|v| v.as_u64()), Some(1));
            assert!(line.get("at").and_then(|at| at.as_u64()).is_some());
        }
        assert_eq!(
            lines[0].get("type").and_then(|t| t.as_str()),
            Some("room"),
            "the roster comes before what was said in it"
        );
        assert_eq!(
            lines[1],
            serde_json::json!({
                "v": 1,
                "at": lines[1].get("at").cloned().unwrap_or_default(),
                "type": "post",
                "room": "build",
                "seq": 1,
                "from": "scout",
                "text": "the suite is green",
                "atUnix": 1_760_000_000u64,
                "said": true,
            }),
            "a post's shape is the contract a later version reads"
        );
        assert_eq!(lines[3].get("type").and_then(|t| t.as_str()), Some("read"));

        let replayed = replay(&path);
        match replayed.rooms.as_slice() {
            [room] => {
                assert_eq!(room.name, "build");
                assert_eq!(room.members, vec!["main", "scout"]);
                assert_eq!(room.log.len(), 1);
                assert_eq!(room.log[0].text, "the suite is green");
                assert_eq!(room.members_seen, vec![("scout".to_string(), 1, 1)]);
                assert_eq!(room.read, 1);
            }
            other => panic!("expected one room, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A crash can only damage the last line, and a fold over the rest still
    /// answers. An unknown version is skipped rather than fatal.
    #[test]
    fn a_truncated_or_unknown_line_costs_only_itself() {
        let path = temp();
        let text = concat!(
            r#"{"v":1,"at":1,"type":"room","room":"build","mode":"serial","members":["main"],"frozen":false}"#,
            "\n",
            r#"{"v":1,"at":2,"type":"post","room":"build","seq":1,"from":"main","text":"first","atUnix":7,"said":true}"#,
            "\n",
            r#"{"v":99,"at":3,"type":"post","room":"build","seq":2,"from":"main","text":"from the future","atUnix":8,"said":true}"#,
            "\n",
            r#"{"v":1,"at":4,"type":"post","room":"buil"#,
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|error| panic!("{error}"));
        }
        std::fs::write(&path, text).unwrap_or_else(|error| panic!("{error}"));
        let replayed = replay(&path);
        match replayed.rooms.as_slice() {
            [room] => {
                assert_eq!(room.mode, "serial");
                assert_eq!(room.log.len(), 1, "the good lines still answer");
                assert_eq!(room.log[0].text, "first");
            }
            other => panic!("expected one room, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A sidecar that was never written replays as a session with no rooms,
    /// which is exactly what it is.
    #[test]
    fn a_missing_sidecar_replays_as_nothing() {
        assert_eq!(
            replay(&std::env::temp_dir().join("bingo-no-such-session.rooms.jsonl")),
            Replay::default()
        );
    }
}
