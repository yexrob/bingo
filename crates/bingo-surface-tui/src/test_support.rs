//! Fixtures every test in the crate draws on: a session state built frame by
//! frame, the interactions the dialog answers, and a `TestBackend` that turns
//! one `draw` into a string a snapshot can pin.

use std::time::Instant;

use bingo_sdk::{
    Answer, AnswerSpec, ContentPart, Display, Event, Frame, Interaction, InteractionId,
    InteractionKind, Item, ItemBody, ItemId, ItemStatus, Level, Origin, Preview, QuestionOption,
    Seq, SessionId, SessionState, SessionSummary, ToolOutput, TurnId, Usage,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use crate::clock::Now;
use crate::ui::Ui;
use crate::view;

pub fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a fixed instant")
}

pub fn summary() -> SessionSummary {
    SessionSummary {
        id: SessionId::from_raw("ses_1"),
        key: None,
        title: None,
        cwd: "/tmp/project".into(),
        parent: None,
        model: Some("fake-1".into()),
        provider: Some("fake".into()),
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
    }
}

pub fn state() -> SessionState {
    SessionState::new(summary())
}

pub fn frame(seq: u64, event: Event) -> Frame {
    Frame {
        seq: Seq(seq),
        ts: ts(),
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event,
    }
}

/// Fold frames into a fresh state, the way every client does.
pub fn folded(frames: Vec<Frame>) -> SessionState {
    let mut state = state();
    for frame in &frames {
        state.apply(frame);
    }
    state
}

pub fn item(id: &str, status: ItemStatus, body: ItemBody) -> Item {
    Item {
        id: ItemId::from_raw(id),
        turn: Some(TurnId::from_raw("trn_1")),
        round: 0,
        status,
        started_at: ts(),
        completed_at: status.is_terminal().then(ts),
        intent: None,
        body,
        meta: Default::default(),
    }
}

pub fn user(id: &str, text: &str) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin::surface("tui"),
        },
    )
}

pub fn assistant(id: &str, text: &str, status: ItemStatus) -> Item {
    item(id, status, ItemBody::Assistant { text: text.into() })
}

pub fn tool(
    id: &str,
    name: &str,
    input: Value,
    output: Option<ToolOutput>,
    status: ItemStatus,
) -> Item {
    item(
        id,
        status,
        ItemBody::ToolCall {
            call_id: "call_1".into(),
            name: name.into(),
            input,
            output,
            progress: None,
            child_session: None,
            duration_ms: Some(12),
        },
    )
}

pub fn running_tool(id: &str, name: &str, progress: &str) -> Item {
    item(
        id,
        ItemStatus::Running,
        ItemBody::ToolCall {
            call_id: "call_1".into(),
            name: name.into(),
            input: json!({ "command": "cargo test" }),
            output: None,
            progress: Some(progress.into()),
            child_session: None,
            duration_ms: None,
        },
    )
}

pub fn diff_output() -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::text("wrote src/lib.rs")],
        is_error: false,
        display: Some(Display::Diff {
            unified: "@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n ok\n".into(),
        }),
    }
}

pub fn started(turn: &str) -> Event {
    Event::TurnStarted {
        turn: TurnId::from_raw(turn),
        inputs: vec![],
        origin: bingo_sdk::TurnOrigin::Submit,
    }
}

pub fn completed(turn: &str, status: bingo_sdk::TurnStatus) -> Event {
    Event::TurnCompleted {
        turn: TurnId::from_raw(turn),
        status,
        usage: Usage::default(),
    }
}

pub fn interaction(kind: InteractionKind, answers: Vec<AnswerSpec>) -> Interaction {
    Interaction {
        id: InteractionId::from_raw("int_1"),
        session: SessionId::from_raw("ses_1"),
        turn: Some(TurnId::from_raw("trn_1")),
        item: Some(ItemId::from_raw("itm_2")),
        opened_at: ts(),
        guard_until: None,
        expires_at: None,
        kind,
        answers,
    }
}

pub fn permission(scope: Option<&str>, preview: Option<Preview>) -> Interaction {
    let mut answers = vec![AnswerSpec::AllowOnce, AnswerSpec::Deny];
    if scope.is_some() {
        answers.insert(1, AnswerSpec::AllowSession);
    }
    interaction(
        InteractionKind::Permission {
            tool: "Edit".into(),
            summary: "Edit src/lib.rs".into(),
            preview,
            session_scope: scope.map(str::to_owned),
        },
        answers,
    )
}

pub fn long_diff() -> Preview {
    Preview::Diff {
        unified: (0..20).map(|i| format!("+line {i}\n")).collect::<String>(),
    }
}

