//! Asking each endpoint what it serves, out of everybody's way. A refresh is
//! never on the path of a turn or of `Host::build`: the cached list is what
//! the kernel answers with, and this replaces it when an endpoint gets round
//! to saying otherwise (ADR-0026 §4).
//!
//! One task per provider, each holding a `Weak<Host>`: a host that is shutting
//! down is not held open by an endpoint that has stopped answering, and what
//! comes back for it is dropped.

use std::sync::{Arc, Weak};

use bingo_sdk::{CatalogKind, GatewayEvent, Provider};
use jiff::Timestamp;

use super::Host;

/// What one provider's refresh came to: how many ids it answered with, or why
/// it could not be asked.
pub(crate) struct Refreshed {
    pub provider: String,
    pub answer: Result<usize, String>,
}

/// Ask every provider whose list is missing or a day old. Nothing waits for
/// these — not `build`, not a turn, not the shutdown.
pub(super) fn in_background(host: &Arc<Host>) {
    let weak = Arc::downgrade(host);
    tokio::spawn(async move {
        let Some(host) = weak.upgrade() else { return };
        let now = Timestamp::now();
        let stale: Vec<Arc<dyn Provider>> = usable(&host.providers().await)
            .into_iter()
            .filter(|p| host.served.stale(p.id(), now))
            .collect();
        let weak = Arc::downgrade(&host);
        drop(host);
        for provider in stale {
            let host = weak.clone();
            tokio::spawn(async move { ask(host, provider).await });
        }
    });
}

/// Ask every usable provider now, however fresh its list is, and say what each
/// answered: `/models refresh`, which waits because a person asked it to.
pub(super) async fn now(host: &Arc<Host>) -> Vec<Refreshed> {
    let providers = usable(&host.providers().await);
    let weak = Arc::downgrade(host);
    futures::future::join_all(
        providers
            .into_iter()
            .map(|provider| ask(weak.clone(), provider)),
    )
    .await
}

/// A provider with no usable credentials is not asked: an endpoint answers a
/// listing with the same 401 it answers a turn with.
fn usable(providers: &[Arc<dyn Provider>]) -> Vec<Arc<dyn Provider>> {
    providers
        .iter()
        .filter(|p| super::check_auth(p.as_ref()).is_ok())
        .cloned()
        .collect()
}

/// One provider's list, kept if it came and announced if it changed. A failure
/// keeps whatever was cached: an endpoint that could not be reached has not
/// withdrawn its models.
async fn ask(host: Weak<Host>, provider: Arc<dyn Provider>) -> Refreshed {
    let answer = provider.models().await;
    let refreshed = |answer| Refreshed {
        provider: provider.id().to_string(),
        answer,
    };
    let models = match answer {
        Ok(models) => models,
        Err(error) => {
            tracing::warn!(provider = provider.id(), %error, "the model list was not refreshed");
            return refreshed(Err(error.to_string()));
        }
    };
    let Some(host) = host.upgrade() else {
        return refreshed(Ok(models.len()));
    };
    let count = models.len();
    if host.served.record(provider.id(), models, Timestamp::now()) {
        let _ = host.gateway.send(GatewayEvent::CatalogChanged {
            kind: CatalogKind::Models,
        });
    }
    refreshed(Ok(count))
}
