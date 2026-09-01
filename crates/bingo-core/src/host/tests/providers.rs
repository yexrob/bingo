//! A provider that arrives after I/O (ADR-0030 §2): the whole of what the
//! kernel does differently for one is resolve it later. A session opens on it,
//! a turn runs on it, and the catalogue lists it and its models through the
//! readers that list every other provider's.

use super::*;
use crate::test_support::ScriptedProviderSource;

static LATE: PluginManifest = PluginManifest {
    id: "test.late-provider",
    version: "0",
    sdk: "^0.1",
    provides: &["provider:late"],
    requires: &[],
    config: None,
};

/// A host whose only provider comes from a source, and the source, still
/// empty: what a plugin looks like before its process has answered.
async fn host_awaiting(source: &Arc<ScriptedProviderSource>) -> Arc<Host> {
    let plugins = vec![TestPlugin::boxed(
        &LATE,
        vec![Contribution::Providers(
            Arc::clone(source) as Arc<dyn ProviderSource>
        )],
    )];
    let config = HostConfig::new(env()).with_layer("user", json!({"model": "m"}));
    Host::build(plugins, config).await.unwrap()
}

/// The provider exists nowhere at boot and is the session's model by the time
/// one is chosen — which is the whole of what "late" means.
#[tokio::test]
async fn a_session_opens_on_a_provider_that_did_not_exist_at_boot() {
    let source = ScriptedProviderSource::new(vec![]);
    let host = host_awaiting(&source).await;
    assert!(
        host.providers().await.is_empty(),
        "the source has not answered yet"
    );
    assert_eq!(
        host.open(
            SessionSelector::Create {
                spec: spec("/work")
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::ProviderUnavailable),
        "and a host with no provider says so"
    );

    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    source.set(vec![Arc::clone(&provider) as Arc<dyn Provider>]);
    let Attachment {
        mut snapshot,
        mut events,
        handle,
        ..
    } = host
        .open(
            SessionSelector::Create {
                spec: spec("/work"),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.summary.provider.as_deref(), Some("scripted"));

    handle.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    while let Some(frame) = events.next().await {
        snapshot.apply(&frame);
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            break;
        }
    }
    assert_eq!(snapshot.last_turn, Some(TurnStatus::Completed));
    assert_eq!(
        provider.requests().len(),
        1,
        "the turn ran on the late provider"
    );
}

fn ids(catalog: &Catalog) -> Vec<&str> {
    catalog.entries.iter().map(|e| e.id.as_str()).collect()
}

/// The catalogue has one reader per kind and a late provider goes through it:
/// its models are the shelf's, filed and enriched exactly as a built-in's are
/// (ADR-0017, ADR-0026 §1). A second list would be the ADR-0011 debt.
#[tokio::test]
async fn the_catalogue_lists_a_late_provider_s_models_where_it_lists_every_other() {
    let source = ScriptedProviderSource::new(vec![]);
    let host = host_awaiting(&source).await;
    assert!(
        host.catalog(CatalogKind::Providers)
            .await
            .unwrap()
            .entries
            .is_empty()
    );

    source.set(vec![
        ScriptedProvider::filed_under("anthropic", vec![]) as Arc<dyn Provider>
    ]);
    let providers = host.catalog(CatalogKind::Providers).await.unwrap();
    assert_eq!(ids(&providers), ["scripted"]);
    assert_eq!(
        providers.entries[0].meta["auth"]["kind"],
        json!("notApplicable")
    );

    let models = host.catalog(CatalogKind::Models).await.unwrap();
    let sonnet = models
        .entries
        .iter()
        .find(|e| e.id == "scripted/claude-sonnet-4-5")
        .unwrap_or_else(|| panic!("the shelf's models are listed: {:?}", ids(&models)));
    assert_eq!(sonnet.meta["provider"], json!("scripted"));
    assert!(
        sonnet.meta["context"].is_u64(),
        "with the facts the snapshot holds: {:?}",
        sonnet.meta
    );
    assert!(
        ids(&models).contains(&"scripted/m"),
        "and the configured model, as for any provider: {:?}",
        ids(&models)
    );
}
