//! The cross-process plugin bridge (ADR-0015): a `plugin.json` under
//! `plugins/<name>/` spawns a process, and that process ships bingo-native
//! tools and commands in whatever language it likes.
//!
//! The wire is the sdk's own types as JSON over JSON-RPC 2.0, one message per
//! line, on the child's stdin and stdout. `schema/plugin.json` is that
//! contract written down, generated from [`wire`] and [`manifest`] — a plugin
//! author who cannot read Rust reads that file and nothing else.
//!
//! Registration is synchronous and does no I/O, so the plugin contributes one
//! source per kind — tools, commands, contributors, compaction strategies,
//! providers — rather than the things themselves (ADR-0009 §1, ADR-0030 §2);
//! `start` reads the two layers and shakes hands with what it finds. With
//! nothing discovered the whole crate is inert.
//!
//! Nothing a process says about itself is believed: a bridge tool's traits are
//! the fail-closed default, so the gate asks about every call (ADR-0015 §4).

pub mod bridge;
pub mod codec;
pub mod command;
pub mod compactor;
pub mod completions;
pub mod config;
pub mod connection;
pub mod contributor;
pub mod deadline;
pub mod discovery;
pub mod hook;
pub mod manager;
pub mod manifest;
pub mod notice;
pub mod provider;
pub mod schema;
pub mod service;
pub mod source;
pub mod tool;
pub mod wire;

#[cfg(test)]
mod testing;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bingo_sdk::{
    CommandSource, CompactorSource, ConfigClaim, ContextSource, Contribution, HookSource,
    HostHandle, Merge, Plugin, PluginError, PluginManifest, ProviderSource, Registrar, ToolSource,
};

pub use bridge::{Bridge, Setting};
pub use command::PluginCommand;
pub use compactor::RemoteCompactor;
pub use config::Settings;
pub use connection::{Connection, log_path};
pub use contributor::{RemoteContributor, contributor_id};
pub use hook::{RemoteHook, hook_id};
pub use manager::Manager;
pub use manifest::{Entry, Manifest};
pub use provider::RemoteProvider;
pub use service::{Hub, RemoteService, ServiceCalls};
pub use source::{
    ID, PluginCommands, PluginCompactors, PluginContributors, PluginHooks, PluginProviders,
    PluginTools,
};
pub use tool::{PluginTool, tool_name};
pub use wire::PROTOCOL;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.plugin-rpc",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tools:plugin-rpc",
        "commands:plugin-rpc",
        "context:plugin-rpc",
        "compactor:plugin-rpc",
        "provider:plugin-rpc",
        "hook:plugin-rpc",
    ],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[("plugins", Merge::ByName)],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// Registers one source per kind, and spawns the discovered plugins on `start`.
#[derive(Debug, Default)]
pub struct PluginRpcPlugin {
    /// Built in `register`, where the settings are, and used by `start` and
    /// `stop`, which are handed nothing but the host.
    manager: OnceLock<Arc<Manager>>,
}

#[async_trait]
impl Plugin for PluginRpcPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let manager = Arc::new(Manager::new(registrar.env().clone(), settings.plugins));
        registrar.add(Contribution::Tools(
            Arc::new(PluginTools::new(Arc::clone(&manager))) as Arc<dyn ToolSource>,
        ));
        registrar.add(Contribution::Commands(
            Arc::new(PluginCommands::new(Arc::clone(&manager))) as Arc<dyn CommandSource>,
        ));
        registrar.add(Contribution::Contexts(
            Arc::new(PluginContributors::new(Arc::clone(&manager))) as Arc<dyn ContextSource>,
        ));
        registrar.add(Contribution::Compactors(
            Arc::new(PluginCompactors::new(Arc::clone(&manager))) as Arc<dyn CompactorSource>,
        ));
        registrar.add(Contribution::Providers(
            Arc::new(PluginProviders::new(Arc::clone(&manager))) as Arc<dyn ProviderSource>,
        ));
        registrar.add(Contribution::Hooks(
            Arc::new(PluginHooks::new(Arc::clone(&manager))) as Arc<dyn HookSource>,
        ));
        self.manager
            .set(manager)
            .map_err(|_| PluginError::Failed("the plugin bridge registered twice".into()))
    }

    /// Waits for every handshake, unlike the MCP client's: a plugin process is
    /// local and the person installed it, so the first turn having its tools
    /// is worth the wait a plugin costs — and a host with no plugins waits for
    /// nothing at all.
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        let Some(manager) = self.manager.get() else {
            return Ok(());
        };
        manager.start(&project_dir(), host).await;
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        if let Some(manager) = self.manager.get() {
            manager.shutdown().await;
        }
        Ok(())
    }
}

/// Which project's `.bingo/plugins` is read. A session's working directory is
/// not knowable here — `Plugin::start` is handed a host and no session, and a
/// `ToolSource` is handed nothing at all — so the process's own directory is
/// the project, which is what `bingo` run in a repository means.
fn project_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registrar(config: serde_json::Value) -> Registrar {
        Registrar::new("bingo.plugin-rpc", config, bingo_sdk::Env::rooted("/tmp"))
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_what_it_claims() {
        assert_eq!(MANIFEST.id, "bingo.plugin-rpc");
        assert_eq!(
            MANIFEST.provides,
            [
                "tools:plugin-rpc",
                "commands:plugin-rpc",
                "context:plugin-rpc",
                "compactor:plugin-rpc",
                "provider:plugin-rpc",
                "hook:plugin-rpc"
            ]
        );
        assert!(MANIFEST.requires.is_empty());
        let claim = MANIFEST.config.expect("a config claim");
        assert_eq!(claim.keys, [("plugins", Merge::ByName)]);
    }

    /// One source per kind the bridge opens, all answering to the same id:
    /// registration is synchronous and knows nothing yet (ADR-0009 §1).
    #[test]
    fn the_plugin_registers_one_source_of_every_kind_it_bridges() {
        let mut registrar = registrar(json!({ "plugins": { "wordcount": {} } }));
        PluginRpcPlugin::default()
            .register(&mut registrar)
            .expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 6);
        for contribution in &contributions {
            let id = match contribution {
                Contribution::Tools(source) => source.id(),
                Contribution::Commands(source) => source.id(),
                Contribution::Contexts(source) => source.id(),
                Contribution::Compactors(source) => source.id(),
                Contribution::Providers(source) => source.id(),
                Contribution::Hooks(source) => source.id(),
                other => panic!("expected a source, got {other:?}"),
            };
            assert_eq!(id, ID);
        }
    }

    #[test]
    fn the_claimed_schema_describes_the_key() {
        let schema = serde_json::to_value(schema()).expect("a schema is json");
        assert!(schema["properties"].get("plugins").is_some(), "{schema}");
    }

    /// The ordinary case: nobody has a plugin, and the crate does nothing.
    #[tokio::test]
    async fn a_host_with_no_plugins_starts_and_stops_quietly() {
        let plugin = PluginRpcPlugin::default();
        plugin.stop().await.expect("stop");
        let mut registrar = registrar(json!({}));
        plugin.register(&mut registrar).expect("register");
        plugin
            .start(bingo_sdk::testing::NoHost::handle())
            .await
            .expect("start");
        plugin.stop().await.expect("stop");
    }
}
