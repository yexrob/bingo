//! One publisher per adapter, shared by every conversation of the app.
use crate::adapter::{ChannelAdapter, MenuEntry};
use bingo_sdk::{Catalog, CatalogKind, GatewayEvent, HostHandle, Level};
use futures::StreamExt;
use std::{sync::Arc, time::Duration};

const INTERVAL: Duration = Duration::from_secs(60);
const CAP: usize = 100;

pub fn entries(catalog: &Catalog, mapped: Vec<MenuEntry>) -> (Vec<MenuEntry>, bool) {
    let mut entries: Vec<_> = catalog
        .entries
        .iter()
        .filter(|e| e.meta["hidden"] != true && e.meta["family"] != "terminal")
        .map(|e| MenuEntry {
            name: e.id.trim_start_matches('/').into(),
            description: e.label.clone(),
        })
        .collect();
    for entry in mapped {
        if let Some(old) = entries.iter_mut().find(|e| e.name == entry.name) {
            *old = entry;
        } else {
            entries.push(entry);
        }
    }
    let truncated = entries.len() > CAP;
    entries.truncate(CAP);
    (entries, truncated)
}

pub async fn run(
    host: &HostHandle,
    adapters: &[Arc<dyn ChannelAdapter>],
    changes: &tokio::sync::watch::Sender<bool>,
) {
    let publishers = adapters
        .iter()
        .filter(|a| a.menu().is_some())
        .map(|a| publish_loop(host, a, changes.subscribe()));
    futures::future::join_all(publishers).await;
    std::future::pending::<()>().await;
}

async fn publish_loop(
    host: &HostHandle,
    adapter: &Arc<dyn ChannelAdapter>,
    mut changes: tokio::sync::watch::Receiver<bool>,
) {
    let mut events = host.gateway_events().chain(futures::stream::pending());
    let mut dirty = false;
    let mut next = tokio::time::Instant::now() + INTERVAL;
    let mut warned = false;
    publish(host, adapter.as_ref(), &mut warned).await;
    loop {
        tokio::select! {
            change = changes.changed() => { if change.is_err() { return; } dirty = true; },
            event = events.next() => match event {
                Some(GatewayEvent::CatalogChanged { kind: CatalogKind::Commands }) => dirty = true,
                Some(_) => {},
                None => return,
            },
            _ = tokio::time::sleep_until(next), if dirty => {
                publish(host, adapter.as_ref(), &mut warned).await;
                dirty = false;
                next = tokio::time::Instant::now() + INTERVAL;
            }
        }
    }
}

async fn publish(host: &HostHandle, adapter: &dyn ChannelAdapter, warned: &mut bool) {
    let Some(menu) = adapter.menu() else {
        return;
    };
    let catalog = match host.catalog(CatalogKind::Commands).await {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "the command menu could not read the catalogue");
            return;
        }
    };
    let (entries, truncated) = entries(&catalog, adapter.mapped_commands());
    if truncated && !*warned {
        *warned = true;
        let _ = host
            .notice(
                Level::Warn,
                "channel.menu.limit",
                "The command menu shows only the first 100 commands.",
            )
            .await;
        tracing::warn!("the command menu shows only the first 100 commands");
    }
    if let Err(error) = menu.publish(&entries).await {
        let _ = host
            .notice(Level::Warn, "channel.menu", &error.to_string())
            .await;
        tracing::warn!(%error, "the command menu could not be published");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::CatalogEntry;
    #[test]
    fn the_catalogue_order_is_kept_hidden_entries_are_absent_and_mappings_win() {
        let catalog = Catalog {
            kind: CatalogKind::Commands,
            entries: (0..102)
                .map(|i| CatalogEntry {
                    id: format!("c{i}"),
                    label: format!("Command {i}"),
                    meta: serde_json::json!({"hidden": i == 0}),
                })
                .collect(),
        };
        let (entries, truncated) = entries(
            &catalog,
            vec![MenuEntry {
                name: "c1".into(),
                description: "mapped".into(),
            }],
        );
        assert!(truncated);
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[0].name, "c1");
        assert_eq!(entries[0].description, "mapped");
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use crate::loopback::{Config, Loopback, Record};
    use bingo_sdk::testing::TestHost;
    #[tokio::test(start_paused = true)]
    async fn catalogue_changes_share_one_debounced_publisher() {
        let host = TestHost::default();
        host.on_catalog(|kind| async move {
            Ok(Catalog {
                kind,
                entries: vec![],
            })
        });
        let gateway = host.gateway();
        let adapter = Arc::new(Loopback::new(Config {
            menu: true,
            ..Default::default()
        }));
        let (changes, rx) = tokio::sync::watch::channel(false);
        let handle = host.handle();
        let platform: Arc<dyn ChannelAdapter> = adapter.clone();
        let job = tokio::spawn(async move { publish_loop(&handle, &platform, rx).await });
        tokio::task::yield_now().await;
        assert_eq!(adapter.records().len(), 1);
        for _ in 0..3 {
            gateway
                .unbounded_send(GatewayEvent::CatalogChanged {
                    kind: CatalogKind::Commands,
                })
                .unwrap();
        }
        changes.send_modify(|v| *v = !*v);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert_eq!(adapter.records().len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(adapter.records().len(), 2);
        let Record::Menu { entries } = &adapter.records()[1] else {
            panic!("menu")
        };
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["new", "stop"]
        );
        job.abort();
    }
}

#[cfg(test)]
mod terminal_commands {
    use super::*;
    #[test]
    fn terminal_commands_are_absent_but_folded_text_commands_stay() {
        let catalog = Catalog {
            kind: CatalogKind::Commands,
            entries: vec![
                bingo_sdk::CatalogEntry {
                    id: "help".into(),
                    label: "keys".into(),
                    meta: serde_json::json!({"family":"terminal"}),
                },
                bingo_sdk::CatalogEntry {
                    id: "status".into(),
                    label: "status".into(),
                    meta: serde_json::json!({"family":"session"}),
                },
                bingo_sdk::CatalogEntry {
                    id: "agents".into(),
                    label: "list agents".into(),
                    meta: serde_json::json!({"family":"session","foldedInto":"crew"}),
                },
            ],
        };
        let (menu, _) = entries(&catalog, vec![]);
        assert_eq!(
            menu.iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["status", "agents"]
        );
    }
}
