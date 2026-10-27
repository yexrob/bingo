//! Boot-only model preference and the deliberate command's write boundary.

use super::Host;
use crate::models::selection::{self, Selection};
use bingo_sdk::{KernelError, SessionId};

pub(super) fn load_model(host: &Host) -> Option<Selection> {
    if host.config.layers.iter().any(|layer| {
        !layer.is_user()
            && (layer.value.contains_key("provider") || layer.value.contains_key("model"))
    }) {
        return None;
    }
    selection::load(&host.env().data_dir.join("last-model.json"))
}

pub(super) async fn restore_model(host: &Host, candidate: Option<&Selection>) {
    if host.remembered.get().is_some() {
        return;
    }
    let Some(choice) = candidate else {
        return;
    };
    if host.restorable_provider(&choice.provider).await {
        let _ = host.remembered.set(choice.clone());
    }
}

/// Called only after the core `/model` command applied its session choice.
pub(crate) async fn remember_model(
    host: &Host,
    session: &SessionId,
    kept: bool,
) -> Result<String, KernelError> {
    let applied = if kept {
        "kept for new sessions"
    } else {
        "this session"
    };
    if host.live(session)?.spec.parent.is_some() {
        return Ok(applied.into());
    }
    let Some(choice) = host.session_model(session).await? else {
        return Ok(applied.into());
    };
    let choice = Selection::new(choice.endpoint.id().into(), choice.id);
    Ok(
        match selection::save(&host.env().data_dir.join("last-model.json"), &choice) {
            Ok(()) if kept => applied.into(),
            Ok(()) => "this session · next-start default".into(),
            Err(error) => format!("{applied} · could not remember next-start default: {error}"),
        },
    )
}
