//! The journal file: a header line, then one durable frame per line in seq
//! order, appended and flushed per frame and never rewritten (ADR-0005).
//!
//! A crash mid-write leaves half a line at the end, which replay drops; an
//! unreadable line anywhere else is corruption and is reported, not skipped.

use std::io::{ErrorKind, Write};
use std::path::Path;

use bingo_sdk::{Frame, KernelError, Seq, SessionId};
use serde::{Deserialize, Serialize};

use crate::layout;
use crate::storage;

pub const FORMAT: &str = "bingo-journal";
pub const VERSION: u32 = 1;

/// Line 1. The version is in the header from the first byte so a reader knows
/// what it is holding before it parses anything else.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    format: String,
    version: u32,
    session: SessionId,
}

/// Start a session's directory and write its header. An existing journal is
/// left alone: version 1 is never rewritten.
pub fn create(dir: &Path, session: &SessionId) -> Result<(), KernelError> {
    std::fs::create_dir_all(dir).map_err(|e| storage(format!("create {}: {e}", dir.display())))?;
    let path = layout::journal(dir);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    match file {
        Ok(mut file) => write_line(&mut file, &header(session)?, &path),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(storage(format!("create {}: {e}", path.display()))),
    }
}

fn header(session: &SessionId) -> Result<String, KernelError> {
    let header = Header {
        format: FORMAT.to_string(),
        version: VERSION,
        session: session.clone(),
    };
    serde_json::to_string(&header).map_err(|e| storage(format!("encode header: {e}")))
}

/// Append one frame. The file is reopened per frame rather than kept open in
/// a map: one small append costs nothing beside the model round-trip that
/// produced the frame, and there is no second place to invalidate on delete.
pub fn append(dir: &Path, frame: &Frame) -> Result<(), KernelError> {
    let path = layout::journal(dir);
    let line = serde_json::to_string(frame).map_err(|e| storage(format!("encode frame: {e}")))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| storage(format!("open {}: {e}", path.display())))?;
    write_line(&mut file, &line, &path)
}

/// One `write_all` per line, so a crash truncates a line rather than
/// interleaving two.
fn write_line(file: &mut std::fs::File, line: &str, path: &Path) -> Result<(), KernelError> {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .map_err(|e| storage(format!("write {}: {e}", path.display())))
}

/// Every frame with `seq > since`, in file order.
pub fn replay(dir: &Path, since: Seq) -> Result<Vec<Frame>, KernelError> {
    let path = layout::journal(dir);
    // Bytes, not a string: a torn last line can end inside a character, and
    // that is a truncated write, not a corrupt journal.
    let bytes =
        std::fs::read(&path).map_err(|e| storage(format!("read {}: {e}", path.display())))?;
    let lines = lines(&bytes);
    check(lines.first(), &path)?;
    frames(&lines, since, &path)
}

/// The lines of the file, without the empty tail a trailing newline leaves.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn check(line: Option<&&[u8]>, path: &Path) -> Result<(), KernelError> {
    let header: Header = line
        .and_then(|line| serde_json::from_slice(line).ok())
        .ok_or_else(|| storage(format!("{} is not a bingo journal", path.display())))?;
    if header.format != FORMAT {
        return Err(storage(format!(
            "{} is not a bingo journal: format {}",
            path.display(),
            header.format
        )));
    }
    if header.version > VERSION {
        return Err(storage(format!(
            "{} was written by a newer bingo (journal version {}, this build reads {VERSION})",
            path.display(),
            header.version
        )));
    }
    Ok(())
}

fn frames(lines: &[&[u8]], since: Seq, path: &Path) -> Result<Vec<Frame>, KernelError> {
    let last = lines.len().saturating_sub(1);
    let mut frames = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(1) {
        match serde_json::from_slice::<Frame>(line) {
            Ok(frame) => {
                if frame.seq > since {
                    frames.push(frame);
                }
            }
            // The tail is what a crash mid-write leaves; replay ends at the
            // last whole frame.
            Err(_) if index == last => break,
            Err(e) => {
                return Err(storage(format!(
                    "{} line {}: {e}",
                    path.display(),
                    index + 1
                )));
            }
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{fixture, frame, session, summary};
    use bingo_sdk::{ErrorCode, Event};

    /// A session directory holding the named fixture as its journal.
    fn planted(name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(layout::journal(dir.path()), fixture(name)).expect("plant the fixture");
        dir
    }

    fn replayed(name: &str, since: Seq) -> Result<Vec<Frame>, KernelError> {
        replay(planted(name).path(), since)
    }

    #[test]
    fn a_clean_journal_replays_every_frame() {
        let frames = replayed("clean.jsonl", Seq::ZERO).expect("a clean journal");
        assert_eq!(
            frames.iter().map(|f| f.seq.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(matches!(frames[0].event, Event::SessionUpdated { .. }));
        assert!(matches!(frames[1].event, Event::SessionClosed { .. }));
    }

    #[test]
    fn since_skips_the_frames_a_client_already_has() {
        let frames = replayed("clean.jsonl", Seq(1)).expect("a clean journal");
        assert_eq!(frames.iter().map(|f| f.seq.0).collect::<Vec<_>>(), vec![2]);
        assert!(
            replayed("clean.jsonl", Seq(2))
                .expect("a clean journal")
                .is_empty()
        );
    }

    #[test]
    fn a_torn_last_line_is_dropped() {
        let frames = replayed("torn.jsonl", Seq::ZERO).expect("a torn tail is not corruption");
        assert_eq!(frames.iter().map(|f| f.seq.0).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_corrupt_middle_line_names_its_line() {
        let err = replayed("corrupt.jsonl", Seq::ZERO).expect_err("corruption is reported");
        assert_eq!(err.code, ErrorCode::Storage);
        assert!(err.message.contains("line 3"), "{err}");
    }

    #[test]
    fn a_newer_version_is_refused() {
        let err = replayed("version2.jsonl", Seq::ZERO).expect_err("version 2 is not readable");
        assert_eq!(err.code, ErrorCode::Storage);
        assert!(err.message.contains("newer bingo"), "{err}");
    }

    #[test]
    fn a_file_without_a_header_is_not_a_journal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let frames = fixture("clean.jsonl");
        let (_header, body) = frames.split_once('\n').expect("the frames");
        std::fs::write(layout::journal(dir.path()), body).expect("write");
        let err = replay(dir.path(), Seq::ZERO).expect_err("no header, no journal");
        assert_eq!(err.code, ErrorCode::Storage);
        assert!(err.message.contains("not a bingo journal"), "{err}");
    }

    #[test]
    fn the_writer_produces_the_clean_fixture() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dir = dir.path().join(session().as_str());
        create(&dir, &session()).expect("create");
        append(
            &dir,
            &frame(1, Event::SessionUpdated { summary: summary() }),
        )
        .expect("append");
        append(
            &dir,
            &frame(
                2,
                Event::SessionClosed {
                    reason: bingo_sdk::CloseReason::Client,
                },
            ),
        )
        .expect("append");
        let written = std::fs::read_to_string(layout::journal(&dir)).expect("read back");
        assert_eq!(written, fixture("clean.jsonl"), "the format is a contract");
    }

    #[test]
    fn a_second_create_keeps_what_the_first_wrote() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dir = dir.path().join(session().as_str());
        create(&dir, &session()).expect("create");
        append(
            &dir,
            &frame(1, Event::SessionUpdated { summary: summary() }),
        )
        .expect("append");
        create(&dir, &session()).expect("create again");
        assert_eq!(replay(&dir, Seq::ZERO).expect("replay").len(), 1);
    }
}
