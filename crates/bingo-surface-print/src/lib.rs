//! The headless `--print` surface.
//!
//! It is a client like any other: it opens a session, submits what it is asked
//! to, folds the frames with `SessionState::apply` and renders them. It holds
//! no session state of its own (ADR-0002) and decides exactly two things — what
//! to answer an interaction with when nobody is at the keyboard, and what to
//! exit with.
//!
//! It runs in one of two shapes. The plain one is a prompt, a turn and an exit
//! code. Under `--input-format stream-json` it is instead a host protocol
//! (`input`): prompts and control requests arrive on stdin as lines, each
//! prompt is a turn, and the run ends when stdin has closed and every prompt
//! has been answered.

mod hosted;
mod input;
mod render;
mod stream_json;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, AnswerSpec, Applied, Attachment, CatalogKind, ClientIdentity, CloseReason,
    ErrorCode, Event, Exit, Frame, FrameStream, HostHandle, Image, Input, IntentId, IntentOutcome,
    Interaction, InteractionKind, KernelError, OpenOptions, Origin, Plugin, PluginError,
    PluginManifest, Question, Registrar, SessionHandle, SessionId, SessionState, Surface,
    SurfaceKind, SurfaceOptions, TurnStatus,
};
use futures::StreamExt;
use tokio::sync::mpsc;

use input::Format;
use render::{Mode, Renderer};
pub use render::{error_report, notice_report};

/// The surface id, and the origin every input it submits carries.
pub const SURFACE_ID: &str = "print";

/// Lines of stdin held for the run while it is busy with a turn. A host that
/// writes faster than the kernel answers waits on the pipe, as it would for any
/// program that reads its input line by line.
const LINE_BUFFER: usize = 32;

/// Everything the surface needs from the terminal, so a test can be the terminal.
pub(crate) trait Console: Send {
    /// Whether a person is at the other end of stdin.
    fn interactive(&self) -> bool;

    /// Whether a person reads stderr (a terminal), so diagnostics are prose.
    fn human(&self) -> bool;

    /// The whole of stdin, read when no prompt came from the command line.
    fn read_all(&mut self) -> io::Result<String>;

    /// One answer line from a person.
    fn read_line(&mut self) -> io::Result<String>;

    /// Stdin as it arrives, one line at a time, for a run whose prompts come
    /// from a host. The stream ends when stdin closes.
    fn lines(&mut self) -> mpsc::Receiver<String>;
}

/// The real terminal.
#[derive(Debug, Default, Clone, Copy)]
struct Terminal;

impl Console for Terminal {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn human(&self) -> bool {
        io::stderr().is_terminal()
    }

    fn read_all(&mut self) -> io::Result<String> {
        io::read_to_string(io::stdin())
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        Ok(line)
    }

    fn lines(&mut self) -> mpsc::Receiver<String> {
        let (lines, stdin) = mpsc::channel(LINE_BUFFER);
        // Reading stdin blocks; the frames must not wait behind it.
        tokio::task::spawn_blocking(move || {
            for line in io::stdin().lines() {
                // A line that cannot be read ends the conversation, as the end
                // of the file does.
                let Ok(line) = line else { break };
                if lines.blocking_send(line).is_err() {
                    break;
                }
            }
        });
        stdin
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PrintSurface;

#[async_trait]
impl Surface for PrintSurface {
    fn id(&self) -> &str {
        SURFACE_ID
    }

    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Exclusive
    }

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError> {
        let mut console = Terminal;
        let mut out = io::stdout();
        let mut err = io::stderr();
        drive(&host, opts, &mut console, &mut out, &mut err).await
    }
}

/// One run, one exit code. Every stream this touches is injected, so the whole
/// surface is exercised without a terminal.
pub(crate) async fn drive(
    host: &HostHandle,
    opts: SurfaceOptions,
    console: &mut (dyn Console + Send),
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> Result<Exit, KernelError> {
    let SurfaceOptions {
        selector,
        cwd,
        prompt,
        args,
        ..
    } = opts;
    let mode = Mode::from_args(&args);
    let renderer = Renderer::new(mode, console.human(), tool_names(host).await);
    let start = start(Format::from_args(&args), prompt, &args, &cwd, console).await?;
    let attachment = host
        .open(
            selector,
            ClientIdentity {
                name: SURFACE_ID.into(),
                surface: SURFACE_ID.into(),
            },
            // The whole tree, whatever the mode: a sub-session's permission
            // prompt reaches a run only through it (ADR-0010 §3), and a run
            // that cannot see the prompt waits on it forever. What of the
            // tree is reported is the renderer's decision.
            OpenOptions::with_children(),
        )
        .await?;
    let run = Attached::open(attachment, renderer, console, out, err)?;
    match start {
        Start::Once { prompt, images } => {
            run.submit(IntentId::mint(), prompt, images);
            run.single().await
        }
        Start::Hosted { first, lines } => {
            run.hosted(lines, first, input::prompts_on_stdio(&args))
                .await
        }
    }
}

/// Where a run's inputs come from, decided before the session is opened
/// because a text run reads the whole of stdin to find its prompt.
enum Start {
    /// One prompt, one turn; the pictures are `--image`'s.
    Once { prompt: String, images: Vec<Image> },
    /// The host protocol: the prompt argument, when there was one, and then
    /// stdin's lines for as long as it stays open.
    Hosted {
        first: Option<String>,
        lines: mpsc::Receiver<String>,
    },
}

async fn start(
    format: Format,
    prompt: Option<String>,
    args: &serde_json::Value,
    cwd: &std::path::Path,
    console: &mut (dyn Console + Send),
) -> Result<Start, KernelError> {
    match format {
        Format::Text => Ok(Start::Once {
            prompt: prompt_from(prompt, console)?,
            images: images_from(args, cwd).await?,
        }),
        Format::StreamJson => Ok(Start::Hosted {
            first: prompt,
            lines: console.lines(),
        }),
    }
}

/// The attached session a run folds — with its sub-sessions when the mode
/// reports them — and the streams it writes to. Both loops share it, and
/// neither keeps anything the states already hold.
struct Attached<'a> {
    root: SessionId,
    /// One reducer per session in the tree, the root's from the snapshot.
    states: BTreeMap<SessionId, SessionState>,
    events: FrameStream,
    handle: SessionHandle,
    renderer: Renderer,
    console: &'a mut (dyn Console + Send),
    out: &'a mut (dyn Write + Send),
    err: &'a mut (dyn Write + Send),
}

impl<'a> Attached<'a> {
    /// The attachment, with the preamble its mode owes already written.
    fn open(
        attachment: Attachment,
        renderer: Renderer,
        console: &'a mut (dyn Console + Send),
        out: &'a mut (dyn Write + Send),
        err: &'a mut (dyn Write + Send),
    ) -> Result<Self, KernelError> {
        let Attachment {
            session,
            snapshot,
            events,
            handle,
        } = attachment;
        renderer.open(&snapshot, &mut *out).map_err(stdio_error)?;
        Ok(Self {
            root: session.clone(),
            states: BTreeMap::from([(session, snapshot)]),
            events,
            handle,
            renderer,
            console,
            out,
            err,
        })
    }

