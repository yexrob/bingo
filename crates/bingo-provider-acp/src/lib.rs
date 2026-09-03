//! ACP adapters as model providers (ADR-0035).
//!
//! Every configured adapter — `{command, args, env}` — is one `Provider`
//! instance, and a new agent is a new row of settings rather than a line of
//! code. The message types come from `agent-client-protocol-schema`; the
//! newline-framed JSON-RPC client loop is written here, in tokio, `Send`,
//! because the official SDK's futures are not and a `ModelStream` must be.
//!
//! The pieces, in the order a turn uses them: [`config`] reads the rows,
//! [`child`] spawns one, [`connection`] speaks to it over [`wire`] by the
//! [`method`] table, [`session`] keeps one conversation per bingo session and
//! climbs the [`ladder`] to get back into it, [`events`] turns what comes back
//! into `ModelEvent`s, and [`provider`] is the face the kernel sees.
//!
//! Two of the request's own fields are the agent's to answer for rather than
//! ours: [`options`] reads the effort and model knobs out of what the agent
//! declared, [`knobs`] remembers where they stand and turns what moved before
//! the next prompt, and [`legacy`] is the older door one adapter has instead
//! (ADR-0037). [`probe`] asks an adapter nobody has prompted what it declares,
//! by opening a session on purpose and dropping it, so the catalogue answers
//! cold.
//!
//! Beside all of that runs [`bridge`], which is the conversation the other
//! way: bingo's shared tools served to the agent as MCP, so an ACP member can
//! act and not only read (ADR-0036).

use std::sync::{Arc, OnceLock};

use bingo_sdk::{
    CatalogKind, ConfigClaim, Contribution, Env, GatewayEvent, HostHandle, Merge, Plugin,
    PluginError, PluginManifest, Registrar,
};
use futures::StreamExt;
use tokio::task::JoinHandle;

pub mod bridge;
pub mod child;
pub mod config;
pub mod connection;
pub mod crossing;
pub mod ear;
pub mod error;
pub mod events;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod knobs;
pub mod ladder;
pub mod legacy;
pub mod method;
pub mod options;
pub mod probe;
pub mod provider;
pub mod refusal;
pub mod render;
pub mod servers;
pub mod session;
pub mod shared;
pub mod transcript;
pub mod wire;

/// Nothing static is provided: which providers exist is the person's
/// configuration to say (ADR-0035 §1), and a build that ships a default row
/// would be a build with an opinion about which agent you use.
static MANIFEST: PluginManifest = PluginManifest {
    id: session::PLUGIN,
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.4",
    provides: &[],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[("acp", Merge::ByName)],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(config::Settings)
}

#[derive(Default)]
pub struct AcpPlugin {
    sessions: OnceLock<Arc<session::Sessions>>,
    /// The task that hears the tools catalogue move. Started with the host,
    /// stopped with the plugin.
    watching: OnceLock<JoinHandle<()>>,
}

impl AcpPlugin {
    /// The registry the providers and the journal listener share. Present
    /// once `register` has run.
    pub fn sessions(&self) -> Option<&Arc<session::Sessions>> {
        self.sessions.get()
    }
}

#[async_trait::async_trait]
impl Plugin for AcpPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let rows = config::adapters(registrar.config()?)?;
        if rows.is_empty() {
            return Ok(());
        }
        let sessions = self.pool(registrar.env());
        registrar.add(Contribution::Hook(Arc::new(ear::Ear::new(
            sessions.clone(),
        ))));
        for instance in provider::providers(rows, &sessions) {
            registrar.provider(instance);
        }
        Ok(())
    }

    /// The host arrives after every plugin has registered; it is what a
    /// permission question is asked through and where the agent's session id
    /// is journaled.
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        let Some(sessions) = self.sessions.get() else {
            return Ok(());
        };
        sessions.set_host(host.clone()).await;
        let _ = self
            .watching
            .set(tokio::spawn(watch(host, sessions.clone())));
        Ok(())
    }

    /// Every adapter child ends with the process that spawned it: dropping
    /// the registry drops every link, and dropping a link takes its process
    /// group. The bridge goes the same way, and the socket with it.
    async fn stop(&self) -> Result<(), PluginError> {
        if let Some(watching) = self.watching.get() {
            watching.abort();
        }
        if let Some(sessions) = self.sessions.get() {
            sessions.close().await;
        }
        Ok(())
    }
}

