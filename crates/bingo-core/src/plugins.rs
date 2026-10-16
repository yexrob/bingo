//! Which plugins a person may switch off, the key a switch is written under,
//! and the one place one is written (ADR-0057 §1, §3, §6).
//!
//! A plugin is what the kernel loads, so the rule for what may be turned off
//! belongs to the kernel and not to whichever face asks. The registry, the
//! `/plugins` command and the `bingo plugins` subcommand all read it here, so
//! there is one rule, and both writers write the same file the same way.

mod kind;
pub use kind::{ContributionKind, switch_kind};

use std::path::Path;

use bingo_sdk::{Env, PluginManifest, PluginStatus};
use serde_json::{Map, Value};

use crate::host::Registry;
use crate::settings::{self, SettingsError};

/// The kernel key the switches live under: a plugin's name to whether it
/// runs. Absent is on (ADR-0057 §1).
pub const KEY: &str = "enabledPlugins";

pub(crate) fn effective_switches(
    plugins: &[Box<dyn bingo_sdk::Plugin>],
    mut off: std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    for plugin in plugins
        .iter()
        .filter(|plugin| needed(plugin.manifest()).is_some())
    {
        off.remove(plugin.manifest().id);
    }
    off
}

/// What a plugin that ignored its switch is said under (ADR-0057 §3).
pub const NEEDED: &str = "PLUGIN_NEEDED";

/// A binary with no session store remembers nothing it was told, and one with
/// no surface has nothing to be told it in.
const STORE: &str = "it keeps the sessions, and nothing here would remember one";
const SURFACE: &str = "it is a surface, and nothing here would run without one";

/// Why this plugin may not be switched off, or `None` for one that may.
///
/// One function is the rule for all three faces (ADR-0057 §3): the registry
/// leaves such a plugin standing and says so, and the two commands refuse to
/// write a switch that would be ignored. Everything else is the person's —
/// the permission policy and every provider included, because a gate without
/// a policy asks and a run without a provider says so.
pub fn needed(manifest: &PluginManifest) -> Option<&'static str> {
    manifest
        .provides
        .iter()
        .find_map(|provided| match provided.split_once(':') {
            Some(("store", _)) => Some(STORE),
            Some(("surface", _)) => Some(SURFACE),
            _ => None,
        })
}

/// What either face says when the switch a person asked for would be ignored
/// (ADR-0057 §3): one rule, said in one sentence.
pub fn ignored(name: &str, why: &str) -> String {
    format!("`{name}` ignores its switch: {why}")
}

/// Every plugin a person could switch or complete: what this build
/// registered, then what each source found for itself (ADR-0057 §5).
///
/// One listing, because the table `/plugins` draws and the catalogue a
/// surface completes a name from have to name the same plugins
/// (ADR-0008 §6a).
pub async fn listing(registry: &Registry) -> Vec<PluginStatus> {
    let mut listed = registry.plugins.clone();
    for source in &registry.sources.plugins {
        listed.extend(source.plugins().await);
    }
    let wishes = registry
        .switches
        .wishes
        .read()
        .unwrap_or_else(|e| e.into_inner());
    for status in &mut listed {
        let now = if registry.switch_kind(&status.id) == Some(bingo_sdk::plugin::PluginSwitch::Now)
        {
            !registry
                .switches
                .running
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&status.id)
        } else {
            status.state.is_on()
        };
        let wish = !wishes.contains(status.id.strip_prefix("plugins:").unwrap_or(&status.id));
        status.state = derived(&status.state, wish, now);
    }
    listed
}

/// Admission failures and repository approval take precedence over a switch.
pub fn derived(state: &bingo_sdk::PluginState, wish: bool, now: bool) -> bingo_sdk::PluginState {
    use bingo_sdk::{PluginState, plugin::NextStart};
    match state {
        PluginState::On => {}
        PluginState::Off { why } if why == bingo_sdk::SWITCHED_OFF => {}
        _ => return state.clone(),
    }
    if wish != now {
        PluginState::Pending {
            now,
            at: NextStart::NextStart,
        }
    } else if now {
        PluginState::On
    } else {
        PluginState::Off {
            why: bingo_sdk::SWITCHED_OFF.into(),
        }
    }
}

/// The one word a listing writes a plugin's state as, in the terminal and in
/// the headless twin alike.
pub fn state(enabled: bool) -> &'static str {
    match enabled {
        true => "on",
        false => "off",
    }
}

/// Write one plugin's switch into the user settings layer, so the next start
/// opens on it (ADR-0003 §5). Every neighbour in the file stays where it is,
/// and so does every other plugin's switch.
pub fn switch(env: &Env, name: &str, enabled: bool) -> Result<(), SettingsError> {
    let path = settings::user_path(env);
    let mut switches = written(&path)?;
    switches.insert(name.to_string(), Value::Bool(enabled));
    settings::remember(&path, &[(KEY, Value::Object(switches))])
}

