use super::*;
use crate::fixtures;
use agent_client_protocol_schema::v1::SessionNotification;

fn update_of(recorded: Value) -> SessionUpdate {
    let note: SessionNotification =
        serde_json::from_value(recorded).expect("a recorded update parses");
    note.update
}

fn response(recorded: Value) -> PromptResponse {
    serde_json::from_value(recorded).expect("a recorded response parses")
}

/// Every update the fixtures record, folded in order, as one turn.
fn turn(recorded: Vec<Value>, ended: Value) -> Vec<ModelEvent> {
    let mut mapper = Mapper::default();
    let mut events: Vec<ModelEvent> = recorded
        .into_iter()
        .flat_map(|body| mapper.update(update_of(body)))
        .collect();
    events.extend(mapper.finish(&response(ended)));
    events
}

fn text_of(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TextDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

fn reasoning_of(events: &[ModelEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn external_marks(events: &[ModelEvent]) -> Vec<&serde_json::Map<String, Value>> {
    events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::ReasoningEnd {
                provider_metadata, ..
            } => provider_metadata.get(NAMESPACE),
            _ => None,
        })
        .collect()
}

fn starts(events: &[ModelEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ModelEvent::TextStart { .. } | ModelEvent::ReasoningStart { .. }
            )
        })
        .count()
}

fn ends(events: &[ModelEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ModelEvent::TextEnd { .. } | ModelEvent::ReasoningEnd { .. }
            )
        })
        .count()
}

#[test]
fn chunks_with_one_message_id_are_one_block() {
    let events = turn(
        vec![
            fixtures::update_agent_message_chunk(),
            fixtures::update_agent_message_chunk_more(),
        ],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(starts(&events), 1, "one message is one block");
    assert_eq!(text_of(&events), "Renaming the module.");
    assert!(matches!(events.last(), Some(ModelEvent::Finish { .. })));
}

/// An adapter that names no message still writes one answer.
#[test]
fn chunks_with_no_message_id_are_still_one_block() {
    let events = turn(
        vec![
            fixtures::update_agent_message_chunk_unkeyed(),
            fixtures::update_agent_message_chunk_unkeyed(),
        ],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(starts(&events), 1);
    assert_eq!(text_of(&events), "no id hereno id here");
}

#[test]
fn a_thought_is_reasoning_and_carries_no_acp_mark() {
    let events = turn(
        vec![fixtures::update_agent_thought_chunk()],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(reasoning_of(&events), "the import list moves too");
    assert!(
        external_marks(&events).is_empty(),
        "the agent thinking is not the agent acting"
    );
}

/// ADR-0035 §4: the agent's own call is first class in the journal and wears
/// the mark. It is never a `ToolCall` event, because that would send the turn
/// into a second round and the gate into a call nobody can run.
#[test]
fn an_agents_tool_call_is_journalled_whole_and_marked_external() {
    let events = turn(
        vec![
            fixtures::update_tool_call(),
            fixtures::update_tool_call_completed(),
        ],
        fixtures::prompt_response_with_usage(),
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            ModelEvent::ToolCall { .. } | ModelEvent::ToolInputStart { .. }
        )),
        "nothing here asks the loop to run anything"
    );
    let said = reasoning_of(&events);
    assert!(said.contains("read Read src/lib.rs (1 - 50)"), "{said}");
    assert!(said.contains("done"), "{said}");
    assert!(said.contains("pub mod wire;"), "{said}");
    let marks = external_marks(&events);
    assert_eq!(marks.len(), 1);
    let mark = marks[0];
    assert_eq!(mark[EXTERNAL], Value::Bool(true));
    assert_eq!(mark["toolCallId"], "toolu_01Read");
    assert_eq!(mark["kind"], "read");
    assert_eq!(mark["status"], "completed");
    assert_eq!(mark["rawInput"]["file_path"], "/work/repo/src/lib.rs");
    assert_eq!(mark["rawOutput"]["lines"], 1);
    assert_eq!(mark["locations"][0]["path"], "/work/repo/src/lib.rs");
}

/// A partial update names only what changed; a client that replaces rather
/// than merges loses the title and the kind on the first one.
#[test]
fn a_partial_update_keeps_what_it_does_not_name() {
    let events = turn(
        vec![
            fixtures::update_tool_call(),
            fixtures::update_tool_call_completed(),
        ],
        fixtures::prompt_response_bare(),
    );
    let mark = external_marks(&events)[0];
    assert_eq!(
        mark["title"], "Read src/lib.rs (1 - 50)",
        "the update said nothing about the title, so it stands"
    );
    assert_eq!(mark["kind"], "read");
}

#[test]
fn a_diff_and_a_terminal_both_survive_whole() {
    let events = turn(
        vec![
            fixtures::update_tool_call_terminal(),
            fixtures::update_tool_call_diff(),
        ],
        fixtures::prompt_response_bare(),
    );
    let said = reasoning_of(&events);
    assert!(said.contains("run npm test"), "{said}");
    assert!(said.contains("terminal command-123"), "{said}");
    assert!(said.contains("+pub mod envelope;"), "{said}");
    let marks = external_marks(&events);
    assert_eq!(marks.len(), 2, "both calls close, finished or not");
    let diff = marks
        .iter()
        .find(|m| m["toolCallId"] == "toolu_02Edit")
        .expect("the edit");
    assert_eq!(diff["content"][0]["type"], "diff");
    assert_eq!(diff["content"][0]["newText"], "pub mod envelope;");
}

#[test]
fn a_failed_call_says_so_and_is_still_a_call() {
    let events = turn(
        vec![fixtures::update_tool_call_failed()],
        fixtures::prompt_response_bare(),
    );
    let mark = external_marks(&events)[0];
    assert_eq!(mark["status"], "failed");
    let said = reasoning_of(&events);
    assert!(said.contains("failed"), "{said}");
    assert!(said.contains("no such file"), "{said}");
}

/// ADR-0035 §6 leaves these unmapped on purpose. Silence is the decision, and
/// a fixture is how it stays one.
#[test]
fn a_plan_a_mode_and_a_command_list_say_nothing() {
    for recorded in [
        fixtures::update_plan(),
        fixtures::update_current_mode(),
        fixtures::update_available_commands(),
    ] {
        let mut mapper = Mapper::default();
        assert!(
            mapper.update(update_of(recorded)).is_empty(),
            "an unmapped update produces no events"
        );
    }
}

#[test]
fn the_end_turn_count_is_what_the_turn_cost() {
    let events = turn(Vec::new(), fixtures::prompt_response_with_usage());
    let Some(ModelEvent::Finish { usage, .. }) = events.last() else {
        panic!("a turn ends with a finish");
    };
    assert_eq!(usage.input_tokens, 1024);
    assert_eq!(usage.output_tokens, 64);
    assert_eq!(usage.cache_read_tokens, 512);
}

/// An adapter that reports no per-turn tokens still says how much context it
/// is holding, and for a stateful session that is what the turn read.
#[test]
fn without_an_end_turn_count_the_context_stands_in() {
    let events = turn(
        vec![fixtures::update_usage()],
        fixtures::prompt_response_bare(),
    );
    let Some(ModelEvent::Finish { usage, .. }) = events.last() else {
        panic!("a turn ends with a finish");
    };
    assert_eq!(usage.input_tokens, 12000);
    assert_eq!(usage.output_tokens, 0, "nothing is invented");
}

#[test]
fn an_adapter_that_counts_nothing_reports_zero() {
    let events = turn(Vec::new(), fixtures::prompt_response_bare());
    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish { usage, .. }) if *usage == Usage::default()
    ));
}

