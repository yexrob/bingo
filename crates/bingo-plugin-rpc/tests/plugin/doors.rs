//! What ADR-0033 opened: the host's own service, over real child processes.
//!
//! Every call here leaves the process as `service/call` under `bingo.host` and
//! comes back through the tool that asked for it — or, for the notice, through
//! nothing at all, which is the point of that one.

use std::sync::Arc;

use bingo_plugin_rpc::Manager;
use bingo_sdk::{Answer, CancellationToken, InteractionKind, Level, ServiceHandle, Tool};
use serde_json::{Value, json};

use crate::harness::{
    CALL_ID, Recorder, Started, calling, context_id, only_tool, said, started_with,
};

/// The stub's tool, by the plugin that offers it.
async fn tool_of(manager: &Manager, plugin: &str) -> Arc<dyn Tool> {
    let name = format!("plugin__{plugin}__echo");
    manager
        .tools()
        .await
        .into_iter()
        .find(|tool| tool.spec().name == name)
        .unwrap_or_else(|| panic!("{plugin} offers a tool"))
}

fn question(text: &str) -> Value {
    json!({
        "kind": "question",
        "question": text,
        "options": [{ "id": "0", "label": "main" }],
        "freeText": true,
        "multi": false,
    })
}

fn ask(call: &str, question: Value) -> Value {
    json!({ "key": "bingo.host", "method": "ask", "params": { "call": call, "question": question } })
}

/// What the stub's process got back for one call on the host's own service:
/// the answer, or the host's refusal in its own words.
async fn asks(started: &Started, person: Answer, asked: Value) -> (Arc<Recorder>, String) {
    let tool = only_tool(&started.manager).await;
    let (recorder, answered) = calling(
        Arc::new(Recorder::answering(person)),
        &tool,
        json!({ "call": asked }),
        started.project.path(),
    )
    .await;
    (recorder, said(&answered.expect("the tool answered")))
}

/// The exit criterion: a bridge tool asks the person mid-call, the question
/// goes through that call's own asking machinery — the one every in-process
/// tool's question rides — and the answer comes back across the pipe.
#[tokio::test]
async fn a_tool_s_mid_call_question_comes_back_as_the_person_answered() {
    let started = started_with(&[("stub", &[])]).await;
    let (recorder, back) = asks(
        &started,
        Answer::Text {
            text: "next".into(),
        },
        ask(CALL_ID, question("Which branch?")),
    )
    .await;
    let back: Value = serde_json::from_str(&back).expect("the door answered json");
    assert_eq!(
        back,
        json!({ "answer": { "kind": "text", "text": "next" } })
    );

    let asked = recorder.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(
        matches!(&asked[0], InteractionKind::Question { question, .. } if question == "Which branch?"),
        "the person was asked the process's own question: {asked:?}"
    );
    started.manager.shutdown().await;
}

/// A call that is not running is not there to ask on, and nothing was minted
/// that could stand in for it.
#[tokio::test]
async fn a_call_that_is_not_running_is_refused_in_words() {
    let started = started_with(&[("stub", &[])]).await;
    let (recorder, refused) = asks(
        &started,
        Answer::Cancel,
        ask("call_gone", question("Which branch?")),
    )
    .await;
    assert_eq!(
        refused,
        "the call call_gone is not one the stub plugin is running: it has ended, or it was never this plugin's"
    );
    assert!(recorder.asked().is_empty(), "nobody was asked anything");
    started.manager.shutdown().await;
}

