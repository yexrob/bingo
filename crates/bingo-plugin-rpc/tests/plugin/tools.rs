//! The two things ADR-0015 opened: a tool and a command, run in another
//! process and answered here.

use std::sync::Arc;
use std::time::Duration;

use bingo_plugin_rpc::Manager;
use bingo_sdk::{CancellationToken, CommandContext, CommandOutcome, SessionId};
use serde_json::json;

use crate::harness::{CALL_ID, Recorder, call, context, only_tool, said, started};

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

/// The calls this process was told to stop, as it read them off the pipe. A
/// drop cannot wait for the notice it sends, so a test that asks about one
/// polls.
async fn cancelled(manager: &Manager, cwd: &std::path::Path) -> String {
    for _ in 0..300 {
        let tool = only_tool(manager).await;
        let (_, answered) = call(&tool, json!({ "cancelled": true }), cwd).await;
        let seen = said(&answered.expect("an output"));
        if !seen.is_empty() {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the process was never told to stop");
}

/// An interrupt is the kernel letting the call go where it stands: nobody
/// reads the answer any more, and `tool/cancel` goes out from the drop. What
/// the process then does about it is the process's own affair — it may still
/// be running the call, and nothing here waits to find out.
#[tokio::test]
async fn a_dropped_call_tells_the_plugin_it_is_no_longer_waited_for() {
    let (manager, _home, project) = started(&[]).await;
    let tool = only_tool(&manager).await;
    let recorder = Arc::new(Recorder::default());
    let cx = context(recorder, project.path(), CancellationToken::new());
    // Boxed rather than pinned in place: this test's whole point is letting
    // the future go, and a `tokio::pin!` handle drops the borrow, not the
    // future.
    let mut running = Box::pin(tool.call(json!({ "awaitCancel": true }), &cx));
    // The call is in flight and the stub is holding it: nothing answers it.
    let held = tokio::time::timeout(Duration::from_millis(200), &mut running).await;
    assert!(
        held.is_err(),
        "the stub answers nothing until it is cancelled"
    );
    drop(running);
    assert_eq!(cancelled(&manager, project.path()).await, CALL_ID);
    manager.shutdown().await;
}
