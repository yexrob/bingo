//! Black-box: `/think` and `/model` reaching an ACP agent (ADR-0037), driven
//! through the real binary against the scripted agent.
//!
//! Nothing here knows anything about the plugin's insides: a settings row, a
//! prompt, a slash command, the frames that came out, and the log the agent
//! wrote of every message it was actually sent — which is what proves the
//! order, because a knob applied after the prompt is a knob applied to the
//! next turn.

use bingo_sdk::IntentOutcome;

use super::*;

/// A model the embedded snapshot has never heard of fails closed on
/// reasoning, so no level would reach any provider for it — the line
/// `/think` itself tells a person to write. It is settings, not code: an ACP
/// agent's model is whatever the agent runs, and the snapshot cannot know.
fn reasons() -> Value {
    json!({ "models": { "scripted/agent": { "reasoning": true } } })
}

/// codex-acp's own id and category, and a ladder that stops at `high` — so an
/// `xhigh` or a `max` has somewhere to clamp to.
fn effort_option() -> Value {
    json!({
        "id": "reasoning_effort",
        "name": "Reasoning effort",
        "category": "thought_level",
        "type": "select",
        "currentValue": "medium",
        "options": [
            { "value": "low", "name": "Low" },
            { "value": "medium", "name": "Medium" },
            { "value": "high", "name": "High" }
        ]
    })
}

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

fn two_turns() -> Value {
    json!([
        one_turn(vec![chunk("First.")]),
        one_turn(vec![chunk("Second.")])
    ])
}

/// Everything the run's `/command`s answered with, run together.
fn receipts(ended: &stream_json::Ended) -> String {
    frames(ended)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::IntentAck {
                outcome: IntentOutcome::Applied { result },
                ..
            } => Some(result.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn config_calls(adapter: &Scripted) -> Vec<Value> {
    adapter
        .heard()
        .into_iter()
        .filter(|line| line["method"] == "session/set_config_option")
        .map(|line| line["params"].clone())
        .collect()
}

/// ADR-0037 §1: the level a person asked for mid-session reaches the agent as
/// one `session/set_config_option`, *before* the prompt that will be answered
/// under it — and clamped to the deepest level this agent has, said in the
/// agent's own word.
#[test]
fn a_thinking_change_reaches_the_agent_before_the_next_prompt() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-1",
            "capabilities": { "resume": true },
            "configOptions": [effort_option()],
            "turns": two_turns()
        }),
        json!({}),
        reasons(),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    // The turn is over before the level moves, and the level has moved before
    // the next turn asks for anything: what is being proven is an order, so
    // nothing here is left to the speed of two processes.
    host.until_event("turnCompleted");
    host.prompt("/think max");
    host.until_event("intentAck");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/set_config_option",
            "session/prompt"
        ],
        "the knob is turned between the turns, not inside one"
    );
    let calls = config_calls(&adapter);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["configId"], "reasoning_effort");
    assert_eq!(
        calls[0]["value"], "high",
        "`max` clamped to the deepest this agent offers"
    );

    let all = notices(frames(&ended));
    let clamped = coded(&all, "ACP_LEVEL");
    assert_eq!(clamped.len(), 1, "{all:?}");
    assert!(
        clamped[0].contains("max") && clamped[0].contains("High"),
        "the clamp is said in the option's own word: {}",
        clamped[0]
    );
}

/// An external agent picks its own models per session, and nothing in the
/// protocol answers "what do you serve" before one is open — so asking before
/// one is neither a hang nor an error: bingo's own label, alone, and no child
/// spawned to find out.
#[test]
fn before_any_session_the_instance_serves_the_label_alone() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-0",
            "configOptions": [model_option()],
            "turns": []
        }),
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
        stdout(&refreshed).contains("scripted 1 models"),
        "{}",
        stdout(&refreshed)
    );

    let mut list = adapter.base();
    let listed = run(list.args(named).arg("/models"));
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let listing = stdout(&listed);
    assert!(listing.contains("\n  agent\n"), "{listing}");
    assert!(
        !listing.contains("fast-model"),
        "nothing was asked of an agent nobody opened a session with: {listing}"
    );
    assert!(
        adapter.methods().is_empty(),
        "and no child was spawned to find out: {:?}",
        adapter.methods()
    );
}

/// ADR-0037 §2: the models the agent declared are this instance's catalogue,
/// served through the door every endpoint-answered list rides (ADR-0026) —
/// with bingo's own `agent` label always in front of them.
#[test]
fn the_model_list_is_the_agents_own_once_a_session_has_opened() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-2",
            "capabilities": { "resume": true },
            "configOptions": [model_option(), effort_option()],
            "turns": two_turns()
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    // A session has to be open for there to be a list; the agent said what it
    // serves when it opened one.
    host.until_event("turnCompleted");
    host.prompt("/models refresh");
    // The refresh is what asks; the listing is what reads. Asking the second
    // before the first answered would be reading the cache it was about to
    // fill.
    host.until_event("intentAck");
    host.prompt("/models");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let listed = receipts(&ended);
    for named in ["agent", "fast-model", "slow-model"] {
        assert!(listed.contains(named), "{named} is not in {listed}");
    }
    assert!(
        !listed.contains("reasoning_effort"),
        "and the other knob is not a model: {listed}"
    );
}

