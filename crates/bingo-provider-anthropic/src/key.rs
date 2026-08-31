//! An API key over time: the places one may rest, and the paste that sets it.
//!
//! The order is ADR-0017 §3 — the `auth.json` entry a paste login wrote under
//! this provider's own id, then whatever the environment or the settings held
//! when the plugin registered. `auth.json` comes first because `/login` and
//! `/logout` must mean something in a shell that already exports a key.
//!
//! The variable belongs to the default instance alone: one ambient key must
//! not feed every proxy, so a named instance is built without one.
//!
//! The openai plugin holds the same brick. It is written twice because a
//! plugin may not import a plugin (ADR-0001) and only the library tier could
//! hold it for both; `bingo-auth-oauth` is where it belongs the day an ADR
//! says so.

use std::path::PathBuf;
use std::sync::Arc;

use bingo_auth_oauth::{CredentialStore, Entry};
use bingo_sdk::{
    Answer, AnswerSpec, AuthStatus, InteractionKind, LoginFlow, LoginMethod, Prompter,
    ProviderError,
};

/// Where a person may put this provider's key.
#[derive(Clone, Debug)]
pub struct Places {
    /// The variable this provider reads; `None` for a named instance.
    pub variable: Option<&'static str>,
    /// The dotted path of this provider's `apiKey` setting.
    pub setting: String,
    /// The settings file, when the host named one.
    pub file: Option<PathBuf>,
}

/// A key found outside the store, and the place that held it — so a receipt
/// and a hint can name where a key came from rather than guess.
#[derive(Clone, Debug)]
pub struct Configured {
    pub key: String,
    pub place: String,
}

/// One endpoint's key: the store a paste writes to, and what was configured
/// before any paste.
#[derive(Debug)]
pub struct ApiKey {
    provider: String,
    places: Places,
    /// Absent for a provider built with its credential already resolved:
    /// there is nowhere for a paste to go, and `login` says so.
    store: Option<Arc<CredentialStore>>,
    configured: Option<Configured>,
}

impl ApiKey {
    pub fn new(
        provider: impl Into<String>,
        places: Places,
        store: Arc<CredentialStore>,
        configured: Option<Configured>,
    ) -> Self {
        Self {
            provider: provider.into(),
            places,
            store: Some(store),
            configured,
        }
    }

    /// A key already resolved, with no store behind it — what a test or an
    /// embedder builds.
    pub fn detached(provider: impl Into<String>, places: Places, key: Option<String>) -> Self {
        Self {
            provider: provider.into(),
            places,
            store: None,
            configured: key.map(|key| Configured {
                key,
                place: "the endpoint it was built with".into(),
            }),
        }
    }

    /// The bearer for one request, or the line that says where a key goes.
    pub fn bearer(&self) -> Result<String, ProviderError> {
        self.resolve()
            .map_err(|message| ProviderError::Auth { message })
    }

    /// Synchronous, so the kernel's refusal at session open and a `/login` in
    /// the same process read the same file.
    pub fn status(&self) -> AuthStatus {
        match self.resolve() {
            Ok(_) => AuthStatus::Ready,
            Err(hint) => AuthStatus::Missing { hint },
        }
    }

