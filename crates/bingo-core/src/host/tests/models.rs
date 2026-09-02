//! The models a host offers: whose facts each carries, and which ids it lists
//! at all — the snapshot's, or the ones the endpoint itself answered with
//! (M35).

use std::path::Path;

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

/// A host that keeps what it learns where the test can watch it, and open a
/// second one on afterwards — a later process, as far as the cache knows.
async fn host_keeping(dir: &Path, provider: &Arc<ScriptedProvider>) -> Arc<Host> {
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    let env = Env {
        home: dir.into(),
        config_dir: dir.into(),
        data_dir: dir.into(),
    };
    let config = HostConfig::new(env).with_layer("cli", json!({"model": "m"}));
    Host::build(plugins, config).await.unwrap()
}

/// The ids `catalog(Models)` offers, with who says each exists.
async fn offered(host: &Arc<Host>) -> Vec<(String, String)> {
    host.catalog(CatalogKind::Models)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| {
            (
                e.label,
                e.meta["source"].as_str().unwrap_or("?").to_string(),
            )
        })
        .collect()
}

/// The catalogue once a background refresh has landed. The refresh runs on
/// tasks of its own, so this waits for it — generously, and never on a
/// scheduling order.
async fn once_refreshed(host: &Arc<Host>) -> Vec<(String, String)> {
    for _ in 0..200 {
        let offered = offered(host).await;
        if offered.iter().any(|(_, source)| source == "endpoint") {
            return offered;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the list the endpoint serves never arrived")
}

fn as_pairs(offered: &[(String, String)]) -> Vec<(&str, &str)> {
    offered
        .iter()
        .map(|(id, source)| (id.as_str(), source.as_str()))
        .collect()
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

/// Before the endpoint has answered, a provider offers its family's shelf;
/// after, it offers what the endpoint says it serves — and only that.
#[tokio::test]
async fn what_the_endpoint_answers_replaces_the_shelf_it_was_filed_under() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::filed_under("deepseek", vec![]);
    let host = host_keeping(dir.path(), &provider).await;

    let before = offered(&host).await;
    assert!(
        before.contains(&("deepseek-v4-flash".to_string(), "catalogue".to_string())),
        "the snapshot's shelf stands until an endpoint says otherwise: {before:?}"
    );

    provider.serves(&["deepseek-v4-pro", "glm-5"]);
    let refreshed = host.refresh_models().await;
    assert_eq!(
        refreshed
            .iter()
            .map(|r| (r.provider.as_str(), r.answer.clone()))
            .collect::<Vec<_>>(),
        [("scripted", Ok(2))]
    );
    let after = offered(&host).await;
    assert_eq!(
        as_pairs(&after),
        [
            ("m", "configured"),
            ("deepseek-v4-pro", "endpoint"),
            ("glm-5", "endpoint"),
        ],
        "the configured model stays first"
    );
}

/// The refresh at start is on nobody's path: `build` returns without it, and
/// what it finds lands afterwards.
#[tokio::test]
async fn the_list_arrives_on_its_own_after_the_host_is_up() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    provider.serves(&["fake-1", "fake-2"]);
    let host = host_keeping(dir.path(), &provider).await;
    assert_eq!(
        as_pairs(&once_refreshed(&host).await),
        [
            ("m", "configured"),
            ("fake-1", "endpoint"),
            ("fake-2", "endpoint")
        ]
    );
}

/// What one process cached is what the next answers with, before anybody is
/// asked again — this endpoint would say something else if it were.
#[tokio::test]
async fn a_list_one_process_cached_is_what_the_next_one_answers_with() {
    let dir = tempfile::tempdir().unwrap();
    let first = ScriptedProvider::new(vec![]);
    first.serves(&["fake-1", "fake-2"]);
    let host = host_keeping(dir.path(), &first).await;
    host.refresh_models().await;
    drop(host);

    let second = ScriptedProvider::new(vec![]);
    second.serves(&["something-else"]);
    let next = host_keeping(dir.path(), &second).await;
    assert_eq!(
        as_pairs(&offered(&next).await),
        [
            ("m", "configured"),
            ("fake-1", "endpoint"),
            ("fake-2", "endpoint")
        ]
    );
}

/// An endpoint that cannot be reached has not withdrawn its models.
#[tokio::test]
async fn an_endpoint_that_cannot_be_asked_keeps_the_list_it_gave() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    provider.serves(&["fake-1"]);
    let host = host_keeping(dir.path(), &provider).await;
    host.refresh_models().await;

    provider.breaks("connection refused");
    let refreshed = host.refresh_models().await;
    let failure = refreshed[0]
        .answer
        .clone()
        .expect_err("it could not be asked");
    assert!(failure.contains("connection refused"), "{failure}");
    assert_eq!(
        as_pairs(&offered(&host).await),
        [("m", "configured"), ("fake-1", "endpoint")]
    );
}

/// A list that changed is worth telling every client about, once; a list that
/// came back the same is not news.
#[tokio::test]
async fn a_changed_list_is_announced_and_an_unchanged_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    let host = host_keeping(dir.path(), &provider).await;
    let mut gateway = host.gateway_events();

    provider.serves(&["fake-1"]);
    host.refresh_models().await;
    assert_eq!(
        gateway.next().await,
        Some(GatewayEvent::CatalogChanged {
            kind: CatalogKind::Models
        })
    );

    host.refresh_models().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), gateway.next())
            .await
            .is_err(),
        "the same list said nothing new"
    );
}

static LOCKED: PluginManifest = PluginManifest {
    id: "test.locked",
    version: "0",
    sdk: "^0.1",
    provides: &["provider:locked"],
    requires: &[],
    config: None,
};

/// A provider with no credentials: an endpoint answers a listing with the
/// same 401 it answers a turn with, so it is never asked for one.
struct Locked;

#[async_trait]
impl Provider for Locked {
    fn id(&self) -> &str {
        "locked"
    }

    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities::default()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        unreachable!("no turn runs on a provider that cannot sign in")
    }

    fn auth(&self) -> AuthStatus {
        AuthStatus::Missing {
            hint: "paste a key".into(),
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        panic!("a provider without credentials was asked what it serves")
    }
}

#[tokio::test]
async fn a_provider_that_cannot_sign_in_is_not_asked() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    provider.serves(&["fake-1"]);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(&LOCKED, vec![Contribution::Provider(Arc::new(Locked))]),
    ];
    let env = Env {
        home: dir.path().into(),
        config_dir: dir.path().into(),
        data_dir: dir.path().into(),
    };
    let host = Host::build(
        plugins,
        HostConfig::new(env).with_layer("cli", json!({"model": "m"})),
    )
    .await
    .unwrap();
    let refreshed = host.refresh_models().await;
    let asked: Vec<&str> = refreshed.iter().map(|r| r.provider.as_str()).collect();
    assert_eq!(asked, ["scripted"]);
}
