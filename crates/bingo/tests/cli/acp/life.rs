//! Black-box: an adapter child's death, and what replaces it (ADR-0035 §3).
//!
//! One rule, met three ways: a child found dead before the prompt is written,
//! one found dead by the write itself, and one that dies having already said
//! something — which is the one case where the turn is not quietly asked
//! again, because half an answer is on the stream and would be said twice.

use super::*;

/// ADR-0035 §3: an adapter that died between turns is replaced, not asked. The
/// replacement climbs back into the same agent session from the journal's own
/// pointer, and the person is told a child went.
#[test]
fn an_adapter_that_died_between_turns_is_replaced_and_said() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-7",
            "capabilities": { "resume": true },
            "turns": [
                { "updates": [chunk("First.")], "stopReason": "end_turn", "thenExit": true },
                one_turn(vec![chunk("Second.")])
            ]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("one");
    host.until("result");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    // The dead child's script starts again from its first turn, so the second
    // bingo turn is answered "First." by a new agent — which is the point:
    // it was answered at all.
    assert_eq!(ended.results().len(), 2, "{:?}", ended.types());
    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "initialize",
            "session/resume",
            "session/prompt"
        ],
        "a second handshake, and back into the same agent session"
    );
}

/// Every frame that says a turn was tried again. The kernel retries a
/// retryable failure in the open, withdrawing what the failed attempt had
/// already said (ADR-0009 §6); a child replaced inside one turn is not that,
/// and shows up here as nothing.
fn retried(ended: &stream_json::Ended) -> Vec<String> {
    frames(ended)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::TurnRetrying { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

/// The same rule as above, from the other side of the race: a child that dies
/// at the write rather than before it.
///
/// `Sessions::prepare` asked whether the child was alive and was told yes; the
/// prompt is what discovers otherwise. ADR-0035 §3 says a child that died
/// between turns is replaced rather than asked, and where the death was found
/// is not the person's business — so the turn buries it, climbs back into the
/// same agent session and puts the question again. One notice, one answer, and
/// no retry frame: nothing of the turn had been said, so nothing had to be
/// withdrawn and said over.
#[test]
fn a_child_that_dies_at_the_prompt_is_replaced_and_asked_again() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-8",
            "capabilities": { "resume": true },
            "turns": [
                one_turn(vec![chunk("Answered.")]),
                // Nothing streamed, then gone: the death a client can only
                // find at its own write.
                { "updates": [], "diesAtPromptOnce": true }
            ]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    host.until_event("turnCompleted");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    // The replacement starts the script over, so the second bingo turn is
    // answered in the first turn's words. That it was answered at all is the
    // whole scenario.
    assert_eq!(
        said(frames(&ended)),
        ["Answered.", "Answered."],
        "both turns answered: {:?}",
        ended.types()
    );
    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            // The prompt the child died on: heard, never answered.
            "session/prompt",
            "initialize",
            "session/resume",
            "session/prompt"
        ],
        "the question was put again on a new child, in the same agent session"
    );

    let all = notices(frames(&ended));
    let respawned = coded(&all, "ACP_RESPAWN");
    assert_eq!(respawned.len(), 1, "one death, one notice: {all:?}");
    assert!(
        retried(&ended).is_empty(),
        "and the turn itself never failed, so nothing was retried over it: {:?}",
        retried(&ended)
    );
}

/// The other half of the same rule: a child that dies *after* it has spoken is
/// not replaced inside the turn. Half an answer is on the stream, and asking
/// again would say that half twice — so the failure is the turn's, and it is
/// the kernel's open retry that withdraws the half and tries again (ADR-0009
/// §6), not this plugin quietly.
#[test]
fn a_child_that_dies_mid_stream_fails_the_turn_it_was_answering() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-9",
            "capabilities": { "resume": true },
            "turns": [{ "updates": [chunk("Half a")], "diesAtPromptOnce": true }]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.driven("agent"));
    host.prompt("one");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let reasons = retried(&ended);
    assert_eq!(
        reasons.len(),
        1,
        "the turn failed and was tried again where a person can see it: {:?}",
        ended.types()
    );
    let all = notices(frames(&ended));
    assert_eq!(
        coded(&all, "ACP_RESPAWN").len(),
        1,
        "the dead child was still buried, once: {all:?}"
    );
}
