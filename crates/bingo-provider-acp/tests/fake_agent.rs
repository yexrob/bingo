//! The scripted agent, over real pipes: what the loop, the ladder and the
//! black-box all stand on. If this passes, a child process spawned from
//! `{command, args, env}` is talking ACP to a `Send` tokio client.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod harness;

use agent_client_protocol_schema::v1::{
    InitializeRequest, NewSessionRequest, PromptRequest, SessionUpdate,
};
use bingo_provider_acp::error::AcpError;
use bingo_provider_acp::events::Mapper;
use bingo_sdk::{ModelEvent, UnifiedFinish};
use harness::{Collector, Fake, connect};
use serde_json::{Value, json};

fn initialize() -> InitializeRequest {
    serde_json::from_value(json!({
        "protocolVersion": 1,
        "clientCapabilities": { "fs": {}, "terminal": false },
        "clientInfo": { "name": "bingo", "version": "test" }
    }))
    .unwrap()
}

fn new_session(cwd: &std::path::Path) -> NewSessionRequest {
    serde_json::from_value(json!({ "cwd": cwd, "mcpServers": [] })).unwrap()
}

fn prompt(session: &str, text: &str) -> PromptRequest {
    serde_json::from_value(json!({
        "sessionId": session,
        "prompt": [{ "type": "text", "text": text }]
    }))
    .unwrap()
}

fn chunk(text: &str) -> Value {
    json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text }, "messageId": "m1" })
}

fn thought(text: &str) -> Value {
    json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": text } })
}

fn tool_call() -> Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "c1",
        "title": "Read src/lib.rs",
        "kind": "read",
        "status": "completed",
        "content": [{ "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }],
        "rawInput": { "file_path": "src/lib.rs" }
    })
}

/// One whole turn: handshake, session, prompt, and the stream folded into
/// bingo's own events.
#[tokio::test]
async fn a_scripted_agent_answers_a_turn_over_real_pipes() {
    let fake = Fake::new(json!({
        "sessionId": "s-1",
        "capabilities": { "loadSession": true, "resume": true },
        "turns": [{
            "updates": [thought("weighing it"), chunk("Hello "), chunk("there."), tool_call()],
            "stopReason": "end_turn",
            "usage": { "totalTokens": 9, "inputTokens": 6, "outputTokens": 3 }
        }]
    }));
    let collector = Collector::new(None);
    let live = connect(&fake, collector.clone());

    let hello = live.connection.call(initialize()).await.unwrap();
    assert_eq!(hello.protocol_version.as_u16(), 1);
    assert!(hello.agent_capabilities.load_session);
    assert!(
        hello
            .agent_capabilities
            .session_capabilities
            .resume
            .is_some()
    );

    let opened = live.connection.call(new_session(fake.cwd())).await.unwrap();
    assert_eq!(opened.session_id.0.as_ref(), "s-1");

    let ended = live
        .connection
        .call(prompt("s-1", "say hello"))
        .await
        .unwrap();

    let mut mapper = Mapper::default();
    let mut events: Vec<ModelEvent> = Vec::new();
    for note in collector.updates.lock().await.drain(..) {
        events.extend(mapper.update(note.update));
    }
    events.extend(mapper.finish(&ended));

    let said: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(said, "Hello there.");
    let marked = events.iter().any(|e| {
        matches!(
            e,
            ModelEvent::ReasoningEnd { provider_metadata, .. }
                if provider_metadata
                    .get("acp")
                    .is_some_and(|m| m["external"] == Value::Bool(true))
        )
    });
    assert!(marked, "the agent's own call wears the mark");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ModelEvent::ToolCall { .. })),
        "and asks the loop to run nothing"
    );
    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish { usage, finish_reason })
            if usage.output_tokens == 3 && finish_reason.unified == UnifiedFinish::Stop
    ));

    assert_eq!(
        fake.methods(),
        vec!["initialize", "session/new", "session/prompt"],
        "and nothing else crossed"
    );
}

/// An agent that advertises neither door refuses both, which is the third rung
/// of the restore ladder existing at all.
#[tokio::test]
async fn an_agent_that_advertises_neither_door_refuses_both() {
    let fake = Fake::new(json!({ "sessionId": "s-2", "capabilities": {} }));
    let live = connect(&fake, Collector::new(None));
    live.connection.call(initialize()).await.unwrap();

    let resumed: Result<_, _> = live
        .connection
        .call::<agent_client_protocol_schema::v1::ResumeSessionRequest>(
            serde_json::from_value(json!({ "sessionId": "s-2", "cwd": fake.cwd() })).unwrap(),
        )
        .await;
    assert!(matches!(resumed, Err(AcpError::Refused(_))));

    let loaded: Result<_, _> = live
        .connection
        .call::<agent_client_protocol_schema::v1::LoadSessionRequest>(
            serde_json::from_value(
                json!({ "sessionId": "s-2", "cwd": fake.cwd(), "mcpServers": [] }),
            )
            .unwrap(),
        )
        .await;
    assert!(matches!(loaded, Err(AcpError::Refused(_))));
}

