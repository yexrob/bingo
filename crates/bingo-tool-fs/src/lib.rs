//! Filesystem tools: the ones a coding turn cannot do without.

mod glob;
mod output;
mod path;
mod read;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Plugin, PluginError, PluginManifest, Registrar, Tool};

pub use glob::{GlobArgs, GlobTool};
pub use read::{ReadArgs, ReadTool};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tools.fs",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["tool:Read", "tool:Glob"],
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
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::any::Any;
    use std::path::{Path, PathBuf};

    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Contribution, Env, Input, IntentId, InteractionKind,
        ItemBody, ItemId, KernelError, Prompter, SessionId, SessionSpec, ToolContext, ToolHost,
        TurnId,
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

    pub(crate) fn context(cwd: &Path) -> ToolContext {
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
            host: Arc::new(NullHost),
        }
    }

    pub(crate) fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write the fixture");
    }

    #[test]
    fn the_plugin_registers_every_tool_its_manifest_promises() {
        let mut registrar = Registrar::new("bingo.tools.fs", serde_json::Value::Null);
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
