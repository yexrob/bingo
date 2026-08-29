//! The headless `--print` surface.
//!
//! It is a client like any other: it opens a session, submits one prompt, folds
//! the frames with `SessionState::apply` and renders them. It holds no session
//! state of its own (ADR-0002) and decides exactly two things — what to answer
//! an interaction with when nobody is at the keyboard, and what to exit with.

mod render;

use std::io::{self, IsTerminal, Write};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::QuestionOption;
use bingo_sdk::{
    Activation, Answer, AnswerSpec, Applied, Attachment, ClientIdentity, CloseReason, ErrorCode,
    Event, Exit, HostHandle, Input, IntentId, Interaction, InteractionKind, KernelError, Origin,
    Plugin, PluginError, PluginManifest, Registrar, SessionHandle, Surface, SurfaceKind,
    SurfaceOptions, TurnStatus,
};
use futures::StreamExt;

use render::{Mode, Renderer, error_line};

/// The surface id, and the origin every input it submits carries.
pub const SURFACE_ID: &str = "print";

/// Everything the surface needs from the terminal, so a test can be the terminal.
pub(crate) trait Console: Send {
    /// Whether a person is at the other end of stdin.
    fn interactive(&self) -> bool;

    /// The whole of stdin, read when no prompt came from the command line.
    fn read_all(&mut self) -> io::Result<String>;

    /// One answer line from a person.
    fn read_line(&mut self) -> io::Result<String>;
}

/// The real terminal.
#[derive(Debug, Default, Clone, Copy)]
struct Terminal;

impl Console for Terminal {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn read_all(&mut self) -> io::Result<String> {
        io::read_to_string(io::stdin())
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        Ok(line)
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

/// One prompt, one turn, one exit code. Every stream this touches is injected,
/// so the whole surface is exercised without a terminal.
pub(crate) async fn drive(
    host: &HostHandle,
    opts: SurfaceOptions,
    console: &mut (dyn Console + Send),
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> Result<Exit, KernelError> {
    let mut renderer = Renderer::new(Mode::from_args(&opts.args));
    let prompt = prompt_from(opts.prompt, console)?;

    let Attachment {
        mut snapshot,
        mut events,
        handle,
        ..
    } = host
        .open(
            opts.selector,
            ClientIdentity {
                name: SURFACE_ID.into(),
                surface: SURFACE_ID.into(),
            },
        )
        .await?;
    handle.submit(
        IntentId::mint(),
        Input::text(prompt, Origin::surface(SURFACE_ID)),
    );

    while let Some(frame) = events.next().await {
        if snapshot.apply(&frame) == Applied::Stale {
            continue;
        }
        renderer
            .render(&frame, &snapshot, &mut *out, &mut *err)
            .map_err(stdio_error)?;
        match react(&frame.event, &handle, console, err)? {
            Next::Await => {}
            Next::Resync => events = handle.events_since(snapshot.seq).await?,
            Next::Exit(exit) => return Ok(exit),
        }
    }
    closed("the event stream ended before the turn completed", err)
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
        Event::SessionClosed { reason } => closed(&close_message(reason), err).map(Next::Exit),
        _ => Ok(Next::Await),
    }
}

fn closed(message: &str, err: &mut (dyn Write + Send)) -> Result<Exit, KernelError> {
    writeln!(err, "{}", error_line(ErrorCode::SessionClosed, message)).map_err(stdio_error)?;
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
        InteractionKind::Question {
            question,
            header,
            options,
            ..
        } => ask_question(
            interaction,
            question,
            header.as_deref(),
            options,
            console,
            err,
        )?,
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
    question: &str,
    header: Option<&str>,
    options: &[QuestionOption],
    console: &mut (dyn Console + Send),
    err: &mut (dyn Write + Send),
) -> io::Result<Answer> {
    match header {
        Some(header) => writeln!(err, "[question] {header}: {question}")?,
        None => writeln!(err, "[question] {question}")?,
    }
    for option in options {
        writeln!(err, "  {} — {}", option.id, option.label)?;
    }
    err.flush()?;
    let chosen = console.read_line()?.trim().to_string();
    Ok(if options.iter().any(|o| o.id == chosen) {
        Answer::Choice { ids: vec![chosen] }
    } else {
        refuse(interaction, "no such option")
    })
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

    use bingo_sdk::{
        Catalog, CatalogKind, ContentPart, DeltaKind, Frame, FrameStream, GatewayStream,
        HistoryChunk, HistoryPage, HostApi, InteractionId, InterruptReason, InterruptScope, Item,
        ItemBody, ItemId, ItemStatus, QuestionOption, Seq, SessionFilter, SessionHandle, SessionId,
        SessionPort, SessionSelector, SessionState, SessionSummary, ToolOutput, TurnId, Usage,
    };
    use jiff::Timestamp;
    use serde_json::json;

    // ---- fixtures -------------------------------------------------------

    fn ts() -> Timestamp {
        Timestamp::from_second(1_700_000_000).expect("a fixed instant")
    }

    fn summary() -> SessionSummary {
        SessionSummary {
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
                child_session: None,
                duration_ms: Some(12),
            },
        )
    }

