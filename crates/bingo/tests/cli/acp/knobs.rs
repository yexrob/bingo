//! Black-box: `/think`, `/model` and an adapter's own row reaching an ACP agent
//! (ADR-0037), driven through the real binary against the scripted agent.
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

/// The knob claude-agent-acp has no flag and no environment variable for: its
/// permission mode is a session config option or it is nothing, which is why a
/// row can say `options` at all.
fn mode_option() -> Value {
    json!({
        "id": "mode",
        "name": "Mode",
        "category": "mode",
        "type": "select",
        "currentValue": "default",
        "options": [
            { "value": "default", "name": "Default" },
            { "value": "dontAsk", "name": "Don't ask" }
        ]
    })
}

/// A row that says what to set once a session with this agent is open.
fn sets(options: Value) -> Value {
    json!({ "options": options })
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

/// An adapter whose permission mode has no flag and no variable is told it
/// through the one door it has, from its own row: once the session is open,
/// before the first prompt, in the agent's own words.
#[test]
fn a_row_option_reaches_the_agent_once_before_its_first_prompt() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-8",
            "capabilities": { "resume": true },
            "configOptions": [mode_option()],
            "turns": two_turns()
        }),
        sets(json!({ "mode": "dontAsk" })),
        json!({}),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    host.until_event("turnCompleted");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/prompt",
            "session/prompt"
        ],
        "once for the session, not once a turn"
    );
    let calls = config_calls(&adapter);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["configId"], "mode");
    assert_eq!(calls[0]["value"], "dontAsk");
}

/// A child that died between turns is a new opening (ADR-0035 §3), and an
/// opening is what the row speaks at: the replacement comes back set the way
/// the row says rather than the way its predecessor was left.
#[test]
fn a_respawned_child_is_set_from_the_row_again() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-9",
            "capabilities": { "resume": true },
            "configOptions": [mode_option()],
            "turns": [
                { "updates": [chunk("First.")], "stopReason": "end_turn", "thenExit": true },
                one_turn(vec![chunk("Second.")])
            ]
        }),
        sets(json!({ "mode": "dontAsk" })),
        json!({}),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("one");
    host.until("result");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/prompt",
            "initialize",
            "session/resume",
            "session/set_config_option",
            "session/prompt"
        ],
        "back into the same agent session, and set again on the way in"
    );
    let calls = config_calls(&adapter);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls.iter().all(|call| call["value"] == "dontAsk"));
}

/// The ids on a row are the agent's own words, and bingo does not know them:
/// one it turns out not to have is a person to tell once, nothing sent, and a
/// turn that still runs.
#[test]
fn an_option_the_agent_never_declared_is_one_notice_and_no_call() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-10",
            "capabilities": { "resume": true },
            "configOptions": [effort_option()],
            "turns": two_turns()
        }),
        sets(json!({ "mode": "dontAsk" })),
        json!({}),
    );
    let out = adapter.turn("say hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new", "session/prompt"],
        "nothing was sent for an option this agent does not have"
    );

    let all = notices(frames_of(&out));
    let told = coded(&all, "ACP_KNOB");
    assert_eq!(told.len(), 1, "{all:?}");
    assert!(told[0].contains("`mode`"), "{}", told[0]);
    assert!(
        told[0].contains("acp.adapters.scripted.options"),
        "the notice names the row that said it: {}",
        told[0]
    );
}

/// The row and `/think` are two hands on one knob, not two knobs: the row sets
/// it on the way in, the first turn's diff finds it already there, and the
/// change after that is one message.
#[test]
fn a_row_effort_and_a_later_change_are_two_messages() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-knob-11",
            "capabilities": { "resume": true },
            "configOptions": [effort_option()],
            "turns": two_turns()
        }),
        sets(json!({ "reasoning_effort": "low" })),
        reasons(),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
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
            "session/set_config_option",
            "session/prompt",
            "session/set_config_option",
            "session/prompt"
        ],
        "stderr: {}",
        ended.err
    );
    let calls = config_calls(&adapter);
    assert_eq!(
        calls
            .iter()
            .map(|call| call["value"].clone())
            .collect::<Vec<_>>(),
        [json!("low"), json!("high")],
        "the row's value first, then the change, and neither twice"
    );
    assert!(
        calls
            .iter()
            .all(|call| call["configId"] == "reasoning_effort"),
        "{calls:?}"
    );
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
