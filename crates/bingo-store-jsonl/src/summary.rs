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
/// rebuild is the same value and not an older one.
fn rebuild(dir: &Path) -> Result<Option<SessionSummary>, KernelError> {
    let latest = journal::replay(dir, Seq::ZERO)?
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::SessionUpdated { summary } => Some(summary),
            _ => None,
        })
        .next_back();
    if let Some(summary) = &latest {
        write(dir, summary)?;
    }
    Ok(latest)
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
            item: bingo_sdk::ItemId::from_raw("itm_1"),
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