/// The chosen one is applied, in the agent's own value id, before the prompt.
#[test]
fn a_model_change_reaches_the_agent_as_its_own_value() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-3",
            "capabilities": { "resume": true },
            "configOptions": [model_option()],
            "turns": two_turns()
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    host.until_event("turnCompleted");
    host.prompt("/model scripted/slow-model");
    host.until_event("intentAck");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/set_config_option",
            "session/prompt"
        ],
        "stderr: {}",
        ended.err
    );
    let calls = config_calls(&adapter);
    assert_eq!(calls[0]["configId"], "model");
    assert_eq!(calls[0]["value"], "slow-model");
}

/// `agent` is bingo's label for the agent's own, and the agent is never told
/// bingo's labels: a whole session on it sends no config call at all.
#[test]
fn the_agent_label_applies_nothing() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-4",
            "capabilities": { "resume": true },
            "configOptions": [model_option(), effort_option()],
            "turns": two_turns()
        }),
    );
    let out = adapter.turn("say hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new", "session/prompt"]
    );
}

/// ADR-0037 §1: an agent with neither knob keeps its own, and the person is
/// told once — never an error, and never a turn that did not run.
#[test]
fn an_agent_with_neither_knob_gets_no_config_call_and_one_notice() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-5",
            "capabilities": { "resume": true },
            "turns": [
                one_turn(vec![chunk("First.")]),
                one_turn(vec![chunk("Second.")]),
                one_turn(vec![chunk("Third.")])
            ]
        }),
        json!({}),
        reasons(),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    // The turn is over before the level moves, and the level has moved before
    // the next turn asks for anything: what is being proven is an order, so
    // nothing here is left to the speed of two processes.
    host.until_event("turnCompleted");
    host.prompt("/think max");
    host.until_event("intentAck");
    host.prompt("two");
    host.until_event("turnCompleted");
    host.prompt("three");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/prompt",
            "session/prompt"
        ],
        "nothing was sent for a knob that is not there"
    );
    let all = notices(frames(&ended));
    let told = coded(&all, "ACP_KNOB");
    assert_eq!(told.len(), 1, "said once, not once a turn: {all:?}");
    assert!(
        told[0].contains("acp.adapters.scripted"),
        "the notice names the row: {}",
        told[0]
    );
}

/// An adapter old enough to have only `session/set_model` is set through it,
/// and the models it listed the old way are the ones it serves.
#[test]
fn an_adapter_with_only_the_legacy_door_is_set_through_it() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-6",
            "capabilities": { "resume": true },
            "legacyModels": [
                { "modelId": "gpt-5[high]", "name": "GPT-5 (high)" },
                { "modelId": "gpt-5[low]", "name": "GPT-5 (low)" }
            ],
            "turns": [one_turn(vec![chunk("First.")])]
        }),
    );
    let out = run(adapter
        .bingo(&["--provider", "scripted", "--model", "gpt-5[low]"])
        .arg("go"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/set_model",
            "session/prompt"
        ],
        "the older door, and before the prompt like the newer one"
    );
    let sent = adapter
        .first("session/set_model")
        .expect("the legacy door was used");
    assert_eq!(sent["modelId"], "gpt-5[low]");
}

/// A spawn that names one of the agent's own models opens its session already
/// set: `SpawnAgent`'s `model` field ends in the same request field every
/// other path ends in, so nothing new was built for it (ADR-0037 §3).
const SPAWN_ON_THE_AGENTS_MODEL: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"say hi",
        "background":false,"provider":"scripted","model":"slow-model"}}}]},
    {"steps":[{"text":"the child said hi"}]}
]}"#;

#[test]
fn a_spawn_that_names_the_agents_model_opens_the_session_already_set() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-knob-7",
            "capabilities": { "resume": true },
            "configOptions": [model_option()],
            "turns": [one_turn(vec![chunk("Hi from the agent.")])]
        }),
    );
    let script = script(SPAWN_ON_THE_AGENTS_MODEL);
    let out = run(adapter
        .bingo(&["--provider", "fake", "--model", "fake-1"])
        .env("BINGO_FAKE_SCRIPT", script.path())
        .arg("spawn one on the agent"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/prompt"
        ],
        "the child's very first prompt is answered under the model it named"
    );
    let calls = config_calls(&adapter);
    assert_eq!(calls[0]["value"], "slow-model");
}