pub fn question(multi: bool, free_text: bool) -> Interaction {
    let mut answers = vec![AnswerSpec::Choice, AnswerSpec::Cancel];
    if free_text {
        answers.push(AnswerSpec::Text);
    }
    interaction(
        InteractionKind::Question {
            question: "Which provider?".into(),
            header: Some("Auth".into()),
            options: vec![
                QuestionOption {
                    id: "a".into(),
                    label: "Anthropic".into(),
                    description: Some("claude models".into()),
                },
                QuestionOption {
                    id: "o".into(),
                    label: "OpenAI".into(),
                    description: None,
                },
            ],
            free_text,
            multi,
        },
        answers,
    )
}

pub fn confirm() -> Interaction {
    interaction(
        InteractionKind::Confirm {
            title: "Delete the branch".into(),
            detail: "feature/x has unmerged commits".into(),
        },
        vec![AnswerSpec::Confirm, AnswerSpec::Cancel],
    )
}

pub fn opened(interaction: Interaction) -> Event {
    Event::InteractionOpened { interaction }
}

pub fn resolved() -> Event {
    Event::InteractionResolved {
        id: InteractionId::from_raw("int_1"),
        answer: Answer::AllowOnce,
        by: bingo_sdk::ResolvedBy::Kernel,
    }
}

pub fn notice(level: Level, text: &str) -> Event {
    Event::Notice {
        level,
        code: "TEST".into(),
        text: text.into(),
    }
}

/// A `Ui` and the instant it was born, so a test can move time by hand.
pub fn scene() -> (Ui, Now) {
    let instant = Instant::now();
    (
        Ui::new(Vec::new(), instant),
        Now {
            instant,
            wall: ts(),
        },
    )
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn typed(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub fn alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::ALT)
}

/// Type a whole line, one key at a time, through the real handler.
pub fn write(ui: &mut Ui, state: &SessionState, text: &str, now: Now) {
    for c in text.chars() {
        crate::input::on_key(ui, state, typed(c), now);
    }
}

/// One frame, rendered into a fixed-size buffer, as text.
pub fn render(state: &SessionState, ui: &Ui, now: Now) -> String {
    draw_sized(80, 24, state, ui, now)
}

pub fn draw_sized(width: u16, height: u16, state: &SessionState, ui: &Ui, now: Now) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| view::draw(state, ui, frame, now))
        .expect("a drawn frame");
    terminal.backend().to_string()
}

// ---- the doubles the loop test drives -----------------------------------

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, ArgSpec, Attachment, Catalog, CatalogEntry, CatalogKind, ClientIdentity,
    CloseReason, CommandSpec, FrameStream, GatewayStream, HistoryChunk, HistoryPage, HostApi,
    HostHandle, Input, IntentId, InterruptScope, KernelError, SessionFilter, SessionHandle,
    SessionPort, SessionSelector,
};

use crate::terminal::Screen;

/// A session that hands back a scripted frame list and remembers the writes.
#[derive(Debug, Default)]
pub struct TestSession {
    frames: Vec<bingo_sdk::Frame>,
    submitted: Mutex<Vec<Input>>,
    answers: Mutex<Vec<(bingo_sdk::InteractionId, bingo_sdk::Answer, Activation)>>,
    interrupts: Mutex<usize>,
    resyncs: Mutex<Vec<Seq>>,
}

impl TestSession {
    /// The live stream ends at a lag marker, as the kernel's does.
    fn live(&self) -> FrameStream {
        let mut frames = Vec::new();
        for frame in &self.frames {
            frames.push(frame.clone());
            if matches!(frame.event, Event::Lagged { .. }) {
                break;
            }
        }
        Box::pin(futures::stream::iter(frames))
    }

    pub fn submitted(&self) -> Vec<Input> {
        self.submitted.lock().expect("no poisoned lock").clone()
    }

    pub fn answers(&self) -> Vec<(bingo_sdk::InteractionId, bingo_sdk::Answer, Activation)> {
        self.answers.lock().expect("no poisoned lock").clone()
    }

    pub fn interrupts(&self) -> usize {
        *self.interrupts.lock().expect("no poisoned lock")
    }

    pub fn resyncs(&self) -> Vec<Seq> {
        self.resyncs.lock().expect("no poisoned lock").clone()
    }
}

#[async_trait]
impl SessionPort for TestSession {
    fn submit(&self, _intent: IntentId, input: Input) {
        self.submitted.lock().expect("no poisoned lock").push(input);
    }

