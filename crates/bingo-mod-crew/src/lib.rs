//! The crew's view, composed only from shared services and session frames.
mod actions;
pub mod cards;
mod command;
mod dispatch;
pub mod draw;
mod guide;
pub mod keys;
mod lifecycle;
mod live;
mod opening;
mod refresh;
pub mod state;
use bingo_sdk::{
    CommandFold, Contribution, ErrorCode, KernelError, Plugin, PluginError, PluginManifest,
    Registrar, SessionId, plugin,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
const ID: &str = "bingo.mod.crew";
const REQUEST: &str = "crew";
type Shared = Arc<Mutex<BTreeMap<SessionId, state::Panel>>>;
#[derive(Default)]
pub struct Crew {
    shared: Shared,
}
static MANIFEST: PluginManifest = PluginManifest {
    id: ID,
    version: env!("CARGO_PKG_VERSION"),
    uses: &[
        bingo_services::agents_control::CAPABILITY,
        bingo_services::rooms_control::CAPABILITY,
        bingo_services::tasks_write::CAPABILITY,
    ],
    requires: &[],
    provides: &[
        "command:crew",
        "command:bingo.mod.crew:ui",
        "service:bingo.mod.crew.pages",
    ],
    config: None,
};
#[async_trait::async_trait]
impl Plugin for Crew {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.add(Contribution::Command(Arc::new(command::Open(
            self.shared.clone(),
        ))));
        registrar.add(Contribution::Command(Arc::new(actions::Handler(
            self.shared.clone(),
        ))));
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "crew".into(),
            names: opening::FOLDED
                .into_iter()
                .map(|(name, _)| name.into())
                .collect(),
        }));
        for (name, tab) in opening::FOLDED {
            registrar.add(Contribution::Interceptor {
                placement: plugin::Placement::Builtin,
                external: None,
                link: Arc::new(opening::Opening {
                    shared: self.shared.clone(),
                    name,
                    tab,
                }),
            });
        }
        registrar.add(Contribution::Interceptor {
            placement: plugin::Placement::Builtin,
            external: None,
            link: Arc::new(draw::Drawing(self.shared.clone())),
        });
        registrar.add(Contribution::Lifecycle(Arc::new(lifecycle::Disable(
            self.shared.clone(),
        ))));
        registrar.add(Contribution::Hook(Arc::new(refresh::Refresh(
            self.shared.clone(),
        ))));
        registrar.add(guide::contribution(registrar));
        Ok(())
    }
    async fn stop(&self) -> Result<(), PluginError> {
        self.shared
            .lock()
            .map_err(|e| PluginError::Failed(e.to_string()))?
            .clear();
        Ok(())
    }
}
fn error(error: impl std::fmt::Display) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, error.to_string())
}
#[cfg(test)]
mod keys_tests;

#[cfg(test)]
mod cards_tests;

#[cfg(test)]
mod live_tests;

#[cfg(test)]
mod dispatch_tests;

#[cfg(test)]
mod opening_tests;

#[cfg(test)]
mod lifecycle_tests;