/// The agent ran its own tools, so a turn never ends in `ToolCalls`.
#[test]
fn every_stop_reason_keeps_its_own_word() {
    let cases = [
        ("end_turn", UnifiedFinish::Stop),
        ("max_tokens", UnifiedFinish::Length),
        ("refusal", UnifiedFinish::ContentFilter),
        ("cancelled", UnifiedFinish::Other),
        ("max_turn_requests", UnifiedFinish::Other),
    ];
    for (raw, unified) in cases {
        let stop: StopReason = serde_json::from_value(json!(raw)).expect("a recorded stop reason");
        let reason = finish_reason(stop);
        assert_eq!(reason.unified, unified, "{raw}");
        assert_eq!(reason.raw.as_deref(), Some(raw));
        assert_ne!(reason.unified, UnifiedFinish::ToolCalls);
    }
}

/// A stream that stops mid-block must not leave a surface waiting.
#[test]
fn a_turn_that_ends_mid_call_closes_everything_it_opened() {
    let events = turn(
        vec![
            fixtures::update_agent_message_chunk(),
            fixtures::update_agent_thought_chunk(),
            fixtures::update_tool_call(),
        ],
        fixtures::prompt_response_cancelled(),
    );
    assert_eq!(
        starts(&events),
        ends(&events),
        "every block that opened is closed"
    );
    assert_eq!(
        external_marks(&events).len(),
        1,
        "the unfinished call is kept"
    );
}

/// `session/load` replays the history it holds, our own turns included.
/// Journalling those would write the conversation twice.
#[test]
fn a_replayed_user_turn_is_never_an_event() {
    let replay = json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "rename the module" }
        }
    });
    let mut mapper = Mapper::default();
    assert!(mapper.update(update_of(replay)).is_empty());
}
