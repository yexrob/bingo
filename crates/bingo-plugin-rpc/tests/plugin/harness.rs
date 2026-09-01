//! What every test here needs: the example binary, a home with it installed,
//! a manager started over that home, and the tool host one call is given.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bingo_plugin_rpc::Manager;
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, Env, InteractionKind, ItemBody, ItemId, KernelError,
    Prompter, SessionId, Tool, ToolContext, ToolError, ToolHost, ToolOutput, TurnId,
};
use serde_json::{Value, json};

/// The example binary, beside the test binary that is running.
pub fn stub_plugin() -> PathBuf {
    let test = std::env::current_exe().expect("a running test binary knows its own path");
    let profile = test
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives at target/<profile>/deps/<test>");
    let stub = profile.join("examples").join("stub_plugin");
    assert!(
        stub.exists(),
        "cargo test builds this crate's examples; {} is missing",
        stub.display()
    );
    stub
}

/// A home whose `<config_dir>/plugins/stub` holds a manifest for the example.
pub fn installed(args: &[&str]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a home");
    let root = home.path().join(".bingo/plugins/stub");
    std::fs::create_dir_all(&root).expect("a plugin directory");
    let manifest = json!({
        "name": "stub",
        "version": "0.1.0",
        "entry": {
            "command": stub_plugin().display().to_string(),
            "args": args,
            "env": { "PLUGIN_HOME": "${PLUGIN_ROOT}" }
        }
    });
    std::fs::write(root.join("plugin.json"), manifest.to_string()).expect("a manifest");
    home
}

/// A manager over that home, started, with an empty project beside it.
pub async fn started(args: &[&str]) -> (Arc<Manager>, tempfile::TempDir, tempfile::TempDir) {
    let home = installed(args);
    let project = tempfile::tempdir().expect("a project");
    let manager = Arc::new(Manager::new(Env::rooted(home.path()), BTreeMap::new()));
    manager.start(project.path()).await;
    (manager, home, project)
}

pub async fn only_tool(manager: &Manager) -> Arc<dyn Tool> {
    let mut tools = manager.tools().await;
    assert_eq!(tools.len(), 1, "the stub offers one tool");
    tools.remove(0)
}

/// Poll until the plugin offers a tool again, or give up. A respawn is
/// deliberately asynchronous, so a test that asks about it polls.
pub async fn respawned(manager: &Manager) -> Vec<Arc<dyn Tool>> {
    for _ in 0..300 {
        let tools = manager.tools().await;
        if !tools.is_empty() {
            return tools;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the plugin never came back");
}

// --------------------------------------------------------------- the double

/// A tool host that keeps what a call told it, so a test can read the live
/// output line and the notices the transcript was given.
#[derive(Debug, Default)]
pub struct Recorder {
    progress: Mutex<Vec<String>>,
    recorded: Mutex<Vec<ItemBody>>,
}

impl Recorder {
    pub fn progress(&self) -> Vec<String> {
        self.progress.lock().unwrap().clone()
    }

    pub fn recorded(&self) -> Vec<ItemBody> {
        self.recorded.lock().unwrap().clone()
    }
}

#[async_trait]
impl Prompter for Recorder {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        Ok(Answer::Cancel)
    }
}

#[async_trait]
impl ToolHost for Recorder {
    fn progress(&self, _item: &ItemId, tail: String) {
        self.progress.lock().unwrap().push(tail);
    }

    async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError> {
        self.recorded.lock().unwrap().push(body);
        Ok(ItemId::from_raw("itm_test"))
    }
}

pub fn context(call: Arc<Recorder>, cwd: &Path, cancel: CancellationToken) -> ToolContext {
    ToolContext {
        call_id: "call_test".into(),
        session: SessionId::from_raw("ses_test"),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: cwd.to_path_buf(),
        cancel,
        env: Arc::new(Env::rooted("/nowhere")),
        host: bingo_sdk::testing::NoHost::handle(),
        call,
    }
}

pub async fn call(
    tool: &Arc<dyn Tool>,
    input: Value,
    cwd: &Path,
) -> (Arc<Recorder>, Result<ToolOutput, ToolError>) {
    let recorder = Arc::new(Recorder::default());
    let cx = context(Arc::clone(&recorder), cwd, CancellationToken::new());
    let answered = tool.call(input, &cx).await;
    (recorder, answered)
}

pub fn said(output: &ToolOutput) -> String {
    output.parts[0].as_text().unwrap_or_default().to_string()
}
