//! Plugin presentation facts read from their registered owners.
use crate::host::Host;
use bingo_sdk::*;
use serde_json::json;

pub(in crate::host) async fn details(host: &Host, catalog: &mut Catalog) {
    let links = host.hook_set().links().await;
    for entry in &mut catalog.entries {
        entry.meta["needed"] = json!(host.needed(&entry.id));
        if let Some(plugin) = host
            .plugins
            .iter()
            .find(|plugin| plugin.manifest().id == entry.id)
        {
            entry.meta["provides"] = json!(plugin.manifest().provides);
        }
        entry.meta["commands"] = json!(
            host.registry
                .commands
                .entries
                .iter()
                .filter(|command| command.plugin == entry.id)
                .map(|command| command.spec())
                .filter(|spec| !spec.hidden)
                .collect::<Vec<_>>()
        );
        let mut contents = plugin::PluginContents::default();
        for source in &host.registry.sources.plugins {
            let found = source.contents(&entry.id).await;
            contents.commands.extend(found.commands);
            contents.tools.extend(found.tools);
        }
        if let Some(commands) = entry.meta["commands"].as_array_mut() {
            commands.extend(contents.commands.into_iter().map(|spec| json!(spec)));
        }
        entry.meta["tools"] = json!(contents.tools);
        entry.meta["links"] = json!(
            links
                .iter()
                .enumerate()
                .filter(|(_, link)| link.plugin == entry.id)
                .map(|(order, link)| json!({"order":order,"matcher":link.interceptor.matcher()}))
                .collect::<Vec<_>>()
        );
    }
}
