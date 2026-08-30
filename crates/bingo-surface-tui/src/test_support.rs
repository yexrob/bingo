//! Fixtures every test in the crate draws on: a session state built frame by
//! frame, the interactions the dialog answers, and a `TestBackend` that turns
//! one `draw` into a string a snapshot can pin.

use std::time::Instant;

use bingo_sdk::{
    Answer, AnswerSpec, ContentPart, Delivery, Event, Frame, Interaction, InteractionId,
    InteractionKind, Item, ItemBody, ItemId, ItemStatus, Level, LoginFlow, Origin, ParentLink,
    Preview, QuestionOption, Seq, SessionId, SessionState, SessionSummary, ToolOutput, TurnId,
    Usage, View,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

use crate::clock::Now;
use crate::tree::Tree;
use crate::ui::Ui;
use crate::view;

pub fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a fixed instant")
}

pub fn summary() -> SessionSummary {
    SessionSummary {
        tools: None,
        system_extra: None,
        driver: Default::default(),
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

/// The sub-session the root's tool call spawned, as its own frames name it.
pub fn child_id() -> SessionId {
    SessionId::from_raw("ses_2")
}

pub fn child_summary(title: &str) -> SessionSummary {
    SessionSummary {
        id: child_id(),
        title: Some(title.into()),
        parent: Some(ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: Some(ItemId::from_raw("itm_1")),
        }),
        ..summary()
    }
}

/// The frame at the head of a child's stream: who it is and whose it is.
pub fn announced(title: &str) -> Event {
    Event::SessionUpdated {
        summary: child_summary(title),
    }
}

pub fn child_frame(seq: u64, event: Event) -> Frame {
    Frame {
        session: child_id(),
        ..frame(seq, event)
    }
}

/// A room under the same root: a session nothing answers, whose journal is
/// the point (ADR-0011 §1). Its id sorts before the sub-agent's, so a switcher
/// can show it between two model rows.
pub fn log_id() -> SessionId {
    SessionId::from_raw("ses_10")
}

pub fn log_summary(title: &str) -> SessionSummary {
    SessionSummary {
        id: log_id(),
        title: Some(title.into()),
        driver: bingo_sdk::Driver::Log,
        model: None,
        provider: None,
        parent: Some(ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: None,
        }),
        ..summary()
    }
}

/// The frame at the head of a room's stream.
pub fn log_announced(title: &str) -> Event {
    Event::SessionUpdated {
        summary: log_summary(title),
    }
}

pub fn log_frame(seq: u64, event: Event) -> Frame {
    Frame {
        session: log_id(),
        ..frame(seq, event)
    }
}

/// A permission the child raised; the root's handle answers it.
pub fn child_permission() -> Interaction {
    Interaction {
        id: InteractionId::from_raw("int_2"),
        session: child_id(),
        ..permission(Some("Edit(src/)"), None)
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

/// What a member posted into a room: a user item that names who wrote it and
/// where, as the room plugin's fan-out stamps it.
pub fn post(id: &str, principal: &str, text: &str) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin {
                surface: "room".into(),
                principal: Some(principal.into()),
                conversation: Some("#design".into()),
            },
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
            duration_ms: None,
        },
    )
}

