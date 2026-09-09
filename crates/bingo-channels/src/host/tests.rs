mod kernel;

use std::collections::BTreeSet;
use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, Env, ErrorCode, Event, HostApi, HostHandle, InteractionId, ItemStatus,
    KernelError, ResolvedBy, SessionSelector, SurfaceOptions, TurnStatus, Usage,
};

use super::*;
use crate::access::{Access, Policy, Rule};
use crate::adapter::{Incoming, Mode, Outcome};
use crate::conversation::Posted;
use crate::fixtures;
use crate::lock::Claim;
use crate::loopback::{self, Loopback, Record};
use kernel::{TestHost, TestSession};
pub use kernel::{nowhere, options};

// ---- the fixture ---------------------------------------------------------

/// The surface running against the double, with one loopback to speak into.
struct Chat {
    host: Arc<TestHost>,
    loopback: Arc<Loopback>,
    _home: tempfile::TempDir,
    /// Stopped with the fixture: a surface outliving its test would keep the
    /// claim on the credential the next one wants.
    _surface: Running,
}

/// A spawned surface that stops when the test does.
struct Running(tokio::task::JoinHandle<Result<Exit, KernelError>>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Chat {
    fn open() -> Self {
        Self::with(loopback::Config::default())
    }

    fn with(config: loopback::Config) -> Self {
        Self::under(config, Access::default())
    }

    fn under(config: loopback::Config, access: Access) -> Self {
        let home = tempfile::tempdir().expect("a temporary home");
        let loopback = Arc::new(Loopback::new(config));
        let surface = ChannelsSurface::new(
            vec![Arc::clone(&loopback) as Arc<dyn ChannelAdapter>],
            // A test should not have to wait for a coalescer.
            Gate {
                min_chars: 1_000,
                interval: Duration::from_millis(10),
            },
            BTreeMap::from([(Loopback::ID.to_string(), access)]),
        );
        let host = Arc::new(TestHost::default());
        let handle = HostHandle(Arc::clone(&host) as Arc<dyn HostApi>);
        let options = SurfaceOptions {
            env: Arc::new(Env::rooted(home.path())),
            ..options("/tmp")
        };
        Self {
            host,
            loopback,
            _home: home,
            _surface: Running(tokio::spawn(
                async move { surface.run(handle, options).await },
            )),
        }
    }

    async fn say(&self, event: Incoming) {
        self.loopback.hear(event).await.expect("the surface hears");
    }

    /// The session the chat opened, once it has.
    async fn session(&self, key: &str) -> Arc<TestSession> {
        self.until(|| self.host.session(key)).await
    }

    /// Poll until something has happened, or fail the scenario.
    async fn until<T>(&self, mut ready: impl FnMut() -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "nothing happened in five seconds; the loopback has: {:?}",
                self.loopback.records()
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Wait until the loopback has been asked for `n` things, and hand them over.
    async fn records(&self, n: usize) -> Vec<Record> {
        self.until(|| {
            let records = self.loopback.records();
            (records.len() >= n).then_some(records)
        })
        .await
    }
}

fn said(conversation: Conversation, text: &str, addressed: bool) -> Incoming {
    Incoming::Message {
        conversation,
        principal: "ou_person".into(),
        text: text.into(),
        images: Vec::new(),
        addressed,
        parent: None,
    }
}

fn hello(chat: &str) -> Incoming {
    said(Conversation::direct(chat), "run the tests", true)
}

/// The same, arriving under a handle — what a platform that threads gives the
/// surface, and the only thing a sign can be put on.
fn spoke(chat: &str, at: &str) -> Incoming {
    match hello(chat) {
        Incoming::Message {
            conversation,
            principal,
            text,
            images,
            addressed,
            ..
        } => Incoming::Message {
            conversation,
            principal,
            text,
            images,
            addressed,
            parent: Some(Posted::new(at)),
        },
        click => click,
    }
}

/// A turn that starts, goes wrong, and ends.
async fn fails(session: &TestSession) {
    session.publish(Event::TurnStarted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        inputs: Vec::new(),
        origin: bingo_sdk::TurnOrigin::Submit,
    });
    session.publish(Event::TurnCompleted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        status: TurnStatus::Failed {
            error: KernelError::new(ErrorCode::ProviderUnavailable, "no provider"),
        },
        usage: Usage::default(),
    });
}

/// A turn that says one thing and ends.
async fn answers(session: &TestSession, text: &str) {
    session.publish(Event::TurnStarted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        inputs: Vec::new(),
        origin: bingo_sdk::TurnOrigin::Submit,
    });
    session.publish(Event::ItemCompleted {
        item: fixtures::assistant("itm_1", text, ItemStatus::Completed),
    });
    session.publish(Event::TurnCompleted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        status: TurnStatus::Completed,
        usage: Usage::default(),
    });
}

