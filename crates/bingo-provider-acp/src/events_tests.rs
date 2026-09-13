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

// ---------------------------------------- the context the agent holds

fn readings(events: &[ModelEvent]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::Context { used, window } => Some((*used, *window)),
            _ => None,
        })
        .collect()
}

fn cuts(events: &[ModelEvent]) -> Vec<(u64, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::Compacted { before, after } => Some((*before, *after)),
            _ => None,
        })
        .collect()
}

/// ADR-0055 §4: every `usage_update` is the agent's own count of what it
/// holds, whoever sent it and whatever else rode with it.
#[test]
fn every_usage_update_is_a_reading() {
    let claude = turn(
        vec![fixtures::update_usage()],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(readings(&claude), [(12_000, 200_000)]);
    let codex = turn(
        vec![fixtures::update_usage_bare()],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(readings(&codex), [(400_000, 1_000_000)]);
    assert!(cuts(&codex).is_empty(), "a first reading fell from nothing");
}

/// The frames a compaction really arrives as: a banner, the reading that
/// fell, another banner. The fall is the only thing that says a cut happened,
/// and the two banners are the adapter's status rather than the agent's words
/// (ADR-0055 §4).
#[test]
fn a_reading_that_fell_is_the_cut_the_agent_made_and_the_banners_are_dropped() {
    let events = turn(
        vec![
            fixtures::update_usage_bare(),
            fixtures::update_compacting_banner(),
            fixtures::update_usage_after_compaction(),
            fixtures::update_compacted_banner(),
            fixtures::update_agent_message_chunk(),
        ],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(cuts(&events), [(400_000, 120_000)]);
    assert_eq!(
        readings(&events),
        [(400_000, 1_000_000), (120_000, 1_000_000)]
    );
    assert_eq!(
        events
            .iter()
            .position(|e| matches!(e, ModelEvent::Compacted { .. })),
        events
            .iter()
            .position(|e| matches!(e, ModelEvent::Context { used: 120_000, .. }))
            .map(|at| at - 1),
        "the cut is said before the reading it explains"
    );
    assert_eq!(
        text_of(&events),
        "Renaming ",
        "and neither banner is anything the agent said"
    );
}

/// The one banner a person needs: the agent tried to make room and could not.
#[test]
fn the_failure_banner_is_the_agents_own_words() {
    let events = turn(
        vec![fixtures::update_compaction_failed_banner()],
        fixtures::prompt_response_bare(),
    );
    assert_eq!(
        text_of(&events),
        "\n\nCompacting failed: the summary model refused"
    );
}

/// A conversation is read across its turns: the agent compacts between two of
/// them as readily as inside one, and the fall is the same fall (ADR-0055 §4).
#[test]
fn a_fall_between_two_turns_is_still_a_cut() {
    let mut first = Mapper::default();
    first.update(update_of(fixtures::update_usage_bare()));
    let held = first.held().expect("the agent said what it holds");
    assert_eq!((held.used, held.window), (400_000, 1_000_000));

    let mut second = Mapper::holding(Some(held));
    let events = second.update(update_of(fixtures::update_usage_after_compaction()));
    assert_eq!(cuts(&events), [(400_000, 120_000)]);
    assert_eq!(readings(&events), [(120_000, 1_000_000)]);
}

/// A reading that grew is a conversation that grew: no cut, just the count.
#[test]
fn a_reading_that_rose_is_no_cut_at_all() {
    let mut mapper = Mapper::holding(Some(Reading {
        used: 100,
        window: 1_000_000,
    }));
    let events = mapper.update(update_of(fixtures::update_usage_bare()));
    assert!(cuts(&events).is_empty());
    assert_eq!(readings(&events), [(400_000, 1_000_000)]);
}
