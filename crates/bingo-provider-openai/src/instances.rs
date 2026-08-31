//! The settings keys as providers (ADR-0017 §§1–3): the default endpoint of
//! each key, and one more provider for every name under `instances`.
//!
//! A name is an identity — it is what `--provider`, `/model <name>/<model>`
//! and `/login <name>` say, and what keys the credential in `auth.json` — so
//! a collision is refused here, at boot, rather than settled somewhere later.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bingo_auth_oauth::CredentialStore;
use bingo_sdk::{Env, PluginError, Provider};

use crate::key::{ApiKey, Configured, Places};
use crate::settings::{OpenAiEndpoint, Settings};
use crate::variant::Variant;
use crate::{API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL, OpenAiProvider};

/// The ids this build answers to before any instance is read. A plugin sees
/// its own settings only, so a name that collides with *another* plugin's
/// instance is caught one layer up, where the registry refuses the second
/// provider of a name and the boot fails with it.
const BUILT_IN: [&str; 4] = ["anthropic", "codex", "fake", "openai"];

/// Every provider the `openai` and `codex` keys name, defaults first.
pub fn providers(settings: Settings, env: &Env) -> Result<Vec<Arc<dyn Provider>>, PluginError> {
    let store = Arc::new(CredentialStore::new(env.data_dir.clone()));
    let file = env.config_dir.join("settings.json");
    let mut named = BTreeSet::new();
    let mut providers = vec![
        registered(default_openai(settings.openai.endpoint, &file, &store)),
        registered(OpenAiProvider::subscription(
            Variant::Codex.provider_id(),
            settings.codex.endpoint,
            store.clone(),
        )),
    ];
    for (name, endpoint) in settings.openai.instances {
        claim(&mut named, &name)?;
        providers.push(registered(keyed(name, endpoint, &file, &store)));
    }
    for (name, endpoint) in settings.codex.instances {
        claim(&mut named, &name)?;
        providers.push(registered(OpenAiProvider::subscription(
            name,
            endpoint,
            store.clone(),
        )));
    }
    Ok(providers)
}

fn registered(provider: OpenAiProvider) -> Arc<dyn Provider> {
    Arc::new(provider)
}

/// Where the default `openai` key may be written. `with_endpoint` builds one
/// too, so the hint reads the same wherever the provider came from.
pub(crate) fn default_places(file: Option<PathBuf>) -> Places {
    Places {
        variable: Some(API_KEY_ENV),
        setting: "openai.apiKey".into(),
        file,
    }
}

/// The environment feeds this one and no instance (ADR-0017 §3).
fn default_openai(
    endpoint: OpenAiEndpoint,
    file: &Path,
    store: &Arc<CredentialStore>,
) -> OpenAiProvider {
    let places = default_places(Some(file.to_path_buf()));
    let key = ApiKey::new(
        Variant::Default.provider_id(),
        places.clone(),
        store.clone(),
        configured(&places, endpoint.api_key),
    );
    OpenAiProvider::keyed(
        Variant::Default.provider_id(),
        key,
        first([std::env::var(BASE_URL_ENV).ok(), endpoint.base_url])
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        endpoint.images,
    )
}

/// One more OpenAI-shaped endpoint under its own name. Its key is its own:
/// the store entry under that name, else the instance's `apiKey`.
fn keyed(
    name: String,
    endpoint: OpenAiEndpoint,
    file: &Path,
    store: &Arc<CredentialStore>,
) -> OpenAiProvider {
    let places = Places {
        // No variable: one exported key must not feed every proxy.
        variable: None,
        setting: format!("openai.instances.{name}.apiKey"),
        file: Some(file.to_path_buf()),
    };
    let key = ApiKey::new(
        name.clone(),
        places.clone(),
        store.clone(),
        configured(&places, endpoint.api_key),
    );
    OpenAiProvider::keyed(
        name,
        key,
        blank(endpoint.base_url).unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        endpoint.images,
    )
}

/// The key one endpoint holds before any paste, and the place that held it.
/// The variable wins over the setting, so one shell can point a run at
/// another key without editing a file.
fn configured(places: &Places, from_settings: Option<String>) -> Option<Configured> {
    if let Some(variable) = places.variable
        && let Some(key) = blank(std::env::var(variable).ok())
    {
        return Some(Configured {
            key,
            place: variable.to_string(),
        });
    }
    blank(from_settings).map(|key| Configured {
        key,
        place: places.setting.clone(),
    })
}

