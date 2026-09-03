use std::any::Any;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, Attachment, Catalog, CatalogEntry, CatalogKind, ClientIdentity,
    CloseReason, Delivery, Env, ErrorCode, Event, Frame, FrameStream, GatewayStream, HistoryChunk,
    HistoryPage, HostApi, HostHandle, Input, IntentId, InteractionId, InterruptScope, ItemStatus,
    KernelError, OpenOptions, ResolvedBy, Seq, SessionFilter, SessionHandle, SessionId,
    SessionPort, SessionSelector, SessionSpec, SessionState, SessionSummary, SurfaceOptions,
    TurnStatus, Usage,
};
use tokio::sync::mpsc;

use super::*;
use crate::adapter::{Incoming, Mode};
use crate::conversation::Posted;
use crate::fixtures;
use crate::lock::Claim;
use crate::loopback::{self, Loopback, Record};

fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

// ---- the kernel double ---------------------------------------------------

/// One session: every attachment gets its own stream, so a chat and a TUI can
/// both be looking at it, which is what the two-surface race needs.
#[derive(Debug, Default)]
pub struct TestSession {
    key: String,
    seq: AtomicU64,
    watchers: Mutex<Vec<mpsc::UnboundedSender<Frame>>>,
    submitted: Mutex<Vec<Input>>,
    answers: Mutex<Vec<(InteractionId, Answer, Activation)>>,
}

impl TestSession {
    fn attach(&self) -> FrameStream {
        let (publisher, frames) = mpsc::unbounded_channel();
        locked(&self.watchers).push(publisher);
        Box::pin(futures::stream::unfold(frames, |mut frames| async move {
            frames.recv().await.map(|frame| (frame, frames))
        }))
    }

    /// Publish a frame as the kernel would, numbering it as it goes.
    pub fn publish(&self, event: Event) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = fixtures::frame(seq, event);
        locked(&self.watchers).retain(|watcher| watcher.send(frame.clone()).is_ok());
    }

    pub fn prompts(&self) -> Vec<String> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { text, .. } => Some(text.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    pub fn origins(&self) -> Vec<bingo_sdk::Origin> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { origin, .. } => Some(origin.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    /// The pictures beside each prompt, in the order they were submitted.
    pub fn pictures(&self) -> Vec<Vec<bingo_sdk::Image>> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { images, .. } => Some(images.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    pub fn answers(&self) -> Vec<(InteractionId, Answer, Activation)> {
        locked(&self.answers).clone()
    }
}

#[async_trait]
impl SessionPort for TestSession {
    fn submit(&self, _intent: IntentId, input: Input) {
        locked(&self.submitted).push(input);
    }

    fn interrupt(&self, _intent: IntentId, _scope: InterruptScope) {}

    fn answer(
        &self,
        _intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        locked(&self.answers).push((interaction, answer, activation));
    }

    async fn history(&self, _page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        Ok(HistoryChunk {
            items: Vec::new(),
            next: None,
            generation: 0,
        })
    }

    async fn events_since(&self, _since: Seq) -> Result<FrameStream, KernelError> {
        Ok(self.attach())
    }
}

#[derive(Debug, Default)]
pub struct TestHost {
    sessions: Mutex<Vec<Arc<TestSession>>>,
    /// Every selector `open` was called with, in order.
    opened: Mutex<Vec<SessionSelector>>,
}

impl TestHost {
    fn session(&self, key: &str) -> Option<Arc<TestSession>> {
        locked(&self.sessions)
            .iter()
            .find(|session| session.key == key)
            .cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        locked(&self.sessions)
            .iter()
            .map(|session| session.key.clone())
            .collect()
    }

    pub fn opened(&self) -> Vec<SessionSelector> {
        locked(&self.opened).clone()
    }

    fn attachment(&self, session: Arc<TestSession>) -> Attachment {
        let mut summary = fixtures::summary();
        summary.key = Some(session.key.clone());
        Attachment {
            session: SessionId::from_raw(fixtures::SESSION),
            snapshot: SessionState::new(summary),
            events: session.attach(),
            handle: SessionHandle(session as Arc<dyn SessionPort>),
        }
    }
}

#[async_trait]
impl HostApi for TestHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        assert!(options.children, "a chat attaches to the whole tree");
        locked(&self.opened).push(selector.clone());
        match selector {
            SessionSelector::ByKey { key } => self
                .session(&key)
                .map(|session| self.attachment(session))
                .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session")),
            SessionSelector::Create {
                spec: SessionSpec { key: Some(key), .. },
            } => {
                let session = Arc::new(TestSession {
                    key,
                    ..TestSession::default()
                });
                locked(&self.sessions).push(Arc::clone(&session));
                Ok(self.attachment(session))
            }
            other => panic!("a chat never opens by {other:?}"),
        }
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        Ok(())
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("this double delivers nothing")
    }

    async fn extend(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double extends nothing")
    }

    async fn signal(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double signals nothing")
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: Vec::<CatalogEntry>::new(),
        })
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A host nothing is ever asked of, for the tests that never get that far.
pub fn nowhere() -> HostHandle {
    HostHandle(Arc::new(TestHost::default()))
}

pub fn options(cwd: &str) -> SurfaceOptions {
    SurfaceOptions {
        cwd: cwd.into(),
        // The channel surface mints its own keys; the selector is a
        // placeholder it never reads.
        selector: SessionSelector::Latest { cwd: cwd.into() },
        prompt: None,
        args: serde_json::Value::Null,
        env: Arc::new(Env::rooted(cwd)),
    }
}

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
        let home = tempfile::tempdir().expect("a temporary home");
        let loopback = Arc::new(Loopback::new(config));
        let surface = ChannelsSurface::new(
            vec![Arc::clone(&loopback) as Arc<dyn ChannelAdapter>],
            // A test should not have to wait for a coalescer.
            Gate {
                min_chars: 1_000,
                interval: Duration::from_millis(10),
            },
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
