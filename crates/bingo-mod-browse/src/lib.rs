//! Browse owns turn navigation, search input and the rewind sheet.
mod draw;
mod guide;
mod handler;
pub mod keys;
mod opening;
pub mod sheet;
use bingo_sdk::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
pub struct Browse;
#[derive(Clone, Default)]
pub(crate) struct Cursor {
    pub selected: Option<TurnId>,
    pub query: String,
    pub current: usize,
    pub editing: Option<String>,
    pub sheet_top: usize,
    pub sheet: Option<Preview>,
    pub mode: sheet::Mode,
}
#[derive(Clone, Default)]
pub(crate) struct Preview {
    pub turns: Vec<transcript::Turn>,
    pub length: usize,
    pub touched: bingo_services::checkpoints::Touched,
}
type Shared = Arc<Mutex<BTreeMap<(SessionId, String), Cursor>>>;
static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.mod.browse",
    version: env!("CARGO_PKG_VERSION"),
    uses: &["service:checkpoints"],
    requires: &[],
    provides: &[
        "command:browse",
        "command:browse:ui",
        "service:bingo.mod.browse.pages",
    ],
    config: None,
};
impl Plugin for Browse {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let shared = Shared::default();
        registrar.add(Contribution::CommandFold(CommandFold {
            into: "browse".into(),
            names: vec!["rewind".into()],
        }));
        registrar.add(Contribution::Command(Arc::new(handler::Open)));
        registrar.add(Contribution::Command(Arc::new(handler::Handler(
            shared.clone(),
        ))));
        registrar.add(Contribution::Interceptor {
            placement: plugin::Placement::Builtin,
            external: None,
            link: Arc::new(opening::Opening),
        });
        registrar.add(Contribution::Interceptor {
            placement: plugin::Placement::Builtin,
            external: None,
            link: Arc::new(draw::Drawing(shared)),
        });
        registrar.add(guide::contribution(registrar));
        Ok(())
    }
}
fn error(e: impl std::fmt::Display) -> KernelError {
    KernelError::new(ErrorCode::Internal, e.to_string())
}
