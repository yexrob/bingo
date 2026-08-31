//! The `plugin.json` a third party writes, and the one substitution it may use.
//!
//! Pure over strings and paths: nothing here touches the disk, so the manifest
//! a test builds and the manifest a directory holds are the same value.

use std::collections::BTreeMap;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The one placeholder a manifest may write, in `command`, in any of `args`
/// and in any `env` value: the directory the manifest itself was read from.
pub const PLUGIN_ROOT: &str = "${PLUGIN_ROOT}";

/// What one directory under `plugins/` declares about itself.
///
/// Unknown keys are kept rather than refused: a manifest written for a later
/// host must still start on this one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// What the plugin calls itself. It must be the name of the directory
    /// holding it: the path is what one layer overrides another by, so two
    /// spellings of the name would be two answers to "which plugin is this".
    pub name: String,
    pub version: String,
    pub entry: Entry,
    /// A JSON Schema for this plugin's own settings — the slice a person
    /// writes under `plugins.<name>`, which reaches the process as
    /// `initialize.config`. It is what a person reads; nothing in this
    /// workspace validates a document against a schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

/// The process to spawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Added to the host's environment, never replacing it.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl Entry {
    /// The same entry with every `${PLUGIN_ROOT}` standing for `root`, which
    /// is how a manifest names the interpreter and the script beside itself
    /// without knowing where the directory was installed.
    pub fn rooted(&self, root: &Path) -> Entry {
        Entry {
            command: expand(&self.command, root),
            args: self.args.iter().map(|arg| expand(arg, root)).collect(),
            env: self
                .env
                .iter()
                .map(|(key, value)| (key.clone(), expand(value, root)))
                .collect(),
        }
    }
}

/// Every occurrence, not the first: a value may name the root twice.
fn expand(raw: &str, root: &Path) -> String {
    raw.replace(PLUGIN_ROOT, &root.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use serde_json::json;

    fn entry(command: &str, args: &[&str], env: &[(&str, &str)]) -> Entry {
        Entry {
            command: command.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: env
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_manifest_is_the_four_keys_the_adr_names() {
        let manifest: Manifest = serde_json::from_value(json!({
            "name": "wordcount",
            "version": "0.1.0",
            "entry": { "command": "python3", "args": ["${PLUGIN_ROOT}/main.py"] }
        }))
        .expect("a manifest");
        assert_eq!(manifest.name, "wordcount");
        assert_eq!(manifest.entry.command, "python3");
        assert!(manifest.entry.env.is_empty());
        assert_eq!(manifest.config, None);
    }

    #[test]
    fn a_key_this_host_does_not_know_does_not_refuse_the_manifest() {
        let manifest: Manifest = serde_json::from_value(json!({
            "name": "later",
            "version": "9.0.0",
            "entry": { "command": "later" },
            "surfaces": ["one a later host will read"]
        }))
        .expect("a manifest from the future still parses");
        assert_eq!(manifest.name, "later");
    }

    #[test]
    fn the_root_reaches_the_command_the_arguments_and_the_environment() {
        let rooted = entry(
            "${PLUGIN_ROOT}/bin/run",
            &["--script", "${PLUGIN_ROOT}/main.py"],
            &[("PLUGIN_HOME", "${PLUGIN_ROOT}")],
        )
        .rooted(Path::new("/plugins/wordcount"));
        assert_eq!(rooted.command, "/plugins/wordcount/bin/run");
        assert_eq!(rooted.args[1], "/plugins/wordcount/main.py");
        assert_eq!(rooted.env["PLUGIN_HOME"], "/plugins/wordcount");
    }

    #[test]
    fn a_value_that_names_the_root_twice_has_both_resolved() {
        let rooted = entry("sh", &["-c", "${PLUGIN_ROOT}/a && ${PLUGIN_ROOT}/b"], &[])
            .rooted(&PathBuf::from("/p"));
        assert_eq!(rooted.args[1], "/p/a && /p/b");
    }

    #[test]
    fn an_entry_that_names_no_root_is_left_as_it_was() {
        let plain = entry("python3", &["main.py"], &[]);
        assert_eq!(plain.rooted(Path::new("/p")), plain);
    }
}