pub(crate) enum Switched {
    Applied(bool, bingo_sdk::plugin::PluginSwitch),
    Overridden(bool, String),
}

pub(crate) async fn switch_in(
    host: &std::sync::Arc<crate::host::Host>,
    session: &bingo_sdk::SessionId,
    name: &str,
    enabled: bool,
) -> Result<Switched, bingo_sdk::KernelError> {
    let (host, session, name) = (host.clone(), session.clone(), name.to_owned());
    let asked = bingo_sdk::render::asked();
    tokio::spawn(async move {
        bingo_sdk::render::under_input(asked, switch_owned(&host, &session, &name, enabled)).await
    })
    .await
    .map_err(|error| {
        bingo_sdk::KernelError::new(bingo_sdk::ErrorCode::Internal, error.to_string())
    })?
}

async fn switch_owned(
    host: &crate::host::Host,
    session: &bingo_sdk::SessionId,
    name: &str,
    enabled: bool,
) -> Result<Switched, bingo_sdk::KernelError> {
    let _guard = host.registry().switches.switching.lock().await;
    let target = name;
    let name = name.strip_prefix("plugins:").unwrap_or(name);
    let path = settings::user_path(host.env());
    let failed = |error: SettingsError| {
        bingo_sdk::KernelError::new(bingo_sdk::ErrorCode::Internal, error.to_string())
    };
    let mut switches = written(&path).map_err(failed)?;
    switches.insert(name.into(), Value::Bool(enabled));
    let value = crate::config::set(host, session, KEY, Value::Object(switches)).await?;
    if !value.is_object() {
        return Err(bingo_sdk::KernelError::new(
            bingo_sdk::ErrorCode::InvalidInput,
            "enabledPlugins must be an object",
        ));
    }
    let enabled = value.get(name).and_then(Value::as_bool).ok_or_else(|| {
        bingo_sdk::KernelError::new(
            bingo_sdk::ErrorCode::InvalidInput,
            "plugin switch must be a boolean",
        )
    })?;
    let (merged, source) = effective_wish(host, name, &value).map_err(failed)?;
    settings::remember(&path, &[(KEY, value)]).map_err(failed)?;
    let effective = merged.enabled_plugins.get(name).copied().unwrap_or(true);
    *host
        .registry()
        .switches
        .wishes
        .write()
        .unwrap_or_else(|e| e.into_inner()) = host.effective_switches(merged.switched_off());
    if effective != enabled {
        host.plugins_changed();
        return Ok(Switched::Overridden(effective, source));
    }
    let result = apply(host, target, effective).await;
    host.plugins_changed();
    result.map(|kind| Switched::Applied(effective, kind))
}

fn effective_wish(
    host: &crate::host::Host,
    name: &str,
    proposed: &Value,
) -> Result<(settings::KernelSettings, String), SettingsError> {
    let path = settings::user_path(host.env());
    let mut layers = vec![settings::Layer::new(
        settings::LayerOrigin::User,
        path.display().to_string(),
        settings::preview_document(&path)?,
    )];
    layers[0].value.insert(KEY.into(), proposed.clone());
    for layer in host
        .settings_layers()
        .iter()
        .filter(|layer| !layer.is_user())
    {
        let mut layer = layer.clone();
        let path = Path::new(&layer.source);
        if path.is_absolute() || path.is_file() {
            layer.value = settings::read_layer(settings::LayerOrigin::Explicit, path)?
                .map(|fresh| fresh.value)
                .unwrap_or_default();
        }
        layers.push(layer);
    }
    let source = layers
        .iter()
        .rev()
        .find(|layer| {
            layer
                .value
                .get(KEY)
                .is_some_and(|value| value.is_null() || value.get(name).is_some())
        })
        .map(|layer| layer.source.clone())
        .unwrap_or_default();
    Ok((settings::merge(&layers, &[])?.kernel, source))
}

async fn apply(
    host: &crate::host::Host,
    name: &str,
    enabled: bool,
) -> Result<bingo_sdk::plugin::PluginSwitch, bingo_sdk::KernelError> {
    use bingo_sdk::plugin::PluginSwitch;
    let registry = host.registry();
    if let Some(kind) = registry.switch_kind(name) {
        if kind == PluginSwitch::Now && switch_running(registry, name, enabled) {
            crate::host::plugin_lifecycle::disabled(host, name).await;
        }
        return Ok(kind.clone());
    }
    for source in &registry.sources.plugins {
        if source
            .plugins()
            .await
            .iter()
            .any(|status| status.id == name || status.id == format!("plugins:{name}"))
        {
            let kind = source.switch_kind(name).await;
            if kind == PluginSwitch::Now {
                return source.switch(name, enabled).await;
            }
            return Ok(kind);
        }
    }
    Ok(PluginSwitch::NextStart {
        why: "plugin unavailable".into(),
    })
}

