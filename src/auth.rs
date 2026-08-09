//! Credential store for providers (D33 §6.2): `~/.local/share/bingo/auth.json`,
//! 0600 on Unix, opencode-compatible shape — one entry per provider:
//!
//! ```json
//! {
//!   "codex":    { "type": "oauth", "access": "...", "refresh": "...", "expires": 1786000000, "accountId": "..." },
//!   "deepseek": { "type": "api", "key": "sk-..." }
//! }
//! ```
//!
//! Separated from `settings.json` on purpose: the settings project layer
//! (`.bingo/settings.json`) gets committed — credentials must never live
//! there. Settings describes what a provider is; this store describes who
//! the user is.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable filename inside the bingo data dir.
const AUTH_FILE: &str = "auth.json";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("failed to read auth store: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse auth store: {0}")]
    Parse(#[from] serde_json::Error),
}

/// One provider's credential. OAuth entries carry access/refresh/expiry and
/// survive across sessions; API entries are static keys (moved here by
/// `/provider` tooling when the user opts in — static keys in settings keep
/// working unchanged).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthEntry {
    /// OAuth token set (opencode-compatible field names).
    Oauth {
        access: String,
        refresh: String,
        /// Unix seconds; past = expired (refresh before use).
        expires: i64,
        #[serde(rename = "accountId", default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    /// Static API key.
    Api { key: String },
}

/// auth.json path: `~/.local/share/bingo/auth.json` (same data dir as
/// transcripts/tasks; D33 §6.2 — data dir, never the config dir).
pub fn auth_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("bingo")
        .join(AUTH_FILE)
}

/// Read-modify-write access to the auth store. bingo is single-process:
/// an in-process mutex is the only lock needed (same convention as the
/// transcript file lock, D15).
#[derive(Debug)]
pub struct AuthStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuthStore {
    pub fn new(home: &Path) -> Self {
        Self {
            path: auth_path(home),
            lock: Mutex::new(()),
        }
    }

    /// Path used by this store (surfaced by /provider for user reference;
    /// consumed by the P4 /provider polish).
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Full store contents; a missing file reads as empty, a corrupt file is
    /// an error (never silently dropped).
    pub fn load(&self) -> Result<HashMap<String, AuthEntry>, AuthError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.load_unlocked()
    }

    /// Lock-free variant for callers that already hold the lock (std::sync
    /// Mutex is not re-entrant — `set`/`remove` must not call `load`).
    fn load_unlocked(&self) -> Result<HashMap<String, AuthEntry>, AuthError> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Ok(HashMap::new());
        };
        let store: HashMap<String, AuthEntry> = serde_json::from_str(&raw)?;
        Ok(store)
    }

    /// One provider's credential.
    pub fn get(&self, provider: &str) -> Result<Option<AuthEntry>, AuthError> {
        Ok(self.load()?.remove(provider))
    }

    /// Write one provider's credential (read-modify-write, atomic).
    pub fn set(&self, provider: &str, entry: AuthEntry) -> Result<(), AuthError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut store = self.load_unlocked()?;
        store.insert(provider.to_string(), entry);
        self.save_locked(&store)
    }

    /// Remove one provider's credential (no-op when absent).
    pub fn remove(&self, provider: &str) -> Result<(), AuthError> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut store = self.load_unlocked()?;
        if store.remove(provider).is_some() {
            self.save_locked(&store)
        } else {
            Ok(())
        }
    }

    /// Atomic write: temp file (0600 on Unix) + rename. Caller holds the lock.
    fn save_locked(&self, store: &HashMap<String, AuthEntry>) -> Result<(), AuthError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no parent dir"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{AUTH_FILE}.tmp"));
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(serde_json::to_string_pretty(store)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn oauth(access: &str, refresh: &str, expires: i64) -> AuthEntry {
        AuthEntry::Oauth {
            access: access.into(),
            refresh: refresh.into(),
            expires,
            account_id: Some("acc_1".into()),
        }
    }

    #[test]
    fn missing_file_reads_empty() {
        let store = AuthStore::new(&tmp_home("empty"));
        assert!(
            store.load().unwrap().is_empty(),
            "missing file reads as empty"
        );
        assert_eq!(store.get("codex").unwrap(), None);
    }

    #[test]
    fn set_get_remove_roundtrip() {
        let home = tmp_home("roundtrip");
        let store = AuthStore::new(&home);
        store.set("codex", oauth("a", "r", 1786000000)).unwrap();
        store
            .set("deepseek", AuthEntry::Api { key: "sk-1".into() })
            .unwrap();
        let all = store.load().unwrap();
        assert_eq!(all.len(), 2, "two providers coexist");
        assert_eq!(
            all.get("codex").unwrap(),
            &oauth("a", "r", 1786000000),
            "oauth entries read back intact"
        );
        assert_eq!(
            all.get("deepseek").unwrap(),
            &AuthEntry::Api { key: "sk-1".into() }
        );

        // Reload (simulating a new session): persisted to disk.
        let store2 = AuthStore::new(&home);
        assert_eq!(
            store2.get("codex").unwrap().unwrap(),
            oauth("a", "r", 1786000000)
        );

        store2.remove("codex").unwrap();
        assert!(
            store2.get("codex").unwrap().is_none(),
            "empty after deletion"
        );
        assert!(
            store2.get("deepseek").unwrap().is_some(),
            "other entries are unaffected"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Unix: the file must be 0600 (the baseline for credential storage).
    #[cfg(unix)]
    #[test]
    fn writes_with_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = tmp_home("perm");
        let store = AuthStore::new(&home);
        store.set("codex", oauth("a", "r", 1786000000)).unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth.json must be 0600, got {mode:o}");
        // After an atomic write (overwrite path) the permissions are preserved.
        store.set("codex", oauth("b", "r2", 1786000001)).unwrap();
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "still 0600 after rewrite, got {mode:o}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let home = tmp_home("corrupt");
        let store = AuthStore::new(&home);
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "{not json").unwrap();
        assert!(
            store.load().is_err(),
            "a corrupt file errors rather than silently clearing"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Compatible with opencode's auth.json shape: the same JSON reads both ways.
    #[test]
    fn opencode_compatible_shape() {
        let home = tmp_home("opencode");
        let store = AuthStore::new(&home);
        store
            .set("opencode", oauth("acc", "ref", 1730000000000))
            .unwrap();
        let raw = std::fs::read_to_string(store.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["opencode"]["type"], "oauth",
            "type discriminator field"
        );
        assert_eq!(parsed["opencode"]["access"], "acc");
        assert_eq!(parsed["opencode"]["refresh"], "ref");
        assert_eq!(parsed["opencode"]["expires"].as_i64(), Some(1730000000000));
        assert_eq!(parsed["opencode"]["accountId"], "acc_1");
        let _ = std::fs::remove_dir_all(&home);
    }
}