pub fn diff_output() -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::text("wrote src/lib.rs")],
        is_error: false,
        display: Some(View::Diff {
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

/// A provider's sign-in, asked by a holding command rather than a turn
/// (ADR-0012 §5): a paste flow takes words, the others only a way out.
pub fn login(flow: LoginFlow) -> Interaction {
    let answers = match flow {
        LoginFlow::Paste => vec![AnswerSpec::Text, AnswerSpec::Cancel],
        _ => vec![AnswerSpec::Cancel],
    };
    Interaction {
        turn: None,
        item: None,
        ..interaction(
            InteractionKind::Login {
                provider: "codex".into(),
                flow,
            },
            answers,
        )
    }
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

/// A plugin publishing the whole of one kind of its state (ADR-0011 §2).
pub fn extended(plugin: &str, kind: &str, payload: Value) -> Event {
    Event::Extension {
        plugin: plugin.into(),
        kind: kind.into(),
        payload,
    }
}

/// The kernel's projection of one plugin's per-session setting (ADR-0009 §5).
pub fn plugin_view(plugin: &str, value: Value) -> Event {
    Event::ConfigChanged {
        config: bingo_sdk::ConfigView {
            plugins: std::collections::BTreeMap::from([(plugin.to_string(), value)]),
            ..Default::default()
        },
    }
}

/// What the permission policy publishes for a session: the mode, the list
/// it may be cycled through, the rules it accepted.
pub fn permission_view(mode: &str) -> Event {
    plugin_view(
        "bingo.permissions",
        json!({
            "mode": mode,
            "modes": ["default", "acceptEdits", "plan", "bypassPermissions", "dontAsk"],
            "rules": [],
        }),
    )
}

/// A session whose policy has published this mode and nothing else.
pub fn with_permission_mode(mode: &str) -> SessionState {
    folded(vec![frame(1, permission_view(mode))])
}

pub fn notice(level: Level, text: &str) -> Event {
    Event::Notice {
        level,
        code: "TEST".into(),
        text: text.into(),
    }
}

/// A transcript with more lines than any test screen has rows.
pub fn long_transcript(items: usize) -> SessionState {
    let mut state = state();
    state.items = (0..items)
        .map(|i| user(&format!("itm_{i}"), &format!("line {i}")))
        .collect();
    state
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

/// A scene whose wall clock is far enough past [`ts`] for a card the kernel
/// opened at `ts` to have finished arriving: the settled screen.
pub fn settled() -> (Ui, Now) {
    let (ui, now) = scene();
    (
        ui,
        Now {
            wall: now.wall + jiff::SignedDuration::from_millis(200),
            ..now
        },
    )
}

/// Open a layer that has finished arriving: what a settled sheet or switcher
/// looks like, rather than the first frame of its slide.
pub fn shown(ui: &mut Ui, open: crate::ui::Open, now: Now) {
    ui.layer
        .show(open, now.instant - std::time::Duration::from_millis(500));
}

/// A synthetic mouse event at a cell of the screen.
pub fn mouse(kind: crossterm::event::MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub fn click(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn dragged(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn wheel(up: bool, column: u16, row: u16) -> MouseEvent {
    let kind = match up {
        true => crossterm::event::MouseEventKind::ScrollUp,
        false => crossterm::event::MouseEventKind::ScrollDown,
    };
    mouse(kind, column, row)
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

/// What a terminal sends for shift+tab: `BackTab`, with the modifier set.
pub fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

/// A tree of one session, which is what most of these tests are about.
pub fn solo(state: &SessionState) -> Tree {
    Tree::new(state.clone())
}

/// Fold frames into a fresh tree, routed by `frame.session` the way the loop
/// does; a child joins on the `SessionUpdated` at the head of its stream.
pub fn folded_tree(frames: Vec<Frame>) -> Tree {
    let mut tree = Tree::new(state());
    for frame in &frames {
        tree.apply(frame);
    }
    tree
}

/// Type a whole line, one key at a time, through the real handler.
pub fn write(ui: &mut Ui, state: &SessionState, text: &str, now: Now) {
    let tree = solo(state);
    for c in text.chars() {
        crate::input::on_key(ui, &tree, typed(c), now);
    }
}

/// One frame, rendered into a fixed-size buffer, as text.
pub fn render(state: &SessionState, ui: &Ui, now: Now) -> String {
    draw_sized(80, 24, state, ui, now)
}

pub fn draw_sized(width: u16, height: u16, state: &SessionState, ui: &Ui, now: Now) -> String {
    draw_tree(width, height, &solo(state), ui, now)
}

pub fn render_tree(tree: &Tree, ui: &Ui, now: Now) -> String {
    draw_tree(80, 24, tree, ui, now)
}

pub fn draw_tree(width: u16, height: u16, tree: &Tree, ui: &Ui, now: Now) -> String {
    drawn(width, height, tree, ui, now).to_string()
}

/// The terminal one draw leaves, for a test that asks where a style landed.
pub fn drawn(width: u16, height: u16, tree: &Tree, ui: &Ui, now: Now) -> TestBackend {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| view::draw(tree, ui, frame, now))
        .expect("a drawn frame");
    terminal.backend().clone()
}

/// The same instant, `ms` further along the wall clock: how far a card the
/// kernel opened has come.
pub fn later(now: Now, ms: i64) -> Now {
    Now {
        wall: now.wall + jiff::SignedDuration::from_millis(ms),
        ..now
    }
}

// ---- the doubles the loop test drives -----------------------------------

use std::any::Any;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, ArgSpec, Attachment, Catalog, CatalogEntry, CatalogKind, ClientIdentity,
    CloseReason, CommandSpec, FrameStream, GatewayStream, HistoryChunk, HistoryPage, HostApi,
    HostHandle, Input, IntentId, InterruptScope, KernelError, OpenOptions, SessionFilter,
    SessionHandle, SessionPort, SessionSelector,
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
    /// The mailbox `open(ById)` hands out for the child in the tree.
    child: Arc<TestSession>,
    closed: Mutex<Vec<SessionId>>,
}

impl TestHost {
    pub fn with(frames: Vec<bingo_sdk::Frame>) -> (HostHandle, Arc<TestSession>) {
        let (host, session, _) = Self::tree(frames);
        (host, session)
    }

    /// The root's mailbox and the child's, which `open(ById)` answers with.
    pub fn tree(frames: Vec<bingo_sdk::Frame>) -> (HostHandle, Arc<TestSession>, Arc<TestSession>) {
        let session = Arc::new(TestSession {
            frames,
            ..Default::default()
        });
        let child = Arc::new(TestSession::default());
        let host = TestHost {
            session: Arc::clone(&session),
            child: Arc::clone(&child),
            closed: Mutex::new(Vec::new()),
        };
        (HostHandle(Arc::new(host)), session, child)
    }
}

#[async_trait]
impl HostApi for TestHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(vec![summary()])
    }

    /// The tree's stream comes with the root; a child is opened for its
    /// mailbox alone, so its stream is empty.
    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        if matches!(&selector, SessionSelector::ById { id } if id == &child_id()) {
            return Ok(Attachment {
                session: child_id(),
                snapshot: SessionState::new(child_summary("reviewer")),
                events: Box::pin(futures::stream::empty()),
                handle: SessionHandle(Arc::clone(&self.child) as Arc<dyn SessionPort>),
            });
        }
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
    /// The bytes handed to the terminal's clipboard, verbatim.
    pub copies: Vec<Vec<u8>>,
}

impl Recorder {
    pub fn last(&self) -> &str {
        self.frames.last().map(String::as_str).unwrap_or_default()
    }
}

impl Screen for Recorder {
    fn draw(&mut self, tree: &Tree, ui: &Ui, now: Now) -> std::io::Result<()> {
        self.frames.push(render_tree(tree, ui, now));
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

    fn copy(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.copies.push(bytes.to_vec());
        Ok(())
    }

    fn rows(&self) -> u16 {
        24
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
    pressed(script, std::time::Duration::from_millis(5))
}

/// The same, with a wait before every press: under `tokio::time::pause` it
/// is time the loop must sit through with nothing to do.
pub fn keys_after(
    wait: std::time::Duration,
    script: Vec<crossterm::event::KeyEvent>,
) -> crate::run::Keys {
    pressed(script, wait)
}

fn pressed(script: Vec<crossterm::event::KeyEvent>, wait: std::time::Duration) -> crate::run::Keys {
    use futures::StreamExt;
    let keys = futures::stream::iter(script).then(move |key| async move {
        tokio::time::sleep(wait).await;
        crossterm::event::Event::Key(key)
    });
    Box::pin(keys.chain(futures::stream::pending()))
}
