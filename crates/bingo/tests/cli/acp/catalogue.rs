//! Black-box: what an ACP instance serves before anybody has said a word to
//! it (Plan M44, ADR-0037 §2, ADR-0026 §4).
//!
//! An agent declares its models when a session opens and nowhere else, so an
//! instance nobody has prompted has nothing of the agent's to serve. `/models
//! refresh` answers that by opening one on purpose and dropping it — which
//! the agent's own log is what proves: a handshake, an opening, and no prompt.
//!
//! These are the scenarios that start on a machine that has never asked this
//! adapter anything, so they build their adapter with `Scripted::cold`.

use bingo_sdk::IntentOutcome;

use super::*;

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

/// The same knob, on an agent that has since been upgraded.
fn later_model_option() -> Value {
    json!({
        "id": "model",
        "name": "Model",
        "category": "model",
        "type": "select",
        "currentValue": "next-model",
        "options": [{ "value": "next-model", "name": "Next" }]
    })
}

/// What every `/command` in the run answered, in the order they answered.
fn answers(ended: &stream_json::Ended) -> Vec<String> {
    frames(ended)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::IntentAck {
                outcome: IntentOutcome::Applied { result },
                ..
            } => Some(result.to_string()),
            _ => None,
        })
        .collect()
}

/// The milestone: a refresh on an instance nobody has prompted lists the
/// agent's own models. One child answers it — a handshake and an opening, no
/// prompt — and what it learned outlives the run through the one cache every
/// endpoint-answered list is kept in (ADR-0026 §4).
#[test]
fn a_cold_refresh_lists_the_agents_own_models() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::cold(
        agent,
        json!({
            "sessionId": "acp-cold-1",
            "configOptions": [model_option()],
            "turns": []
        }),
        json!({}),
    );
    let named = ["--provider", "scripted", "--model", "agent"];
    let mut refresh = adapter.base();
    let refreshed = run(refresh.args(named).arg("/models refresh"));
    assert_eq!(
        refreshed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&refreshed)
    );
    assert!(
        stdout(&refreshed).contains("scripted 3 models"),
        "the label and the two the agent declared: {}",
        stdout(&refreshed)
    );
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new"],
        "one child, and it was never prompted"
    );

    // A second process, which asks nothing: what the first one learned is
    // already on the machine.
    let mut list = adapter.base();
    let listed = run(list.args(named).arg("/models"));
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let listing = stdout(&listed);
    for model in ["agent", "fast-model", "slow-model"] {
        assert!(listing.contains(model), "{model} is not in {listing}");
    }
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new"],
        "and no second child was spawned to say it again: {listing}"
    );
}

/// A live conversation's declaration is the fresher of the two: the agent it
/// is talking to now is the one that answers. A cold list learned before it
/// does not stand in front of it.
#[test]
fn a_live_sessions_declaration_supersedes_the_cold_one() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::cold(
        agent,
        json!({
            "sessionId": "acp-cold-2",
            "capabilities": { "resume": true },
            "configOptions": [model_option()],
            "turns": [one_turn(vec![chunk("First.")])]
        }),
        json!({}),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("/models refresh");
    host.until_event("intentAck");
    // The agent is upgraded between the cold ask and the conversation.
    adapter.obeys(json!({
        "sessionId": "acp-cold-2",
        "capabilities": { "resume": true },
        "configOptions": [later_model_option()],
        "turns": [one_turn(vec![chunk("First.")])]
    }));
    host.prompt("one");
    host.until_event("turnCompleted");
    host.prompt("/models refresh");
    host.until_event("intentAck");
    host.prompt("/models");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let said = answers(&ended);
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(said[0].contains("3 models"), "the cold ask: {}", said[0]);
    assert!(said[1].contains("2 models"), "the live one: {}", said[1]);
    let listing = &said[2];
    assert!(listing.contains("next-model"), "{listing}");
    assert!(
        !listing.contains("fast-model"),
        "what the conversation says now replaces what the cold ask found: {listing}"
    );
}

/// An adapter that cannot be started is not an error in a catalogue: the
/// instance serves the one label it always serves, the run ends well, and the
/// person is told once — where a person can hear it, which is not the moment
/// the background top-up finds out.
#[test]
fn a_probe_that_cannot_start_the_adapter_serves_the_label_and_says_so() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::cold(
        agent,
        json!({ "sessionId": "acp-cold-3", "turns": [] }),
        json!({ "command": "bingo-no-such-adapter-xyz" }),
    );
    let out = run(adapter
        .bingo(&["--provider", "scripted", "--model", "agent"])
        .arg("/models refresh"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let all = notices(frames_of(&out));
    let told = coded(&all, "ACP_PROBE");
    assert_eq!(told.len(), 1, "said once, not once an asking: {all:?}");
    assert!(
        told[0].contains("bingo-no-such-adapter-xyz"),
        "in the adapter's own words: {}",
        told[0]
    );
    assert!(
        told[0].contains("acp.adapters.scripted"),
        "and it names the row: {}",
        told[0]
    );
    assert!(
        adapter.methods().is_empty(),
        "nothing was ever spoken to: {:?}",
        adapter.methods()
    );
}
