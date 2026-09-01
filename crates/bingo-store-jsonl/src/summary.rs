//! `summary.json`: the latest `SessionSummary` beside the journal it derives
//! from. It exists so `list` never reads a journal body; deleting every one
//! loses nothing, because a missing one is rebuilt from its journal (ADR-0005).

use std::cmp::Reverse;
use std::path::Path;

use bingo_sdk::{Event, KernelError, Seq, SessionFilter, SessionSummary};

use crate::storage;
use crate::{journal, layout};

/// Write the summary where a crash cannot leave half of it: a temporary file
/// in the same directory, renamed over the old one.
pub fn write(dir: &Path, summary: &SessionSummary) -> Result<(), KernelError> {
    let tmp = layout::summary_tmp(dir);
    let json = serde_json::to_vec(summary).map_err(|e| storage(format!("encode summary: {e}")))?;
    std::fs::write(&tmp, &json).map_err(|e| storage(format!("write {}: {e}", tmp.display())))?;
    let path = layout::summary(dir);
    std::fs::rename(&tmp, &path).map_err(|e| storage(format!("rename {}: {e}", path.display())))
}

/// The session's summary, rebuilt and written back when the file is gone or
/// unreadable. `None` when the journal holds no summary to rebuild from.
pub fn of(dir: &Path) -> Result<Option<SessionSummary>, KernelError> {
    match read(dir) {
        Some(summary) => Ok(Some(summary)),
        None => rebuild(dir),
    }
}

