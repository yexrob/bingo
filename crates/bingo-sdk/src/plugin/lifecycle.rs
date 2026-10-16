use async_trait::async_trait;

use crate::{HostHandle, KernelError};

/// Cleanup owned by the plugin that registers this contribution.
#[async_trait]
pub trait PluginLifecycle: Send + Sync {
    /// Called after this resident plugin changes from running to disabled.
    ///
    /// This is not a pending wish, boot-off or process shutdown notification.
    /// The host awaits callbacks in registration order before acknowledging the
    /// switch, even though the owner's ordinary contributions are already off.
    /// Errors, panics and exceeding [`crate::HOOK_DEADLINE`] produce warnings;
    /// remaining callbacks still run and the confirmed disable is not undone.
    ///
    /// Catalogue reads and cleanup signals are available. Do not synchronously
    /// await another plugin switch: switches are serialized through cleanup.
    async fn disabled(&self, host: &HostHandle) -> Result<(), KernelError>;
}
