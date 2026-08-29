//! Filesystem tools: the ones a coding turn cannot do without.

mod ask;
mod diff;
mod edit;
mod glob;
mod grep;
mod output;
mod path;
mod read;
mod write;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Plugin, PluginError, PluginManifest, Registrar, Tool};

pub use ask::{AskArgs, AskOption, AskQuestion, AskUserQuestionTool};
pub use edit::{EditArgs, EditTool};
pub use glob::{GlobArgs, GlobTool};
pub use grep::{GrepArgs, GrepTool, OutputMode};
pub use read::{ReadArgs, ReadTool};
pub use write::{WriteArgs, WriteTool};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tools.fs",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:Read",
        "tool:Glob",
        "tool:Grep",
        "tool:Edit",
        "tool:Write",
        "tool:AskUserQuestion",
    ],
    requires: &[],
    config: None,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct FsPlugin;

#[async_trait]
impl Plugin for FsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.tool(Arc::new(ReadTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(GlobTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(GrepTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(EditTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(WriteTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(AskUserQuestionTool) as Arc<dyn Tool>);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::any::Any;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Contribution, Env, ErrorCode, Input, IntentId,
        InteractionKind, ItemBody, ItemId, KernelError, Prompter, SessionId, SessionSpec,
        ToolContext, ToolHost, TurnId,
    };

    /// A tool host that answers nothing: every tool but `AskUserQuestion`
    /// reaches none of it.
    #[derive(Debug)]
    struct NullHost;

    #[async_trait]
    impl Prompter for NullHost {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }

    #[async_trait]
    impl ToolHost for NullHost {
        fn progress(&self, _item: &ItemId, _tail: String) {}

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

    /// A host that answers from a script and keeps what it was asked, so a
    /// question can be put without a surface to put it to.
    #[derive(Debug, Default)]
    pub(crate) struct ScriptedHost {
        answers: Mutex<VecDeque<Answer>>,
        asked: Mutex<Vec<(InteractionKind, Vec<AnswerSpec>)>>,
    }

    impl ScriptedHost {
        pub(crate) fn new(answers: Vec<Answer>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                asked: Mutex::new(Vec::new()),
            })
        }

        /// Every interaction the tool opened, in order.
        pub(crate) fn asked(&self) -> Vec<(InteractionKind, Vec<AnswerSpec>)> {
            self.asked.lock().map(|a| a.clone()).unwrap_or_default()
        }
    }

    #[async_trait]
    impl Prompter for ScriptedHost {
        async fn ask(
            &self,
            kind: InteractionKind,
            answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            if let Ok(mut asked) = self.asked.lock() {
                asked.push((kind, answers));
            }
            let next = self.answers.lock().ok().and_then(|mut a| a.pop_front());
            next.ok_or_else(|| KernelError::new(ErrorCode::Internal, "the script ran out"))
        }
    }

    #[async_trait]
    impl ToolHost for ScriptedHost {
        fn progress(&self, _item: &ItemId, _tail: String) {}

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

    pub(crate) fn context(cwd: &Path) -> ToolContext {
        context_with(cwd, Arc::new(NullHost))
    }

    pub(crate) fn context_with(cwd: &Path, host: Arc<dyn ToolHost>) -> ToolContext {
        ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd: cwd.to_path_buf(),
            cancel: CancellationToken::new(),
            env: Arc::new(Env {
                home: PathBuf::from("/tmp"),
                config_dir: PathBuf::from("/tmp"),
                data_dir: PathBuf::from("/tmp"),
            }),
            host,
        }
    }

    pub(crate) fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write the fixture");
    }

    #[test]
    fn the_plugin_registers_every_tool_its_manifest_promises() {
        let mut registrar = Registrar::new(
            "bingo.tools.fs",
            serde_json::Value::Null,
            bingo_sdk::Env::rooted("/tmp"),
        );
        FsPlugin.register(&mut registrar).expect("register");
        let names: Vec<String> = registrar
            .into_contributions()
            .iter()
            .map(|c| match c {
                Contribution::Tool(tool) => tool.spec().name.clone(),
                other => panic!("expected a tool, got {other:?}"),
            })
            .collect();
        let promised: Vec<String> = FsPlugin
            .manifest()
            .provides
            .iter()
            .map(|p| p.trim_start_matches("tool:").to_string())
            .collect();
        assert_eq!(names, promised);
    }
}
