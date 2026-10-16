//! One command-driven panel for plugins and MCP servers.
pub mod cards;
pub mod chain;
pub mod draw;
pub mod groups;
mod guide;
mod keys;
mod opening;
pub mod trust;
pub mod verbs;
use bingo_sdk::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
pub struct Plugins;
#[derive(Default)]
pub struct Cursor {
    pub selected: String,
    pub expanded: bool,
    pub trust: Option<trust::Choice>,
    /// A waiting server's first `⏎` was pressed: its card shows what would
    /// run and the next `⏎` approves it.
    pub reviewing: bool,
}
type Shared = Arc<Mutex<BTreeMap<(SessionId, String), Cursor>>>;
static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.mod.plugins",
    version: env!("CARGO_PKG_VERSION"),
    uses: &["service:mcp.servers"],
    requires: &[],
    provides: &["command:plugins:ui", "service:bingo.mod.plugins.pages"],
    config: None,
};
impl Plugin for Plugins {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let state = Shared::default();
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "plugins".into(),
            names: vec!["mcp".into()],
        }));
        registrar.add(Contribution::Command(Arc::new(keys::Handler(
            state.clone(),
        ))));
        for link in [
            Arc::new(opening::Opening("plugins")) as Arc<dyn Interceptor>,
            Arc::new(opening::Opening("mcp")),
            Arc::new(draw::Drawing(state)),
        ] {
            registrar.add(Contribution::Interceptor {
                placement: plugin::Placement::Builtin,
                external: None,
                link,
            });
        }
        registrar.add(guide::contribution(registrar));
        Ok(())
    }
}
fn error(error: impl std::fmt::Display) -> KernelError {
    KernelError::new(ErrorCode::Internal, error.to_string())
}
async fn servers(host: &HostHandle) -> Vec<bingo_services::mcp::ServerRow> {
    match bingo_services::mcp::find(host) {
        Some(service) => service.rows().await,
        None => vec![],
    }
}

#[cfg(test)]
mod card_tests;
#[cfg(test)]
mod tests;