// ---- the tests -----------------------------------------------------------

#[tokio::test]
async fn a_message_opens_a_session_keyed_by_its_chat_and_carries_who_spoke() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    let origin = chat.until(|| session.origins().first().cloned()).await;
    assert_eq!(session.prompts(), ["run the tests"]);
    assert_eq!(origin.surface, "channels");
    assert_eq!(origin.principal.as_deref(), Some("ou_person"));
    assert_eq!(origin.conversation.as_deref(), Some("loopback/oc_1"));
    assert!(
        matches!(
            chat.host.opened().first(),
            Some(SessionSelector::ByKey { key }) if key == "loopback/oc_1"
        ),
        "an existing session is continued before a new one is minted: {:?}",
        chat.host.opened()
    );
}

/// A picture reaches the kernel beside the words that came with it, and a
/// picture alone is an ask with no words (ADR-0040).
#[tokio::test]
async fn a_picture_is_submitted_beside_its_words() {
    let chat = Chat::open();
    let image = bingo_sdk::Image::from_bytes("image/png", b"png").expect("a picture");
    let Incoming::Message {
        conversation,
        principal,
        addressed,
        parent,
        ..
    } = hello("oc_1")
    else {
        panic!("a message");
    };
    chat.say(Incoming::Message {
        conversation,
        principal,
        text: String::new(),
        images: vec![image.clone()],
        addressed,
        parent,
    })
    .await;
    let session = chat.session("loopback/oc_1").await;
    chat.until(|| session.pictures().first().cloned()).await;
    assert_eq!(session.prompts(), [""]);
    assert_eq!(session.pictures(), [vec![image]]);
}

#[tokio::test]
async fn a_second_message_continues_the_same_session() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    chat.say(hello("oc_1")).await;
    chat.until(|| (session.prompts().len() == 2).then_some(()))
        .await;
    assert_eq!(chat.host.keys(), ["loopback/oc_1"], "one session, not two");
}

#[tokio::test]
async fn a_thread_is_a_session_of_its_own() {
    let chat = Chat::open();
    chat.say(said(
        Conversation::group("oc_1").in_thread("omt_9"),
        "@bingo run the tests",
        true,
    ))
    .await;
    chat.session("loopback/oc_1/omt_9").await;
}

#[tokio::test]
async fn a_group_that_did_not_address_the_bot_opens_nothing() {
    let chat = Chat::open();
    chat.say(said(Conversation::group("oc_1"), "who is there?", false))
        .await;
    chat.say(said(
        Conversation::group("oc_2"),
        "@bingo run the tests",
        true,
    ))
    .await;
    chat.session("loopback/oc_2").await;
    assert_eq!(
        chat.host.keys(),
        ["loopback/oc_2"],
        "silence in a group is not a session"
    );
}

/// The policy answers where the mention alone used to (ADR-0051 §4). A
/// refusal is silent: nothing is opened, and nothing is said back — a person
/// who may not speak here is not told so in the chat.
#[tokio::test]
async fn a_principal_the_policy_refuses_opens_nothing_and_hears_nothing() {
    let chat = Chat::under(
        loopback::Config::default(),
        Access {
            group: Rule {
                policy: Policy::Blocklist,
                list: BTreeSet::from(["ou_person".to_string()]),
                mention: false,
            },
            ..Access::default()
        },
    );
    chat.say(said(Conversation::group("oc_1"), "run the tests", false))
        .await;
    chat.say(hello("oc_2")).await;
    chat.session("loopback/oc_2").await;
    assert_eq!(
        chat.host.keys(),
        ["loopback/oc_2"],
        "the blocklisted group opened no session"
    );
    assert!(
        chat.loopback.records().is_empty(),
        "and was answered with nothing: {:?}",
        chat.loopback.records()
    );
}

#[tokio::test]
async fn an_answer_streams_into_one_message_and_is_finished_there() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    answers(&session, "Two tests failed.").await;
    let records = chat.records(3).await;
    assert!(
        matches!(&records[0], Record::Send { mode: Mode::Stream, text, .. } if text.is_empty()),
        "{records:?}"
    );
    assert!(
        matches!(&records[1], Record::Replace { text, .. } if text == "Two tests failed."),
        "{records:?}"
    );
    assert!(
        matches!(&records[2], Record::Finish { text, .. } if text == "Two tests failed."),
        "{records:?}"
    );
}