fn switch_running(registry: &Registry, name: &str, enabled: bool) -> bool {
    let mut off = registry
        .switches
        .running
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if enabled {
        off.remove(name);
        false
    } else {
        off.insert(name.into())
    }
}

/// What the user layer already says about the plugins, read to be written
/// back. Only that layer, because only that layer is ever written.
fn written(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    let document = settings::preview_document(path)?;
    match document.get(KEY) {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(switches)) => Ok(switches.clone()),
        Some(_) => Err(SettingsError::Type {
            key: KEY.to_string(),
            layer: path.display().to_string(),
            message: "expected an object of plugin names to booleans".into(),
        }),
    }
}

/// Waiting catalogue IDs include the owning command, so clients need no feature cases.
pub fn refused_line(entry: &bingo_sdk::CatalogEntry) -> Option<String> {
    let name = entry.id.strip_prefix("plugins:")?;
    let state = serde_json::from_value(entry.meta.get("state")?.clone()).ok()?;
    match state {
        bingo_sdk::PluginState::Failed { why } => Some(format!("{name}: {why}")),
        _ => None,
    }
}

pub fn waiting_line(entry: &bingo_sdk::CatalogEntry) -> Option<String> {
    let state: bingo_sdk::PluginState =
        serde_json::from_value(entry.meta.get("state")?.clone()).ok()?;
    let (_, name) = entry.id.split_once(':')?;
    Some(format!("{name}: {}", waiting_reason(&entry.id, &state)?))
}

pub fn waiting_reason(id: &str, state: &bingo_sdk::PluginState) -> Option<String> {
    if !matches!(state, bingo_sdk::PluginState::Waiting { .. }) {
        return None;
    }
    let (command, name) = id.split_once(':')?;
    Some(format!(
        "{}; /{command} approve {name}",
        state.reason().unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn waiting_hints_do_not_invent_a_binary_subcommand() {
        assert_eq!(
            waiting_reason(
                "extension:p",
                &bingo_sdk::PluginState::Waiting { changed: false }
            )
            .unwrap(),
            "waits for your yes; /extension approve p"
        );
    }

    fn manifest(provides: &'static [&'static str]) -> PluginManifest {
        PluginManifest {
            id: "test.plugin",
            version: "0",
            uses: &[],
            provides,
            requires: &[],
            config: None,
        }
    }

    /// What the binary cannot run without keeps its switch; everything else
    /// is the person's to turn off.
    #[test]
    fn a_store_and_a_surface_may_not_be_switched_off_and_the_rest_may() {
        let table: &[(&'static [&'static str], bool)] = &[
            (&["store:jsonl"], true),
            (&["surface:tui", "service:pages"], true),
            (&["tool:Fetch"], false),
            (&["provider:anthropic"], false),
            (&["policy:default"], false),
            (&[], false),
            // A capability whose name merely starts with the word is not one.
            (&["service:storefront"], false),
        ];
        for (provides, protected) in table {
            let manifest = manifest(provides);
            assert_eq!(
                needed(&manifest).is_some(),
                *protected,
                "{provides:?}: {:?}",
                needed(&manifest)
            );
        }
    }

    fn env(home: &Path) -> Env {
        Env::rooted(home)
    }

    #[test]
    fn a_switch_joins_the_user_layer_without_disturbing_it() {
        let home = tempfile::tempdir().expect("a home");
        let env = env(home.path());
        let path = settings::user_path(&env);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::fs::write(&path, "model = \"m\"\n").expect("the settings");

        switch(&env, "bingo.tools.web", false).expect("the switch is written");
        switch(&env, "wordcount", true).expect("and so is the next one");

        let document = settings::read_document(&path).expect("a document");
        assert_eq!(document["model"], json!("m"), "a neighbour is untouched");
        assert_eq!(
            document[KEY],
            json!({ "bingo.tools.web": false, "wordcount": true })
        );

        switch(&env, "bingo.tools.web", true).expect("a switch flips back");
        let document = settings::read_document(&path).expect("a document");
        assert_eq!(
            document[KEY],
            json!({ "bingo.tools.web": true, "wordcount": true })
        );
    }

    /// A file whose key is not an object is refused rather than overwritten:
    /// what a person wrote is not this command's to throw away.
    #[test]
    fn a_key_of_the_wrong_shape_is_refused_and_the_file_is_left_alone() {
        let home = tempfile::tempdir().expect("a home");
        let env = env(home.path());
        let path = settings::user_path(&env);
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        let before = format!("{KEY} = [\"bingo.tools.web\"]\n");
        std::fs::write(&path, &before).expect("the settings");

        let refused = switch(&env, "bingo.tools.web", false).expect_err("not an object");
        assert!(refused.to_string().contains(KEY), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("the file"), before);
    }
}