    /// A key is pasted; there is no issuer for a browser or a device to talk
    /// to (ADR-0017 §4).
    pub async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: Option<LoginMethod>,
    ) -> Result<String, ProviderError> {
        match method.unwrap_or(LoginMethod::Paste) {
            LoginMethod::Paste => self.paste(prompter).await,
            other => Err(ProviderError::Unsupported {
                message: format!(
                    "{provider} takes an API key: sign in with `/login {provider} paste`, not {}.",
                    spelling(other),
                    provider = self.provider,
                ),
            }),
        }
    }

    /// The pasted credential goes to `auth.json` (0600) under this provider's
    /// own id — never to a settings file, which a project layer commits.
    async fn paste(&self, prompter: Arc<dyn Prompter>) -> Result<String, ProviderError> {
        let store = self.store()?;
        let answer = prompter
            .ask(
                InteractionKind::Login {
                    provider: self.provider.clone(),
                    flow: LoginFlow::Paste,
                },
                vec![AnswerSpec::Text, AnswerSpec::Cancel],
            )
            .await;
        let Ok(Answer::Text { text }) = answer else {
            return Err(ProviderError::Auth {
                message: "Sign-in cancelled.".into(),
            });
        };
        let key = text.trim().to_string();
        if key.is_empty() {
            return Err(ProviderError::Auth {
                message: "No credential was pasted.".into(),
            });
        }
        store
            .write(&self.provider, Entry::Api { key })
            .map_err(|error| ProviderError::Config {
                message: error.to_string(),
            })?;
        Ok(format!("Signed in to {} with a pasted key.", self.provider))
    }

    /// `/logout` deletes what a paste wrote. A key the settings or the
    /// environment hold is not this command's to delete, so the receipt names
    /// it rather than leave a person wondering why the endpoint still answers.
    pub fn forget(&self) -> Result<String, ProviderError> {
        self.store()?
            .remove(&self.provider)
            .map_err(|error| ProviderError::Config {
                message: error.to_string(),
            })?;
        Ok(match &self.configured {
            Some(configured) => format!(
                "Signed out of {}; {} still applies.",
                self.provider, configured.place
            ),
            None => format!("Signed out of {}.", self.provider),
        })
    }

    /// `auth.json`, then what was configured; the error is what a person does
    /// about it.
    fn resolve(&self) -> Result<String, String> {
        match self.stored() {
            Ok(Some(key)) => Ok(key),
            Ok(None) => self.configured_key().ok_or_else(|| self.hint()),
            // An unreadable store is not a key; a configured one still is,
            // and when there is none the store's error is what to fix.
            Err(unreadable) => self.configured_key().ok_or(unreadable),
        }
    }

    fn stored(&self) -> Result<Option<String>, String> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        match store.read(&self.provider) {
            Ok(Some(Entry::Api { key })) => Ok(Some(key)),
            // A token set under a key provider's name is not a key.
            Ok(_) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn configured_key(&self) -> Option<String> {
        self.configured.as_ref().map(|held| held.key.clone())
    }

    fn store(&self) -> Result<&Arc<CredentialStore>, ProviderError> {
        self.store
            .as_ref()
            .ok_or_else(|| ProviderError::Unsupported {
                message: format!("{} holds no credential store", self.provider),
            })
    }

    /// Every place this provider reads, in the order it reads them.
    fn hint(&self) -> String {
        let mut ways = vec![format!("run `/login {}` to paste a key", self.provider)];
        if let Some(variable) = self.places.variable {
            ways.push(format!("set {variable}"));
        }
        ways.push(match &self.places.file {
            Some(file) => format!("set {} in {}", self.places.setting, file.display()),
            None => format!("configure {} in settings", self.places.setting),
        });
        format!("No {} key: {}.", self.provider, one_of(&ways))
    }
}

