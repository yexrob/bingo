//! The loop. It selects over four things — the terminal, the frame stream, a
//! tick, and the results of the few host calls it makes — and it never awaits
//! the kernel: `submit`, `interrupt` and `answer` return nothing, and `open`,
//! `events_since`, `sessions` and `catalog` are spawned and mailed back. A key
//! press can therefore never wait for a turn.

use std::collections::HashSet;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bingo_sdk::{
    Attachment, CatalogKind, ClientIdentity, CloseReason, CommandSpec, Event, Exit, FrameStream,
    HostHandle, Input, IntentId, IntentOutcome, InterruptScope, KernelError, Level, SessionFilter,
    SessionHandle, SessionId, SessionSelector, SessionState, SessionSummary, SurfaceOptions, View,
};
use crossterm::event::Event as Term;
use futures::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::clock::Now;
use crate::effect::Effect;
use crate::terminal::Screen;
use crate::ui::{Picker, Ui};
use crate::{SURFACE_ID, commands, history, input};

/// How often the spinner and the elapsed counter move.
const TICK: Duration = Duration::from_millis(100);
/// Sessions the `/resume` picker lists.
const RECENT: usize = 20;

pub(crate) type Keys = Pin<Box<dyn Stream<Item = Term> + Send>>;

/// The results of the host calls the loop spawns.
enum Reply {
    Attached(Box<Attachment>),
    Resynced(FrameStream),
    Sessions(Vec<SessionSummary>),
    Commands(Vec<CommandSpec>),
    Models(Vec<String>),
    Failed(KernelError),
}

/// The session this surface is attached to. Its state is the reducer's, never
/// the surface's.
struct Attached {
    id: SessionId,
    state: SessionState,
    handle: SessionHandle,
}

struct Run {
    host: HostHandle,
    data_dir: std::path::PathBuf,
    session: Attached,
    ui: Ui,
    /// The intents this client minted, so an ack meant for another client is
    /// not reported here.
    mine: HashSet<IntentId>,
    replies: mpsc::Sender<Reply>,
    exit: Option<Exit>,
}

pub(crate) fn identity() -> ClientIdentity {
    ClientIdentity {
        name: SURFACE_ID.into(),
        surface: SURFACE_ID.into(),
    }
}

pub(crate) async fn drive(
    host: &HostHandle,
    opts: SurfaceOptions,
    screen: &mut dyn Screen,
    mut keys: Keys,
) -> Result<Exit, KernelError> {
    let attachment = host.open(opts.selector, identity()).await?;
    let (tx, mut replies) = mpsc::channel(16);
    let mut events = Some(attachment.events);
    let mut run = Run {
        host: host.clone(),
        data_dir: opts.env.data_dir.clone(),
        session: Attached {
            id: attachment.session,
            state: attachment.snapshot,
            handle: attachment.handle,
        },
        ui: Ui::new(history::load(&opts.env.data_dir), Instant::now()),
        mine: HashSet::new(),
        replies: tx,
        exit: None,
    };
    run.fetch_catalogs();
    if let Some(prompt) = opts.prompt {
        run.effect(Effect::Submit(bingo_sdk::Input::text(
            prompt,
            bingo_sdk::Origin::surface(SURFACE_ID),
        )));
    }
    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            frame = next_frame(&mut events) => match frame {
                Some(frame) => run.frame(&frame, screen)?,
                None => events = None,
            },
            key = keys.next() => match key {
                Some(event) => run.terminal_event(event),
                None => run.exit = Some(Exit { code: 0 }),
            },
            Some(reply) = replies.recv() => run.reply(reply, &mut events),
            _ = tick.tick() => {}
        }
        run.ui.expire(Instant::now());
        if let Some(exit) = run.exit.take() {
            let _ = run.host.close(&run.session.id, CloseReason::Client).await;
            return Ok(exit);
        }
        run.paint(screen)?;
    }
}

