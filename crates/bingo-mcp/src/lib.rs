//! The MCP client: configured servers dialled in the background, their tools
//! offered to the model untrusted, their stderr sent to a log.
//!
//! Registration is synchronous and does no I/O, so the plugin contributes a
//! [`ToolSource`] rather than tools: `start` puts one dial per enabled server
//! on its own task and returns at once, and whatever has landed by the time a
//! turn begins is that turn's tool set (ADR-0009 §1). The first turn of a
//! session may therefore run before a slow server arrives — that is the
//! design; the alternative is a session that will not start until the slowest
//! server on the list has answered.
//!
//! Nothing a server says about itself is believed: an MCP tool's traits are
//! the fail-closed default, so the gate asks about every call (ADR-0009 §2).

pub mod command;
pub mod config;
pub mod dial;
pub mod manager;
pub mod rows;
pub mod source;
pub mod tool;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bingo_sdk::{
    Command, ConfigClaim, Contribution, HostHandle, Merge, Plugin, PluginError, PluginManifest,
    Registrar, ServiceHandle, ToolSource, WireService,
};

pub use command::McpCommand;
pub use config::{Server, Settings};
pub use dial::CONNECT_TIMEOUT;
pub use manager::{Manager, Status};
pub use rows::{Rows, SERVERS};
pub use source::McpSource;
pub use tool::{McpTool, tool_name};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.mcp",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["tools:mcp", "command:mcp", "service:mcp.servers"],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[
            ("mcpServers", Merge::ByName),
            ("disabledMcpServers", Merge::Accumulate),
        ],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// Registers the tool source and `/mcp`, and dials the servers on `start`.
#[derive(Debug, Default)]
pub struct McpPlugin {
    /// Built in `register`, where the settings are, and used by `start` and
    /// `stop`, which are handed nothing but the host.
    manager: OnceLock<Arc<Manager>>,
}

#[async_trait]
impl Plugin for McpPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let manager = Arc::new(Manager::new(
            settings.mcp_servers,
            &settings.disabled_mcp_servers,
            registrar.env().data_dir.clone(),
        ));
        registrar.add(Contribution::Tools(
            Arc::new(McpSource::new(Arc::clone(&manager))) as Arc<dyn ToolSource>,
        ));
        registrar.add(Contribution::Command(
            Arc::new(McpCommand::new(Arc::clone(&manager))) as Arc<dyn Command>,
        ));
        registrar.add(service(Arc::clone(&manager)));
        self.manager
            .set(manager)
            .map_err(|_| PluginError::Failed("the mcp plugin registered twice".into()))
    }

    /// Returns the moment the dialling is on its own task, not when it lands.
    async fn start(&self, _host: HostHandle) -> Result<(), PluginError> {
        let Some(manager) = self.manager.get().cloned() else {
            return Ok(());
        };
        tokio::spawn(async move { manager.dial_enabled().await });
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        if let Some(manager) = self.manager.get() {
            manager.shutdown().await;
        }
        Ok(())
    }
}

/// The rows, under the key another plugin looks them up by. Both faces are
/// the one object: the typed lookup is a `ServiceHandle` over the wire face,
/// which is how a service met by method rather than by type is reached from in
/// process (ADR-0031 §4).
fn service(manager: Arc<Manager>) -> Contribution {
    let wire = Arc::new(Rows::new(manager)) as Arc<dyn WireService>;
    Contribution::Service {
        key: SERVERS.to_string(),
        value: Arc::new(ServiceHandle::new(Arc::clone(&wire))),
        wire: Some(wire),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registrar(config: serde_json::Value) -> Registrar {
        Registrar::new("bingo.mcp", config, bingo_sdk::Env::rooted("/tmp"))
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_what_it_claims() {
        assert_eq!(MANIFEST.id, "bingo.mcp");
        assert_eq!(
            MANIFEST.provides,
            ["tools:mcp", "command:mcp", "service:mcp.servers"]
        );
        assert!(MANIFEST.requires.is_empty());
        let claim = MANIFEST.config.expect("a config claim");
        assert_eq!(claim.keys[0], ("mcpServers", Merge::ByName));
        assert_eq!(claim.keys[1], ("disabledMcpServers", Merge::Accumulate));
    }

    #[test]
    fn the_plugin_registers_a_tool_source_a_command_and_the_rows() {
        let mut registrar = registrar(json!({
            "mcpServers": { "files": { "command": "npx", "args": ["-y", "files"] } }
        }));
        McpPlugin::default()
            .register(&mut registrar)
            .expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 3);
        match &contributions[0] {
            Contribution::Tools(source) => assert_eq!(source.id(), "mcp"),
            other => panic!("expected a tool source, got {other:?}"),
        }
        match &contributions[1] {
            Contribution::Command(command) => assert_eq!(command.spec().name, "mcp"),
            other => panic!("expected a command, got {other:?}"),
        }
        match &contributions[2] {
            Contribution::Service { key, wire, .. } => {
                assert_eq!(key, SERVERS);
                assert!(wire.is_some(), "one object, both faces");
            }
            other => panic!("expected the rows service, got {other:?}"),
        }
    }

    #[test]
    fn a_server_this_crate_cannot_read_stops_the_plugin_rather_than_being_dropped() {
        let mut registrar = registrar(json!({
            "mcpServers": { "legacy": { "type": "sse", "url": "http://localhost:8000/sse" } }
        }));
        let error = McpPlugin::default()
            .register(&mut registrar)
            .expect_err("refused");
        assert!(matches!(error, PluginError::Config(_)), "{error}");
        assert!(error.to_string().contains("sse"), "{error}");
    }

    #[test]
    fn the_claimed_schema_describes_both_keys() {
        let schema = serde_json::to_value(schema()).expect("a schema is json");
        let properties = &schema["properties"];
        assert!(properties.get("mcpServers").is_some(), "{schema}");
        assert!(properties.get("disabledMcpServers").is_some(), "{schema}");
    }

    #[tokio::test]
    async fn a_plugin_that_registered_nothing_starts_and_stops_quietly() {
        let plugin = McpPlugin::default();
        plugin.stop().await.expect("stop");
        let mut registrar = registrar(json!({}));
        plugin.register(&mut registrar).expect("register");
        plugin.stop().await.expect("stop");
    }
}
