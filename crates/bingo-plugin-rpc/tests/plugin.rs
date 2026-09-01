//! What the bridge does against a real plugin process.
//!
//! The process is this crate's `stub_plugin` example: `cargo test` builds a
//! crate's examples, so the binary is always beside the test binary
//! (`target/<profile>/examples/stub_plugin` next to `target/<profile>/deps/`),
//! with no build script and no second manifest. Everything here discovers it
//! the way a person's `plugins/` directory would be discovered.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bingo_plugin_rpc::{Manager, log_path};
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, CommandContext, CommandOutcome, Env, InteractionKind,
    Interrupt, ItemBody, ItemId, KernelError, Level, Prompter, SessionId, Tool, ToolContext,
    ToolError, ToolHost, ToolOutput, TurnId,
};
use serde_json::{Value, json};

// ------------------------------------------------------------- the fixtures

/// The example binary, beside the test binary that is running.
fn stub_plugin() -> PathBuf {
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
fn installed(args: &[&str]) -> tempfile::TempDir {
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
async fn started(args: &[&str]) -> (Arc<Manager>, tempfile::TempDir, tempfile::TempDir) {
    let home = installed(args);
    let project = tempfile::tempdir().expect("a project");
    let manager = Arc::new(Manager::new(Env::rooted(home.path()), BTreeMap::new()));
    manager.start(project.path()).await;
    (manager, home, project)
}

async fn only_tool(manager: &Manager) -> Arc<dyn Tool> {
    let mut tools = manager.tools().await;
    assert_eq!(tools.len(), 1, "the stub offers one tool");
    tools.remove(0)
}

/// Poll until the plugin offers a tool again, or give up. A respawn is
/// deliberately asynchronous, so a test that asks about it polls.
async fn respawned(manager: &Manager) -> Vec<Arc<dyn Tool>> {
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
struct Recorder {
    progress: Mutex<Vec<String>>,
    recorded: Mutex<Vec<ItemBody>>,
}

impl Recorder {
    fn progress(&self) -> Vec<String> {
        self.progress.lock().unwrap().clone()
    }

    fn recorded(&self) -> Vec<ItemBody> {
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

fn context(call: Arc<Recorder>, cwd: &Path, cancel: CancellationToken) -> ToolContext {
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

async fn call(
    tool: &Arc<dyn Tool>,
    input: Value,
    cwd: &Path,
) -> (Arc<Recorder>, Result<ToolOutput, ToolError>) {
    let recorder = Arc::new(Recorder::default());
    let cx = context(Arc::clone(&recorder), cwd, CancellationToken::new());
    let answered = tool.call(input, &cx).await;
    (recorder, answered)
}

fn said(output: &ToolOutput) -> String {
    output.parts[0].as_text().unwrap_or_default().to_string()
}

// ----------------------------------------------------------------- the tests

#[tokio::test]
async fn a_plugin_s_tools_are_named_for_it_and_are_untrusted() {
    let (manager, _home, _project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let spec = tool.spec();
    assert_eq!(spec.name, "plugin__stub__echo");
    assert_eq!(spec.meta["plugin"], json!("stub"));
    let traits = tool.traits(&json!({}));
    assert!(
        !traits.trusted,
        "nothing a process says about itself is a fact"
    );
    assert!(!traits.read_only && !traits.concurrency_safe);
    assert_eq!(traits.interrupt, Interrupt::Block);
    manager.shutdown().await;
}

#[tokio::test]
async fn a_plugin_s_commands_keep_the_name_the_plugin_gave_them() {
    let (manager, _home, project) = started(&[]).await;
    let commands = manager.commands().await;
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].spec().name, "stub");
    let cx = CommandContext {
        session: SessionId::from_raw("ses_test"),
        cwd: project.path().to_path_buf(),
        host: bingo_sdk::testing::NoHost::handle(),
    };
    let outcome = commands[0].run("two words", &cx).await.expect("it ran");
    let CommandOutcome::Applied { message } = outcome else {
        panic!("the stub answers with an applied outcome");
    };
    let message = message.expect("with a message");
    assert!(message.starts_with("stub in "), "{message}");
    assert!(message.ends_with(": two words"), "{message}");
    manager.shutdown().await;
}

#[tokio::test]
async fn a_call_crosses_the_pipe_and_the_output_comes_back() {
    let (manager, _home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let (_, answered) = call(&tool, json!({ "text": "hello" }), project.path()).await;
    assert_eq!(said(&answered.expect("an output")), "hello");
    manager.shutdown().await;
}

#[tokio::test]
async fn the_plugin_root_reaches_the_process_through_its_environment() {
    let (manager, home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let (_, answered) = call(&tool, json!({ "env": "PLUGIN_HOME" }), project.path()).await;
    assert_eq!(
        said(&answered.expect("an output")),
        home.path()
            .join(".bingo/plugins/stub")
            .display()
            .to_string()
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_progress_notification_becomes_the_call_s_live_output_line() {
    let (manager, _home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let (recorder, answered) = call(
        &tool,
        json!({ "progress": ["reading", "counting"], "text": "done" }),
        project.path(),
    )
    .await;
    assert_eq!(said(&answered.expect("an output")), "done");
    assert_eq!(recorder.progress(), ["reading", "counting"]);
    manager.shutdown().await;
}

/// A bridge tool's `Interrupt` is `Block`: the interrupt is passed down as
/// `tool/cancel` and the call is still awaited, because a write dropped
/// mid-flight is in an unknown state.
#[tokio::test]
async fn an_interrupt_reaches_the_plugin_and_the_call_still_answers() {
    let (manager, _home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let recorder = Arc::new(Recorder::default());
    let cancel = CancellationToken::new();
    let cx = context(Arc::clone(&recorder), project.path(), cancel.clone());
    let running = tool.call(json!({ "awaitCancel": true }), &cx);
    tokio::pin!(running);
    // The call is in flight and the stub is holding it; only the cancel frees it.
    let held = tokio::time::timeout(Duration::from_millis(200), &mut running).await;
    assert!(
        held.is_err(),
        "the stub answers nothing until it is cancelled"
    );
    cancel.cancel();
    let output = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the cancel reached the plugin")
        .expect("an output");
    assert_eq!(said(&output), "cancelled");
    manager.shutdown().await;
}

#[tokio::test]
async fn a_plugin_s_stderr_goes_to_a_log_under_the_data_directory() {
    let (manager, home, _project) = started(&[]).await;
    let log = log_path(&Env::rooted(home.path()).data_dir, "stub");
    assert!(log.exists(), "{} was never opened", log.display());
    manager.shutdown().await;
}

/// The exit criterion of ADR-0015 §5: a dead process answers nothing, says so
/// once, and is back on the next read.
#[tokio::test]
async fn a_killed_process_leaves_one_notice_empty_sources_and_a_working_respawn() {
    let (manager, _home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;

    let (recorder, answered) = call(&tool, json!({ "die": true }), project.path()).await;
    let error = answered.expect_err("a process that ended answers nothing");
    assert!(
        matches!(&error, ToolError::Failed(why) if why.starts_with("stub: ")),
        "{error}"
    );
    let notices = recorder.recorded();
    assert_eq!(notices.len(), 1, "one death is one notice: {notices:?}");
    let ItemBody::Notice { level, code, text } = &notices[0] else {
        panic!("the transcript was given a notice");
    };
    assert_eq!(*level, Level::Warn);
    assert_eq!(code, "PLUGIN_DIED");
    assert!(text.contains("stub"), "{text}");

    assert!(
        manager.tools().await.is_empty(),
        "a dead plugin's source answers nothing"
    );
    assert_eq!(respawned(&manager).await.len(), 1, "and it comes back");

    let tool = only_tool(&manager).await;
    let (_, answered) = call(&tool, json!({ "text": "alive again" }), project.path()).await;
    assert_eq!(said(&answered.expect("an output")), "alive again");
    manager.shutdown().await;
}

#[tokio::test]
async fn an_unknown_protocol_major_refuses_the_handshake_with_a_notice() {
    let (manager, _home, _project) = started(&["--protocol", "99"]).await;
    let said = manager.notices().drain();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].code, "PLUGIN_UNAVAILABLE");
    assert!(said[0].text.contains("protocol 99"), "{}", said[0].text);
    assert!(
        manager.tools().await.is_empty(),
        "a plugin whose wire is unknown contributes nothing"
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn a_plugin_whose_command_is_gone_is_reported_and_contributes_nothing() {
    let home = tempfile::tempdir().expect("a home");
    let root = home.path().join(".bingo/plugins/missing");
    std::fs::create_dir_all(&root).expect("a plugin directory");
    std::fs::write(
        root.join("plugin.json"),
        json!({
            "name": "missing",
            "version": "0.1.0",
            "entry": { "command": "bingo-no-such-plugin" }
        })
        .to_string(),
    )
    .expect("a manifest");
    let project = tempfile::tempdir().expect("a project");
    let manager = Manager::new(Env::rooted(home.path()), BTreeMap::new());
    manager.start(project.path()).await;
    let said = manager.notices().drain();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].code, "PLUGIN_UNAVAILABLE");
    assert!(manager.tools().await.is_empty());
    assert!(manager.commands().await.is_empty());
    manager.shutdown().await;
}
