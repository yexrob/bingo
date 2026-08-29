//! The `Bash` tool: one shell command per call, in its own process group.
//!
//! The plugin is assembled from four bricks: the tables that refuse a command
//! before anything is spawned ([`reject`]), the bounded [`output`], the
//! [`tail`] the user watches while the command runs, and the process lifecycle.
//! See docs/plans/M1-provider-tools-gate.md.

pub mod output;
pub mod reject;
pub mod tail;

#[cfg(test)]
pub(crate) mod tests {
    use std::any::Any;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Env, Input, IntentId, InteractionKind, ItemBody,
        ItemId, KernelError, Prompter, SessionId, SessionSpec, ToolContext, ToolHost, TurnId,
    };

    /// A tool host that keeps every progress tail it is handed, which is all a
    /// shell command ever reaches for.
    #[derive(Debug, Default)]
    pub(crate) struct RecordingHost {
        tails: Mutex<Vec<String>>,
    }

    impl RecordingHost {
        pub(crate) fn tails(&self) -> Vec<String> {
            self.tails.lock().expect("the tails").clone()
        }
    }

    #[async_trait]
    impl Prompter for RecordingHost {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }

    #[async_trait]
    impl ToolHost for RecordingHost {
        fn progress(&self, _item: &ItemId, tail: String) {
            self.tails.lock().expect("the tails").push(tail);
        }

        async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
            Ok(ItemId::from_raw("itm_test"))
        }

        async fn spawn_session(&self, _spec: SessionSpec) -> Result<SessionId, KernelError> {
            Ok(SessionId::from_raw("ses_test"))
        }

        fn submit(&self, _to: &SessionId, _intent: IntentId, _input: Input) {}

        fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
            None
        }
    }

    /// A call context in the current directory, and the host recording it.
    pub(crate) fn context() -> (Arc<RecordingHost>, ToolContext) {
        context_in(std::env::temp_dir())
    }

    pub(crate) fn context_in(cwd: PathBuf) -> (Arc<RecordingHost>, ToolContext) {
        let host = Arc::new(RecordingHost::default());
        let cx = ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd,
            cancel: CancellationToken::new(),
            env: Arc::new(Env {
                home: PathBuf::from("/tmp"),
                config_dir: PathBuf::from("/tmp"),
                data_dir: PathBuf::from("/tmp"),
            }),
            host: host.clone(),
        };
        (host, cx)
    }
}
