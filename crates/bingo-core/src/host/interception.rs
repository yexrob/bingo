//! Context and catalogue reads shared by interception sites.
use super::*;

pub(crate) mod reserved;

pub(super) fn hook_set(host: &Host) -> HookSet {
    HookSet {
        running: host.registry.switches.running.clone(),
        links: host.registry.links.clone(),
        interceptors: host
            .registry
            .sources
            .interceptors
            .iter()
            .map(|source| {
                Arc::new(RegisteredSource {
                    host: host.weak.clone(),
                    source: source.clone(),
                }) as Arc<dyn InterceptorSource>
            })
            .collect(),
        grants: host.settings.kernel.plugin_trust.clone(),
        fixed: host.registry.hooks.clone(),
        sources: host.registry.sources.hooks.clone(),
    }
}

pub(super) fn catalogue_context(host: &Host, session: Option<&SessionId>) -> HookContext {
    let cwd = session
        .and_then(|id| host.live(id).ok())
        .map(|live| live.spec.cwd)
        .unwrap_or_else(|| host.config.env.home.clone());
    HookContext {
        session: session
            .cloned()
            .unwrap_or_else(|| SessionId::from_raw("host")),
        turn: None,
        item: None,
        cwd,
        provider: None,
        model: None,
        host: host.plugin_host.clone(),
    }
}

pub(super) async fn catalogue_answer(
    host: &Host,
    kind: CatalogKind,
    cx: &HookContext,
) -> Result<Catalog, KernelError> {
    register_pending(host).await;
    let resolved = host.providers().await;
    let pins = super::pins::pinned_models(host);
    let mut catalog = Catalog {
        kind,
        entries: catalog::entries(
            &host.registry,
            &resolved,
            host.settings.kernel.model.as_deref(),
            &host.served,
            kind,
            &cx.cwd,
        )
        .await,
    };
    if kind == CatalogKind::Models {
        super::catalog::limits::apply(host, &resolved, &mut catalog.entries);
        for entry in &mut catalog.entries {
            if entry.meta.get("context").is_some() || pins.contains(&entry.id) {
                entry.meta["pinned"] = serde_json::json!(pins.contains(&entry.id));
            }
        }
        catalog.entries.sort_by_key(|entry| {
            pins.iter()
                .position(|id| id == &entry.id)
                .unwrap_or(usize::MAX)
        });
    }
    let report = crate::chain::Report::default();
    let mut result = crate::chain::autocomplete::catalog(
        catalog,
        &host.hook_set().links().await,
        cx,
        report.clone(),
    )
    .await;
    if kind == CatalogKind::Plugins
        && let Ok(catalog) = &mut result
    {
        protect_grants(host, catalog).await;
        catalog::details(host, catalog).await;
    }
    if kind == CatalogKind::Commands
        && let Ok(catalog) = &mut result
    {
        catalog
            .entries
            .retain(|entry| entry.meta["hidden"].as_bool() != Some(true));
        catalog::command_folds(&host.registry, &mut catalog.entries);
    }
    for notice in report.take() {
        if let ItemBody::Notice { level, code, text } = notice {
            let _ = host.notice(level, &code, &text).await;
        }
    }
    result
}

struct RegisteredSource {
    host: Weak<Host>,
    source: Arc<dyn InterceptorSource>,
}
#[async_trait]
impl InterceptorSource for RegisteredSource {
    fn id(&self) -> &str {
        self.source.id()
    }
    async fn interceptors(&self) -> Vec<ExternalInterceptor> {
        if let Some(host) = self.host.upgrade() {
            register_pending(&host).await;
        }
        self.source.interceptors().await
    }
    async fn tier(&self, name: &str) -> Option<Tier> {
        let host = self.host.upgrade()?;
        crate::plugins::listing(&host.registry)
            .await
            .into_iter()
            .find(|status| status.id == name)?
            .grant
            .map(|grant| grant.tier)
    }
}

/// Nothing that waits registers while the trust file cannot be read: no
/// grant is guessed. The person is told which file, once per thing it says.
pub(super) async fn register_pending(host: &Host) {
    let trust = match host.current_trust() {
        Ok(trust) => {
            told(host, None);
            trust
        }
        Err(error) => return tell_unread_trust(host, &error).await,
    };
    for source in &host.registry.sources.plugins {
        for registration in source.registrations(&trust).await {
            let name = registration.name.clone();
            if let Some(why) = reserved::taken(&host.registry, &name) {
                source.registered(&name, Err(why)).await;
                continue;
            }
            let links = registration_links(host).await;
            let result = crate::chain::registration::register(
                registration,
                &links,
                &host.catalogue_context(None),
            )
            .await;
            source.registered(&name, result).await;
        }
    }
}

async fn tell_unread_trust(host: &Host, error: &crate::settings::SettingsError) {
    let path = crate::settings::user_path(host.env());
    let text = format!(
        "plugins waiting for trust stay off: {} cannot be read ({error}); fix it and they register",
        path.display()
    );
    if told(host, Some(&text)) {
        return;
    }
    // Nobody open to hear it is no telling: the next pass says it again.
    if host
        .notice(Level::Warn, "PLUGIN_TRUST_UNREAD", &text)
        .await
        .is_err()
    {
        told(host, None);
    }
}

/// Whether `said` was the last thing told; records it either way.
fn told(host: &Host, said: Option<&str>) -> bool {
    let mut last = host.trust_unread.lock().unwrap_or_else(|e| e.into_inner());
    let same = last.as_deref() == said;
    *last = said.map(str::to_string);
    same
}

async fn registration_links(host: &Host) -> Vec<crate::chain::Link> {
    let off = host
        .registry
        .switches
        .running
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let mut links: Vec<_> = host
        .registry
        .links
        .iter()
        .filter(|link| !off.contains(&link.plugin))
        .cloned()
        .collect();
    let statuses = crate::plugins::listing(&host.registry).await;
    for source in &host.registry.sources.interceptors {
        for external in source.interceptors().await {
            let status = statuses.iter().find(|status| status.id == external.plugin);
            if status.is_some_and(|status| !status.state.is_on()) {
                continue;
            }
            let tier = status
                .and_then(|status| status.grant.as_ref())
                .map(|grant| grant.tier)
                .or_else(|| {
                    host.settings
                        .kernel
                        .plugin_trust
                        .get(&external.plugin)
                        .map(|trust| trust.tier)
                })
                .unwrap_or(Tier::Tighten);
            links.push(crate::chain::Link {
                plugin: external.plugin.clone(),
                placement: bingo_sdk::plugin::Placement::Outer,
                external: Some(external.plugin),
                interceptor: external.link,
                tier,
            });
        }
    }
    crate::chain::ordered(links)
}

async fn protect_grants(host: &Host, catalog: &mut Catalog) {
    let statuses = crate::plugins::listing(&host.registry).await;
    for entry in &mut catalog.entries {
        let Some(status) = statuses.iter().find(|status| status.id == entry.id) else {
            entry.meta = serde_json::json!({"state":{"kind":"failed", "why":"unknown plugin"}});
            continue;
        };
        entry.meta = super::catalog::plugin_entry(status).meta;
    }
}
