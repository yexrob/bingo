//! The environment a session's hooks inherit, and the file a `SessionStart` hook
//! writes it in.
//!
//! It lives in memory for as long as the session does and is never written back
//! to settings: what a hook exported is a fact about this run, not a preference.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use bingo_sdk::SessionId;

use crate::env_file;

#[derive(Debug)]
pub struct Sessions {
    dir: PathBuf,
    live: Mutex<HashMap<SessionId, BTreeMap<String, String>>>,
}

impl Sessions {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            live: Mutex::new(HashMap::new()),
        }
    }

    /// The path `BINGO_ENV_FILE` points a `SessionStart` hook at.
    pub fn file(&self, session: &SessionId) -> PathBuf {
        env_file::path(&self.dir, session)
    }

    /// Start this session's `BINGO_ENV_FILE` empty, which is what keeps one run
    /// from inheriting another's exports.
    pub fn open(&self, session: &SessionId) {
        let path = self.file(session);
        let opened = std::fs::create_dir_all(&self.dir).and_then(|()| std::fs::write(&path, ""));
        if let Err(error) = opened {
            tracing::warn!(
                path = %path.display(),
                %error,
                "cannot open BINGO_ENV_FILE for this session"
            );
        }
    }

    /// Take up whatever the file holds now. Called after each `SessionStart` hook,
    /// so a later one both sees the earlier one's exports and may add its own.
    pub fn absorb(&self, session: &SessionId) {
        let path = self.file(session);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let read = env_file::parse(&text);
        if read.is_empty() {
            return;
        }
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.entry(session.clone()).or_default().extend(read);
    }

    /// What every hook in this session runs with, beyond the inherited environment.
    pub fn env(&self, session: &SessionId) -> BTreeMap<String, String> {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.get(session).cloned().unwrap_or_default()
    }

    /// The session is over: forget its exports and remove its file.
    pub fn close(&self, session: &SessionId) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.remove(session);
        drop(live);
        let path = self.file(session);
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    #[cfg(test)]
    fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions() -> (tempfile::TempDir, Sessions) {
        let dir = tempfile::tempdir().expect("temp dir");
        let sessions = Sessions::new(dir.path().join("hooks"));
        (dir, sessions)
    }

    #[test]
    fn what_a_hook_exported_reaches_every_later_hook() {
        let (_dir, sessions) = sessions();
        let session = SessionId::from_raw("ses_01");
        sessions.open(&session);
        let path = sessions.file(&session);
        std::fs::write(&path, "export FOO=bar\n").expect("write");
        sessions.absorb(&session);
        assert_eq!(sessions.env(&session)["FOO"], "bar");
    }

    #[test]
    fn two_hooks_appending_both_survive() {
        let (_dir, sessions) = sessions();
        let session = SessionId::from_raw("ses_01");
        sessions.open(&session);
        let path = sessions.file(&session);
        std::fs::write(&path, "A=1\n").expect("write");
        sessions.absorb(&session);
        std::fs::write(&path, "A=1\nB=2\n").expect("write");
        sessions.absorb(&session);
        let env = sessions.env(&session);
        assert_eq!((env["A"].as_str(), env["B"].as_str()), ("1", "2"));
    }

    #[test]
    fn opening_a_session_starts_from_nothing() {
        let (_dir, sessions) = sessions();
        let session = SessionId::from_raw("ses_01");
        sessions.open(&session);
        let path = sessions.file(&session);
        std::fs::write(&path, "STALE=yes\n").expect("write");
        sessions.open(&session);
        sessions.absorb(&session);
        assert!(sessions.env(&session).is_empty());
    }

    #[test]
    fn closing_forgets_the_environment_and_the_file() {
        let (_dir, sessions) = sessions();
        let session = SessionId::from_raw("ses_01");
        sessions.open(&session);
        let path = sessions.file(&session);
        std::fs::write(&path, "FOO=bar\n").expect("write");
        sessions.absorb(&session);
        sessions.close(&session);
        assert!(sessions.env(&session).is_empty());
        assert!(!path.exists(), "the file outlived the session");
    }

    #[test]
    fn one_session_never_sees_another_s_exports() {
        let (_dir, sessions) = sessions();
        let (one, two) = (SessionId::from_raw("ses_01"), SessionId::from_raw("ses_02"));
        sessions.open(&one);
        sessions.open(&two);
        let path = sessions.file(&one);
        std::fs::write(&path, "FOO=bar\n").expect("write");
        sessions.absorb(&one);
        sessions.absorb(&two);
        assert_eq!(sessions.env(&one)["FOO"], "bar");
        assert!(sessions.env(&two).is_empty());
    }

    #[test]
    fn a_session_nobody_opened_has_no_environment() {
        let (_dir, sessions) = sessions();
        let session = SessionId::from_raw("ses_01");
        sessions.absorb(&session);
        assert!(sessions.env(&session).is_empty());
        assert!(
            !sessions.dir().exists(),
            "an unused session made a directory"
        );
    }
}