/// A load replays the history it holds. The client must hear it and journal
/// none of it.
#[tokio::test]
async fn a_load_replays_and_the_replay_is_ours_to_swallow() {
    let fake = Fake::new(json!({
        "sessionId": "s-3",
        "capabilities": { "loadSession": true },
        "replay": [
            { "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "first" } },
            chunk("an answer from before")
        ]
    }));
    let collector = Collector::new(None);
    let live = connect(&fake, collector.clone());
    live.connection.call(initialize()).await.unwrap();
    live.connection
        .call::<agent_client_protocol_schema::v1::LoadSessionRequest>(
            serde_json::from_value(
                json!({ "sessionId": "s-3", "cwd": fake.cwd(), "mcpServers": [] }),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let replayed = collector.updates.lock().await;
    assert_eq!(replayed.len(), 2, "the replay arrives");
    assert!(matches!(
        replayed[0].update,
        SessionUpdate::UserMessageChunk(_)
    ));
    let mut mapper = Mapper::default();
    assert!(
        mapper.update(replayed[0].update.clone()).is_empty(),
        "and our own turn coming back is no event"
    );
}

/// The person's choice, by the agent's own option id, reaching the agent.
#[tokio::test]
async fn a_permission_answer_reaches_the_agent_by_the_agents_own_id() {
    let fake = Fake::new(json!({
        "sessionId": "s-4",
        "turns": [{
            "permission": {
                "toolCall": { "toolCallId": "c1", "title": "Edit src/lib.rs", "kind": "edit" },
                "options": [
                    { "optionId": "allow-once", "name": "Yes", "kind": "allow_once" },
                    { "optionId": "reject", "name": "No", "kind": "reject_once" }
                ]
            },
            "updates": [chunk("edited")],
            "stopReason": "end_turn"
        }]
    }));
    let live = connect(&fake, Collector::new(Some("allow-once")));
    live.connection.call(initialize()).await.unwrap();
    live.connection.call(new_session(fake.cwd())).await.unwrap();
    let ended = live
        .connection
        .call(prompt("s-4", "edit it"))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(ended.stop_reason).unwrap(),
        json!("end_turn")
    );
    let answered = fake.wait_for("permission/answered").await;
    assert_eq!(answered["outcome"]["outcome"], "selected");
    assert_eq!(answered["outcome"]["optionId"], "allow-once");
}

/// A cancel ends the turn the way ACP says, and the child goes with the
/// handle rather than outliving the test.
#[tokio::test]
async fn a_cancel_ends_the_turn_and_the_child_goes_with_the_handle() {
    let fake = Fake::new(json!({
        "sessionId": "s-5",
        "turns": [{ "updates": [chunk("working")], "awaitCancel": true }]
    }));
    let live = connect(&fake, Collector::new(None));
    live.connection.call(initialize()).await.unwrap();
    live.connection.call(new_session(fake.cwd())).await.unwrap();

    let connection = std::sync::Arc::new(live.connection);
    let asking = tokio::spawn({
        let connection = connection.clone();
        async move { connection.call(prompt("s-5", "work")).await }
    });
    fake.wait_for("session/prompt").await;
    connection
        .notify::<agent_client_protocol_schema::v1::CancelNotification>(
            serde_json::from_value(json!({ "sessionId": "s-5" })).unwrap(),
        )
        .unwrap();
    let ended = asking.await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(ended.stop_reason).unwrap(),
        json!("cancelled")
    );
    assert!(fake.methods().iter().any(|m| m == "session/cancel"));
}

/// An adapter with no login refuses where it refuses, and the person is told
/// in the adapter's own words.
#[tokio::test]
async fn an_agent_with_no_login_refuses_the_session_in_its_own_words() {
    let fake = Fake::new(json!({ "sessionId": "s-6", "authRequired": true }));
    let live = connect(&fake, Collector::new(None));
    live.connection.call(initialize()).await.unwrap();
    let refused = live.connection.call(new_session(fake.cwd())).await;
    let Err(error) = refused else {
        panic!("an agent with no credential does not open a session");
    };
    let told: bingo_sdk::ProviderError = error.into();
    assert!(matches!(
        told,
        bingo_sdk::ProviderError::Auth { ref message } if message == "Authentication required"
    ));
    assert!(!told.retryable());
}