/// A stream that has ended never wakes the loop again.
async fn next_frame(events: &mut Option<FrameStream>) -> Option<bingo_sdk::Frame> {
    match events.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

impl Run {
    fn paint(&mut self, screen: &mut dyn Screen) -> Result<(), KernelError> {
        screen.title(&title(&self.session.state)).map_err(stdio)?;
        screen
            .draw(&self.session.state, &self.ui, Now::real())
            .map_err(stdio)
    }

    fn frame(
        &mut self,
        frame: &bingo_sdk::Frame,
        screen: &mut dyn Screen,
    ) -> Result<(), KernelError> {
        if self.session.state.apply(frame) == bingo_sdk::Applied::Stale {
            return Ok(());
        }
        self.ui
            .dialog
            .focus_on(self.session.state.interactions.first());
        match &frame.event {
            // The lagged stream ends at its marker; the reducer left `seq` at
            // the last frame it applied, so replay from there fills the gap.
            Event::Lagged { .. } => self.resync(),
            Event::InteractionOpened { .. } => screen.bell().map_err(stdio)?,
            Event::Notice { level, text, .. } => {
                self.ui.notify(*level, text.clone(), Instant::now())
            }
            Event::IntentAck { intent, outcome } => self.ack(intent, outcome),
            Event::SessionClosed { .. } => self.exit = Some(Exit { code: 0 }),
            _ => {}
        }
        Ok(())
    }

    /// An ack for an intent this client minted; another client's is its own
    /// business.
    fn ack(&mut self, intent: &IntentId, outcome: &IntentOutcome) {
        if !self.mine.remove(intent) {
            return;
        }
        match outcome {
            IntentOutcome::Rejected { error } => {
                self.ui
                    .notify(Level::Error, error.message.clone(), Instant::now())
            }
            IntentOutcome::Applied { result } => self.applied(result),
            _ => {}
        }
    }

    fn applied(&mut self, result: &Value) {
        if let Some(message) = result.get("message").and_then(Value::as_str) {
            self.ui
                .notify(Level::Info, message.to_string(), Instant::now());
        }
        if let Some(view) = result
            .get("view")
            .and_then(|v| serde_json::from_value::<View>(v.clone()).ok())
        {
            self.ui.block = Some(view);
        }
    }

    fn terminal_event(&mut self, event: Term) {
        match event {
            Term::Key(key) => {
                self.session.state.mark_read();
                let effects = input::on_key(&mut self.ui, &self.session.state, key, Now::real());
                self.apply(effects);
            }
            Term::Paste(text) => input::on_paste(&mut self.ui, &text),
            _ => {}
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            self.effect(effect);
        }
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::Submit(input) => self.submit(input),
            Effect::Interrupt => {
                let intent = self.mint();
                self.session.handle.interrupt(intent, InterruptScope::Head);
            }
            Effect::Answer {
                interaction,
                answer,
                activation,
            } => {
                let intent = self.mint();
                self.session
                    .handle
                    .answer(intent, interaction, answer, activation);
            }
            Effect::Open(selector) => self.open(selector),
            Effect::ListSessions => self.list_sessions(),
            Effect::Exit => self.exit = Some(Exit { code: 0 }),
        }
    }

    fn submit(&mut self, input: Input) {
        if let Input::Text { text, .. } = &input {
            history::append(&self.data_dir, text);
        }
        let intent = self.mint();
        self.session.handle.submit(intent, input);
    }

    fn mint(&mut self) -> IntentId {
        let intent = IntentId::mint();
        self.mine.insert(intent.clone());
        intent
    }

    fn resync(&mut self) {
        let handle = self.session.handle.clone();
        let since = self.session.state.seq;
        self.spawn(async move { handle.events_since(since).await.map(Reply::Resynced) });
    }

    fn open(&mut self, selector: SessionSelector) {
        self.ui.opening = true;
        let host = self.host.clone();
        self.spawn(async move {
            host.open(selector, identity())
                .await
                .map(|a| Reply::Attached(Box::new(a)))
        });
    }

    fn list_sessions(&mut self) {
        let host = self.host.clone();
        let filter = SessionFilter {
            cwd: Some(std::path::PathBuf::from(&self.session.state.summary.cwd)),
            parent: None,
            limit: Some(RECENT),
        };
        self.spawn(async move { host.sessions(filter).await.map(Reply::Sessions) });
    }

    /// Both catalogues are read once: the dropdown ranks them, it does not
    /// watch them.
    fn fetch_catalogs(&mut self) {
        let host = self.host.clone();
        self.spawn(async move {
            host.catalog(CatalogKind::Commands)
                .await
                .map(|c| Reply::Commands(commands::specs_from(&c)))
        });
        let host = self.host.clone();
        self.spawn(async move {
            host.catalog(CatalogKind::Models)
                .await
                .map(|c| Reply::Models(c.entries.into_iter().map(|e| e.id).collect()))
        });
    }

    fn spawn(&self, call: impl Future<Output = Result<Reply, KernelError>> + Send + 'static) {
        let replies = self.replies.clone();
        tokio::spawn(async move {
            let reply = call.await.unwrap_or_else(Reply::Failed);
            let _ = replies.send(reply).await;
        });
    }

    fn reply(&mut self, reply: Reply, events: &mut Option<FrameStream>) {
        match reply {
            Reply::Attached(attachment) => self.attach(*attachment, events),
            Reply::Resynced(stream) => *events = Some(stream),
            Reply::Sessions(sessions) => {
                self.ui.picker = Some(Picker {
                    sessions,
                    selected: 0,
                })
            }
            Reply::Commands(specs) => self.ui.catalog = specs,
            Reply::Models(ids) => self.ui.models = ids,
            Reply::Failed(error) => {
                self.ui.opening = false;
                self.ui.notify(Level::Error, error.message, Instant::now());
            }
        }
    }

    /// Swap sessions: the old attachment is closed on its own task, the new
    /// state replaces the old, and the scroll starts at the bottom again.
    fn attach(&mut self, attachment: Attachment, events: &mut Option<FrameStream>) {
        let host = self.host.clone();
        let old = std::mem::replace(&mut self.session.id, attachment.session);
        tokio::spawn(async move { host.close(&old, CloseReason::Client).await });
        self.session.state = attachment.snapshot;
        self.session.handle = attachment.handle;
        *events = Some(attachment.events);
        self.ui.opening = false;
        self.ui.scroll = Default::default();
        self.ui.picker = None;
        self.ui
            .dialog
            .focus_on(self.session.state.interactions.first());
    }
}

