//! A running-state transition reaches its owner's cleanup after dispatch stops.
use std::sync::Arc;

use bingo_sdk::{HOOK_DEADLINE, HostApi, Level, PluginLifecycle};
use futures::FutureExt;

use super::Host;

pub(super) struct OwnedLifecycle {
    pub plugin: String,
    pub observer: Arc<dyn PluginLifecycle>,
}

pub(crate) async fn disabled(host: &Host, plugin: &str) {
    for entry in host
        .registry
        .switches
        .lifecycle
        .iter()
        .filter(|entry| entry.plugin == plugin)
    {
        let callback =
            std::panic::AssertUnwindSafe(entry.observer.disabled(&host.plugin_host)).catch_unwind();
        let error = match tokio::time::timeout(HOOK_DEADLINE, callback).await {
            Ok(Ok(Ok(()))) => continue,
            Ok(Ok(Err(error))) => error.message,
            Ok(Err(_)) => "cleanup panicked".into(),
            Err(_) => "cleanup timed out".into(),
        };
        tracing::warn!(plugin, %error, "plugin disable cleanup failed");
        let text = format!("{plugin} is off; {error}");
        let _ = host
            .notice(Level::Warn, "PLUGIN_DISABLE_FAILED", &text)
            .await;
    }
}
