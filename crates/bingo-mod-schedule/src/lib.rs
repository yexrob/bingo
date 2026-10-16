//! Schedules through the shared pane and the owner's service.
mod confirmation;
mod disable;
pub use confirmation::Confirmation;
pub mod draw;
mod guide;
mod keys;
mod live;
mod panel;
mod rows;
mod whole;
use bingo_sdk::{
    CancellationToken, CommandFold, Contribution, ErrorCode, HostHandle, Interceptor, KernelError,
    Plugin, PluginError, PluginManifest, Registrar, SessionId, plugin,
};
use bingo_services::schedule;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
const ID: &str = "bingo.mod.schedule";
const REQUEST: &str = "schedules";
#[derive(Default)]
pub struct SchedulePanel {
    shared: Shared,
    stop: CancellationToken,
}
#[derive(Default)]
pub(crate) struct Cursor {
    selected: Option<rows::Selection>,
    confirming: Option<Confirmation>,
    seen: Option<std::time::Instant>,
}
type Shared = Arc<Mutex<BTreeMap<SessionId, Cursor>>>;
static MANIFEST: PluginManifest = PluginManifest {
    id: ID,
    version: env!("CARGO_PKG_VERSION"),
    uses: &[schedule::CAPABILITY],
    requires: &[],
    provides: &[
        "command:bingo.mod.schedule:ui",
        "service:bingo.mod.schedule.pages",
    ],
    config: None,
};
#[async_trait::async_trait]
impl Plugin for SchedulePanel {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let shared = self.shared.clone();
        registrar.add(Contribution::Command(Arc::new(keys::Handler(
            shared.clone(),
        ))));
        registrar.add(Contribution::Lifecycle(Arc::new(disable::Disable(
            shared.clone(),
        ))));
        for link in [
            Arc::new(panel::Open(shared.clone(), "schedule")) as Arc<dyn Interceptor>,
            Arc::new(panel::Open(shared.clone(), "wake")),
            Arc::new(draw::Drawing(shared.clone())),
        ] {
            registrar.add(Contribution::Interceptor {
                placement: plugin::Placement::Builtin,
                external: None,
                link,
            });
        }
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "schedule".into(),
            names: vec!["wake".into()],
        }));
        registrar.add(Contribution::Hook(Arc::new(live::Refresh(shared))));
        registrar.add(guide::contribution(registrar));
        Ok(())
    }
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        live::start(host, self.shared.clone(), self.stop.clone());
        Ok(())
    }
    async fn stop(&self) -> Result<(), PluginError> {
        self.stop.cancel();
        Ok(())
    }
}
fn error(error: impl std::fmt::Display) -> KernelError {
    KernelError::new(ErrorCode::Internal, error.to_string())
}
async fn listing(
    host: &HostHandle,
    session: &SessionId,
) -> Result<(schedule::Listing, Option<schedule::Pending>), KernelError> {
    let service = schedule::find(host).ok_or_else(|| error("schedule service unavailable"))?;
    Ok((
        service.list().await?,
        service.pending(session.clone()).await?,
    ))
}