    fn human(&self) -> bool {
        self.console.human()
    }

    fn submit(&self, intent: IntentId, text: String, images: Vec<Image>) {
        self.handle.submit(
            intent,
            Input::Text {
                text,
                images,
                origin: Origin::surface(SURFACE_ID),
            },
        );
    }

    /// Fold and render one frame; `false` when it was stale or from a session
    /// this run has no head for, and nothing else should look at it.
    fn show(&mut self, frame: &Frame) -> Result<bool, KernelError> {
        let Some(state) = self.state_of(frame) else {
            return Ok(false);
        };
        if state.apply(frame) == Applied::Stale {
            return Ok(false);
        }
        let (state, root) = (&self.states[&frame.session], &self.states[&self.root]);
        self.renderer
            .render(frame, state, root, &mut *self.out, &mut *self.err)
            .map_err(stdio_error)?;
        Ok(true)
    }

    /// The frame's session, folded from its head when it is new.
    fn state_of(&mut self, frame: &Frame) -> Option<&mut SessionState> {
        if !self.states.contains_key(&frame.session) {
            let Event::SessionUpdated { summary } = &frame.event else {
                return None;
            };
            self.states
                .insert(frame.session.clone(), SessionState::new(summary.clone()));
        }
        self.states.get_mut(&frame.session)
    }

    fn root(&self) -> &SessionState {
        &self.states[&self.root]
    }

    /// Re-read the journal from the last frame applied, filling the gap a lag
    /// marker announced.
    async fn resync(&mut self) -> Result<(), KernelError> {
        self.events = self.handle.events_since(self.root().seq).await?;
        Ok(())
    }

    /// A sub-session's frame concerns the run only when it needs a person:
    /// its turns, acks and closing are the root's business to report.
    fn concerns_the_run(&self, frame: &Frame) -> bool {
        frame.session == self.root || asks_a_person(&frame.event)
    }

    /// One prompt, one turn, one exit code.
    async fn single(mut self) -> Result<Exit, KernelError> {
        while let Some(frame) = self.events.next().await {
            if !self.show(&frame)? || !self.concerns_the_run(&frame) {
                continue;
            }
            match react(
                &frame.event,
                &self.handle,
                &mut *self.console,
                &mut *self.err,
            )? {
                Next::Await => {}
                Next::Resync => self.resync().await?,
                Next::Exit(exit) => return Ok(exit),
            }
        }
        let human = self.human();
        closed(
            "the event stream ended before the turn completed",
            &mut *self.err,
            human,
        )
    }
}

/// The tools the stream-json preamble advertises; the host is the only place
/// that knows them.
/// The tool names the init line lists; a catalogue that cannot be read
/// lists none rather than stopping the run.
async fn tool_names(host: &HostHandle) -> Vec<String> {
    host.catalog(CatalogKind::Tools)
        .await
        .map(|c| c.entries.into_iter().map(|entry| entry.id).collect())
        .unwrap_or_default()
}

/// The prompt from the command line, or the whole of stdin when there is none.
fn prompt_from(
    argument: Option<String>,
    console: &mut (dyn Console + Send),
) -> Result<String, KernelError> {
    let prompt = match argument {
        Some(prompt) => prompt,
        None => console.read_all().map_err(stdio_error)?,
    };
    if prompt.trim().is_empty() {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            "no prompt: pass one as an argument or on stdin",
        ));
    }
    Ok(prompt)
}

/// `--image`'s sources (`args.images`), read at the edge: a path off this
/// machine or a URL this machine fetches (ADR-0041 §3), and one that does not
/// read is the run's answer, before a session is opened for it. A relative
/// path is the session's, so `--cwd` moves it with everything else.
async fn images_from(
    args: &serde_json::Value,
    cwd: &std::path::Path,
) -> Result<Vec<Image>, KernelError> {
    let words = args
        .get("images")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut images = Vec::with_capacity(words.len());
    for word in words.iter().filter_map(serde_json::Value::as_str) {
        // Nothing is kept: a run that prints once and ends has nothing to
        // read a second time (M61).
        let image = bingo_pictures::load(&bingo_pictures::Source::parse(word, cwd), None)
            .await
            .map_err(|e| KernelError::new(ErrorCode::InvalidInput, format!("{word}: {e}")))?;
        images.push(image);
    }
    Ok(images)
}

fn asks_a_person(event: &Event) -> bool {
    matches!(
        event,
        Event::InteractionOpened { .. }
            | Event::InteractionResolved { .. }
            | Event::InteractionCancelled { .. }
    )
}

/// What a rendered frame leaves the run to do next.
enum Next {
    /// Keep reading the current stream.
    Await,
    /// Re-read the journal from the last applied frame.
    Resync,
    Exit(Exit),
}

