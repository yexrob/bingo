//! Reading and forgetting through the owners' services.
pub mod draw;
pub mod fence;
mod guide;
mod keys;
mod lifecycle;
mod panel;
pub mod rows;
use bingo_sdk::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub struct MemoryPanel;
#[derive(Default)]
struct Cursor {
    selected: usize,
    groups: Vec<rows::Group>,
    confirming: Option<rows::Target>,
}
type Shared = Arc<Mutex<BTreeMap<SessionId, Cursor>>>;
/// The plugin id the memory pane is placed, drawn and answered under.
pub const ID: &str = "bingo.mod.memory";
const REQUEST: &str = "memory";
static MANIFEST: PluginManifest = PluginManifest {
    id: ID,
    version: env!("CARGO_PKG_VERSION"),
    uses: &[
        bingo_services::memory::KEY.name(),
        bingo_services::experience::KEY.name(),
    ],
    requires: &[],
    provides: &[
        "command:bingo.mod.memory:ui",
        "service:bingo.mod.memory.pages",
    ],
    config: None,
};
impl Plugin for MemoryPanel {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let shared = Shared::default();
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "memory".into(),
            names: vec!["experience".into()],
        }));
        registrar.add(Contribution::Command(Arc::new(keys::Handler(
            shared.clone(),
            registrar.env().home.clone(),
        ))));
        for link in [
            Arc::new(panel::Open(
                shared.clone(),
                registrar.env().home.clone(),
                "memory",
            )) as Arc<dyn Interceptor>,
            Arc::new(panel::Open(
                shared.clone(),
                registrar.env().home.clone(),
                "experience",
            )),
            Arc::new(draw::Drawing(shared.clone())),
            Arc::new(lifecycle::Lifecycle(shared.clone())),
            Arc::new(lifecycle::Disable(shared)),
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
fn unavailable() -> KernelError {
    KernelError::new(ErrorCode::NotFound, "the owning service is unavailable")
}

#[cfg(test)]
mod opening_tests;