/// A summary that will not parse is treated as a missing one: it is derived,
/// so the journal decides and the repair is silent.
fn read(dir: &Path) -> Option<SessionSummary> {
    let bytes = std::fs::read(layout::summary(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The last `SessionUpdated` in the journal is what the file held, so the
/// rebuild is the same value and not an older one — except for the count,
/// which the file kept fresher than any frame did and which the whole journal
/// therefore has to say again.
fn rebuild(dir: &Path) -> Result<Option<SessionSummary>, KernelError> {
    let frames = journal::replay(dir, Seq::ZERO)?;
    let messages = frames
        .iter()
        .filter(|frame| frame.event.completes_a_message())
        .count();
    let latest = frames
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::SessionUpdated { summary } => Some(summary),
            _ => None,
        })
        .next_back()
        .map(|summary| SessionSummary {
            messages: Some(messages as u64),
            ..summary
        });
    if let Some(summary) = &latest {
        write(dir, summary)?;
    }
    Ok(latest)
}

/// A message landed: the count in the file moves and the journal grows nothing
/// (ADR-0005 §5 — the derived file is where freshness lives). A file that never
/// counted, or is gone, is rebuilt from the journal, which already holds this
/// frame and counts it there; after that the cheap path serves every message.
pub fn count_message(dir: &Path) -> Result<(), KernelError> {
    let counted = read(dir).and_then(|summary| Some((summary.messages?, summary)));
    match counted {
        Some((messages, summary)) => write(
            dir,
            &SessionSummary {
                messages: Some(messages + 1),
                ..summary
            },
        ),
        None => rebuild(dir).map(drop),
    }
}

/// Every session the filter admits, most recently updated first.
pub fn list(root: &Path, filter: &SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
    let mut listed = Vec::new();
    for session in layout::sessions(root)? {
        let Some(mut summary) = of(&session.dir)? else {
            continue;
        };
        // The journal's mtime is when the session was last touched; the copy
        // in the file is only what the last frame happened to carry.
        summary.updated_at = session.updated_at;
        if matches(&summary, filter) {
            listed.push(summary);
        }
    }
    listed.sort_by_key(|summary| Reverse(summary.updated_at));
    if let Some(limit) = filter.limit {
        listed.truncate(limit);
    }
    Ok(listed)
}

fn matches(summary: &SessionSummary, filter: &SessionFilter) -> bool {
    let cwd = filter
        .cwd
        .as_ref()
        .is_none_or(|cwd| cwd.to_string_lossy() == summary.cwd);
    let parent = filter.parent.as_ref().is_none_or(|parent| {
        summary
            .parent
            .as_ref()
            .is_some_and(|link| &link.session == parent)
    });
    cwd && parent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{frame, session, summary};
    use bingo_sdk::{ParentLink, SessionId};

    fn planted() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        journal::create(dir.path(), &session()).expect("create");
        dir
    }

    fn item(id: &str, body: bingo_sdk::ItemBody) -> bingo_sdk::Item {
        bingo_sdk::Item {
            id: bingo_sdk::ItemId::from_raw(id),
            turn: None,
            round: 0,
            status: bingo_sdk::ItemStatus::Completed,
            started_at: crate::tests::stamp(),
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn said(seq: u64, id: &str) -> bingo_sdk::Frame {
        frame(
            seq,
            Event::ItemCompleted {
                item: item(
                    id,
                    bingo_sdk::ItemBody::User {
                        parts: vec![bingo_sdk::ContentPart::text("hi")],
                        origin: bingo_sdk::Origin::surface("test"),
                    },
                ),
            },
        )
    }

    fn answered(seq: u64, id: &str) -> bingo_sdk::Frame {
        frame(
            seq,
            Event::ItemCompleted {
                item: item(id, bingo_sdk::ItemBody::Assistant { text: "ok".into() }),
            },
        )
    }

    /// A tool call is work around a message, not a message.
    fn called(seq: u64, id: &str) -> bingo_sdk::Frame {
        frame(
            seq,
            Event::ItemCompleted {
                item: item(
                    id,
                    bingo_sdk::ItemBody::ToolCall {
                        call_id: "call_1".into(),
                        name: "Read".into(),
                        input: serde_json::Value::Null,
                        output: None,
                        progress: None,
                        duration_ms: None,
                    },
                ),
            },
        )
    }

    /// The three that must agree: the file the appends kept, the file a
    /// rebuild writes, and the journal they both read.
    fn counted(dir: &Path) -> Option<u64> {
        of(dir).expect("a summary").and_then(|s| s.messages)
    }

    #[test]
    fn the_count_in_the_file_moves_with_a_message_and_the_journal_grows_none() {
        let dir = planted();
        write(dir.path(), &summary()).expect("write");
        journal::append(
            dir.path(),
            &frame(1, Event::SessionUpdated { summary: summary() }),
        )
        .expect("append");
        for message in [said(2, "itm_1"), answered(3, "itm_2"), called(4, "itm_3")] {
            journal::append(dir.path(), &message).expect("append");
            if message.event.completes_a_message() {
                count_message(dir.path()).expect("count");
            }
        }
        assert_eq!(counted(dir.path()), Some(2), "an ask and an answer");
        let frames = journal::replay(dir.path(), Seq::ZERO).expect("replay");
        assert_eq!(
            frames
                .iter()
                .filter(|f| matches!(f.event, Event::SessionUpdated { .. }))
                .count(),
            1,
            "freshness cost the journal nothing"
        );
    }

    #[test]
    fn a_rebuilt_summary_recounts_the_whole_journal() {
        let dir = planted();
        write(dir.path(), &summary()).expect("write");
        for message in [
            frame(1, Event::SessionUpdated { summary: summary() }),
            said(2, "itm_1"),
            answered(3, "itm_2"),
            said(4, "itm_3"),
        ] {
            journal::append(dir.path(), &message).expect("append");
        }
        std::fs::remove_file(layout::summary(dir.path())).expect("remove");
        assert_eq!(
            counted(dir.path()),
            Some(3),
            "a torn summary comes back true from the journal"
        );
    }

    /// The M32 migration, and the whole of it: a file that never counted is
    /// not started at one — the journal it derives from says the number.
    #[test]
    fn a_file_that_never_counted_is_rebuilt_rather_than_guessed() {
        let dir = planted();
        let old = SessionSummary {
            messages: None,
            ..summary()
        };
        write(dir.path(), &old).expect("write");
        assert_eq!(
            read(dir.path()).and_then(|s| s.messages),
            None,
            "an old file lies about nothing"
        );
        for message in [
            frame(1, Event::SessionUpdated { summary: old }),
            said(2, "itm_1"),
            answered(3, "itm_2"),
        ] {
            journal::append(dir.path(), &message).expect("append");
        }
        count_message(dir.path()).expect("count");
        assert_eq!(counted(dir.path()), Some(2), "not 1");
    }

    #[test]
    fn a_deleted_summary_is_rebuilt_from_the_journal_unchanged() {
        let dir = planted();
        let mut latest = summary();
        latest.title = Some("the second name".into());
        journal::append(
            dir.path(),
            &frame(1, Event::SessionUpdated { summary: summary() }),
        )
        .expect("append");
        journal::append(
            dir.path(),
            &frame(
                2,
                Event::SessionUpdated {
                    summary: latest.clone(),
                },
            ),
        )
        .expect("append");
        write(dir.path(), &latest).expect("write");

        let before = of(dir.path()).expect("read").expect("a summary");
        std::fs::remove_file(layout::summary(dir.path())).expect("remove");
        let after = of(dir.path()).expect("rebuild").expect("a summary");

        assert_eq!(before, after, "deleting a summary loses nothing");
        assert_eq!(after.title.as_deref(), Some("the second name"));
        assert!(
            layout::summary(dir.path()).is_file(),
            "the rebuilt summary is written back"
        );
    }

    #[test]
    fn an_unreadable_summary_is_rebuilt_rather_than_reported() {
        let dir = planted();
        journal::append(
            dir.path(),
            &frame(1, Event::SessionUpdated { summary: summary() }),
        )
        .expect("append");
        std::fs::write(layout::summary(dir.path()), b"{ not json").expect("corrupt it");
        assert_eq!(of(dir.path()).expect("rebuild"), Some(summary()));
    }

    #[test]
    fn a_journal_without_a_summary_frame_has_none() {
        let dir = planted();
        assert_eq!(of(dir.path()).expect("no summary"), None);
    }

    #[test]
    fn a_filter_admits_by_cwd_and_by_parent() {
        let mut child = summary();
        child.parent = Some(ParentLink {
            session: SessionId::from_raw("ses_parent"),
            item: Some(bingo_sdk::ItemId::from_raw("itm_1")),
        });
        let filter = |cwd: Option<&str>, parent: Option<&str>| SessionFilter {
            cwd: cwd.map(Into::into),
            parent: parent.map(SessionId::from_raw),
            limit: None,
        };
        assert!(matches(&child, &filter(Some("/work"), Some("ses_parent"))));
        assert!(!matches(&child, &filter(Some("/elsewhere"), None)));
        assert!(!matches(&child, &filter(None, Some("ses_other"))));
        assert!(!matches(&summary(), &filter(None, Some("ses_parent"))));
        assert!(matches(&summary(), &SessionFilter::default()));
    }
}
