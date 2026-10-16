//! Model choice and provider setup through catalogues and minted handlers.
mod bridge;
pub mod card;
mod commands;
mod describe;
mod disable;
pub mod draw;
pub mod guess;
mod guide;
mod keys;
pub mod label;
mod lifecycle;
mod pins;
mod receipts;
pub mod rows;
pub mod sheet;
mod sheet_actions;
pub mod state;
use bingo_sdk::{
    CommandFold, Contribution, ErrorCode, Interceptor, KernelError, Plugin, PluginError,
    PluginManifest, Registrar, SessionId, plugin, render,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
type Shared = Arc<Mutex<BTreeMap<SessionId, state::Picker>>>;
#[derive(Default)]
pub struct Models;
static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.mod.models",
    version: env!("CARGO_PKG_VERSION"),
    uses: &[],
    requires: &[],
    provides: &[
        "command:bingo.mod.models:ui",
        "service:bingo.mod.models.pages",
    ],
    config: None,
};
impl Plugin for Models {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let state = Shared::default();
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "model".into(),
            names: vec!["models".into(), "provider".into()],
        }));
        registrar.add(Contribution::Command(Arc::new(keys::Handler(
            state.clone(),
        ))));
        registrar.add(Contribution::Hook(Arc::new(lifecycle::Lifecycle(
            state.clone(),
        ))));
        registrar.add(Contribution::Lifecycle(Arc::new(disable::Disable(
            state.clone(),
        ))));
        for link in [
            Arc::new(commands::Commands(state.clone())) as Arc<dyn Interceptor>,
            Arc::new(receipts::Receipts),
            Arc::new(describe::Describe),
            Arc::new(draw::Drawing(state.clone(), render::Component::ModelPicker)),
            Arc::new(draw::Drawing(state, render::Component::InfoNotice)),
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
    KernelError::new(ErrorCode::InvalidInput, error.to_string())
}

#[cfg(test)]
mod test_support;