/// Everything a frame asks of the surface once it has been rendered.
fn react(
    event: &Event,
    handle: &SessionHandle,
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> Result<Next, KernelError> {
    match event {
        Event::InteractionOpened { interaction } => {
            let (answer, activation) = decide(interaction, console, err).map_err(stdio_error)?;
            handle.answer(IntentId::mint(), interaction.id.clone(), answer, activation);
            Ok(Next::Await)
        }
        // The lagged stream ends at its marker; the reducer left `seq` at the
        // last frame it applied, so replay from there fills the gap.
        Event::Lagged { .. } => Ok(Next::Resync),
        Event::TurnCompleted { status, .. } => Ok(Next::Exit(exit_for(status))),
        Event::SessionClosed { reason } => {
            closed(&close_message(reason), err, console.human()).map(Next::Exit)
        }
        // The one submit this surface makes was refused: there will be no
        // turn to wait for.
        Event::IntentAck {
            outcome: IntentOutcome::Rejected { error },
            ..
        } => rejected(error, err, console.human()).map(Next::Exit),
        // The one submit was an instant command and this is its receipt (the
        // renderer already printed it): there is no turn to wait for. An
        // answer's own ack carries no receipt keys and holds the run.
        Event::IntentAck {
            outcome: IntentOutcome::Applied { result },
            ..
        } if ["message", "view", "item"]
            .iter()
            .any(|k| result.get(k).is_some()) =>
        {
            Ok(Next::Exit(Exit { code: 0 }))
        }
        _ => Ok(Next::Await),
    }
}

fn rejected(
    error: &KernelError,
    err: &mut (dyn Write + Send),
    human: bool,
) -> Result<Exit, KernelError> {
    writeln!(err, "{}", error_report(error.code, &error.message, human)).map_err(stdio_error)?;
    Ok(Exit { code: 1 })
}

fn closed(message: &str, err: &mut (dyn Write + Send), human: bool) -> Result<Exit, KernelError> {
    writeln!(
        err,
        "{}",
        error_report(ErrorCode::SessionClosed, message, human)
    )
    .map_err(stdio_error)?;
    Ok(Exit { code: 1 })
}

fn exit_for(status: &TurnStatus) -> Exit {
    match status {
        TurnStatus::Completed => Exit { code: 0 },
        TurnStatus::Failed { .. } => Exit { code: 1 },
        TurnStatus::Interrupted { .. } => Exit { code: 130 },
    }
}

fn close_message(reason: &CloseReason) -> String {
    match reason {
        CloseReason::Client => "the session was closed".into(),
        CloseReason::Shutdown => "the host is shutting down".into(),
        CloseReason::Deleted => "the session was deleted".into(),
        CloseReason::Error { message } => message.clone(),
    }
}

fn stdio_error(e: io::Error) -> KernelError {
    KernelError::new(ErrorCode::Internal, format!("stdio: {e}"))
}

/// What to answer an interaction with. Nobody at the keyboard is a refusal,
/// never an approval.
fn decide(
    interaction: &Interaction,
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<(Answer, Activation)> {
    if !console.interactive() {
        return Ok((
            refuse(interaction, "non-interactive"),
            Activation::Programmatic,
        ));
    }
    let answer = match &interaction.kind {
        InteractionKind::Permission {
            tool,
            summary,
            session_scope,
            ..
        } => ask_permission(tool, summary, session_scope.as_deref(), console, err)?,
        InteractionKind::Question(question) => ask_question(interaction, question, console, err)?,
        InteractionKind::Form { questions, .. } => ask_form(questions, console, err)?,
        _ => refuse(interaction, "this surface cannot answer that"),
    };
    Ok((answer, Activation::Keyboard))
}

fn ask_permission(
    tool: &str,
    summary: &str,
    session_scope: Option<&str>,
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<Answer> {
    writeln!(
        err,
        "[permission] {tool}: {summary}  [y]es / [a]lways this session / [n]o"
    )?;
    err.flush()?;
    Ok(match console.read_line()?.trim().chars().next() {
        Some('y' | 'Y') => Answer::AllowOnce,
        // Without a scope there is no session rule to install, so the
        // widest honest answer is this one call.
        Some('a' | 'A') => match session_scope {
            Some(scope) => Answer::AllowSession {
                scope: scope.to_string(),
            },
            None => Answer::AllowOnce,
        },
        _ => Answer::Deny { feedback: None },
    })
}

fn ask_question(
    interaction: &Interaction,
    question: &Question,
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<Answer> {
    let chosen = put(question, console, err)?;
    Ok(if question.options.iter().any(|o| o.id == chosen) {
        Answer::Choice {
            ids: vec![chosen],
            other: None,
        }
    } else {
        refuse(interaction, "no such option")
    })
}

/// A form is asked one question at a time, in the order they were asked, and
/// answered once: the same shape a person at a pipe already knows, N times.
fn ask_form(
    questions: &[Question],
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<Answer> {
    let mut answers = Vec::new();
    for question in questions {
        let line = put(question, console, err)?;
        answers.push(slot(question, line));
    }
    Ok(Answer::Form { answers })
}

/// One question written out, and the line that came back.
fn put(
    question: &Question,
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<String> {
    match &question.header {
        Some(header) => writeln!(err, "[question] {header}: {}", question.question)?,
        None => writeln!(err, "[question] {}", question.question)?,
    }
    for option in &question.options {
        writeln!(err, "  {} — {}", option.id, option.label)?;
    }
    err.flush()?;
    Ok(console.read_line()?.trim().to_string())
}

/// What one line means to one question of a form: the option it names, else
/// words of their own where the question takes them, else a question skipped.
fn slot(question: &Question, line: String) -> Answer {
    if question.options.iter().any(|o| o.id == line) {
        return Answer::Choice {
            ids: vec![line],
            other: None,
        };
    }
    match question.free_text && !line.is_empty() {
        true => Answer::Text { text: line },
        false => Answer::Cancel,
    }
}

/// The narrowest refusal the interaction will accept.
fn refuse(interaction: &Interaction, why: &str) -> Answer {
    if interaction.answers.contains(&AnswerSpec::Deny) {
        Answer::Deny {
            feedback: Some(why.into()),
        }
    } else {
        Answer::Cancel
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.surface.print",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["surface:print"],
    requires: &[],
    config: None,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct PrintPlugin;

#[async_trait]
impl Plugin for PrintPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.surface(Arc::new(PrintSurface) as Arc<dyn Surface>);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::any::Any;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use bingo_sdk::{
        Catalog, CatalogEntry, ContentPart, Delivery, DeltaKind, Frame, FrameStream, GatewayStream,
        HistoryChunk, HistoryPage, HostApi, InteractionId, InterruptReason, InterruptScope, Item,
        ItemBody, ItemId, ItemStatus, QuestionOption, Seq, SessionFilter, SessionHandle, SessionId,
        SessionPort, SessionSelector, SessionState, SessionSummary, ToolOutput, TurnId, TurnOrigin,
        Usage,
    };
    use jiff::Timestamp;
    use serde_json::{Value, json};

    // ---- fixtures -------------------------------------------------------

    fn ts() -> Timestamp {
        Timestamp::from_second(1_700_000_000).expect("a fixed instant")
    }

    fn summary() -> SessionSummary {
        SessionSummary {
            tools: None,
            system_extra: None,
            driver: Default::default(),
            id: SessionId::from_raw("ses_1"),
            key: None,
            title: None,
            cwd: "/tmp".into(),
            parent: None,
            model: Some("fake-1".into()),
            provider: Some("fake".into()),
            created_at: ts(),
            updated_at: ts(),
            usage: Usage::default(),
            busy: false,
            messages: None,
        }
    }

    pub(crate) fn session_state() -> SessionState {
        SessionState::new(summary())
    }

    pub(crate) fn frame(seq: u64, event: Event) -> Frame {
        Frame {
            seq: Seq(seq),
            ts: ts(),
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event,
        }
    }

    fn item(id: &str, status: ItemStatus, body: ItemBody) -> Item {
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

    pub(crate) fn assistant(id: &str, text: &str, status: ItemStatus) -> Item {
        item(id, status, ItemBody::Assistant { text: text.into() })
    }

    pub(crate) fn tool_call(
        id: &str,
        name: &str,
        output: Option<ToolOutput>,
        status: ItemStatus,
    ) -> Item {
        item(
            id,
            status,
            ItemBody::ToolCall {
                call_id: "call_1".into(),
                name: name.into(),
                input: json!({ "file_path": "Cargo.toml" }),
                output,
                progress: None,
                duration_ms: Some(12),
            },
        )
    }

    /// A shell line the person ran themselves (M65): outside every turn, as
    /// the kernel journals one.
    pub(crate) fn shell(id: &str, command: &str, output: &str, exit: Option<i32>) -> Item {
        Item {
            turn: None,
            ..item(
                id,
                ItemStatus::Completed,
                ItemBody::Shell {
                    command: command.into(),
                    output: output.into(),
                    exit,
                    cwd: "/tmp/p".into(),
                },
            )
        }
    }

    pub(crate) fn permission(session_scope: Option<&str>) -> Interaction {
        Interaction {
            id: InteractionId::from_raw("int_1"),
            session: SessionId::from_raw("ses_1"),
            turn: Some(TurnId::from_raw("trn_1")),
            item: Some(ItemId::from_raw("itm_2")),
            opened_at: ts(),
            guard_until: None,
            expires_at: None,
            kind: InteractionKind::Permission {
                tool: "Read".into(),
                summary: "Read Cargo.toml".into(),
                preview: None,
                session_scope: session_scope.map(str::to_owned),
            },
            answers: vec![
                AnswerSpec::AllowOnce,
                AnswerSpec::AllowSession,
                AnswerSpec::Deny,
            ],
        }
    }

    pub(crate) fn question(options: &[(&str, &str)]) -> Interaction {
        Interaction {
            kind: InteractionKind::Question(Question {
                question: "Which file?".into(),
                header: None,
                options: options
                    .iter()
                    .map(|(id, label)| QuestionOption {
                        id: (*id).into(),
                        label: (*label).into(),
                        description: None,
                        role: None,
                        preview: None,
                    })
                    .collect(),
                free_text: false,
                multi: false,
            }),
            answers: vec![AnswerSpec::Choice, AnswerSpec::Cancel],
            ..permission(None)
        }
    }

    fn completed(seq: u64) -> Frame {
        frame(
            seq,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Completed,
                usage: Usage::default(),
            },
        )
    }

    // ---- the test double ------------------------------------------------

    pub(crate) fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A session that hands back a scripted frame list and remembers the
    /// writes. A *live* one also answers: its stream stays open the way an
    /// attached session's does, and every submission becomes a turn, so a run
    /// that waits for its turns can be driven to its exit.
    #[derive(Debug)]
    pub(crate) struct TestSession {
        frames: Vec<Frame>,
        /// The live end of the stream, kept only while the session answers.
        publisher: Mutex<Option<mpsc::UnboundedSender<Frame>>>,
        /// The stream `open` hands out, taken once.
        stream: Mutex<Option<mpsc::UnboundedReceiver<Frame>>>,
        seq: AtomicU64,
        turns: AtomicU64,
        /// The options each `open` asked for.
        opened: Mutex<Vec<OpenOptions>>,
        submitted: Mutex<Vec<Input>>,
        answers: Mutex<Vec<(InteractionId, Answer, Activation)>>,
        interrupts: Mutex<Vec<InterruptScope>>,
    }

    impl TestSession {
        fn new(frames: Vec<Frame>, live: bool) -> Self {
            let (publisher, stream) = mpsc::unbounded_channel();
            for frame in &frames {
                let _ = publisher.send(frame.clone());
            }
            let seq = frames.iter().map(|f| f.seq.0).max().unwrap_or(0);
            Self {
                frames,
                publisher: Mutex::new(live.then_some(publisher)),
                stream: Mutex::new(Some(stream)),
                seq: AtomicU64::new(seq),
                turns: AtomicU64::new(0),
                opened: Mutex::default(),
                submitted: Mutex::default(),
                answers: Mutex::default(),
                interrupts: Mutex::default(),
            }
        }

        /// The canned frames, then whatever the session publishes; a session
        /// that is not live has dropped its end, so the stream ends with them.
        fn stream(&self) -> FrameStream {
            let Some(stream) = locked(&self.stream).take() else {
                return Box::pin(futures::stream::empty());
            };
            Box::pin(futures::stream::unfold(stream, |mut stream| async move {
                stream.recv().await.map(|frame| (frame, stream))
            }))
        }

        fn publish(&self, event: Event) {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(publisher) = &*locked(&self.publisher) {
                let _ = publisher.send(frame(seq, event));
            }
        }

        /// The frames a live kernel would answer a submission with: the input
        /// as an item of a turn, and the turn, opened and closed.
        fn run_turn(&self, intent: IntentId, text: &str, origin: Origin) {
            let turn = TurnId::from_raw(format!(
                "trn_{}",
                self.turns.fetch_add(1, Ordering::Relaxed) + 1
            ));
            let item = Item {
                id: ItemId::from_raw(format!("itm_{turn}")),
                turn: Some(turn.clone()),
                intent: Some(intent.clone()),
                body: ItemBody::User {
                    parts: vec![ContentPart::text(text)],
                    origin,
                },
                ..assistant("itm_0", "", ItemStatus::Completed)
            };
            let inputs = vec![item.id.clone()];
            self.publish(Event::ItemCompleted { item });
            self.publish(Event::TurnStarted {
                turn: turn.clone(),
                inputs,
                origin: TurnOrigin::Submit,
            });
            self.publish(Event::IntentAck {
                intent,
                outcome: IntentOutcome::TurnStarted { turn: turn.clone() },
            });
            self.publish(Event::TurnCompleted {
                turn,
                status: TurnStatus::Completed,
                usage: Usage::default(),
            });
        }

        pub(crate) fn opened(&self) -> Vec<OpenOptions> {
            locked(&self.opened).clone()
        }

        pub(crate) fn submitted(&self) -> Vec<Input> {
            locked(&self.submitted).clone()
        }

        pub(crate) fn prompts(&self) -> Vec<String> {
            self.submitted()
                .iter()
                .filter_map(|input| match input {
                    Input::Text { text, .. } => Some(text.clone()),
                    Input::Action { .. } => None,
                })
                .collect()
        }

        pub(crate) fn answers(&self) -> Vec<(InteractionId, Answer, Activation)> {
            locked(&self.answers).clone()
        }

        pub(crate) fn interrupts(&self) -> Vec<InterruptScope> {
            locked(&self.interrupts).clone()
        }
    }

    #[async_trait]
    impl SessionPort for TestSession {
        fn submit(&self, intent: IntentId, input: Input) {
            locked(&self.submitted).push(input.clone());
            if let Input::Text { text, origin, .. } = input {
                self.run_turn(intent, &text, origin);
            }
        }

        fn interrupt(&self, _intent: IntentId, scope: InterruptScope) {
            locked(&self.interrupts).push(scope);
        }

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

        /// The journal replay: durable frames only, like the kernel's.
        async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
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
    pub(crate) struct TestHost {
        session: Arc<TestSession>,
    }

    impl TestHost {
        /// A session that replays the frames and then ends the stream.
        fn with(frames: Vec<Frame>) -> (HostHandle, Arc<TestSession>) {
            Self::of(TestSession::new(frames, false))
        }

        /// A session that stays open and answers what it is asked.
        pub(crate) fn live(frames: Vec<Frame>) -> (HostHandle, Arc<TestSession>) {
            Self::of(TestSession::new(frames, true))
        }

        fn of(session: TestSession) -> (HostHandle, Arc<TestSession>) {
            let session = Arc::new(session);
            let host = TestHost {
                session: Arc::clone(&session),
            };
            (HostHandle(Arc::new(host)), session)
        }
    }

    #[async_trait]
    impl HostApi for TestHost {
        async fn sessions(
            &self,
            _filter: SessionFilter,
        ) -> Result<Vec<SessionSummary>, KernelError> {
            Ok(vec![summary()])
        }

        async fn open(
            &self,
            _selector: SessionSelector,
            _who: ClientIdentity,
            options: OpenOptions,
        ) -> Result<Attachment, KernelError> {
            locked(&self.session.opened).push(options);
            Ok(Attachment {
                session: SessionId::from_raw("ses_1"),
                snapshot: session_state(),
                events: self.session.stream(),
                handle: SessionHandle(Arc::clone(&self.session) as Arc<dyn SessionPort>),
            })
        }

        async fn close(
            &self,
            _session: &SessionId,
            _reason: CloseReason,
        ) -> Result<(), KernelError> {
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
                entries: vec![CatalogEntry {
                    id: "Read".into(),
                    label: "Read".into(),
                    meta: Value::Null,
                }],
            })
        }

        fn gateway_events(&self) -> GatewayStream {
            Box::pin(futures::stream::empty())
        }

        fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
            None
        }
    }

    /// A console a test speaks through.
    #[derive(Debug)]
    pub(crate) struct TestConsole {
        interactive: bool,
        human: bool,
        stdin: String,
        lines: VecDeque<String>,
        /// The host protocol's stdin; absent means a stdin already at its end.
        host: Option<mpsc::Receiver<String>>,
    }

    impl TestConsole {
        fn headless() -> Self {
            Self {
                interactive: false,
                human: false,
                stdin: String::new(),
                lines: VecDeque::new(),
                host: None,
            }
        }

        fn typing(lines: &[&str]) -> Self {
            Self {
                interactive: true,
                lines: lines.iter().map(|l| format!("{l}\n")).collect(),
                ..Self::headless()
            }
        }

        /// A console whose stdin is these lines and then the end of the file.
        pub(crate) fn hosted(lines: &[String]) -> Self {
            let (writer, host) = mpsc::channel(lines.len().max(1));
            for line in lines {
                let _ = writer.try_send(line.clone());
            }
            Self {
                host: Some(host),
                ..Self::headless()
            }
        }

        /// A console whose stdin the test writes to as the run goes; dropping
        /// the sender is the end of the file.
        pub(crate) fn fed() -> (Self, mpsc::Sender<String>) {
            let (writer, host) = mpsc::channel(LINE_BUFFER);
            (
                Self {
                    host: Some(host),
                    ..Self::headless()
                },
                writer,
            )
        }
    }

    impl Console for TestConsole {
        fn interactive(&self) -> bool {
            self.interactive
        }

        fn human(&self) -> bool {
            self.human
        }

        fn read_all(&mut self) -> io::Result<String> {
            Ok(std::mem::take(&mut self.stdin))
        }

        fn read_line(&mut self) -> io::Result<String> {
            Ok(self.lines.pop_front().unwrap_or_default())
        }

        fn lines(&mut self) -> mpsc::Receiver<String> {
            self.host.take().unwrap_or_else(|| {
                let (_closed, stdin) = mpsc::channel(1);
                stdin
            })
        }
    }

    pub(crate) fn options(prompt: Option<&str>, args: serde_json::Value) -> SurfaceOptions {
        SurfaceOptions {
            cwd: "/tmp".into(),
            selector: SessionSelector::ById {
                id: SessionId::from_raw("ses_1"),
            },
            prompt: prompt.map(str::to_owned),
            args,
            env: Arc::new(bingo_sdk::Env::rooted("/tmp")),
        }
    }

    pub(crate) struct Run {
        pub(crate) exit: Result<Exit, KernelError>,
        pub(crate) out: String,
        pub(crate) err: String,
        pub(crate) session: Arc<TestSession>,
    }

    async fn play(frames: Vec<Frame>, console: &mut TestConsole, opts: SurfaceOptions) -> Run {
        let (host, session) = TestHost::with(frames);
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let exit = drive(&host, opts, console, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8_lossy(&out).into_owned(),
            err: String::from_utf8_lossy(&err).into_owned(),
            session,
        }
    }

    /// The common case: a prompt on the command line, nobody at the keyboard.
    async fn headless(frames: Vec<Frame>) -> Run {
        play(
            frames,
            &mut TestConsole::headless(),
            options(Some("hi"), json!({})),
        )
        .await
    }

    async fn answering(frames: Vec<Frame>, typed: &str) -> Run {
        typing(frames, &[typed]).await
    }

    /// A run that answers one line per question, in order.
    async fn typing(frames: Vec<Frame>, lines: &[&str]) -> Run {
        play(
            frames,
            &mut TestConsole::typing(lines),
            options(Some("hi"), json!({})),
        )
        .await
    }

    /// The two questions one `AskUserQuestion` call opens (M53).
    pub(crate) fn form() -> Interaction {
        let asked = |header: &str, options: &[(&str, &str)]| Question {
            question: format!("Which {header}?"),
            header: Some(header.into()),
            options: options
                .iter()
                .map(|(id, label)| QuestionOption {
                    id: (*id).into(),
                    label: (*label).into(),
                    description: None,
                    role: None,
                    preview: None,
                })
                .collect(),
            free_text: true,
            multi: false,
        };
        Interaction {
            kind: InteractionKind::Form {
                title: None,
                questions: vec![
                    asked("store", &[("0", "Postgres"), ("1", "SQLite")]),
                    asked("runtime", &[("0", "tokio"), ("1", "smol")]),
                ],
            },
            answers: vec![AnswerSpec::Form, AnswerSpec::Cancel],
            ..permission(None)
        }
    }

    fn opened(interaction: Interaction) -> Vec<Frame> {
        vec![
            frame(1, Event::InteractionOpened { interaction }),
            completed(2),
        ]
    }

    // ---- the tests ------------------------------------------------------

    #[tokio::test]
    async fn a_text_turn_prints_prose_and_exits_zero() {
        let run = headless(vec![
            frame(
                1,
                Event::ItemStarted {
                    item: assistant("itm_1", "", ItemStatus::Running),
                },
            ),
            frame(
                2,
                Event::ItemDelta {
                    item: ItemId::from_raw("itm_1"),
                    n: 0,
                    kind: DeltaKind::Text,
                    data: "Hello".into(),
                },
            ),
            frame(
                3,
                Event::ItemCompleted {
                    item: assistant("itm_1", "Hello", ItemStatus::Completed),
                },
            ),
            completed(4),
        ])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(run.out, "Hello\n");
        assert_eq!(run.err, "");
        let submitted = run.session.submitted();
        assert_eq!(submitted.len(), 1);
        assert!(matches!(
            &submitted[0],
            Input::Text { text, origin, .. } if text == "hi" && origin.surface == "print"
        ));
    }

    #[tokio::test]
    async fn a_rejected_submit_is_one_error_line_and_exit_one() {
        let run = headless(vec![frame(
            1,
            Event::IntentAck {
                intent: IntentId::from_raw("req_1"),
                outcome: IntentOutcome::Rejected {
                    error: KernelError::new(ErrorCode::SessionClosed, "the session is closed"),
                },
            },
        )])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 1 }));
        assert_eq!(run.out, "");
        assert_eq!(
            run.err,
            "[error] code=SESSION_CLOSED msg=the session is closed\n"
        );
    }

    #[tokio::test]
    async fn json_mode_writes_one_frame_per_line_and_no_prose() {
        let run = play(
            vec![
                frame(
                    1,
                    Event::ItemCompleted {
                        item: assistant("itm_1", "Hello", ItemStatus::Completed),
                    },
                ),
                completed(2),
            ],
            &mut TestConsole::headless(),
            options(Some("hi"), json!({ "outputFormat": "json" })),
        )
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(run.out.lines().count(), 2);
        for line in run.out.lines() {
            serde_json::from_str::<Frame>(line).expect("a frame per line");
        }
    }

    /// The preamble is written when the session opens, before the prompt is
    /// even submitted, and it carries the host's tool catalogue.
    #[tokio::test]
    async fn stream_json_mode_opens_with_the_preamble_and_ends_with_the_result() {
        let run = play(
            vec![
                frame(
                    1,
                    Event::ItemCompleted {
                        item: assistant("itm_1", "Hello", ItemStatus::Completed),
                    },
                ),
                completed(2),
            ],
            &mut TestConsole::headless(),
            options(Some("hi"), json!({ "outputFormat": "stream-json" })),
        )
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        let lines: Vec<Value> = run
            .out
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["subtype"], json!("init"));
        assert_eq!(lines[0]["tools"], json!(["Read"]));
        assert_eq!(lines[1]["type"], json!("assistant"));
        assert_eq!(lines[2]["result"], json!("Hello"));
        assert_eq!(run.err, "");
    }

    #[tokio::test]
    async fn a_failed_turn_reports_the_error_and_exits_one() {
        let run = headless(vec![frame(
            1,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Failed {
                    error: KernelError::new(ErrorCode::ProviderUnavailable, "no provider"),
                },
                usage: Usage::default(),
            },
        )])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 1 }));
        assert_eq!(
            run.err,
            "[error] code=PROVIDER_UNAVAILABLE msg=no provider\n"
        );
        assert_eq!(run.out, "");
    }

    /// A frame of the sub-session `ses_2`, whose parent is the root.
    fn child(seq: u64, event: Event) -> Frame {
        let mut f = frame(seq, event);
        f.session = SessionId::from_raw("ses_2");
        f
    }

    fn done(turn: &str) -> Event {
        Event::TurnCompleted {
            turn: TurnId::from_raw(turn),
            status: TurnStatus::Completed,
            usage: Usage::default(),
        }
    }

    /// The root's turn, during which a sub-session announces itself, asks a
    /// permission, says something and ends its turn; then the root answers.
    fn a_tree_with_a_childs_prompt() -> Vec<Frame> {
        let mut child_summary = summary();
        child_summary.id = SessionId::from_raw("ses_2");
        child_summary.parent = Some(bingo_sdk::ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: Some(ItemId::from_raw("itm_1")),
        });
        let asked = Interaction {
            session: SessionId::from_raw("ses_2"),
            ..permission(None)
        };
        vec![
            frame(
                1,
                Event::TurnStarted {
                    turn: TurnId::from_raw("trn_1"),
                    inputs: Vec::new(),
                    origin: TurnOrigin::Submit,
                },
            ),
            child(
                1,
                Event::SessionUpdated {
                    summary: child_summary,
                },
            ),
            child(2, Event::InteractionOpened { interaction: asked }),
            child(
                3,
                Event::ItemCompleted {
                    item: assistant("itm_c", "the child's prose", ItemStatus::Completed),
                },
            ),
            child(4, done("trn_c")),
            frame(
                2,
                Event::ItemCompleted {
                    item: assistant("itm_2", "after the child", ItemStatus::Completed),
                },
            ),
            frame(3, done("trn_1")),
        ]
    }

    /// The run is attached to the tree (ADR-0010 §3): a sub-session's turn
    /// ending is its own business, and the run goes on to the root's.
    #[tokio::test]
    async fn a_sub_sessions_turn_ending_does_not_end_the_run() {
        let run = headless(a_tree_with_a_childs_prompt()).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert!(run.out.contains("after the child"), "{}", run.out);
    }

    /// A text run attaches to the tree as well: a sub-session's prompt has
    /// nobody else to reach, so it is refused here as the root's would be,
    /// while what the sub-session says stays off stdout.
    #[tokio::test]
    async fn a_text_run_refuses_a_sub_sessions_prompt_and_keeps_its_prose_off_stdout() {
        let run = headless(a_tree_with_a_childs_prompt()).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(run.out, "after the child\n");
        assert_eq!(run.session.opened(), [OpenOptions::with_children()]);
        let answers = run.session.answers();
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].0, InteractionId::from_raw("int_1"));
        assert!(matches!(answers[0].1, Answer::Deny { .. }), "{answers:?}");
    }

    #[tokio::test]
    async fn a_json_run_reports_the_root_alone_while_answering_the_tree() {
        let run = play(
            a_tree_with_a_childs_prompt(),
            &mut TestConsole::headless(),
            options(Some("hi"), json!({ "outputFormat": "json" })),
        )
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        for line in run.out.lines() {
            let frame: Frame = serde_json::from_str(line).expect("a frame per line");
            assert_eq!(frame.session, SessionId::from_raw("ses_1"), "{line}");
        }
        assert_eq!(
            run.session.answers().len(),
            1,
            "the child's prompt was answered"
        );
    }

    #[tokio::test]
    async fn an_interrupted_turn_exits_with_the_signal_code() {
        let run = headless(vec![frame(
            1,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Interrupted {
                    reason: InterruptReason::UserCancel,
                },
                usage: Usage::default(),
            },
        )])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 130 }));
    }

    #[tokio::test]
    async fn a_session_closed_before_the_turn_ends_is_an_error_exit() {
        let run = headless(vec![frame(
            1,
            Event::SessionClosed {
                reason: CloseReason::Error {
                    message: "the host went away".into(),
                },
            },
        )])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 1 }));
        assert_eq!(
            run.err,
            "[error] code=SESSION_CLOSED msg=the host went away\n"
        );
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_a_completion_is_an_error_exit() {
        let run = headless(vec![frame(
            1,
            Event::ItemCompleted {
                item: assistant("itm_1", "half a thought", ItemStatus::Completed),
            },
        )])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 1 }));
        assert_eq!(
            run.out, "half a thought",
            "no turn ended, so no closing newline"
        );
        assert!(run.err.starts_with("[error] code=SESSION_CLOSED msg="));
    }

    #[tokio::test]
    async fn without_a_terminal_a_permission_is_denied_immediately() {
        let run = headless(opened(permission(Some("Read(//tmp)")))).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(
            run.session.answers().as_slice(),
            &[(
                InteractionId::from_raw("int_1"),
                Answer::Deny {
                    feedback: Some("non-interactive".into())
                },
                Activation::Programmatic,
            )]
        );
        assert_eq!(run.err, "", "nobody is there to read a prompt");
    }

    #[tokio::test]
    async fn at_a_terminal_yes_allows_the_call_once() {
        let run = answering(opened(permission(None)), "y").await;
        let answers = run.session.answers();
        assert_eq!(answers[0].1, Answer::AllowOnce);
        assert_eq!(answers[0].2, Activation::Keyboard);
        assert_eq!(
            run.err,
            "[permission] Read: Read Cargo.toml  [y]es / [a]lways this session / [n]o\n"
        );
    }

    #[tokio::test]
    async fn always_installs_the_session_scope_the_kernel_offered() {
        let run = answering(opened(permission(Some("Read(//tmp)"))), "a").await;
        assert_eq!(
            run.session.answers()[0].1,
            Answer::AllowSession {
                scope: "Read(//tmp)".into()
            }
        );
    }

    #[tokio::test]
    async fn always_without_a_scope_falls_back_to_allowing_once() {
        let run = answering(opened(permission(None)), "a").await;
        assert_eq!(run.session.answers()[0].1, Answer::AllowOnce);
    }

    #[tokio::test]
    async fn anything_else_denies() {
        let run = answering(opened(permission(None)), "").await;
        assert_eq!(run.session.answers()[0].1, Answer::Deny { feedback: None });
    }

    #[tokio::test]
    async fn a_question_is_answered_with_the_option_the_person_typed() {
        let run = answering(
            opened(question(&[("a", "Cargo.toml"), ("b", "README.md")])),
            "b",
        )
        .await;
        assert_eq!(
            run.session.answers()[0].1,
            Answer::Choice {
                ids: vec!["b".into()],
                other: None,
            }
        );
        assert!(run.err.contains("[question] Which file?"));
        assert!(run.err.contains("b — README.md"));
    }

    /// Every question of a form is put in turn and answered once (M53): an
    /// option's id where it names one, the words where it does not, and a
    /// blank line for a question skipped.
    #[tokio::test]
    async fn a_form_is_put_one_question_at_a_time_and_answered_once() {
        let run = typing(opened(form()), &["1", "async-std"]).await;
        assert_eq!(
            run.session.answers()[0].1,
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
        assert!(
            run.err.contains("[question] store: Which store?"),
            "{}",
            run.err
        );
        assert!(
            run.err.contains("[question] runtime: Which runtime?"),
            "{}",
            run.err
        );
    }

    #[tokio::test]
    async fn a_question_of_a_form_left_blank_is_skipped_and_the_rest_still_land() {
        let run = typing(opened(form()), &["", "0"]).await;
        assert_eq!(
            run.session.answers()[0].1,
            Answer::Form {
                answers: vec![
                    Answer::Cancel,
                    Answer::Choice {
                        ids: vec!["0".into()],
                        other: None,
                    },
                ]
            }
        );
    }

    #[tokio::test]
    async fn a_question_answered_with_nonsense_is_cancelled() {
        let run = answering(opened(question(&[("a", "Cargo.toml")])), "z").await;
        assert_eq!(run.session.answers()[0].1, Answer::Cancel);
    }

    /// The live stream ends at the marker; the surface re-reads the journal
    /// from the last frame it applied and finds the completion there.
    #[tokio::test]
    async fn a_lag_marker_re_reads_the_journal_and_the_turn_still_ends() {
        let run = headless(vec![
            frame(
                3,
                Event::Lagged {
                    from: Seq(2),
                    to: Seq(3),
                },
            ),
            completed(4),
        ])
        .await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
    }

    #[tokio::test]
    async fn the_prompt_is_read_from_stdin_when_the_command_line_has_none() {
        let mut console = TestConsole::headless();
        console.stdin = "from stdin\n".into();
        let run = play(vec![completed(1)], &mut console, options(None, json!({}))).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert!(matches!(
            &run.session.submitted()[0],
            Input::Text { text, .. } if text == "from stdin\n"
        ));
    }

    #[tokio::test]
    async fn a_person_at_the_terminal_reads_prose_not_the_machine_line() {
        let mut console = TestConsole::headless();
        console.human = true;
        let failed = frame(
            1,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Failed {
                    error: KernelError::new(
                        ErrorCode::AuthRequired,
                        "The anthropic provider has no credentials. Set ANTHROPIC_API_KEY.",
                    ),
                },
                usage: Usage::default(),
            },
        );
        let run = play(vec![failed], &mut console, options(Some("hi"), json!({}))).await;
        assert_eq!(run.exit, Ok(Exit { code: 1 }));
        assert_eq!(
            run.err,
            "error: The anthropic provider has no credentials. Set ANTHROPIC_API_KEY.\n"
        );
    }

    #[tokio::test]
    async fn an_image_that_does_not_read_is_invalid_input_before_any_turn() {
        let run = play(
            vec![completed(1)],
            &mut TestConsole::headless(),
            options(Some("look"), json!({ "images": ["/nowhere/shot.txt"] })),
        )
        .await;
        let Err(error) = run.exit else {
            panic!("the run must not start");
        };
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.starts_with("/nowhere/shot.txt: "), "{error}");
        assert!(run.out.is_empty(), "nothing reached stdout");
    }

    /// The pictures one prompt was submitted with.
    fn submitted_pictures(run: &Run, prompt: &str) -> Vec<Image> {
        let submitted = run.session.submitted();
        let [Input::Text { text, images, .. }] = submitted.as_slice() else {
            panic!("one prompt: {submitted:?}");
        };
        assert_eq!(text, prompt);
        images.clone()
    }

    #[tokio::test]
    async fn an_image_that_reads_goes_beside_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, bingo_pictures::testing::png_bytes(2, 2)).unwrap();
        let run = play(
            vec![completed(1)],
            &mut TestConsole::headless(),
            options(Some("look"), json!({ "images": [path.to_string_lossy()] })),
        )
        .await;
        let images = submitted_pictures(&run, "look");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
    }

    /// A format no provider takes is one a `--print` caller still has on
    /// disk: it is decoded at the edge and journaled as PNG (ADR-0041 §2).
    #[tokio::test]
    async fn an_image_of_a_wider_type_is_png_by_the_time_it_is_submitted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.bmp");
        let bmp = bingo_pictures::testing::drawn(4, 3, bingo_pictures::testing::ImageFormat::Bmp);
        std::fs::write(&path, bmp).unwrap();
        let run = play(
            vec![completed(1)],
            &mut TestConsole::headless(),
            options(Some("look"), json!({ "images": [path.to_string_lossy()] })),
        )
        .await;
        assert_eq!(submitted_pictures(&run, "look")[0].media_type, "image/png");
    }

    /// A relative `--image` is the session's, not the process's: `--cwd`
    /// moves the picture with everything else the run is about.
    #[tokio::test]
    async fn a_relative_image_is_read_from_the_sessions_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("shot.png"),
            bingo_pictures::testing::png_bytes(2, 2),
        )
        .unwrap();
        let mut opts = options(Some("look"), json!({ "images": ["shot.png"] }));
        opts.cwd = dir.path().to_path_buf();
        let run = play(vec![completed(1)], &mut TestConsole::headless(), opts).await;
        assert_eq!(submitted_pictures(&run, "look").len(), 1);
    }

    #[tokio::test]
    async fn an_empty_prompt_is_invalid_input() {
        let run = play(
            vec![completed(1)],
            &mut TestConsole::headless(),
            options(None, json!({})),
        )
        .await;
        assert_eq!(
            run.exit,
            Err(KernelError::new(
                ErrorCode::InvalidInput,
                "no prompt: pass one as an argument or on stdin"
            ))
        );
    }

    #[tokio::test]
    async fn a_tool_round_keeps_stdout_prose_only() {
        let run = headless(vec![
            frame(
                1,
                Event::ItemStarted {
                    item: tool_call("itm_2", "Read", None, ItemStatus::Running),
                },
            ),
            frame(
                2,
                Event::ItemCompleted {
                    item: tool_call(
                        "itm_2",
                        "Read",
                        Some(ToolOutput::text("ok")),
                        ItemStatus::Completed,
                    ),
                },
            ),
            frame(
                3,
                Event::ItemCompleted {
                    item: assistant("itm_3", "Read it.", ItemStatus::Completed),
                },
            ),
            completed(4),
        ])
        .await;
        assert_eq!(run.out, "Read it.\n");
        assert_eq!(
            run.err,
            "[tool] Read {\"file_path\":\"Cargo.toml\"}\n[tool] Read ok (12ms)\n"
        );
    }

    #[tokio::test]
    async fn a_user_item_is_never_echoed_to_stdout() {
        let echoed = Item {
            body: ItemBody::User {
                parts: vec![ContentPart::text("hi")],
                origin: Origin::surface(SURFACE_ID),
            },
            ..assistant("itm_0", "", ItemStatus::Completed)
        };
        let run = headless(vec![
            frame(1, Event::ItemCompleted { item: echoed }),
            completed(2),
        ])
        .await;
        assert_eq!(run.out, "");
    }
    #[test]
    fn the_plugin_registers_the_print_surface() {
        let mut registrar = Registrar::new(
            "bingo.surface.print",
            serde_json::Value::Null,
            bingo_sdk::Env::rooted("/tmp"),
        );
        PrintPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        match &contributions[0] {
            bingo_sdk::Contribution::Surface(surface) => {
                assert_eq!(surface.id(), SURFACE_ID);
                assert_eq!(surface.kind(), SurfaceKind::Exclusive);
            }
            other => panic!("expected a surface, got {other:?}"),
        }
        assert_eq!(PrintPlugin.manifest().provides, &["surface:print"]);
    }
}
