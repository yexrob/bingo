use bingo_sdk::{HostHandle, KernelError, PluginLifecycle};
use serde_json::Value;

pub struct Disable(pub crate::Shared);
#[async_trait::async_trait]
impl PluginLifecycle for Disable {
    async fn disabled(&self, host: &HostHandle) -> Result<(), KernelError> {
        let sessions = {
            let mut cursors = self.0.lock().map_err(crate::error)?;
            std::mem::take(&mut *cursors)
                .into_keys()
                .collect::<Vec<_>>()
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

#[cfg(test)]
mod tests {
    use super::Disable;
    use bingo_sdk::{ErrorCode, KernelError, PluginLifecycle, SessionId, testing};

    #[tokio::test]
    async fn confirmed_disable_retires_every_schedule_panel_without_catalogue_reads() {
        let host = testing::TestHost::default();
        host.on_signal(|_, _, _, _| async { Ok(()) });
        let shared = crate::Shared::default();
        let sessions = [SessionId::from_raw("one"), SessionId::from_raw("two")];
        for session in &sessions {
            shared
                .lock()
                .unwrap()
                .insert(session.clone(), Default::default());
        }
        let lifecycle = Disable(shared.clone());
        lifecycle.disabled(&host.handle()).await.unwrap();
        assert!(shared.lock().unwrap().is_empty());
        let calls = host.calls();
        assert_eq!(calls.len(), sessions.len());
        for (call, expected) in calls.iter().zip(&sessions) {
            let testing::HostCall::Signal {
                session,
                plugin,
                kind,
                payload,
            } = call
            else {
                panic!("cleanup only removes its own pane signals");
            };
            assert_eq!(session, expected);
            assert_eq!(plugin, crate::ID);
            assert_eq!(kind, crate::REQUEST);
            assert!(payload.is_null());
        }
        lifecycle.disabled(&host.handle()).await.unwrap();
        assert_eq!(
            host.calls().len(),
            2,
            "retired panels are not touched again"
        );
    }

    #[tokio::test]
    async fn a_failed_signal_still_retires_later_sessions_and_clears_all_state() {
        let host = testing::TestHost::default();
        let shared = crate::Shared::default();
        let sessions = [SessionId::from_raw("one"), SessionId::from_raw("two")];
        for session in &sessions {
            shared
                .lock()
                .unwrap()
                .insert(session.clone(), Default::default());
        }
        let observed = shared.clone();
        host.on_signal(move |session, _, _, _| {
            let observed = observed.clone();
            async move {
                assert!(
                    observed.lock().unwrap().is_empty(),
                    "state retires before signalling"
                );
                if session == SessionId::from_raw("one") {
                    Err(KernelError::new(
                        ErrorCode::SessionNotFound,
                        "first session disappeared",
                    ))
                } else {
                    Ok(())
                }
            }
        });
        let error = Disable(shared.clone())
            .disabled(&host.handle())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            KernelError::new(ErrorCode::SessionNotFound, "first session disappeared")
        );
        assert!(shared.lock().unwrap().is_empty());
        let calls = host.calls();
        assert_eq!(calls.len(), sessions.len());
        for (call, expected) in calls.iter().zip(&sessions) {
            let testing::HostCall::Signal { session, .. } = call else {
                panic!("cleanup only removes pane signals");
            };
            assert_eq!(session, expected);
        }
    }
}