/// A name may be claimed once, and never one the build already answers to.
fn claim(named: &mut BTreeSet<String>, name: &str) -> Result<(), PluginError> {
    if name.is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        return Err(PluginError::Config(format!(
            "provider instance `{name}`: a name is one word without `/`, \
             because it is what `--provider` and `/model <name>/<model>` say"
        )));
    }
    if BUILT_IN.contains(&name) {
        return Err(PluginError::Config(format!(
            "provider instance `{name}` collides with the built-in provider of that name"
        )));
    }
    if !named.insert(name.to_string()) {
        return Err(PluginError::Config(format!(
            "provider instance `{name}` is named twice"
        )));
    }
    Ok(())
}

/// The first value set to something other than blanks. A blank counts as
/// unset wherever it is written, so an exported empty variable does not
/// shadow a configured one.
pub(crate) fn first<const N: usize>(values: [Option<String>; N]) -> Option<String> {
    values.into_iter().find_map(blank)
}

fn blank(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::Provider;

    fn store(directory: &tempfile::TempDir) -> Arc<CredentialStore> {
        Arc::new(CredentialStore::new(directory.path().to_path_buf()))
    }

    /// An instance's endpoint is its own, and the public one when it names
    /// none — a proxy that differs only by key is one line of settings.
    #[test]
    fn an_instance_talks_to_its_own_base_url_or_the_public_one() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("settings.json");
        let built = |endpoint| keyed("proxy1".into(), endpoint, &file, &store(&directory));
        assert_eq!(
            built(OpenAiEndpoint {
                base_url: Some("http://127.0.0.1:8080/".into()),
                ..OpenAiEndpoint::default()
            })
            .base_url(),
            "http://127.0.0.1:8080",
            "the trailing slash is not part of an endpoint"
        );
        assert_eq!(
            built(OpenAiEndpoint::default()).base_url(),
            DEFAULT_BASE_URL
        );
        assert_eq!(built(OpenAiEndpoint::default()).id(), "proxy1");
    }

    /// The model catalogue is asked for a provider's *family* (ADR-0017): an
    /// instance's models are its wire shape's, or the dropdown shows it empty.
    #[test]
    fn an_instance_serves_its_variants_family() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let file = directory.path().join("settings.json");
        let proxy = keyed(
            "proxy1".into(),
            OpenAiEndpoint::default(),
            &file,
            &store(&directory),
        );
        assert_eq!((proxy.id(), proxy.family()), ("proxy1", "openai"));
        let work = OpenAiProvider::subscription(
            "work",
            crate::settings::CodexEndpoint::default(),
            store(&directory),
        );
        assert_eq!((work.id(), work.family()), ("work", "codex"));
    }

    fn names(taken: &[&str]) -> Result<(), PluginError> {
        let mut named = BTreeSet::new();
        taken.iter().try_for_each(|name| claim(&mut named, name))
    }

    fn refusal(taken: &[&str]) -> String {
        names(taken).expect_err("a refusal").to_string()
    }

    #[test]
    fn a_name_that_is_taken_is_refused_by_name() {
        assert!(names(&["proxy1", "proxy2", "work"]).is_ok());
        assert!(refusal(&["codex"]).contains("`codex`"));
        assert!(refusal(&["openai"]).contains("built-in"));
        assert!(refusal(&["fake"]).contains("`fake`"));
        assert!(refusal(&["anthropic"]).contains("`anthropic`"));
        assert!(refusal(&["proxy1", "proxy1"]).contains("named twice"));
    }

    /// A name reaches `--provider` and `/model <name>/<model>`, both of which
    /// split on the characters this refuses.
    #[test]
    fn a_name_that_could_not_be_typed_is_refused() {
        assert!(refusal(&[""]).contains("one word"));
        assert!(refusal(&["two words"]).contains("one word"));
        assert!(refusal(&["one/two"]).contains("one word"));
    }

    #[test]
    fn a_blank_value_counts_as_unset_wherever_it_is_written() {
        assert_eq!(first([None, None]), None);
        assert_eq!(
            first([Some("  ".into()), Some(" b ".into())]),
            Some("b".into())
        );
        assert_eq!(
            first([Some(" a ".into()), Some("b".into())]),
            Some("a".into())
        );
    }

    /// `std::env::set_var` is unsafe in Rust 2024 and this workspace forbids
    /// `unsafe`, so the environment half of the rule is exercised through the
    /// resolver the providers are built from.
    #[test]
    fn an_instance_has_no_variable_to_read() {
        let places = Places {
            variable: None,
            setting: "openai.instances.proxy1.apiKey".into(),
            file: None,
        };
        let held = configured(&places, Some("sk-instance".into())).expect("the setting");
        assert_eq!(held.key, "sk-instance");
        assert_eq!(held.place, "openai.instances.proxy1.apiKey");
        assert!(configured(&places, Some("   ".into())).is_none());
    }
}
