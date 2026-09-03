//! The cold ask, against the scripted agent as a real child process: what an
//! instance nobody has prompted answers when the catalogue asks it what it
//! serves (Plan M44, ADR-0037 §2).
//!
//! The agent's own log is what proves the shape of the ask — one handshake,
//! one `session/new`, no prompt and no second child.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod harness;

use bingo_provider_acp::config::Adapter;
use bingo_provider_acp::session::Sessions;
use bingo_sdk::Env;
use harness::{Fake, agent_binary};
use serde_json::{Value, json};

/// codex-acp's own shape: the models ride the model-shaped config option that
/// `session/new` answers with.
fn model_option() -> Value {
    json!({
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": "fast-model",
        "options": [
            { "value": "fast-model", "name": "Fast" },
            { "value": "slow-model", "name": "Slow" }
        ]
    })
}

/// One row pointing at the scripted agent, with whatever else the scenario
/// wants written onto it.
fn row(fake: &Fake, extra: Value) -> Adapter {
    let mut written = json!({
        "command": agent_binary().display().to_string(),
        "env": fake.env(),
    });
    let (Some(into), Some(from)) = (written.as_object_mut(), extra.as_object()) else {
        panic!("both are objects");
    };
    for (key, value) in from {
        into.insert(key.clone(), value.clone());
    }
    serde_json::from_value(written).expect("a row")
}

fn ids(models: &[bingo_sdk::ModelInfo]) -> Vec<&str> {
    models.iter().map(|model| model.id.as_str()).collect()
}

fn sessions(fake: &Fake) -> std::sync::Arc<Sessions> {
    Sessions::new(Env::rooted(fake.home.path()))
}

/// An instance nobody has prompted still has a catalogue: the ask opens a
/// session on purpose, keeps what the answer declared, and drops the child.
/// Nothing is prompted, so it costs the agent no model time.
#[tokio::test]
async fn a_cold_ask_opens_one_session_and_keeps_what_the_agent_declared() {
    let fake = Fake::new(json!({
        "sessionId": "cold-1",
        "configOptions": [model_option()],
        "turns": []
    }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({}));

    let models = sessions.models("scripted", &adapter).await;
    assert_eq!(ids(&models), ["fast-model", "slow-model"]);
    assert_eq!(
        fake.methods(),
        ["initialize", "session/new"],
        "a handshake and one opening — nothing was ever asked of the agent"
    );
}

/// A row's own options are what an *opening for a person* says (ADR-0037 §4).
/// A cold ask is not one: it turns nothing, because the values it would turn
/// are not what it came to read, and the session it opened is about to go.
#[tokio::test]
async fn a_cold_ask_turns_none_of_the_rows_own_knobs() {
    let fake = Fake::new(json!({
        "sessionId": "cold-2",
        "configOptions": [model_option()],
        "turns": []
    }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({ "options": { "model": "slow-model" } }));

    let models = sessions.models("scripted", &adapter).await;
    assert_eq!(ids(&models), ["fast-model", "slow-model"]);
    assert_eq!(
        fake.methods(),
        ["initialize", "session/new"],
        "the row is a person's session's business, not a listing's"
    );
}

/// Asked twice, spawned once: the answer is kept for the run. How often a
/// process asks again is the host's own gate (ADR-0026 §4), and a child per
/// listing would be a spawn every time somebody opened a menu.
#[tokio::test]
async fn a_second_ask_is_answered_from_the_first_and_spawns_nothing() {
    let fake = Fake::new(json!({
        "sessionId": "cold-3",
        "configOptions": [model_option()],
        "turns": []
    }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({}));

    let first = sessions.models("scripted", &adapter).await;
    // The script the second ask would read declares something else entirely.
    // Nothing reads it: the answer to the first is the answer to both.
    fake.rewrite(json!({ "sessionId": "cold-3b", "turns": [] }));
    let second = sessions.models("scripted", &adapter).await;

    assert_eq!(ids(&first), ["fast-model", "slow-model"]);
    assert_eq!(ids(&second), ids(&first));
    assert_eq!(
        fake.methods(),
        ["initialize", "session/new"],
        "one child for the run, not one for each asking"
    );
}

/// Two callers arriving together are one child: whoever is second waits for
/// the answer the first is already getting. `/models refresh` and the
/// background top-up can land on one instance at the same moment.
#[tokio::test]
async fn two_askers_at_once_are_one_child() {
    let fake = Fake::new(json!({
        "sessionId": "cold-4",
        "configOptions": [model_option()],
        "turns": []
    }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({}));

    let (first, second) = tokio::join!(
        sessions.models("scripted", &adapter),
        sessions.models("scripted", &adapter)
    );
    assert_eq!(ids(&first), ["fast-model", "slow-model"]);
    assert_eq!(ids(&second), ids(&first));
    assert_eq!(fake.methods(), ["initialize", "session/new"]);
}

/// An agent with no model-shaped option and only the older list is read
/// through that door too — the cold ask harvests the whole declaration, not
/// one field of it.
#[tokio::test]
async fn a_cold_ask_reads_the_older_door_as_well() {
    let fake = Fake::new(json!({
        "sessionId": "cold-5",
        "legacyModels": [
            { "modelId": "gpt-5[high]", "name": "GPT-5 (high)" },
            { "modelId": "gpt-5[low]", "name": "GPT-5 (low)" }
        ],
        "turns": []
    }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({}));

    let models = sessions.models("scripted", &adapter).await;
    assert_eq!(ids(&models), ["gpt-5[high]", "gpt-5[low]"]);
}

/// A row nothing can start is not an error: the catalogue serves what it
/// honestly can, which is nothing of the agent's, and the person is told
/// elsewhere. Asked again it is still nothing, and still no second attempt.
#[tokio::test]
async fn an_adapter_that_will_not_start_serves_nothing_and_does_not_fail() {
    let fake = Fake::new(json!({ "sessionId": "cold-6", "turns": [] }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({ "command": "bingo-no-such-adapter-xyz" }));

    assert!(sessions.models("missing", &adapter).await.is_empty());
    assert!(sessions.models("missing", &adapter).await.is_empty());
    assert!(fake.methods().is_empty(), "nothing was ever spoken to");
}

/// An agent that refuses to open a session — no login, most often — has
/// declared nothing, and that is the whole of the answer.
#[tokio::test]
async fn an_agent_that_refuses_the_opening_declares_nothing() {
    let fake = Fake::new(json!({ "sessionId": "cold-7", "authRequired": true }));
    let sessions = sessions(&fake);
    let adapter = row(&fake, json!({}));

    assert!(sessions.models("scripted", &adapter).await.is_empty());
    assert_eq!(
        fake.methods(),
        ["initialize", "session/new"],
        "it was asked, and it said no"
    );
}
