//! Black-box: an ACP agent's permission question put to a person and answered
//! by them (ADR-0039 §3).
//!
//! The other three ways a question can end are `--print` runs and live with the
//! rest of the ACP scenarios (`tests/cli/acp/asking.rs`). This one needs a
//! client that can answer a question with one of its options, which is what
//! every attached surface does and what the JSON-RPC face makes reachable from
//! a test: `bingo serve --stdio`, one settings row naming the scripted agent,
//! and the answer sent back the way a TUI sends it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, AnswerRole, Attachment, Event, Frame, HostApi, Input, IntentId,
    Interaction, InteractionKind, ItemBody, OpenOptions, Origin, Question, QuestionOption,
    SessionSelector, SessionSpec,
};
use futures::StreamExt;
use serde_json::{Value, json};

mod support;

use support::{LIMIT, Server, ready, until_completed, who};

/// The scripted agent is a binary of another crate, built beside this one.
/// `cargo test --workspace` and CI build it; a bare `cargo test -p bingo` does
/// not, and this says so once rather than failing on a file nobody in that
/// invocation asked for.
fn fake_agent() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_BIN_EXE_bingo")).with_file_name(format!(
        "bingo-fake-acp-agent{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !path.exists() {
        eprintln!(
            "the ACP black-box is skipped: {} is not built. Run the suite as \
             `cargo test --workspace`.",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// One adapter configured in a home, the script it obeys, and the log it
/// appends every message it received to. The home itself goes to the server,
/// which is what keeps it alive: it is that run's `HOME` and the session's cwd.
struct Configured {
    cwd: PathBuf,
    settings: String,
    log: PathBuf,
}

impl Configured {
    fn new(agent: &Path) -> (Configured, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("acp-script.json");
        let log = home.path().join("acp-log.jsonl");
        std::fs::write(&script, asks_first().to_string()).unwrap();
        let settings = home.path().join("settings.json");
        std::fs::write(
            &settings,
            json!({ "acp": { "adapters": { "scripted": {
                "command": agent,
                "env": {
                    "BINGO_FAKE_ACP_SCRIPT": script,
                    "BINGO_FAKE_ACP_LOG": log,
                }
            }}}})
            .to_string(),
        )
        .unwrap();
        asked_before(home.path());
        let configured = Configured {
            cwd: home.path().to_path_buf(),
            settings: settings.display().to_string(),
            log,
        };
        (configured, home)
    }

    /// The option the agent was sent back, from the answer it logged.
    fn answered(&self) -> Value {
        let body = std::fs::read_to_string(&self.log).unwrap_or_default();
        body.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|line| line["method"] == "permission/answered")
            .expect("the agent got an answer")["params"]["outcome"]
            .clone()
    }
}

/// A home that was already told what this adapter serves, so the cold ask
/// every fresh machine makes (ADR-0026 §4) does not put a second child beside
/// the one this scenario is about.
fn asked_before(home: &Path) {
    let file = home.join(".bingo/data/served-models.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let served = json!({
        "scripted": {
            "fetched": jiff::Timestamp::now().to_string(),
            "models": [{ "id": "agent" }]
        }
    });
    std::fs::write(&file, served.to_string()).unwrap();
}

/// One turn in which the agent asks before it says anything: a narrow yes, a
/// standing yes, a narrow no.
fn asks_first() -> Value {
    json!({
        "sessionId": "acp-asked-by-a-person",
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
            "updates": [{
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "Edited." },
                "messageId": "m1"
            }],
            "stopReason": "end_turn"
        }]
    })
}

fn session(cwd: PathBuf) -> SessionSelector {
    SessionSelector::Create {
        spec: SessionSpec {
            cwd,
            provider: Some("scripted".into()),
            model: Some("agent".into()),
            ..SessionSpec::default()
        },
    }
}

/// Fold frames until one opens an interaction, or say what arrived instead.
async fn until_asked(attachment: &mut Attachment) -> Interaction {
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    let mut seen = Vec::new();
    loop {
        tokio::select! {
            frame = attachment.events.next() => {
                let frame = frame.expect("the stream stays open");
                attachment.snapshot.apply(&frame);
                if let Event::InteractionOpened { interaction } = frame.event {
                    return interaction;
                }
                seen.push(frame.event);
            }
            _ = &mut deadline => panic!("nobody was asked anything: {seen:?}"),
        }
    }
}