/// `bingo — <directory>`, marked while something needs a person.
fn title(state: &SessionState) -> String {
    let cwd = Path::new(&state.summary.cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| state.summary.cwd.clone());
    let mark = if state.attention() {
        crate::theme::THINKING
    } else {
        ""
    };
    format!("{mark}bingo — {cwd}")
}

fn stdio(e: std::io::Error) -> KernelError {
    KernelError::new(bingo_sdk::ErrorCode::Internal, format!("terminal: {e}"))
}

/// The real terminal's key stream.
pub(crate) fn terminal_keys() -> Keys {
    Box::pin(crossterm::event::EventStream::new().filter_map(|e| async move { e.ok() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{CloseReason, ItemStatus, Seq, TurnStatus};
    use crossterm::event::KeyCode;

    struct Harness {
        home: tempfile::TempDir,
        recorder: Recorder,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().expect("a scratch home"),
                recorder: Recorder::default(),
            }
        }

        async fn go(
            &mut self,
            frames: Vec<bingo_sdk::Frame>,
            script: Vec<crossterm::event::KeyEvent>,
            prompt: Option<&str>,
        ) -> (Exit, std::sync::Arc<TestSession>) {
            let (host, session) = TestHost::with(frames);
            let exit = drive(
                &host,
                options(prompt, self.home.path()),
                &mut self.recorder,
                keys(script),
            )
            .await
            .expect("the loop ran");
            (exit, session)
        }
    }

    fn closed(seq: u64) -> bingo_sdk::Frame {
        frame(
            seq,
            Event::SessionClosed {
                reason: CloseReason::Client,
            },
        )
    }

    #[tokio::test]
    async fn frames_are_folded_and_drawn_until_the_session_closes() {
        let mut harness = Harness::new();
        let frames = vec![
            frame(
                1,
                Event::ItemCompleted {
                    item: user("itm_1", "run the tests"),
                },
            ),
            frame(
                2,
                Event::ItemCompleted {
                    item: assistant("itm_2", "All green.", ItemStatus::Completed),
                },
            ),
            closed(3),
        ];
        let (exit, _) = harness.go(frames, vec![], None).await;
        assert_eq!(exit, Exit { code: 0 });
        assert!(harness.recorder.last().contains("All green."));
        assert!(harness.recorder.last().contains("run the tests"));
    }

    #[tokio::test]
    async fn the_first_prompt_is_submitted_as_soon_as_the_session_is_open() {
        let mut harness = Harness::new();
        let (_, session) = harness.go(vec![closed(1)], vec![], Some("hello")).await;
        assert_eq!(
            session.submitted(),
            vec![Input::text("hello", bingo_sdk::Origin::surface("tui"))]
        );
    }

    #[tokio::test]
    async fn a_lag_marker_makes_the_loop_re_read_the_journal() {
        let mut harness = Harness::new();
        let frames = vec![
            frame(
                1,
                Event::ConfigChanged {
                    config: Default::default(),
                },
            ),
            frame(
                9,
                Event::Lagged {
                    from: Seq(2),
                    to: Seq(9),
                },
            ),
            frame(
                4,
                Event::ItemCompleted {
                    item: assistant("itm_2", "replayed", ItemStatus::Completed),
                },
            ),
            closed(5),
        ];
        let (exit, session) = harness.go(frames, vec![], None).await;
        assert_eq!(exit, Exit { code: 0 });
        assert_eq!(
            session.resyncs(),
            vec![Seq(1)],
            "the reducer left seq at the last frame it applied"
        );
        assert!(harness.recorder.last().contains("replayed"));
    }

    #[tokio::test]
    async fn what_is_typed_reaches_the_kernel_and_ctrl_d_ends_the_run() {
        let mut harness = Harness::new();
        let script = vec![typed('h'), typed('i'), key(KeyCode::Enter), ctrl('d')];
        let (exit, session) = harness.go(vec![], script, None).await;
        assert_eq!(exit, Exit { code: 0 });
        assert_eq!(
            session.submitted(),
            vec![Input::text("hi", bingo_sdk::Origin::surface("tui"))]
        );
    }

    #[tokio::test]
    async fn a_dialog_answered_at_the_keyboard_reaches_the_kernel() {
        let mut harness = Harness::new();
        let frames = vec![frame(1, opened(permission(Some("Edit(src/)"), None)))];
        let script = vec![typed('2'), ctrl('d')];
        let (_, session) = harness.go(frames, script, None).await;
        assert_eq!(
            session.answers(),
            vec![(
                bingo_sdk::InteractionId::from_raw("int_1"),
                bingo_sdk::Answer::AllowSession {
                    scope: "Edit(src/)".into()
                },
                bingo_sdk::Activation::Keyboard,
            )]
        );
        assert_eq!(harness.recorder.bells, 1, "an opened prompt rings once");
    }

    #[tokio::test]
    async fn esc_during_a_turn_interrupts_it() {
        let mut harness = Harness::new();
        let frames = vec![frame(1, started("trn_1"))];
        let script = vec![key(KeyCode::Esc), ctrl('d')];
        let (_, session) = harness.go(frames, script, None).await;
        assert_eq!(session.interrupts(), 1);
    }

    #[tokio::test]
    async fn the_title_marks_a_session_that_needs_a_person() {
        let mut harness = Harness::new();
        let frames = vec![
            frame(1, started("trn_1")),
            frame(2, completed("trn_1", TurnStatus::Completed)),
            closed(3),
        ];
        harness.go(frames, vec![], None).await;
        assert!(
            harness
                .recorder
                .titles
                .iter()
                .any(|t| t == "bingo — project"),
            "{:?}",
            harness.recorder.titles
        );
        assert!(
            harness
                .recorder
                .titles
                .iter()
                .any(|t| t == "✻ bingo — project"),
            "{:?}",
            harness.recorder.titles
        );
    }

    #[tokio::test]
    async fn a_submitted_line_is_appended_to_the_prompt_history() {
        let mut harness = Harness::new();
        let script = vec![typed('l'), typed('s'), key(KeyCode::Enter), ctrl('d')];
        harness.go(vec![], script, None).await;
        let data = bingo_sdk::Env::rooted(harness.home.path()).data_dir;
        assert_eq!(crate::history::load(&data), vec!["ls"]);
    }
}
