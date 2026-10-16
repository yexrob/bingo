use bingo_sdk::{HostHandle, KernelError, PluginLifecycle};
use serde_json::Value;

pub struct Disable(pub crate::Shared);

#[async_trait::async_trait]
impl PluginLifecycle for Disable {
    async fn disabled(&self, host: &HostHandle) -> Result<(), KernelError> {
        let sessions = {
            let mut panels = self.0.lock().map_err(crate::error)?;
            std::mem::take(&mut *panels).into_keys().collect::<Vec<_>>()
        };
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = host
                .signal(&session, crate::ID, crate::REQUEST, Value::Null)
                .await
            {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
