//! Black-box: what becomes of a `session/request_permission` (ADR-0039 §3),
//! driven through the real binary against the scripted agent.
//!
//! Three of the four ways a question can end are here, because all three are a
//! `--print` run and read together: the session's own stance answers it
//! (allow, refuse), or nobody is there and it falls closed. The fourth — a
//! person answering — needs a surface that can, and lives in
//! `tests/acp_asked.rs`.
//!
//! Nothing here knows anything about the plugin's insides: a settings row, a
//! prompt, the frames that came out, and the log the agent wrote of the answer
//! it was actually sent.

use bingo_sdk::{AnswerRole, InteractionKind, Question};

use super::*;

/// One turn in which the agent asks before it says anything. Both adapters'
/// shapes in one: a narrow yes, a standing yes, a narrow no.
fn asks_first() -> Value {
    json!({
        "sessionId": "acp-asked",
        "capabilities": { "resume": true },
        "turns": [{
            "permission": {
                "toolCall": { "toolCallId": "c1", "title": "Edit src/lib.rs", "kind": "edit" },
                "options": [
                    { "optionId": "allow-once", "name": "Yes", "kind": "allow_once" },
                    { "optionId": "allow-always", "name": "Yes, and stop asking", "kind": "allow_always" },
                    { "optionId": "reject", "name": "No", "kind": "reject_once" }
                ]
            },
            "updates": [chunk("Done.")],
            "stopReason": "end_turn"
        }]
    })
}

/// The option the agent was sent back, from the answer it logged.
fn answered(adapter: &Scripted) -> Value {
    adapter
        .first("permission/answered")
        .expect("the agent got an answer")["outcome"]
        .clone()
}

fn interactions(frames: Vec<Frame>) -> Vec<InteractionKind> {
    frames
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::InteractionOpened { interaction } => Some(interaction.kind),
            _ => None,
        })
        .collect()
}

/// One run of one turn, with whatever the scenario says about permissions on
/// the command line.
fn run_asking(adapter: &Scripted, extra: &[&str]) -> Output {
    let mut args = vec!["--provider", "scripted", "--model", "agent"];
    args.extend_from_slice(extra);
    run(adapter.bingo(&args).arg("edit it"))
}

/// A session that lets everything happen answers the agent's own allow option
/// and asks nobody: no interaction is opened, and there is nothing to say
/// about it afterwards.
#[test]
fn a_bypass_session_answers_the_agents_own_allow_option() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(agent, asks_first());
    let out = run_asking(&adapter, &["--permission-mode", "bypassPermissions"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(said(frames_of(&out)), ["Done."], "the turn went on");

    assert_eq!(answered(&adapter)["outcome"], "selected");
    assert_eq!(
        answered(&adapter)["optionId"],
        "allow-once",
        "the narrow yes, not the standing one"
    );
    assert!(
        interactions(frames_of(&out)).is_empty(),
        "nobody was asked: {:?}",
        interactions(frames_of(&out))
    );
    let all = notices(frames_of(&out));
    assert!(
        coded(&all, "ACP_ASKED").is_empty(),
        "a question that was answered is not worth a notice: {all:?}"
    );
}

/// A session that refuses everything answers the agent's own reject option, at
/// once and for the same reason: the person said what to do, so there is
/// nothing to tell them.
#[test]
fn a_dont_ask_session_answers_the_agents_own_reject_option() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(agent, asks_first());
    let out = run_asking(&adapter, &["--permission-mode", "dontAsk"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(said(frames_of(&out)), ["Done."], "the turn went on");

    assert_eq!(answered(&adapter)["optionId"], "reject");
    assert!(
        interactions(frames_of(&out)).is_empty(),
        "nobody was asked: {:?}",
        interactions(frames_of(&out))
    );
    let all = notices(frames_of(&out));
    assert!(
        coded(&all, "ACP_ASKED").is_empty(),
        "the person answered it in advance: {all:?}"
    );
}

/// Headless, in the mode that would ask: the question is put — in the agent's
/// own words, with the two roles a session may answer marked — and the surface
/// with nobody at it declines. That is nobody having answered, so the agent
/// gets its own refusal and one notice names both ways out.
#[test]
fn a_headless_run_puts_the_question_and_falls_closed_when_nobody_answers() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(agent, asks_first());
    let out = run_asking(&adapter, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(said(frames_of(&out)), ["Done."], "the turn went on");

    let asked = interactions(frames_of(&out));
    let [
        InteractionKind::Question(Question {
            question,
            header,
            options,
            ..
        }),
    ] = asked.as_slice()
    else {
        panic!("one question, and it is the agent's: {asked:?}");
    };
    assert_eq!(question, "Edit src/lib.rs", "the agent's own title");
    assert_eq!(header.as_deref(), Some("scripted"), "and its own name");
    assert_eq!(
        options
            .iter()
            .map(|option| (option.id.as_str(), option.label.as_str(), option.role))
            .collect::<Vec<_>>(),
        [
            ("allow-once", "Yes", Some(AnswerRole::Allowing)),
            ("allow-always", "Yes, and stop asking", None),
            ("reject", "No", Some(AnswerRole::Refusing))
        ],
        "its ids and labels verbatim; the standing yes is a person's alone"
    );

    assert_eq!(answered(&adapter)["outcome"], "selected");
    assert_eq!(answered(&adapter)["optionId"], "reject");
    let all = notices(frames_of(&out));
    let told = coded(&all, "ACP_ASKED");
    assert_eq!(told.len(), 1, "said once: {all:?}");
    assert!(
        told[0].contains("acp.adapters.scripted") && told[0].contains("got no answer"),
        "the notice says what happened and names the row: {}",
        told[0]
    );
}

/// The other door stays declined (ADR-0039 §3): free-form input is another
/// interaction shape, and the person is told the once.
#[test]
fn an_elicitation_is_still_declined_and_said_once() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-elicit",
            "capabilities": { "resume": true },
            "turns": [{
                "elicitation": {
                    "mode": "form",
                    "sessionId": "acp-elicit",
                    "requestedSchema": { "type": "object", "properties": {} },
                    "message": "Which branch?"
                },
                "updates": [chunk("Asked nothing.")],
                "stopReason": "end_turn"
            }]
        }),
    );
    let out = run_asking(&adapter, &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let answered = adapter
        .first("elicitation/answered")
        .expect("the agent got an answer");
    assert_eq!(answered["action"], "decline");
    assert!(
        interactions(frames_of(&out)).is_empty(),
        "and nobody was asked anything"
    );
    assert_eq!(coded(&notices(frames_of(&out)), "ACP_ASKED").len(), 1);
}
