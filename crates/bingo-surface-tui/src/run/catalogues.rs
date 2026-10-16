use super::*;

/// The catalogues whose ids a command's argument may name, by the source name
/// its `ArgSpec::Catalog` gives.
const CATALOGUES: &[(&str, CatalogKind)] = &[
    ("models", CatalogKind::Models),
    ("providers", CatalogKind::Providers),
    ("plugins", CatalogKind::Plugins),
];

/// The catalogues are read at start and again whenever the host says one
/// of them was rebuilt: the dropdown follows, it does not poll.
// 200 ms lets the usual local catalogue bind the first key without making
// first paint depend on a plugin's multi-second handshake.
const INITIAL_WAIT: Duration = Duration::from_millis(200);
const COMMAND_WAIT: Duration = Duration::from_secs(5);

pub(super) async fn fetch_all(run: &mut Run) {
    run.typeahead.catalogue_pending = true;
    let generation = next_fetch(run);
    let host = run.host.clone();
    let session = run.session.tree.view().clone();
    let mut request = Box::pin(request_commands(host, session));
    match tokio::time::timeout(INITIAL_WAIT, &mut request).await {
        Ok(result) => received(run, generation, result),
        Err(_) => run.spawn(async move { Ok(Reply::Commands(generation, request.await)) }),
    }
    for (source, kind) in CATALOGUES {
        fetch(run, source, *kind);
    }
}

pub(super) fn fetch_commands(run: &mut Run) {
    let generation = next_fetch(run);
    let host = run.host.clone();
    let session = run.session.tree.view().clone();
    run.spawn(async move {
        Ok(Reply::Commands(
            generation,
            request_commands(host, session).await,
        ))
    });
}

pub(super) fn view_changed(run: &mut Run) {
    run.ui.catalogs.commands.clear();
    run.ui.placed.pulled.invalidate();
    run.typeahead.catalogue_pending = true;
    fetch_commands(run);
}

async fn request_commands(
    host: HostHandle,
    session: SessionId,
) -> Result<Vec<bingo_sdk::CatalogEntry>, KernelError> {
    tokio::time::timeout(
        COMMAND_WAIT,
        host.catalog_for(&session, CatalogKind::Commands),
    )
    .await
    .map_err(|_| KernelError::new(bingo_sdk::ErrorCode::Timeout, "command catalogue timed out"))?
    .map(|catalog| catalog.entries)
}

fn next_fetch(run: &mut Run) -> u64 {
    run.command_fetch += 1;
    run.command_fetch
}

pub(super) fn received(
    run: &mut Run,
    fetch: u64,
    result: Result<Vec<bingo_sdk::CatalogEntry>, KernelError>,
) {
    if fetch != run.command_fetch {
        return;
    }
    run.typeahead.catalogue_pending = false;
    match result {
        Ok(entries) => {
            run.ui.catalogs.commands = entries;
            run.ui.placed.pulled.invalidate();
            site_keys::notices(run);
        }
        Err(error) => run.ui.notify(
            Level::Warn,
            format!("Command catalogue unavailable: {}", error.message),
            Instant::now(),
        ),
    }
    typeahead::drain(run);
}

pub(super) fn waiting(run: &Run, key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    run.typeahead.catalogue_pending
        && key != crossterm::event::KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
        && (key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            || matches!(key.code, KeyCode::F(_)))
}

/// One catalogue's ids, on the loop's own host-call lane.
pub(super) fn fetch(run: &mut Run, source: &'static str, kind: CatalogKind) {
    let host = run.host.clone();
    run.spawn(async move {
        host.catalog(kind)
            .await
            .map(|c| Reply::Catalogue(source.to_string(), c.entries))
    });
}

/// A catalogue the host rebuilt — a provider's endpoint answered what it
/// serves (ADR-0026 §4) — is read again, and only that one.
pub(super) fn changed(run: &mut Run, kind: CatalogKind) {
    if kind == CatalogKind::Plugins && crate::plugins::active(&run.ui) {
        run.ui.placed.pulled.invalidate();
    }
    if kind == CatalogKind::Commands {
        run.ui.placed.pulled.invalidate();
        fetch_commands(run);
        return;
    }
    let Some((source, _)) = CATALOGUES.iter().find(|(_, listed)| *listed == kind) else {
        return;
    };
    fetch(run, source, kind);
}

#[cfg(test)]
#[path = "catalogues_tests.rs"]
mod tests;