fn options(kind: &InteractionKind) -> Vec<QuestionOption> {
    match kind {
        InteractionKind::Question(Question { options, .. }) => options.clone(),
        other => panic!("a permission request is a question: {other:?}"),
    }
}

fn notices(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::Notice { code, text, .. } => Some(format!("{code}: {text}")),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The question reaches the person in the agent's own words, and the option
/// they pick reaches the agent as its own option id (ADR-0039 §3). Nothing is
/// said about it afterwards: a question that was answered is not a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_person_answers_the_agents_question_and_the_agent_gets_their_choice() {
    let Some(agent) = fake_agent() else { return };
    let (configured, home) = Configured::new(&agent);
    let mut server = Server::spawn_at(
        home,
        r#"{"responses":[]}"#,
        &["--settings", &configured.settings],
    );
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(
            session(configured.cwd.clone()),
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    attachment.handle.submit(
        IntentId::mint(),
        Input::text("edit it", Origin::surface("test")),
    );

    let asked = until_asked(&mut attachment).await;
    let InteractionKind::Question(Question {
        question, header, ..
    }) = &asked.kind
    else {
        panic!("a permission request is a question: {:?}", asked.kind);
    };
    assert_eq!(question, "Edit src/lib.rs", "the agent's own title");
    assert_eq!(header.as_deref(), Some("scripted"), "and its own name");
    assert_eq!(
        options(&asked.kind)
            .iter()
            .map(|option| (option.id.clone(), option.label.clone(), option.role))
            .collect::<Vec<_>>(),
        [
            (
                "allow-once".to_string(),
                "Yes".to_string(),
                Some(AnswerRole::Allowing)
            ),
            (
                "allow-always".to_string(),
                "Yes, and stop asking".to_string(),
                None
            ),
            (
                "reject".to_string(),
                "No".to_string(),
                Some(AnswerRole::Refusing)
            )
        ],
        "the agent's ids and labels verbatim"
    );

    // As a surface answers: not from the keyboard, so the stray-keystroke
    // guard on a freshly opened question has nothing to guard against.
    attachment.handle.answer(
        IntentId::mint(),
        asked.id.clone(),
        Answer::Choice {
            ids: vec!["allow-once".into()],
            other: None,
        },
        Activation::Pointer,
    );
    let frames = until_completed(&mut attachment).await;
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame.event, Event::InteractionResolved { .. })),
        "the question was resolved, not cancelled under"
    );
    assert_eq!(configured.answered()["outcome"], "selected");
    assert_eq!(
        configured.answered()["optionId"],
        "allow-once",
        "the person's choice, in the agent's own spelling"
    );
    assert!(
        !notices(&frames)
            .iter()
            .any(|said| said.contains("ACP_ASKED")),
        "a question a person answered is nobody's refusal: {:?}",
        notices(&frames)
    );

    // The agent is answering again, so the answer reached it: the turn ends
    // with what it said afterwards.
    let said: Vec<String> = frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::Assistant { text } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(said, ["Edited."]);

    tokio::time::timeout(Duration::from_secs(10), kernel.shutdown())
        .await
        .expect("the server shuts down")
        .unwrap();

    // And it is in the journal on disk, as a gate question's is: an
    // interaction is durable, so what was asked and what was answered survive
    // the run that asked it.
    let journaled = journal(&server.sessions_dir());
    let opened = journaled
        .iter()
        .find(|line| line["event"]["type"] == "interactionOpened")
        .expect("the question is journaled");
    assert_eq!(
        opened["event"]["interaction"]["kind"]["options"][0]["role"], "allowing",
        "the role rides the option it was written on"
    );
    let resolved = journaled
        .iter()
        .find(|line| line["event"]["type"] == "interactionResolved")
        .expect("and so is the answer");
    assert_eq!(resolved["event"]["answer"]["ids"][0], "allow-once");
}

/// Every frame every session of this run journaled.
fn journal(sessions: &Path) -> Vec<Value> {
    let mut lines = Vec::new();
    collect(sessions, &mut lines);
    lines
}

fn collect(dir: &Path, into: &mut Vec<Value>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            into.extend(
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok()),
            );
        }
    }
}