    fn interrupt(&self, _intent: IntentId, _scope: InterruptScope) {
        *self.interrupts.lock().expect("no poisoned lock") += 1;
    }

    fn answer(
        &self,
        _intent: IntentId,
        interaction: bingo_sdk::InteractionId,
        answer: bingo_sdk::Answer,
        activation: Activation,
    ) {
        self.answers
            .lock()
            .expect("no poisoned lock")
            .push((interaction, answer, activation));
    }

    async fn history(&self, _page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        Ok(HistoryChunk {
            items: Vec::new(),
            next: None,
            generation: 0,
        })
    }

    /// The journal replay: durable frames only, like the kernel's.
    async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        self.resyncs.lock().expect("no poisoned lock").push(since);
        let frames: Vec<_> = self
            .frames
            .iter()
            .filter(|f| f.seq > since && f.event.is_durable())
            .cloned()
            .collect();
        Ok(Box::pin(futures::stream::iter(frames)))
    }
}

#[derive(Debug)]
pub struct TestHost {
    session: Arc<TestSession>,
    closed: Mutex<Vec<SessionId>>,
}

impl TestHost {
    pub fn with(frames: Vec<bingo_sdk::Frame>) -> (HostHandle, Arc<TestSession>) {
        let session = Arc::new(TestSession {
            frames,
            ..Default::default()
        });
        let host = TestHost {
            session: Arc::clone(&session),
            closed: Mutex::new(Vec::new()),
        };
        (HostHandle(Arc::new(host)), session)
    }
}

#[async_trait]
impl HostApi for TestHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(vec![summary()])
    }

    async fn open(
        &self,
        _selector: SessionSelector,
        _who: ClientIdentity,
    ) -> Result<Attachment, KernelError> {
        Ok(Attachment {
            session: SessionId::from_raw("ses_1"),
            snapshot: state(),
            events: self.session.live(),
            handle: SessionHandle(Arc::clone(&self.session) as Arc<dyn SessionPort>),
        })
    }

    async fn close(&self, session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        self.closed
            .lock()
            .expect("no poisoned lock")
            .push(session.clone());
        Ok(())
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        let entries = match kind {
            CatalogKind::Commands => vec![CatalogEntry {
                id: "model".into(),
                label: "model".into(),
                meta: serde_json::to_value(CommandSpec {
                    name: "model".into(),
                    aliases: vec![],
                    hint: "[provider/]model".into(),
                    args: ArgSpec::Catalog {
                        source: "models".into(),
                    },
                    instant: true,
                    family: "kernel".into(),
                })
                .expect("a serialisable spec"),
            }],
            _ => vec![CatalogEntry {
                id: "fake/fake-1".into(),
                label: "fake-1".into(),
                meta: Value::Null,
            }],
        };
        Ok(Catalog { kind, entries })
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A screen that keeps what it was asked to paint.
#[derive(Debug, Default)]
pub struct Recorder {
    pub frames: Vec<String>,
    pub titles: Vec<String>,
    pub bells: usize,
}

impl Recorder {
    pub fn last(&self) -> &str {
        self.frames.last().map(String::as_str).unwrap_or_default()
    }
}

impl Screen for Recorder {
    fn draw(&mut self, state: &SessionState, ui: &Ui, now: Now) -> std::io::Result<()> {
        self.frames.push(draw_sized(80, 24, state, ui, now));
        Ok(())
    }

    fn title(&mut self, text: &str) -> std::io::Result<()> {
        if self.titles.last().map(String::as_str) != Some(text) {
            self.titles.push(text.to_string());
        }
        Ok(())
    }

    fn bell(&mut self) -> std::io::Result<()> {
        self.bells += 1;
        Ok(())
    }
}

/// The options a surface is handed, pointed at a scratch directory.
pub fn options(prompt: Option<&str>, home: &std::path::Path) -> bingo_sdk::SurfaceOptions {
    bingo_sdk::SurfaceOptions {
        cwd: home.to_path_buf(),
        selector: SessionSelector::Create {
            spec: Default::default(),
        },
        prompt: prompt.map(str::to_owned),
        args: Value::Null,
        env: Arc::new(bingo_sdk::Env::rooted(home)),
    }
}

/// A key stream that yields its script and then keeps waiting, as a real
/// terminal does. Each press is held back a moment so the frames already on
/// the stream are folded first, which is the order a person would see.
pub fn keys(script: Vec<crossterm::event::KeyEvent>) -> crate::run::Keys {
    use futures::StreamExt;
    let typed = futures::stream::iter(script).then(|key| async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        crossterm::event::Event::Key(key)
    });
    Box::pin(typed.chain(futures::stream::pending()))
}
