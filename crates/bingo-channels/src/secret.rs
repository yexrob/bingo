//! Where a channel's secret comes from (ADR-0020 §8).
//!
//! Until now it was the environment and nothing else. That is right for a
//! bingo started from a shell and impossible for one started at boot: launchd
//! and systemd hand a service no exported variables, and writing the secret
//! into the service file would put it in a file that is neither 0600 nor
//! private — the very thing ADR-0012 §2 moved credentials out of settings to
//! avoid.
//!
//! So there are two sources and a fixed order. The environment still wins, and
//! a shell that exports the variable behaves exactly as it did; `auth.json`
//! answers when the shell has nothing to say. Nothing here ever renders the
//! secret: a [`Source`] is what a person is shown, and it names a variable or
//! a file.

use std::path::PathBuf;

use bingo_auth_oauth::{CredentialStore, Entry};
use bingo_sdk::Env;
use serde_json::Value;

use crate::feishu::Feishu;
use crate::loopback::Loopback;
use crate::settings::{APP_SECRET, Settings};

/// `auth.json`'s key for one adapter's secret. One namespace, so a channel can
/// never collide with a provider's entry.
pub fn credential(id: &str) -> String {
    format!("channels.{id}")
}

/// Where a secret came from. Never the secret itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An exported variable, which wins whenever it is set.
    Environment { variable: String },
    /// `auth.json`, under [`credential`].
    Store { path: PathBuf, key: String },
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Environment { variable } => write!(f, "the environment ({variable})"),
            Source::Store { path, key } => write!(f, "{} ({key})", path.display()),
        }
    }
}

/// A secret and where it was found.
#[derive(Clone, Debug)]
pub struct Found {
    pub source: Source,
    pub secret: String,
}

/// The secret an adapter signs with: the environment, else the store, else
/// nothing at all.
///
/// A variable exported as the empty string counts as nothing: it is what a
/// half-written shell profile leaves behind, and treating it as a secret only
/// turns a missing credential into an authentication error further away.
pub fn find(env: &Env, id: &str, variable: &str) -> Option<Found> {
    let exported = std::env::var(variable).ok().filter(|set| !set.is_empty());
    if let Some(secret) = exported {
        return Some(Found {
            source: Source::Environment {
                variable: variable.to_string(),
            },
            secret,
        });
    }
    let key = credential(id);
    let store = CredentialStore::new(env.data_dir.clone());
    let Some(Entry::Api { key: secret }) = store.read(&key).ok().flatten() else {
        return None;
    };
    Some(Found {
        source: Source::Store {
            path: store.path().to_path_buf(),
            key,
        },
        secret,
    })
}

/// Write one adapter's secret to the store, and say where it went.
pub fn store(env: &Env, id: &str, secret: String) -> Result<PathBuf, String> {
    let store = CredentialStore::new(env.data_dir.clone());
    store
        .write(&credential(id), Entry::Api { key: secret })
        .map_err(|e| e.to_string())?;
    Ok(store.path().to_path_buf())
}

/// One adapter these settings name, and what it signs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirement {
    /// The adapter id — what `bingo channels secret <id>` takes.
    pub id: &'static str,
    /// The variable it reads, where it needs a secret at all.
    pub variable: Option<&'static str>,
}

/// Every adapter this build has, and what each signs with. The one place
/// those two facts live together; everything below is a filter of it.
fn adapters() -> [Requirement; 2] {
    [
        Requirement {
            // A loopback dials a peer on this machine; there is nothing to sign.
            id: Loopback::ID,
            variable: None,
        },
        Requirement {
            id: Feishu::ID,
            variable: Some(APP_SECRET),
        },
    ]
}

/// The adapters a secret can be stored for: what `bingo channels secret`
/// accepts, so a typo is refused rather than written down.
pub fn signing() -> Vec<Requirement> {
    adapters()
        .into_iter()
        .filter(|wanted| wanted.variable.is_some())
        .collect()
}

/// Every adapter a merged settings object asks for, with its credential.
///
/// The bin asks this so `gateway doctor` can name a channel's credential
/// without the bin ever learning how a channel is spelled — the same reason
/// [`crate::wanted`] exists, and now the same answer behind both.
pub fn configured(settings: &Value) -> Vec<Requirement> {
    let Ok(settings) = serde_json::from_value::<Settings>(settings.clone()) else {
        return Vec::new();
    };
    adapters()
        .into_iter()
        .filter(|wanted| match wanted.id {
            Loopback::ID => settings.channels.loopback.is_some(),
            Feishu::ID => settings.channels.feishu.is_some(),
            _ => false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(home: &std::path::Path) -> Env {
        Env::rooted(home)
    }

    #[test]
    fn a_secret_is_the_store_s_when_the_environment_has_none() {
        let home = tempfile::tempdir().expect("a temporary home");
        let env = env(home.path());
        assert!(
            find(&env, "feishu", "BINGO_TEST_SECRET_NEVER_SET").is_none(),
            "neither source has one"
        );

        let path = store(&env, "feishu", "s-from-disk".into()).expect("it is written");
        let found = find(&env, "feishu", "BINGO_TEST_SECRET_NEVER_SET").expect("the store has it");
        assert_eq!(found.secret, "s-from-disk");
        assert_eq!(
            found.source,
            Source::Store {
                path,
                key: "channels.feishu".into()
            }
        );
        assert!(
            found.source.to_string().contains("channels.feishu"),
            "what a person is shown names the key, not the secret: {}",
            found.source
        );
        assert!(!found.source.to_string().contains("s-from-disk"));
    }

    #[test]
    fn the_store_is_keyed_so_a_channel_can_never_collide_with_a_provider() {
        assert_eq!(credential("feishu"), "channels.feishu");
        assert_eq!(credential("loopback"), "channels.loopback");
    }

    #[test]
    fn only_the_channels_the_settings_name_are_asked_for_and_only_feishu_signs() {
        assert!(configured(&json!({})).is_empty());
        assert_eq!(
            configured(&json!({ "channels": { "loopback": {} } })),
            [Requirement {
                id: "loopback",
                variable: None
            }]
        );
        assert_eq!(
            configured(&json!({ "channels": { "feishu": { "appId": "cli_a" } } })),
            [Requirement {
                id: "feishu",
                variable: Some(APP_SECRET)
            }]
        );
        assert_eq!(
            configured(&json!({ "channels": { "loopback": {}, "feishu": {} } })).len(),
            2
        );
        assert!(
            configured(&json!({ "channels": "nonsense" })).is_empty(),
            "settings that will not parse ask for nothing"
        );
    }
}
