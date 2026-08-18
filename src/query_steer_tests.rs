//! D83 acceptance: the tool barrier as the query loop actually reaches it.
//!
//! Separate from `query::tests` only because `query.rs` is at its line cap; these are
//! loop tests and read as part of that suite (they borrow its mock server helpers).

use super::tests::{bash_tool_turn, spawn_api, test_session, text_turn};
use super::*;
use crate::steer::SteerQueue;

/// Headless hooks wired to a steer channel holding `items`, plus the channel itself:
/// whether it is empty afterwards is whether the barrier took them.
fn steering_hooks(items: Vec<crate::steer::SteerItem>) -> (EngineHost, Arc<SteerQueue>) {
    let queue = Arc::new(SteerQueue::new());
    queue.rearm(items);
    let source = Arc::clone(&queue);
    let mut ui = headless_hooks();
    ui.requests.steer = Arc::new(move || source.take());
    (ui, queue)
}

/// D83: the barrier folds the queued message into the very message that carries
/// the tool results — after them, because the API rejects a user message whose
/// tool_result blocks do not come first.
#[tokio::test]
async fn steering_lands_after_the_tool_results_of_the_same_message() {
    let base_url = spawn_api(vec![
        bash_tool_turn("tu_1", "echo one"),
        text_turn("done", "end_turn"),
    ])
    .await;
    let session = test_session(base_url, None);
    let (ui, queue) = steering_hooks(vec![crate::steer::SteerItem {
        id: 7,
        text: "use tabs".into(),
    }]);
    let outcome = run_query(&session, Vec::new(), "go", &[], &ui, None)
        .await
        .unwrap();

    let results = outcome
        .messages
        .iter()
        .find(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        })
        .expect("the tool results entered the history");
    let kinds: Vec<&str> = results
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::ToolResult { .. } => "result",
            ContentBlock::Text { .. } => "text",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["result", "text"],
        "tool_result blocks stay first; the steered text follows them"
    );
    assert!(
        matches!(&results.content[1], ContentBlock::Text { text }
            if text == "[Message from user, sent while you were working]\nuse tabs"),
        "the model reads the marker above the user's words: {:?}",
        results.content[1]
    );
    assert!(queue.is_empty(), "the barrier took it");
}

/// A reply with no tool call has no barrier: nothing is assembled, nothing is sent
/// again, and the queue stays the composer's to submit at TurnEnd.
#[tokio::test]
async fn a_turn_without_tools_never_reaches_a_barrier() {
    let base_url = spawn_api(vec![text_turn("hi", "end_turn")]).await;
    let session = test_session(base_url, None);
    let (ui, queue) = steering_hooks(vec![crate::steer::SteerItem {
        id: 1,
        text: "wait".into(),
    }]);
    run_query(&session, Vec::new(), "go", &[], &ui, None)
        .await
        .unwrap();
    assert!(
        !queue.is_empty(),
        "with no tool barrier the message is still the composer's"
    );
}

/// An interrupted turn stops here: folding a message into a request that will never
/// be sent would swallow it, so it stays queued for the user to edit or resubmit.
#[tokio::test]
async fn an_interrupted_turn_takes_nothing_from_the_queue() {
    let base_url = spawn_api(vec![bash_tool_turn("tu_1", "sleep 5")]).await;
    let session = test_session(base_url, None);
    let (ui, queue) = steering_hooks(vec![crate::steer::SteerItem {
        id: 1,
        text: "stop that".into(),
    }]);
    let (tx, rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn({
        let session = session.clone();
        async move {
            let outcome = run_query(&session, Vec::new(), "go", &[], &ui, Some(rx)).await;
            (outcome, ui)
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    tx.send(true).unwrap();
    let (outcome, _ui) = handle.await.unwrap();
    assert!(outcome.unwrap().aborted, "the turn closes as interrupted");
    assert!(
        !queue.is_empty(),
        "the queue is intact: the user still owns the message"
    );
}