#[tokio::test]
async fn without_an_edit_the_answer_arrives_whole_and_once() {
    let chat = Chat::with(loopback::Config {
        edits: false,
        threads: false,
        typing: false,
        ..loopback::Config::default()
    });
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    answers(&session, "Two tests failed.").await;
    let records = chat.records(1).await;
    assert_eq!(
        records,
        [Record::Send {
            to: Conversation::direct("oc_1"),
            id: Posted::new("m1"),
            text: "Two tests failed.".into(),
            mode: Mode::Once,
        }]
    );
}

/// The sign brackets the turn a message started (ADR-0051 §5): up before the
/// chat says anything back, down when that turn has ended.
#[tokio::test]
async fn a_sign_goes_up_on_the_message_that_spoke_and_comes_off_at_the_end() {
    let chat = Chat::open();
    chat.say(spoke("oc_1", "om_1")).await;
    let session = chat.session("loopback/oc_1").await;
    assert_eq!(
        chat.records(1).await[0],
        Record::Acknowledge {
            at: Posted::new("om_1"),
        },
        "before a word is said back"
    );
    answers(&session, "Two tests failed.").await;
    let records = chat.records(5).await;
    assert_eq!(
        records.last(),
        Some(&Record::Acknowledged {
            at: Posted::new("om_1"),
            outcome: Outcome::Done,
        }),
        "{records:?}"
    );
}

#[tokio::test]
async fn a_turn_that_failed_takes_the_sign_off_as_a_failure() {
    let chat = Chat::open();
    chat.say(spoke("oc_1", "om_1")).await;
    let session = chat.session("loopback/oc_1").await;
    fails(&session).await;
    let records = chat.records(3).await;
    assert_eq!(
        records.last(),
        Some(&Record::Acknowledged {
            at: Posted::new("om_1"),
            outcome: Outcome::Failed,
        }),
        "{records:?}"
    );
}

/// A sign is an affordance, not the answer. A platform that refused to put
/// one up has not refused the turn.
#[tokio::test]
async fn a_sign_that_would_not_go_up_costs_no_part_of_the_answer() {
    let chat = Chat::open();
    chat.loopback.refuse_once("acknowledge");
    chat.say(spoke("oc_1", "om_1")).await;
    let session = chat.session("loopback/oc_1").await;
    answers(&session, "Two tests failed.").await;
    let records = chat.records(3).await;
    assert!(
        !records.iter().any(|record| matches!(
            record,
            Record::Acknowledge { .. } | Record::Acknowledged { .. }
        )),
        "no sign was ever up, so none comes off: {records:?}"
    );
    assert!(
        records.iter().any(
            |record| matches!(record, Record::Finish { text, .. } if text == "Two tests failed.")
        ),
        "the answer arrived anyway: {records:?}"
    );
}

/// The permission the fake kernel opens mid-turn, and what settles it.
async fn asks(session: &TestSession) {
    session.publish(Event::TurnStarted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        inputs: Vec::new(),
        origin: bingo_sdk::TurnOrigin::Submit,
    });
    session.publish(Event::InteractionOpened {
        interaction: fixtures::permission(None),
    });
}

#[tokio::test]
async fn a_platform_that_cannot_stream_says_it_is_typing_instead() {
    let chat = Chat::with(loopback::Config {
        edits: false,
        threads: false,
        ..loopback::Config::default()
    });
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    answers(&session, "Two tests failed.").await;
    let records = chat.records(2).await;
    assert_eq!(
        records[0],
        Record::Typing {
            to: Conversation::direct("oc_1"),
        },
        "the answer will arrive whole and late, so say something meanwhile"
    );
}

#[tokio::test]
async fn a_platform_that_streams_needs_no_typing_affordance() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    answers(&session, "Two tests failed.").await;
    let records = chat.records(3).await;
    assert!(
        !records.iter().any(|r| matches!(r, Record::Typing { .. })),
        "the message writing itself is the sign: {records:?}"
    );
}

#[tokio::test]
async fn a_question_becomes_buttons_and_a_click_answers_it() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    asks(&session).await;
    let records = chat.records(1).await;
    let Record::Ask { id, question, .. } = &records[0] else {
        panic!("expected buttons, got {records:?}");
    };
    assert_eq!(question.choices.len(), 2, "{question:?}");
    assert_eq!(id, &Posted::new("m1"));

    chat.say(Incoming::Click {
        conversation: Conversation::direct("oc_1"),
        principal: "ou_person".into(),
        question: question.id.clone(),
        choice: "1".into(),
    })
    .await;
    let answered = chat.until(|| session.answers().first().cloned()).await;
    assert_eq!(answered.0, InteractionId::from_raw("int_1"));
    assert_eq!(answered.1, Answer::AllowOnce);
    assert_eq!(answered.2, Activation::Pointer, "a button is a pointer");
}