    fn permission(session_scope: Option<&str>) -> Interaction {
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

    fn question(options: &[(&str, &str)]) -> Interaction {
        Interaction {
            kind: InteractionKind::Question {
                question: "Which file?".into(),
                header: None,
                options: options
                    .iter()
                    .map(|(id, label)| QuestionOption {
                        id: (*id).into(),
                        label: (*label).into(),
                        description: None,
                    })
                    .collect(),
                free_text: false,
                multi: false,
            },
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

    /// A session that hands back a scripted frame list and remembers the writes.
    #[derive(Debug, Default)]
    struct TestSession {
        frames: Vec<Frame>,
        submitted: Mutex<Vec<Input>>,
        answers: Mutex<Vec<(InteractionId, Answer, Activation)>>,
    }

    impl TestSession {
        fn stream(&self, since: Seq) -> FrameStream {
            let frames: Vec<_> = self
                .frames
                .iter()
                .filter(|f| f.seq > since)
                .cloned()
                .collect();
            Box::pin(futures::stream::iter(frames))
        }

        fn submitted(&self) -> Vec<Input> {
            self.submitted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn answers(&self) -> Vec<(InteractionId, Answer, Activation)> {
            self.answers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    #[async_trait]
    impl SessionPort for TestSession {
        fn submit(&self, _intent: IntentId, input: Input) {
            self.submitted
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(input);
        }

        fn interrupt(&self, _intent: IntentId, _scope: InterruptScope) {}

        fn answer(
            &self,
            _intent: IntentId,
            interaction: InteractionId,
            answer: Answer,
            activation: Activation,
        ) {
            self.answers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
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
    struct TestHost {
        session: Arc<TestSession>,
    }

    impl TestHost {
        fn with(frames: Vec<Frame>) -> (HostHandle, Arc<TestSession>) {
            let session = Arc::new(TestSession {
                frames,
                ..Default::default()
            });
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
        ) -> Result<Attachment, KernelError> {
            Ok(Attachment {
                session: SessionId::from_raw("ses_1"),
                snapshot: session_state(),
                events: self.session.stream(Seq::ZERO),
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

        fn catalog(&self, kind: CatalogKind) -> Catalog {
            Catalog {
                kind,
                entries: Vec::new(),
            }
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
    struct TestConsole {
        interactive: bool,
        stdin: String,
        lines: VecDeque<String>,
    }

    impl TestConsole {
        fn headless() -> Self {
            Self {
                interactive: false,
                stdin: String::new(),
                lines: VecDeque::new(),
            }
        }

        fn typing(lines: &[&str]) -> Self {
            Self {
                interactive: true,
                stdin: String::new(),
                lines: lines.iter().map(|l| format!("{l}\n")).collect(),
            }
        }
    }

    impl Console for TestConsole {
        fn interactive(&self) -> bool {
            self.interactive
        }

        fn read_all(&mut self) -> io::Result<String> {
            Ok(std::mem::take(&mut self.stdin))
        }

        fn read_line(&mut self) -> io::Result<String> {
            Ok(self.lines.pop_front().unwrap_or_default())
        }
    }

    fn options(prompt: Option<&str>, args: serde_json::Value) -> SurfaceOptions {
        SurfaceOptions {
            cwd: "/tmp".into(),
            selector: SessionSelector::ById {
                id: SessionId::from_raw("ses_1"),
            },
            prompt: prompt.map(str::to_owned),
            args,
        }
    }

    struct Run {
        exit: Result<Exit, KernelError>,
        out: String,
        err: String,
        session: Arc<TestSession>,
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
        play(
            frames,
            &mut TestConsole::typing(&[typed]),
            options(Some("hi"), json!({})),
        )
        .await
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
                ids: vec!["b".into()]
            }
        );
        assert!(run.err.contains("[question] Which file?"));
        assert!(run.err.contains("b — README.md"));
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
