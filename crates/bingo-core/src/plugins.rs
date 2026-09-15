//! Which plugins a person may switch off, the key a switch is written under,
//! and the one place one is written (ADR-0057 §1, §3, §6).
//!
//! A plugin is what the kernel loads, so the rule for what may be turned off
//! belongs to the kernel and not to whichever face asks. The registry, the
//! `/plugins` command and the `bingo plugins` subcommand all read it here, so
//! there is one rule, and both writers write the same file the same way.

use std::path::Path;

use bingo_sdk::{Env, PluginManifest};
use serde_json::{Map, Value};

use crate::settings::{self, SettingsError};

/// The kernel key the switches live under: a plugin's name to whether it
/// runs. Absent is on (ADR-0057 §1).
pub const KEY: &str = "enabledPlugins";

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

/// What the user layer already says about the plugins, read to be written
/// back. Only that layer, because only that layer is ever written.
fn written(path: &Path) -> Result<Map<String, Value>, SettingsError> {
    let document = settings::read_document(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(provides: &'static [&'static str]) -> PluginManifest {
        PluginManifest {
            id: "test.plugin",
            version: "0",
            sdk: "^0.1",
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
        std::fs::write(&path, json!({ "model": "m" }).to_string()).expect("the settings");

        switch(&env, "bingo.tools.web", false).expect("the switch is written");
        switch(&env, "wordcount", true).expect("and so is the next one");

        let document = settings::read_document(&path).expect("plain JSON");
        assert_eq!(document["model"], json!("m"), "a neighbour is untouched");
        assert_eq!(
            document[KEY],
            json!({ "bingo.tools.web": false, "wordcount": true })
        );

        switch(&env, "bingo.tools.web", true).expect("a switch flips back");
        let document = settings::read_document(&path).expect("plain JSON");
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
        let before = json!({ KEY: ["bingo.tools.web"] }).to_string();
        std::fs::write(&path, &before).expect("the settings");

        let refused = switch(&env, "bingo.tools.web", false).expect_err("not an object");
        assert!(refused.to_string().contains(KEY), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).expect("the file"), before);
    }
}