/// A chat has no card that walks tabs, so a set of questions is asked one
/// message at a time and answered once, at the last of them (M53). The message
/// just answered says what it was answered with and loses its buttons.
#[tokio::test]
async fn a_form_is_asked_one_message_at_a_time_and_answered_once() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    session.publish(Event::TurnStarted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        inputs: Vec::new(),
        origin: bingo_sdk::TurnOrigin::Submit,
    });
    session.publish(Event::InteractionOpened {
        interaction: fixtures::form(),
    });
    let records = chat.records(1).await;
    let Record::Ask { question, .. } = &records[0] else {
        panic!("expected the first question, got {records:?}");
    };
    assert_eq!(question.prompt, "Store: Which store?");

    // The first reply fills one slot: the kernel hears nothing yet, the
    // answered message loses its buttons, and the next question arrives.
    chat.say(Incoming::Click {
        conversation: Conversation::direct("oc_1"),
        principal: "ou_person".into(),
        question: question.id.clone(),
        choice: "2".into(),
    })
    .await;
    let records = chat.records(3).await;
    assert_eq!(
        records[1],
        Record::Settle {
            at: Posted::new("m1"),
            outcome: "chose SQLite".into(),
        },
        "no live button outlives its question: {records:?}"
    );
    let Record::Ask { question, .. } = &records[2] else {
        panic!("expected the second question, got {records:?}");
    };
    assert_eq!(question.prompt, "Runtime: Which runtime?");
    assert!(
        session.answers().is_empty(),
        "nothing is sent until all of it is"
    );

    // The last reply answers the whole set, in the order it was asked.
    chat.say(said(Conversation::direct("oc_1"), "async-std", true))
        .await;
    let answered = chat.until(|| session.answers().first().cloned()).await;
    assert_eq!(answered.0, InteractionId::from_raw("int_1"));
    assert_eq!(
        answered.1,
        Answer::Form {
            answers: vec![
                Answer::Choice {
                    ids: vec!["1".into()],
                    other: None,
                },
                Answer::Text {
                    text: "async-std".into()
                },
            ]
        }
    );
}

#[tokio::test]
async fn without_buttons_the_numbered_rung_is_drawn_and_a_reply_answers_it() {
    let chat = Chat::with(loopback::Config {
        buttons: false,
        ..loopback::Config::default()
    });
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    asks(&session).await;
    let records = chat.records(1).await;
    let Record::Send { text, .. } = &records[0] else {
        panic!("expected a numbered list, got {records:?}");
    };
    assert!(text.contains("1. Allow once"), "{text}");
    assert!(text.contains("2. Deny"), "{text}");

    chat.say(said(Conversation::direct("oc_1"), "2", true))
        .await;
    let answered = chat.until(|| session.answers().first().cloned()).await;
    assert_eq!(answered.1, Answer::Deny { feedback: None });
    assert_eq!(
        answered.2,
        Activation::Pointer,
        "a message that had to be sent is not a stray keystroke"
    );
    assert!(
        session.prompts().len() == 1,
        "an answer is not also a prompt: {:?}",
        session.prompts()
    );
}

/// Neither buttons nor an edit: there is no live button to strip, and the
/// outcome is said rather than lost.
#[tokio::test]
async fn with_nothing_to_edit_the_outcome_is_said_in_a_message_of_its_own() {
    let chat = Chat::with(loopback::Config {
        buttons: false,
        edits: false,
        threads: false,
        ..loopback::Config::default()
    });
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    asks(&session).await;
    chat.records(1).await;
    session.publish(Event::InteractionResolved {
        id: InteractionId::from_raw("int_1"),
        answer: Answer::AllowOnce,
        by: ResolvedBy::Client {
            name: "tui".into(),
            surface: "tui".into(),
        },
    });
    let records = chat.records(2).await;
    let Record::Send { text, .. } = records.last().expect("the outcome") else {
        panic!("expected a message, got {records:?}");
    };
    assert!(text.contains("approved in the TUI"), "{text}");
}

#[tokio::test]
async fn a_resolution_at_another_surface_edits_the_card_this_chat_showed() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    asks(&session).await;
    chat.records(1).await;

    // The person walked over to the TUI and approved it there.
    session.publish(Event::InteractionResolved {
        id: InteractionId::from_raw("int_1"),
        answer: Answer::AllowOnce,
        by: ResolvedBy::Client {
            name: "tui".into(),
            surface: "tui".into(),
        },
    });
    let records = chat.records(2).await;
    assert_eq!(
        records[1],
        Record::Settle {
            at: Posted::new("m1"),
            outcome: "approved in the TUI".into(),
        },
        "no live button outlives its question"
    );
}

