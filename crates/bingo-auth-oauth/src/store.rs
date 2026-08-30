//! `<data_dir>/auth.json` (ADR-0012 §2): one entry per provider id.
//!
//! Separate from settings on purpose — the settings project layer is
//! committed, and a token must never be committable. The shape is opencode's,
//! so a credential minted by either tool reads in the other; the path is
//! ours, and neither reads the other's file.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AuthError;

const AUTH_FILE: &str = "auth.json";

/// One provider's credential. An OAuth entry is refreshed in place; an API
/// entry is a key minted elsewhere and used as the bearer as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Entry {
    #[serde(rename = "oauth")]
    OAuth {
        access: String,
        refresh: String,
        /// Unix seconds; `0` when the issuer said nothing.
        expires: i64,
        #[serde(rename = "accountId", default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    #[serde(rename = "api")]
    Api { key: String },
}

/// Read-modify-write over the whole file. bingo is one process per user, so
/// an in-process lock is the only lock; the rename is what makes a concurrent
/// reader see either the old file or the new one and never half of either.
#[derive(Debug)]
pub struct CredentialStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl CredentialStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join(AUTH_FILE),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self, provider: &str) -> Result<Option<Entry>, AuthError> {
        let _guard = self.locked();
        Ok(self.load()?.remove(provider))
    }

    pub fn write(&self, provider: &str, entry: Entry) -> Result<(), AuthError> {
        let _guard = self.locked();
        let mut entries = self.load()?;
        entries.insert(provider.to_string(), entry);
        self.save(&entries)
    }

    /// A provider that was never there is already removed.
    pub fn remove(&self, provider: &str) -> Result<(), AuthError> {
        let _guard = self.locked();
        let mut entries = self.load()?;
        match entries.remove(provider) {
            Some(_) => self.save(&entries),
            None => Ok(()),
        }
    }

    /// A poisoned lock still guards a consistent file: every write is a whole
    /// file, so a panic mid-write left nothing half-applied to protect.
    fn locked(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// A missing file is an empty store; a corrupt one is an error, never
    /// silently emptied — emptying it would delete a credential to hide a bug.
    fn load(&self) -> Result<BTreeMap<String, Entry>, AuthError> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Ok(BTreeMap::new());
        };
        serde_json::from_str(&raw).map_err(|e| AuthError::Store(format!("{}: {e}", self.display())))
    }

    fn save(&self, entries: &BTreeMap<String, Entry>) -> Result<(), AuthError> {
        let directory = self
            .path
            .parent()
            .ok_or_else(|| AuthError::Store(format!("{} has no directory", self.display())))?;
        std::fs::create_dir_all(directory).map_err(|e| self.io("create the directory", e))?;
        let temporary = directory.join(format!("{AUTH_FILE}.tmp"));
        self.write_private(&temporary, entries)?;
        std::fs::rename(&temporary, &self.path).map_err(|e| self.io("rename", e))
    }

    /// The mode is set on the empty file, before a byte of it exists: a
    /// credential is never readable by anyone else, not even for an instant.
    fn write_private(
        &self,
        temporary: &Path,
        entries: &BTreeMap<String, Entry>,
    ) -> Result<(), AuthError> {
        let mut file = std::fs::File::create(temporary).map_err(|e| self.io("create", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| self.io("restrict", e))?;
        }
        let json = serde_json::to_string_pretty(entries)
            .map_err(|e| AuthError::Store(format!("encode: {e}")))?;
        file.write_all(json.as_bytes())
            .map_err(|e| self.io("write", e))?;
        file.sync_all().map_err(|e| self.io("flush", e))
    }

    fn io(&self, what: &str, error: std::io::Error) -> AuthError {
        AuthError::Store(format!("{what} {}: {error}", self.display()))
    }

    fn display(&self) -> std::path::Display<'_> {
        self.path.display()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store(directory: &tempfile::TempDir) -> CredentialStore {
        CredentialStore::new(directory.path().join("data"))
    }

    fn oauth() -> Entry {
        Entry::OAuth {
            access: "access-1".into(),
            refresh: "refresh-1".into(),
            expires: 1_786_000_000,
            account_id: Some("acc_1".into()),
        }
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_store() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(store(&directory).read("codex").expect("a read"), None);
    }

    #[test]
    fn an_entry_round_trips_and_its_neighbours_are_untouched() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        store.write("codex", oauth()).expect("a write");
        store
            .write("other", Entry::Api { key: "sk-1".into() })
            .expect("a write");
        assert_eq!(store.read("codex").expect("a read"), Some(oauth()));

        // A second store over the same directory is a later process.
        let reopened = CredentialStore::new(directory.path().join("data"));
        assert_eq!(reopened.read("codex").expect("a read"), Some(oauth()));
        reopened.remove("codex").expect("a removal");
        assert_eq!(reopened.read("codex").expect("a read"), None);
        assert_eq!(
            reopened.read("other").expect("a read"),
            Some(Entry::Api { key: "sk-1".into() })
        );
        reopened.remove("codex").expect("removing twice is a no-op");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_after_the_first_write_and_after_a_rewrite() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        let mode = || {
            std::fs::metadata(store.path())
                .expect("the file")
                .permissions()
                .mode()
                & 0o777
        };
        store.write("codex", oauth()).expect("a write");
        assert_eq!(mode(), 0o600, "0600 after the first write");
        store
            .write("codex", Entry::Api { key: "sk-2".into() })
            .expect("a rewrite");
        assert_eq!(mode(), 0o600, "still 0600 after a rewrite");
    }

    #[test]
    fn a_corrupt_file_is_an_error_rather_than_a_silent_emptying() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        std::fs::create_dir_all(directory.path().join("data")).expect("the directory");
        std::fs::write(store.path(), "{not json").expect("a corrupt file");
        assert!(matches!(store.read("codex"), Err(AuthError::Store(_))));
    }

    /// The shape ADR-0012 §2 names, compared as JSON so a field rename fails
    /// here rather than against a live issuer.
    #[test]
    fn the_serialised_shape_is_the_one_the_adr_names() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        store.write("codex", oauth()).expect("a write");
        store
            .write("openai", Entry::Api { key: "sk-1".into() })
            .expect("a write");
        let raw = std::fs::read_to_string(store.path()).expect("the file");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&raw).expect("json"),
            json!({
                "codex": {
                    "type": "oauth",
                    "access": "access-1",
                    "refresh": "refresh-1",
                    "expires": 1_786_000_000,
                    "accountId": "acc_1",
                },
                "openai": { "type": "api", "key": "sk-1" },
            })
        );
    }

    #[test]
    fn an_account_id_nobody_knows_is_left_out_of_the_file() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        store
            .write(
                "codex",
                Entry::OAuth {
                    access: "a".into(),
                    refresh: "r".into(),
                    expires: 0,
                    account_id: None,
                },
            )
            .expect("a write");
        let raw = std::fs::read_to_string(store.path()).expect("the file");
        assert!(!raw.contains("accountId"), "{raw}");
    }
}
