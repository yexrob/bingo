//! What ADR-0032 opened: a hook registered at the kernel's points, living in
//! another process, over a real child.
//!
//! The stub declares four — `guard` on `beforeTool` for one tool name,
//! `prefix` on `submit`, `watch` on two observation points, and `silent`,
//! which is asked and never answers. Every crossing is written into the map
//! its `kv` service serves, so what did and did not reach the process is read
//! back out of the process's own memory rather than inferred.

use std::sync::Arc;
use std::time::Duration;

use bingo_plugin_rpc::Manager;
use bingo_sdk::{
    Hook, HookMatcher, HookOutcome, HookPoint, Input, Origin, Phase, ServiceHandle, ToolCall,
};
use serde_json::{Value, json};

use crate::harness::{Started, hook_context, started_with};

async fn with_hooks() -> Started {
    started_with(&[("stub", &[])]).await
}

/// The hook the stub declared under this name, as the kernel would see it.
async fn hook_of(manager: &Manager, id: &str) -> Arc<dyn Hook> {
    let want = format!("stub:{id}");
    manager
        .hooks()
        .await
        .into_iter()
        .find(|hook| hook.id() == want)
        .unwrap_or_else(|| panic!("the stub declares a {id} hook"))
}

/// What the process wrote down about the crossings it saw, read through its
/// own service. `null` is a process nothing has reached.
async fn crossings(started: &Started) -> Value {
    let kv = started
        .host
        .service::<ServiceHandle>("kv")
        .expect("the stub serves kv");
    kv.call("get", json!({ "key": "hooks" }))
        .await
        .expect("the read crossed")
}

fn bash(input: Value) -> ToolCall {
    ToolCall {
        call_id: "c1".into(),
        name: "Bash".into(),
        input,
    }
}

/// The exit criterion of ADR-0032: a hook in another process rewrites the call
/// the model asked for, and the host applies what came back.
#[tokio::test]
async fn a_matched_call_is_rewritten_by_a_hook_in_another_process() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "guard").await;
    let mut call = bash(json!({ "command": "ls" }));
    let outcome = hook.before_tool(&mut call, &hook_context()).await;
    assert_eq!(outcome, HookOutcome::Continue);
    assert_eq!(
        call.input,
        json!({ "command": "ls --safe" }),
        "the process's rewrite replaced the call the model asked for"
    );
    assert_eq!(crossings(&started).await, json!("guard/beforeTool"));
    started.manager.shutdown().await;
}

/// Tighten-only, from the far side: the refusal is the hook's own words, and
/// the call it refused is untouched.
#[tokio::test]
async fn a_hook_in_another_process_refuses_a_call_in_its_own_words() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "guard").await;
    let mut call = bash(json!({ "deny": "not that one" }));
    let outcome = hook.before_tool(&mut call, &hook_context()).await;
    assert_eq!(
        outcome,
        HookOutcome::Deny {
            reason: "not that one".into()
        }
    );
    assert_eq!(call.input, json!({ "deny": "not that one" }));
    started.manager.shutdown().await;
}

/// The matcher is handshake data, so asking what a hook claims costs no
/// crossing at all — which is what keeps an event the matcher rules out off
/// the pipe entirely. The proof is the process's own memory: nothing was
/// written, because nothing was sent. (That the kernel then skips on this
/// matcher is `HookSet::at`'s to prove, and it does.)
#[tokio::test]
async fn what_a_hook_claims_is_answered_without_the_pipe() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "guard").await;
    assert_eq!(
        hook.matcher(),
        HookMatcher {
            points: vec![HookPoint::BeforeTool],
            tool: Some("Bash".into()),
        }
    );
    assert_eq!(
        crossings(&started).await,
        Value::Null,
        "the process was never asked anything"
    );
    started.manager.shutdown().await;
}

/// `on_submit` owns a mutable argument too: the line reaches the turn as the
/// process would have it.
#[tokio::test]
async fn a_submit_hook_in_another_process_rewrites_the_line() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "prefix").await;
    let mut input = Input::text("hello", Origin::surface("test"));
    let outcome = hook.on_submit(&mut input, &hook_context()).await;
    assert_eq!(outcome, HookOutcome::Continue);
    let Input::Text { text, origin, .. } = &input else {
        panic!("a text submission stays one");
    };
    assert_eq!(text, "hello (stub)");
    assert_eq!(origin.surface, "test", "the origin is the person's");
    assert_eq!(crossings(&started).await, json!("prefix/submit"));
    started.manager.shutdown().await;
}

/// An observation crosses as a notification: the process answers it with
/// nothing, and the call still returns — a lane that waited on an answer
/// would never come back here. That it arrived is read afterwards, on the
/// same pipe, so the ordering is the pipe's rather than a sleep's.
#[tokio::test]
async fn an_observation_point_lands_as_a_notification_nobody_waits_on() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "watch").await;
    hook.on_session(Phase::Start, &hook_context()).await;
    hook.on_turn(
        Phase::End,
        &bingo_sdk::TurnId::from_raw("trn_test"),
        &[],
        &hook_context(),
    )
    .await;
    assert_eq!(crossings(&started).await, json!("watch/session,watch/turn"));
    assert!(
        started.said().is_empty(),
        "nothing was awaited, so no deadline was missed"
    );
    started.manager.shutdown().await;
}

/// hooks-shell's precedent (ADR-0032 §5): a hook that does not answer never
/// gets to decide. The clock is paused, so the deadline is spent and no wall
/// time is; the turn goes on with `Continue` and a notice names the hook.
#[tokio::test]
async fn a_hook_past_its_deadline_never_decides_and_a_notice_names_it() {
    let started = with_hooks().await;
    let hook = hook_of(&started.manager, "silent").await;
    tokio::time::pause();
    let outcome = tokio::time::timeout(Duration::from_secs(60), hook.on_stop(&hook_context()))
        .await
        .expect("the deadline is spent on a paused clock, not waited out");
    tokio::time::resume();
    assert_eq!(outcome, HookOutcome::Continue);
    // The same one drain the notice door rides: a hook's notice reaches the
    // person through the host, with no tool call anywhere in this test.
    let (_, text) = started.heard("HOOK_UNANSWERED").await;
    assert!(
        text.contains("stub:silent") && text.contains("within 5s"),
        "{text}"
    );
    assert_eq!(
        crossings(&started).await,
        json!("silent/stop"),
        "it was asked; it did not answer"
    );
    started.manager.shutdown().await;
}
