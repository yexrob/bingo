//! The models a host offers: whose facts each carries, and which ids it lists
//! at all — the snapshot's, or the ones the endpoint itself answered with
//! (M35).

use super::*;

/// A provider registered under its own name while speaking another shape's
/// wire (ADR-0017): the proxy every question here is about.
async fn host_fronting(family: &str) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::filed_under(family, vec![]);
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    let config = HostConfig::new(env()).with_layer("cli", json!({"model": "m"}));
    (Host::build(plugins, config).await.unwrap(), provider)
}

fn facts(provider: &str, model: &str) -> crate::models::ModelFacts {
    crate::models::ModelCatalog::embedded()
        .lookup(provider, model)
        .unwrap_or_else(|| panic!("the snapshot knows {provider}/{model}"))
}

/// The instance is called `scripted`, so the snapshot has no shelf under its
/// name: its family's is where its models are, and a dated id resolves there
/// as its family's do. A vendor's own id is found wherever it is filed.
#[tokio::test]
async fn a_named_instance_reads_its_family_s_shelf_then_the_id_s_own() {
    let (host, provider) = host_fronting("openai").await;
    let dated = host.resolve_model(provider.as_ref(), "gpt-5.5-pro-2026-01-01");
    assert_eq!(
        dated.context_window,
        facts("openai", "gpt-5.5-pro").context_window,
        "a dated snapshot of the family's model is that model"
    );

    let proxied = host.resolve_model(provider.as_ref(), "deepseek-v4-pro");
    let known = facts("deepseek", "deepseek-v4-pro");
    assert_eq!(proxied.context_window, known.context_window);
    assert_eq!(proxied.max_output, known.max_output);
    assert!(
        proxied.reasoning,
        "a proxied model that reasons is not asked to stop"
    );

    let private = host.resolve_model(provider.as_ref(), "house-private-1");
    assert!(
        !private.reasoning && private.context_window > 0,
        "an id nobody carries still fails closed on the default, not on nothing"
    );
}
