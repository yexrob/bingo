//! The one settings key this plugin claims: `acp.adapters`.
//!
//! An adapter is three fields — command, args, env — and its name. A new agent
//! is a new row and no code (ADR-0035 §1), which is why nothing about any
//! particular adapter is written down here: the npm scopes renamed themselves
//! twice in 2026 and a default row would have aged badly.
//!
//! The name is an identity: it is what `--provider` and `/model <name>/<model>`
//! say, so the rules `bingo-provider-anthropic` settles for its instances
//! (ADR-0017 §§1–3) are the rules here — one word, no `/`, never a name the
//! build already answers to.

use std::collections::{BTreeMap, BTreeSet};

use bingo_sdk::PluginError;
use schemars::JsonSchema;
use serde::Deserialize;

/// The provider ids this build answers to before any adapter is read. A name
/// that collides with another plugin's instance is caught one layer up, where
/// the registry refuses the second provider of a name.
const BUILT_IN: [&str; 4] = ["anthropic", "codex", "fake", "openai"];

/// The family every ACP instance files its models under, whatever it is
/// called. `Provider::id` is the adapter's own name because that is what a
/// person types; `Provider::family` is this, because that is the shape it
/// speaks.
pub const FAMILY: &str = "acp";

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub acp: Acp,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Acp {
    /// One provider per row, by the name a person types.
    pub adapters: BTreeMap<String, Adapter>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Adapter {
    /// The program to run: `npx`, `gemini`, `cursor-agent`, a path.
    pub command: String,
    pub args: Vec<String>,
    /// Added to the environment this process already has. An adapter reads its
    /// own credentials from there, so clearing it would take away the login it
    /// depends on.
    pub env: BTreeMap<String, String>,
    /// A row a person keeps but does not want registered today.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// The rows worth registering, each checked for a name a person could type
/// and a command that could be run. A row that is wrong is refused at boot,
/// by name, rather than failing at the first turn.
pub fn adapters(settings: Settings) -> Result<Vec<(String, Adapter)>, PluginError> {
    let mut named = BTreeSet::new();
    let mut rows = Vec::new();
    for (name, adapter) in settings.acp.adapters {
        if !adapter.enabled {
            continue;
        }
        claim(&mut named, &name)?;
        if adapter.command.trim().is_empty() {
            return Err(PluginError::Config(format!(
                "acp.adapters.{name}: `command` is what gets run, and it is empty"
            )));
        }
        rows.push((name, adapter));
    }
    Ok(rows)
}

fn claim(named: &mut BTreeSet<String>, name: &str) -> Result<(), PluginError> {
    if name.is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        return Err(PluginError::Config(format!(
            "acp adapter `{name}`: a name is one word without `/`, \
             because it is what `--provider` and `/model <name>/<model>` say"
        )));
    }
    if BUILT_IN.contains(&name) {
        return Err(PluginError::Config(format!(
            "acp adapter `{name}` collides with the built-in provider of that name"
        )));
    }
    if !named.insert(name.to_string()) {
        return Err(PluginError::Config(format!(
            "acp adapter `{name}` is named twice"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn parse(value: Value) -> Settings {
        serde_json::from_value(value).expect("settings parse")
    }

    fn rows(value: Value) -> Result<Vec<(String, Adapter)>, PluginError> {
        adapters(parse(value))
    }

    fn refusal(value: Value) -> String {
        rows(value).expect_err("a refusal").to_string()
    }

    #[test]
    fn a_person_with_no_acp_key_registers_nothing() {
        assert!(rows(json!({})).expect("no rows").is_empty());
        assert!(
            rows(json!({ "acp": {} })).expect("no rows").is_empty(),
            "and the key alone ships no adapter of its own"
        );
    }

    /// Three fields and a name is the whole of an adapter (ADR-0035 §1).
    #[test]
    fn an_adapter_is_a_command_its_arguments_and_its_environment() {
        let rows = rows(json!({
            "acp": { "adapters": {
                "claude": {
                    "command": "npx",
                    "args": ["-y", "@agentclientprotocol/claude-agent-acp"],
                    "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:8080" }
                }
            }}
        }))
        .expect("one row");
        assert_eq!(rows.len(), 1);
        let (name, adapter) = &rows[0];
        assert_eq!(name, "claude");
        assert_eq!(adapter.command, "npx");
        assert_eq!(adapter.args[1], "@agentclientprotocol/claude-agent-acp");
        assert_eq!(adapter.env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8080");
        assert!(adapter.enabled, "a row is on unless it says otherwise");
    }

    #[test]
    fn a_row_that_is_turned_off_registers_no_provider() {
        let rows = rows(json!({
            "acp": { "adapters": { "gemini": { "command": "gemini", "enabled": false } } }
        }))
        .expect("no rows");
        assert!(rows.is_empty());
    }

    /// A name reaches `--provider` and `/model <name>/<model>`, both of which
    /// split on the characters this refuses.
    #[test]
    fn a_name_that_could_not_be_typed_is_refused() {
        let named = |name: &str| json!({ "acp": { "adapters": { name: { "command": "x" } } } });
        assert!(refusal(named("acp/claude")).contains("one word"));
        assert!(refusal(named("two words")).contains("one word"));
        assert!(refusal(named("openai")).contains("built-in"));
        assert!(
            refusal(named("codex")).contains("built-in"),
            "`codex` is already a provider id; the ACP row is `codex-acp`"
        );
    }

    #[test]
    fn a_row_with_nothing_to_run_is_refused_by_name() {
        let said = refusal(json!({
            "acp": { "adapters": { "broken": { "args": ["-y"] } } }
        }));
        assert!(said.contains("acp.adapters.broken"), "{said}");
        assert!(said.contains("command"), "{said}");
    }

    #[test]
    fn the_schema_describes_the_key_it_claims() {
        let schema = serde_json::to_value(schemars::schema_for!(Settings)).expect("json");
        assert!(schema["properties"]["acp"].is_object(), "{schema}");
    }
}