/// The refusal the door exists for: one plugin's live call is not another
/// plugin's to ask on. The held call is proved live by its own progress line
/// arriving before the second plugin asks — no sleep decides this.
#[tokio::test]
async fn another_plugin_s_live_call_is_refused_in_words() {
    let started = started_with(&[("one", &[]), ("two", &[])]).await;
    let mine = tool_of(&started.manager, "one").await;
    let held = Arc::new(Recorder::default());
    let cancel = CancellationToken::new();
    let cx = context_id(
        "call_one",
        Arc::clone(&held),
        started.project.path(),
        cancel.clone(),
    );
    let running = tokio::spawn(async move {
        mine.call(json!({ "awaitCancel": true, "progress": ["holding"] }), &cx)
            .await
    });
    while held.progress().is_empty() {
        tokio::task::yield_now().await;
    }

    let theirs = tool_of(&started.manager, "two").await;
    let (_, answered) = calling(
        Arc::new(Recorder::default()),
        &theirs,
        json!({ "call": ask("call_one", question("Which branch?")) }),
        started.project.path(),
    )
    .await;
    assert_eq!(
        said(&answered.expect("the tool answered")),
        "the call call_one is not one the two plugin is running: it has ended, or it was never this plugin's"
    );
    assert!(
        held.asked().is_empty(),
        "the call that is running was never asked anything"
    );

    cancel.cancel();
    let answered = running.await.expect("the held call is joined");
    assert_eq!(said(&answered.expect("it answered")), "cancelled");
    started.manager.shutdown().await;
}

/// The verdict plane is not a door (ADR-0033 Consequences): a process may put
/// a question, never a prompt whose answer is a permission.
#[tokio::test]
async fn a_permission_prompt_is_not_a_question_a_plugin_may_open() {
    let started = started_with(&[("stub", &[])]).await;
    let (recorder, refused) = asks(
        &started,
        Answer::AllowOnce,
        ask(
            CALL_ID,
            json!({ "kind": "permission", "tool": "Bash", "summary": "rm -rf /" }),
        ),
    )
    .await;
    assert_eq!(
        refused,
        "a plugin may ask a question; a permission, a confirmation and a login are the host's own"
    );
    assert!(recorder.asked().is_empty(), "nothing was put to the person");
    started.manager.shutdown().await;
}

/// M28's rule, on the host's own service: a method that is not a door is
/// answered with the doors that are.
#[tokio::test]
async fn a_door_the_host_does_not_have_is_answered_with_the_ones_it_does() {
    let started = started_with(&[("stub", &[])]).await;
    let (_, refused) = asks(
        &started,
        Answer::Cancel,
        json!({ "key": "bingo.host", "method": "complete", "params": {} }),
    )
    .await;
    assert_eq!(
        refused,
        "the service bingo.host does not speak complete; it speaks ask, notice"
    );
    started.manager.shutdown().await;
}

/// The other exit criterion: a plugin says one line the moment it starts, with
/// no tool call anywhere in this test, and the person hears it. This is the
/// drain M29 carried, fixed — the notice does not wait for a call that a
/// session may never make.
#[tokio::test]
async fn a_plugin_s_own_line_surfaces_with_no_tool_call_in_flight() {
    let started = started_with(&[("stub", &["--announce", "the index is stale"])]).await;
    let (level, text) = started.heard("PLUGIN_NOTICE").await;
    assert_eq!(level, Level::Warn);
    assert_eq!(
        text, "stub: the index is stale",
        "under the name the plugin is installed as"
    );
    started.manager.shutdown().await;
}

/// It is a service like any other: registered through `open_service`, so an
/// in-process consumer finds it under its key by the one lookup, and the key
/// is taken before any process is spawned.
#[tokio::test]
async fn the_host_s_own_service_is_in_the_registry_under_its_key() {
    let started = started_with(&[("stub", &[])]).await;
    let doors = started
        .host
        .service::<ServiceHandle>("bingo.host")
        .expect("the bridge entered it through open_service");
    let why = doors
        .call(
            "ask",
            json!({ "call": CALL_ID, "question": question("Which?") }),
        )
        .await
        .expect_err("the registry's face runs no plugin's call")
        .to_string();
    assert_eq!(why, "the host is running no call of its own to ask on");
    started.manager.shutdown().await;
}
