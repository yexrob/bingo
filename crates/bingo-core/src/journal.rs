//! The in-memory session store: the journal a process keeps for the sessions
//! it created and forgets on exit. It is what the kernel runs on when no store
//! plugin is registered, and what every kernel test runs on.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bingo_sdk::{
    ErrorCode, Frame, KernelError, Seq, SessionFilter, SessionId, SessionStore, SessionSummary,
};

#[derive(Debug, Default)]
pub struct MemoryStore {
    sessions: Mutex<BTreeMap<SessionId, Entry>>,
}

#[derive(Debug)]
struct Entry {
    summary: SessionSummary,
    frames: Vec<Frame>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SessionId, Entry>> {
        // The map holds plain data; a poisoned lock has nothing to protect.
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn missing(session: &SessionId) -> KernelError {
        KernelError::new(ErrorCode::SessionNotFound, format!("no session {session}"))
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn create(&self, summary: &SessionSummary) -> Result<(), KernelError> {
        self.lock().insert(
            summary.id.clone(),
            Entry {
                summary: summary.clone(),
                frames: Vec::new(),
            },
        );
        Ok(())
    }

    async fn append(&self, session: &SessionId, frame: &Frame) -> Result<(), KernelError> {
        let mut sessions = self.lock();
        let entry = sessions
            .get_mut(session)
            .ok_or_else(|| Self::missing(session))?;
        if let bingo_sdk::Event::SessionUpdated { summary } = &frame.event {
            entry.summary = summary.clone();
        }
        entry.frames.push(frame.clone());
        Ok(())
    }

    async fn replay(&self, session: &SessionId, since: Seq) -> Result<Vec<Frame>, KernelError> {
        let sessions = self.lock();
        let entry = sessions
            .get(session)
            .ok_or_else(|| Self::missing(session))?;
        Ok(entry
            .frames
            .iter()
            .filter(|f| f.seq > since)
            .cloned()
            .collect())
    }

    async fn list(&self, filter: &SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        let sessions = self.lock();
        let mut out: Vec<SessionSummary> = sessions
            .values()
            .map(|e| e.summary.clone())
            .filter(|s| {
                filter
                    .cwd
                    .as_ref()
                    .is_none_or(|cwd| cwd.to_string_lossy() == s.cwd)
            })
            .filter(|s| {
                filter
                    .parent
                    .as_ref()
                    .is_none_or(|p| s.parent.as_ref().is_some_and(|l| &l.session == p))
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        self.lock()
            .remove(session)
            .map(|_| ())
            .ok_or_else(|| Self::missing(session))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Event, Usage};
    use jiff::Timestamp;

    fn summary(id: &str, cwd: &str, second: i64) -> SessionSummary {
        let ts = Timestamp::from_second(second).unwrap();
        SessionSummary {
            driver: Default::default(),
            id: SessionId::from_raw(id),
            key: None,
            title: None,
            cwd: cwd.into(),
            parent: None,
            model: None,
            provider: None,
            created_at: ts,
            updated_at: ts,
            usage: Usage::default(),
            busy: false,
        }
    }

    fn frame(session: &str, seq: u64) -> Frame {
        Frame {
            seq: Seq(seq),
            ts: Timestamp::from_second(0).unwrap(),
            session: SessionId::from_raw(session),
            cause: None,
            event: Event::CatalogChanged { kind: "x".into() },
        }
    }

    #[tokio::test]
    async fn replay_returns_frames_after_the_cursor() {
        let store = MemoryStore::new();
        store.create(&summary("ses_a", "/a", 1)).await.unwrap();
        for seq in 1..=3 {
            store
                .append(&SessionId::from_raw("ses_a"), &frame("ses_a", seq))
                .await
                .unwrap();
        }
        let tail = store
            .replay(&SessionId::from_raw("ses_a"), Seq(1))
            .await
            .unwrap();
        assert_eq!(tail.iter().map(|f| f.seq.0).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(
            store
                .append(&SessionId::from_raw("ses_zz"), &frame("ses_zz", 1))
                .await
                .unwrap_err()
                .code,
            ErrorCode::SessionNotFound
        );
    }

    #[tokio::test]
    async fn list_filters_by_cwd_and_orders_newest_first() {
        let store = MemoryStore::new();
        store.create(&summary("ses_a", "/a", 1)).await.unwrap();
        store.create(&summary("ses_b", "/a", 5)).await.unwrap();
        store.create(&summary("ses_c", "/c", 3)).await.unwrap();
        let listed = store
            .list(&SessionFilter {
                cwd: Some("/a".into()),
                parent: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["ses_b", "ses_a"]
        );
        store.delete(&SessionId::from_raw("ses_b")).await.unwrap();
        assert_eq!(
            store.list(&SessionFilter::default()).await.unwrap().len(),
            2
        );
    }
}