/// `a, b, or c` — every way a person has, in one line.
fn one_of(ways: &[String]) -> String {
    match ways.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn spelling(method: LoginMethod) -> &'static str {
    match method {
        LoginMethod::Browser => "browser",
        LoginMethod::Device => "device",
        LoginMethod::Paste => "paste",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(variable: Option<&'static str>) -> Places {
        Places {
            variable,
            setting: "anthropic.apiKey".into(),
            file: Some(PathBuf::from("/home/me/.bingo/settings.json")),
        }
    }

    fn store(directory: &tempfile::TempDir) -> Arc<CredentialStore> {
        Arc::new(CredentialStore::new(directory.path().to_path_buf()))
    }

    fn configured(key: &str, place: &str) -> Option<Configured> {
        Some(Configured {
            key: key.into(),
            place: place.into(),
        })
    }

    /// The order ADR-0017 §3 fixes, read off one provider as the store fills
    /// and empties under it.
    #[test]
    fn the_store_is_read_before_what_was_configured() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        let key = ApiKey::new(
            "anthropic",
            places(Some("ANTHROPIC_API_KEY")),
            store.clone(),
            configured("sk-settings", "ANTHROPIC_API_KEY"),
        );
        assert_eq!(key.bearer().expect("the configured key"), "sk-settings");

        store
            .write(
                "anthropic",
                Entry::Api {
                    key: "sk-pasted".into(),
                },
            )
            .expect("a write");
        assert_eq!(key.bearer().expect("the stored key"), "sk-pasted");

        store.remove("anthropic").expect("a removal");
        assert_eq!(
            key.bearer().expect("the configured key again"),
            "sk-settings"
        );
    }

    #[test]
    fn a_neighbours_entry_is_not_this_providers_key() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        store
            .write("anthropic", Entry::Api { key: "sk-1".into() })
            .expect("a write");
        let instance = ApiKey::new("proxy1", places(None), store, None);
        assert!(matches!(instance.status(), AuthStatus::Missing { .. }));
    }

    #[test]
    fn a_token_set_under_a_key_providers_name_is_not_a_key() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        store
            .write(
                "proxy1",
                Entry::OAuth {
                    access: "at".into(),
                    refresh: "rt".into(),
                    expires: 0,
                    account_id: None,
                },
            )
            .expect("a write");
        let key = ApiKey::new("proxy1", places(None), store, None);
        assert!(matches!(key.status(), AuthStatus::Missing { .. }));
    }

    #[test]
    fn the_hint_names_every_place_this_provider_reads() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let default = ApiKey::new(
            "anthropic",
            places(Some("ANTHROPIC_API_KEY")),
            store(&directory),
            None,
        );
        assert_eq!(
            default.status(),
            AuthStatus::Missing {
                hint: "No anthropic key: run `/login anthropic` to paste a key, set ANTHROPIC_API_KEY, \
                       or set anthropic.apiKey in /home/me/.bingo/settings.json."
                    .into()
            }
        );

        let instance = ApiKey::new(
            "proxy1",
            Places {
                variable: None,
                setting: "anthropic.instances.proxy1.apiKey".into(),
                file: None,
            },
            store(&directory),
            None,
        );
        assert_eq!(
            instance.status(),
            AuthStatus::Missing {
                hint: "No proxy1 key: run `/login proxy1` to paste a key, or configure \
                       anthropic.instances.proxy1.apiKey in settings."
                    .into()
            },
            "a named instance reads no variable"
        );
    }

    #[tokio::test]
    async fn a_pasted_key_lands_in_the_store_and_a_logout_takes_it_out() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        let key = ApiKey::new(
            "proxy1",
            places(None),
            store.clone(),
            configured("sk-settings", "anthropic.instances.proxy1.apiKey"),
        );
        let receipt = key
            .login(Arc::new(Pasting("  sk-pasted  ")), None)
            .await
            .expect("a paste");
        assert_eq!(receipt, "Signed in to proxy1 with a pasted key.");
        assert_eq!(
            store.read("proxy1").expect("a read"),
            Some(Entry::Api {
                key: "sk-pasted".into()
            }),
            "the credential is trimmed and stored under the provider's own id"
        );

        assert_eq!(
            key.forget().expect("a logout"),
            "Signed out of proxy1; anthropic.instances.proxy1.apiKey still applies."
        );
        assert_eq!(store.read("proxy1").expect("a read"), None);
    }

    #[tokio::test]
    async fn an_empty_paste_and_a_refused_dialog_store_nothing() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = store(&directory);
        let key = ApiKey::new("proxy1", places(None), store.clone(), None);
        assert!(matches!(
            key.login(Arc::new(Pasting("   ")), None).await,
            Err(ProviderError::Auth { .. })
        ));
        assert!(matches!(
            key.login(Arc::new(Refusing), None).await,
            Err(ProviderError::Auth { .. })
        ));
        assert_eq!(store.read("proxy1").expect("a read"), None);
    }

    #[tokio::test]
    async fn a_browser_or_a_device_says_paste_is_the_way() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let key = ApiKey::new("proxy1", places(None), store(&directory), None);
        for method in [LoginMethod::Browser, LoginMethod::Device] {
            let error = key
                .login(Arc::new(Refusing), Some(method))
                .await
                .expect_err("unsupported");
            assert!(
                matches!(&error, ProviderError::Unsupported { message }
                    if message.contains("/login proxy1 paste")),
                "{error:?}"
            );
        }
    }

    /// A person who pastes `text`.
    struct Pasting(&'static str);

    #[async_trait::async_trait]
    impl Prompter for Pasting {
        async fn ask(
            &self,
            kind: InteractionKind,
            answers: Vec<AnswerSpec>,
        ) -> Result<Answer, bingo_sdk::KernelError> {
            assert!(matches!(
                kind,
                InteractionKind::Login {
                    flow: LoginFlow::Paste,
                    ..
                }
            ));
            assert_eq!(answers, vec![AnswerSpec::Text, AnswerSpec::Cancel]);
            Ok(Answer::Text {
                text: self.0.into(),
            })
        }
    }

    /// A person who cancels.
    struct Refusing;

    #[async_trait::async_trait]
    impl Prompter for Refusing {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, bingo_sdk::KernelError> {
            Ok(Answer::Cancel)
        }
    }
}
