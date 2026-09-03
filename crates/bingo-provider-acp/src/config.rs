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
//!
//! Two rows, as a person writes them in `settings.json`:
//!
//! ```json
//! {
//!   "acp": {
//!     "adapters": {
//!       "claude": {
//!         "command": "npx",
//!         "args": ["-y", "@agentclientprotocol/claude-agent-acp",
//!                  "--permission-mode", "acceptEdits"]
//!       },
//!       "codex-acp": {
//!         "command": "npx",
//!         "args": ["-y", "@agentclientprotocol/codex-acp"],
//!         "env": { "CODEX_APPROVAL_POLICY": "on-request" },
//!         "enabled": false
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! `bingo --provider claude --model agent` then runs a turn through it. The
//! model name is bingo's label for the conversation and never crosses: the
//! agent picks its own. Login is the adapter's own (`claude login`, `codex
//! login`), and so is permission: what the agent may do is said in *its*
//! words, on its row — the `--permission-mode` and `CODEX_APPROVAL_POLICY`
//! above — because bingo refuses a `session/request_permission` rather than
//! standing a second gate in front of the agent's own (ADR-0035 §5).

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

/// The label for "whatever model the agent would have used on its own"
/// (ADR-0037 §2). bingo's word and not the agent's: it is always served, is
/// always valid, and never crosses the wire.
pub const AGENT: &str = "agent";

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

/// `Default` is written out below rather than derived: two of these fields are
/// on unless a row says otherwise, and it is what serde fills a missing field
/// from — so what a blank row means is said once, in one place.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
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
    pub enabled: bool,
    /// What this agent is offered over the tool bridge, when the derived set
    /// is not what a person wants (ADR-0036 §6). An explicit list *replaces*
    /// the derivation — including the exclusion, because on their own machine
    /// their word is the last one — and is checked for nothing but existence.
    /// Absent, the offer is the turn's own tool list and syncs itself.
    pub tools: Option<Vec<String>>,
    /// Whether the MCP servers this person configured for bingo ride
    /// `session/new` so the agent dials them itself (ADR-0036 §4). On by
    /// default: one hop instead of two, and their tools leave the bridge so
    /// nothing is served twice. Off keeps the rows — and the credentials in
    /// them — home, and the sourced tools cross the bridge instead, gated and
    /// untrusted as ever.
    pub forward_mcp: bool,
}

/// What a row with nothing but a command means: on, forwarding, and offered
/// whatever the turn is offered.
impl Default for Adapter {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            enabled: true,
            tools: None,
            forward_mcp: true,
        }
    }
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

    /// The two words a row may say about the tool bridge, and what it means
    /// when it says neither (ADR-0036 §§4, 6).
    #[test]
    fn a_row_that_says_nothing_about_the_bridge_forwards_and_derives_its_offer() {
        let rows = rows(json!({
            "acp": { "adapters": { "claude": { "command": "npx" } } }
        }))
        .expect("one row");
        assert!(rows[0].1.forward_mcp, "forwarding is on unless it is off");
        assert_eq!(
            rows[0].1.tools, None,
            "and the offer is the turn's own, not a list"
        );
    }

    #[test]
    fn a_row_may_name_its_own_offer_and_keep_its_servers_home() {
        let rows = rows(json!({
            "acp": { "adapters": { "claude": {
                "command": "npx",
                "tools": ["SendMessage", "TaskCreate"],
                "forwardMcp": false
            } } }
        }))
        .expect("one row");
        assert_eq!(
            rows[0].1.tools.as_deref(),
            Some(["SendMessage".to_string(), "TaskCreate".to_string()].as_slice())
        );
        assert!(!rows[0].1.forward_mcp);
    }

    /// An empty list is a person saying "none", which is not the same as
    /// saying nothing.
    #[test]
    fn an_empty_offer_list_is_a_choice_and_not_an_absent_one() {
        let rows = rows(json!({
            "acp": { "adapters": { "claude": { "command": "npx", "tools": [] } } }
        }))
        .expect("one row");
        assert_eq!(rows[0].1.tools.as_deref(), Some([].as_slice()));
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