/// A tool registered after a session started still reaches its agent: the
/// catalogue moving is `tools/list_changed` on every live conversation
/// (ADR-0036 §1). What moved is not carried — `list_changed` means "ask
/// again", and the offer is derived when it is asked for.
async fn watch(host: HostHandle, sessions: Arc<session::Sessions>) {
    let mut events = host.gateway_events();
    while let Some(event) = events.next().await {
        if matches!(
            event,
            GatewayEvent::CatalogChanged {
                kind: CatalogKind::Tools
            }
        ) {
            sessions.offer_changed().await;
        }
    }
}

impl AcpPlugin {
    fn pool(&self, env: &Env) -> Arc<session::Sessions> {
        self.sessions
            .get_or_init(|| session::Sessions::new(env.clone()))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registrar(config: serde_json::Value) -> Registrar {
        Registrar::new(
            MANIFEST.id,
            config,
            Env::rooted(std::env::temp_dir().join("bingo-acp-test")),
        )
    }

    fn registered(config: serde_json::Value) -> Vec<String> {
        let plugin = AcpPlugin::default();
        let mut registrar = registrar(config);
        plugin.register(&mut registrar).expect("it registers");
        registrar
            .into_contributions()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect()
    }

    /// A person with no adapters gets no providers, no listener and no
    /// children — the plugin is inert until it is configured.
    #[test]
    fn nothing_is_registered_until_an_adapter_is_configured() {
        assert!(registered(json!({})).is_empty());
        assert!(registered(json!({ "acp": { "adapters": {} } })).is_empty());
    }

    #[test]
    fn every_row_becomes_a_provider_of_its_own_name() {
        let contributions = registered(json!({
            "acp": { "adapters": {
                "claude": { "command": "npx", "args": ["-y", "@agentclientprotocol/claude-agent-acp"] },
                "codex-acp": { "command": "npx", "args": ["-y", "@agentclientprotocol/codex-acp"] }
            }}
        }));
        assert!(
            contributions.contains(&"Provider(claude)".to_string()),
            "{contributions:?}"
        );
        assert!(
            contributions.contains(&"Provider(codex-acp)".to_string()),
            "{contributions:?}"
        );
        assert!(
            contributions.contains(&"Hook(acp.journal)".to_string()),
            "the journal listener comes with them: {contributions:?}"
        );
    }

    /// A row that could not work is refused at boot, by name, rather than at
    /// the first turn.
    #[test]
    fn a_row_that_could_not_work_stops_the_boot() {
        let plugin = AcpPlugin::default();
        let mut registrar = registrar(json!({
            "acp": { "adapters": { "openai": { "command": "x" } } }
        }));
        let refused = plugin.register(&mut registrar).expect_err("a refusal");
        assert!(refused.to_string().contains("built-in"), "{refused}");
    }

    #[test]
    fn the_manifest_claims_one_key_and_promises_no_provider_of_its_own() {
        assert_eq!(MANIFEST.id, "bingo.acp");
        assert!(MANIFEST.provides.is_empty());
        assert!(MANIFEST.requires.is_empty());
        let claim = MANIFEST.config.expect("a config claim");
        assert_eq!(claim.keys, [("acp", Merge::ByName)]);
        let schema = serde_json::to_value(schema()).expect("a schema is json");
        assert!(schema["properties"]["acp"].is_object(), "{schema}");
    }
}