#[tokio::test]
async fn a_second_surface_on_one_credential_refuses_loudly() {
    let home = tempfile::tempdir().expect("a temporary home");
    let here = |home: &std::path::Path| SurfaceOptions {
        env: Arc::new(Env::rooted(home)),
        ..options("/tmp")
    };
    let surface = || {
        ChannelsSurface::new(
            vec![Arc::new(Loopback::new(loopback::Config::default())) as Arc<dyn ChannelAdapter>],
            Gate::default(),
            BTreeMap::new(),
        )
    };
    // What the first process left behind while it runs.
    let held = Claim::take(&Env::rooted(home.path()).data_dir, "loopback", "offline")
        .expect("the first process claims it");
    let error = surface()
        .run(nowhere(), here(home.path()))
        .await
        .expect_err("the second must refuse");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(
        error.message.contains("another bingo already runs"),
        "{error}"
    );
    drop(held);
    // With the first process gone the claim is free, and the surface starts.
    let options = here(home.path());
    let started = tokio::spawn(async move { surface().run(nowhere(), options).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!started.is_finished(), "the second run holds the claim now");
    started.abort();
}

// ---- what a refusal must not cost ----------------------------------------
//
// A platform refuses things: it rate-limits, it closes a streamed card out
// from under a long answer, it declines a button layout. None of those may
// cost the conversation the *question*, because a question that never arrives
// is a session waiting on an interaction nobody was ever shown — and every
// message after it queues behind a turn that can never end. That is the "it
// worked for a few messages and then everything stuck, and reconnecting did
// not help" failure, and no reconnect can help it: the stall is in the
// session, not in the socket.

/// A turn that says something and then stops to ask — the shape that carries
/// the answer and the question in one `Finalize`.
async fn answers_then_asks(session: &TestSession, text: &str) {
    session.publish(Event::TurnStarted {
        turn: bingo_sdk::TurnId::from_raw(fixtures::TURN),
        inputs: Vec::new(),
        origin: bingo_sdk::TurnOrigin::Submit,
    });
    session.publish(Event::ItemCompleted {
        item: fixtures::assistant("itm_1", text, ItemStatus::Completed),
    });
    session.publish(Event::InteractionOpened {
        interaction: fixtures::permission(None),
    });
}

#[tokio::test]
async fn a_refused_finish_still_asks_the_question_and_says_the_answer_whole() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    chat.loopback.refuse_once("finish");
    answers_then_asks(&session, "Two tests failed.").await;

    let records = chat.records(3).await;
    assert!(
        records.iter().any(|record| matches!(
            record,
            Record::Send { text, .. } if text.contains("Two tests failed.")
        )),
        "the answer is not lost with the card it was written into: {records:?}"
    );
    let question = records
        .iter()
        .find_map(|record| match record {
            Record::Ask { question, .. } => Some(question.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the question survives a refused finish: {records:?}"));

    // And it is a real question, not a message that looks like one: clicking
    // it answers the interaction the session is waiting on.
    chat.say(Incoming::Click {
        conversation: Conversation::direct("oc_1"),
        principal: "ou_person".into(),
        question: question.id.clone(),
        choice: "1".into(),
    })
    .await;
    let answered = chat.until(|| session.answers().first().cloned()).await;
    assert_eq!(answered.0, InteractionId::from_raw("int_1"));
    assert_eq!(answered.1, Answer::AllowOnce);
}

#[tokio::test]
async fn refused_buttons_ask_in_words_rather_than_losing_the_question() {
    let chat = Chat::open();
    chat.say(hello("oc_1")).await;
    let session = chat.session("loopback/oc_1").await;
    chat.loopback.refuse_once("ask");
    asks(&session).await;

    let records = chat.records(1).await;
    let numbered = records
        .iter()
        .find_map(|record| match record {
            Record::Send { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the question is asked in words instead: {records:?}"));
    assert!(numbered.contains("1. Allow once"), "{numbered}");
    assert!(numbered.contains("2. Deny"), "{numbered}");

    // The rung it was drawn as is the rung it is answered on.
    chat.say(said(Conversation::direct("oc_1"), "2", true))
        .await;
    let answered = chat.until(|| session.answers().first().cloned()).await;
    assert_eq!(answered.1, Answer::Deny { feedback: None });
    assert_eq!(
        session.prompts().len(),
        1,
        "the reply answered the question rather than starting a turn: {:?}",
        session.prompts()
    );
}
