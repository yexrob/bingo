//! The loop. It selects over four things — the terminal, the frame stream, a
//! tick, and the results of the few host calls it makes — and it never awaits
//! the kernel: `submit`, `interrupt` and `answer` return nothing, and `open`,
//! `events_since`, `sessions` and `catalog` are spawned and mailed back. A key
//! press can therefore never wait for a turn.
//!
//! The attachment carries the whole tree (ADR-0010 §3): one stream, every
//! frame stamped with its own session, folded into one reducer state each by
//! [`crate::tree::Tree`].

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::time::{Duration, Instant};

use bingo_sdk::{
    Attachment, CatalogKind, ClientIdentity, CloseReason, CommandSpec, Event, Exit, FrameStream,
    HostHandle, Input, IntentId, IntentOutcome, InterruptScope, KernelError, Level, OpenOptions,
    SessionFilter, SessionHandle, SessionId, SessionSelector, SessionState, SessionSummary,
    SurfaceOptions, View,
};
use crossterm::event::Event as Term;
use futures::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::clock::Now;
use crate::effect::Effect;
use crate::terminal::Screen;
use crate::tree::{self, Tree};
use crate::ui::{Open, Picker, Ui};
use crate::{SURFACE_ID, commands, history, input};

/// How often a frame is redrawn *while something is moving*. Nothing moves
/// when nothing is happening, and then there is no tick at all: an idle
/// surface draws zero frames (§6).
const TICK: Duration = Duration::from_millis(100);
/// Sessions the `/resume` picker lists.
const RECENT: usize = 20;
/// What a write says while the mailbox of the session in view is still on its
/// way: it is refused, never held.
pub const NOT_YET: &str = "still opening that session — try again";

pub(crate) type Keys = Pin<Box<dyn Stream<Item = Term> + Send>>;

/// The results of the host calls the loop spawns.
enum Reply {
    Attached(Box<Attachment>),
    Resynced(FrameStream),
    /// A child's mailbox, so a line typed in its view reaches it. The
    /// attachment's own stream is dropped: the tree's carries the child.
    Handle(SessionId, SessionHandle),
    Sessions(Vec<SessionSummary>),
    Commands(Vec<CommandSpec>),
    /// One catalogue's ids, by the source name a command's argument gives.
    Catalogue(String, Vec<String>),
    Failed(KernelError),
}

/// What this surface is attached to: the reducer's state for every session
/// in the tree, and one mailbox per session it may write to.
struct Attached {
    tree: Tree,
    handles: HashMap<SessionId, SessionHandle>,
}

impl Attached {
    fn new(snapshot: SessionState, handle: SessionHandle) -> Self {
        let tree = Tree::new(snapshot);
        let handles = HashMap::from([(tree.root_id().clone(), handle)]);
        Self { tree, handles }
    }

    /// The mailbox the keyboard writes to: the session on screen.
    fn writer(&self) -> Option<SessionHandle> {
        self.handles.get(self.tree.view()).cloned()
    }

    /// The mailbox that answers an interaction, wherever in the tree it was
    /// opened (ADR-0010 §3).
    fn root(&self) -> Option<SessionHandle> {
        self.handles.get(self.tree.root_id()).cloned()
    }
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
    /// A selection a key asked for, handed to the terminal between frames.
    clipboard: Option<String>,
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
    let attachment = host
        .open(opts.selector, identity(), OpenOptions::with_children())
        .await?;
    let (tx, mut replies) = mpsc::channel(16);
    let mut events = Some(attachment.events);
    let mut run = Run {
        host: host.clone(),
        data_dir: opts.env.data_dir.clone(),
        session: Attached::new(attachment.snapshot, attachment.handle),
        ui: Ui::new(history::load(&opts.env.data_dir), Instant::now()),
        mine: HashSet::new(),
        replies: tx,
        clipboard: None,
        exit: None,
    };
    run.fetch_catalogs();
    if let Some(prompt) = opts.prompt {
        run.effect(Effect::Submit(bingo_sdk::Input::text(
            prompt,
            bingo_sdk::Origin::surface(SURFACE_ID),
        )));
    }
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
            () = tick(run.animating(Instant::now())) => {}
        }
        run.ui.expire(Instant::now());
        if let Some(exit) = run.exit.take() {
            let root = run.session.tree.root_id().clone();
            let _ = run.host.close(&root, CloseReason::Client).await;
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

/// The animation clock: a tick while something moves, and nothing at all
/// while nothing does — the one place a redraw can happen without an event.
async fn tick(animating: bool) {
    match animating {
        true => tokio::time::sleep(TICK).await,
        false => std::future::pending().await,
    }
}

impl Run {
    /// Whether the next frame would differ from this one on its own: a turn
    /// spins, the transcript eases where a key sent it, a notice is holding
    /// the status line until its time is up.
    fn animating(&self, now: Instant) -> bool {
        self.session.tree.sessions().any(SessionState::busy)
            || self.ui.scroll.moving(now)
            || self.ui.layer_moving(now)
            || !self.ui.notices.is_empty()
    }

    fn paint(&mut self, screen: &mut dyn Screen) -> Result<(), KernelError> {
        self.hand_over(screen)?;
        screen.title(&title(&self.session.tree)).map_err(stdio)?;
        screen
            .draw(&self.session.tree, &self.ui, Now::real())
            .map_err(stdio)
    }

    /// The selection goes to the terminal's own clipboard between frames, as
    /// the bell and the title do. A terminal that will not take one is told
    /// about, not worked around.
    fn hand_over(&mut self, screen: &mut dyn Screen) -> Result<(), KernelError> {
        let Some(text) = self.clipboard.take() else {
            return Ok(());
        };
        match crate::select::osc52(&text) {
            Some(bytes) => screen.copy(&bytes).map_err(stdio),
            None => {
                let refused = crate::select::refused(text.len());
                self.ui.notify(Level::Warn, refused, Instant::now());
                Ok(())
            }
        }
    }

    fn frame(
        &mut self,
        frame: &bingo_sdk::Frame,
        screen: &mut dyn Screen,
    ) -> Result<(), KernelError> {
        if self.session.tree.apply(frame) == bingo_sdk::Applied::Stale {
            return Ok(());
        }
        self.refocus();
        match &frame.event {
            // The lagged stream ends at its marker; the reducer left `seq` at
            // the last frame it applied, so replay from there fills the gap.
            Event::Lagged { .. } => self.resync(),
            Event::InteractionOpened { .. } => screen.bell().map_err(stdio)?,
            Event::Notice { level, text, .. } => {
                self.ui.notify(*level, text.clone(), Instant::now())
            }
            Event::IntentAck { intent, outcome } => self.ack(intent, outcome),
            Event::SessionClosed { .. } => self.closed(&frame.session),
            _ => {}
        }
        Ok(())
    }

    /// The root closing ends the run; a child closing leaves the tree, and
    /// the view comes back to the root with it.
    fn closed(&mut self, session: &SessionId) {
        if self.session.tree.is_root(session) {
            self.exit = Some(Exit { code: 0 });
            return;
        }
        self.session.tree.close(session);
        self.session.handles.remove(session);
    }

    /// The dialog follows the tree's first open interaction, whosever it is.
    fn refocus(&mut self) {
        let open = self.session.tree.open_interaction().map(|(_, open)| open);
        self.ui.dialog.focus_on(open);
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
                self.session.tree.mark_read();
                let effects = input::on_key(&mut self.ui, &self.session.tree, key, Now::real());
                self.apply(effects);
            }
            Term::Mouse(mouse) => {
                let effects = input::on_mouse(&mut self.ui, &self.session.tree, mouse, Now::real());
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
            Effect::Interrupt => self.interrupt(),
            Effect::Answer {
                interaction,
                answer,
                activation,
            } => self.answer(interaction, answer, activation),
            Effect::View(session) => self.show(session),
            Effect::Open(selector) => self.open(selector),
            Effect::ListSessions => self.list_sessions(),
            Effect::Copy(text) => self.clipboard = Some(text),
            Effect::Exit => self.exit = Some(Exit { code: 0 }),
        }
    }

    fn submit(&mut self, input: Input) {
        let Some(handle) = self.session.writer() else {
            return self.not_yet();
        };
        if let Input::Text { text, .. } = &input {
            history::append(&self.data_dir, text);
        }
        let intent = self.mint();
        handle.submit(intent, input);
    }

    fn interrupt(&mut self) {
        let Some(handle) = self.session.writer() else {
            return self.not_yet();
        };
        let intent = self.mint();
        handle.interrupt(intent, InterruptScope::Head);
    }

    /// An answer goes through the root: its handle knows which session asked.
    fn answer(
        &mut self,
        interaction: bingo_sdk::InteractionId,
        answer: bingo_sdk::Answer,
        activation: bingo_sdk::Activation,
    ) {
        let Some(handle) = self.session.root() else {
            return self.not_yet();
        };
        let intent = self.mint();
        handle.answer(intent, interaction, answer, activation);
    }

    fn not_yet(&mut self) {
        self.ui.notify(Level::Warn, NOT_YET, Instant::now());
    }

    /// Paint another session of the tree, and fetch its mailbox the first
    /// time, so what is typed there reaches it.
    fn show(&mut self, session: SessionId) {
        self.session.tree.show(&session);
        self.ui.scroll = Default::default();
        self.refocus();
        if self.session.handles.contains_key(&session) {
            return;
        }
        let host = self.host.clone();
        let id = session.clone();
        self.spawn(async move {
            host.open(
                SessionSelector::ById { id: session },
                identity(),
                OpenOptions::default(),
            )
            .await
            .map(|attachment| Reply::Handle(id, attachment.handle))
        });
    }

    fn mint(&mut self) -> IntentId {
        let intent = IntentId::mint();
        self.mine.insert(intent.clone());
        intent
    }

    /// The tree's stream is the root's, and so is the replay that heals it.
    fn resync(&mut self) {
        let Some(handle) = self.session.root() else {
            return;
        };
        let since = self.session.tree.root().seq;
        self.spawn(async move { handle.events_since(since).await.map(Reply::Resynced) });
    }

    /// `/clear` and `/resume` replace the whole tree, children and all.
    fn open(&mut self, selector: SessionSelector) {
        self.ui.opening = true;
        let host = self.host.clone();
        self.spawn(async move {
            host.open(selector, identity(), OpenOptions::with_children())
                .await
                .map(|a| Reply::Attached(Box::new(a)))
        });
    }

    fn list_sessions(&mut self) {
        let host = self.host.clone();
        let filter = SessionFilter {
            cwd: Some(std::path::PathBuf::from(
                &self.session.tree.root().summary.cwd,
            )),
            parent: None,
            limit: Some(RECENT),
        };
        self.spawn(async move { host.sessions(filter).await.map(Reply::Sessions) });
    }

    /// The catalogues are read once: the dropdown ranks them, it does not
    /// watch them.
    fn fetch_catalogs(&mut self) {
        let host = self.host.clone();
        self.spawn(async move {
            host.catalog(CatalogKind::Commands)
                .await
                .map(|c| Reply::Commands(commands::specs_from(&c)))
        });
        for (source, kind) in [
            ("models", CatalogKind::Models),
            ("providers", CatalogKind::Providers),
        ] {
            let host = self.host.clone();
            self.spawn(async move {
                host.catalog(kind).await.map(|c| {
                    Reply::Catalogue(
                        source.to_string(),
                        c.entries.into_iter().map(|e| e.id).collect(),
                    )
                })
            });
        }
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
            Reply::Handle(session, handle) => {
                self.session.handles.insert(session, handle);
            }
            Reply::Sessions(sessions) => self.ui.layer.show(
                Open::Picker(Picker {
                    sessions,
                    selected: 0,
                }),
                Instant::now(),
            ),
            Reply::Commands(specs) => self.ui.catalogs.commands = specs,
            Reply::Catalogue(source, ids) => {
                self.ui.catalogs.values.insert(source, ids);
            }
            Reply::Failed(error) => {
                self.ui.opening = false;
                self.ui.notify(Level::Error, error.message, Instant::now());
            }
        }
    }

    /// Swap trees: the old attachment is closed on its own task, the new one
    /// replaces it whole, and the scroll starts at the bottom again.
    fn attach(&mut self, attachment: Attachment, events: &mut Option<FrameStream>) {
        let host = self.host.clone();
        let old = self.session.tree.root_id().clone();
        tokio::spawn(async move { host.close(&old, CloseReason::Client).await });
        self.session = Attached::new(attachment.snapshot, attachment.handle);
        *events = Some(attachment.events);
        self.ui.opening = false;
        self.ui.scroll = Default::default();
        self.ui.layer.close(Instant::now());
        self.refocus();
    }
}

/// `bingo — <directory>`, and the session in view when it is not the root.
/// The mark is the whole tree's: a child that asks is one a person must come
/// back to.
fn title(tree: &Tree) -> String {
    let cwd = tree::directory(&tree.root().summary.cwd);
    let mark = if tree.attention() {
        crate::theme::THINKING
    } else {
        ""
    };
    let child = match tree.viewing() {
        Some(child) => format!(" {} {}", crate::theme::CHILD, tree::name(child)),
        None => String::new(),
    };
    format!("{mark}bingo — {cwd}{child}")
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

    #[test]
    fn a_view_whose_mailbox_has_not_landed_writes_nowhere() {
        let handle = bingo_sdk::SessionHandle(std::sync::Arc::new(TestSession::default()));
        let mut attached = Attached::new(state(), handle);
        attached.tree.apply(&child_frame(1, announced("reviewer")));
        attached.tree.show(&child_id());
        assert!(
            attached.writer().is_none(),
            "a key is refused until the child's mailbox arrives"
        );
        assert!(
            attached.root().is_some(),
            "an answer still has the root to go through"
        );
    }

    #[tokio::test]
    async fn a_childs_frames_are_folded_into_its_own_state() {
        let mut harness = Harness::new();
        let frames = vec![
            frame(
                1,
                Event::ItemCompleted {
                    item: tool(
                        "itm_1",
                        "SpawnAgent",
                        serde_json::json!({"prompt": "review it"}),
                        None,
                        ItemStatus::Completed,
                    ),
                },
            ),
            child_frame(1, announced("reviewer")),
            child_frame(2, started("trn_9")),
            closed(2),
        ];
        let (exit, _) = harness.go(frames, vec![], None).await;
        assert_eq!(exit, Exit { code: 0 });
        let screen = harness.recorder.last();
        assert!(screen.contains("1 running"), "{screen}");
        assert!(screen.contains("↳ reviewer · running"), "{screen}");
    }

    #[tokio::test]
    async fn a_permission_a_child_raised_is_answered_through_the_root_handle() {
        let mut harness = Harness::new();
        let frames = vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, opened(child_permission())),
        ];
        let (_, session) = harness.go(frames, vec![typed('y'), ctrl('d')], None).await;
        assert_eq!(
            session.answers(),
            vec![(
                bingo_sdk::InteractionId::from_raw("int_2"),
                bingo_sdk::Answer::AllowOnce,
                bingo_sdk::Activation::Keyboard,
            )],
            "the tree's handle routes an answer to whoever asked"
        );
        assert!(
            harness.recorder.last().contains("↳ reviewer"),
            "the dialog names the child: {}",
            harness.recorder.last()
        );
    }

    #[tokio::test]
    async fn a_line_typed_in_a_childs_view_is_submitted_on_the_childs_handle() {
        let mut harness = Harness::new();
        let (host, root, child) = TestHost::tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, started("trn_9")),
        ]);
        let script = vec![
            ctrl('g'),
            key(KeyCode::Down),
            key(KeyCode::Enter),
            typed('h'),
            typed('i'),
            key(KeyCode::Enter),
            ctrl('d'),
        ];
        let exit = drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(script),
        )
        .await
        .expect("the loop ran");
        assert_eq!(exit, Exit { code: 0 });
        assert_eq!(
            child.submitted(),
            vec![Input::text("hi", bingo_sdk::Origin::surface("tui"))]
        );
        assert!(root.submitted().is_empty(), "the root was not written to");
    }

    #[tokio::test]
    async fn a_child_that_closes_leaves_the_tree_and_the_run_goes_on() {
        let mut harness = Harness::new();
        let frames = vec![
            child_frame(1, announced("reviewer")),
            child_frame(
                2,
                Event::SessionClosed {
                    reason: CloseReason::Client,
                },
            ),
            frame(
                1,
                Event::ItemCompleted {
                    item: assistant("itm_1", "still here", ItemStatus::Completed),
                },
            ),
            closed(2),
        ];
        let (exit, _) = harness.go(frames, vec![], None).await;
        assert_eq!(exit, Exit { code: 0 });
        let screen = harness.recorder.last();
        assert!(screen.contains("still here"), "{screen}");
        assert!(!screen.contains("agent"), "the child is gone: {screen}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_surface_draws_nothing_at_all() {
        let mut harness = Harness::new();
        let (host, _) = TestHost::with(vec![]);
        // Two seconds pass before each key, on the runtime's virtual clock.
        let script = vec![key(KeyCode::Right), ctrl('d')];
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys_after(Duration::from_secs(2), script),
        )
        .await
        .expect("the loop ran");
        assert_eq!(
            harness.recorder.frames.len(),
            5,
            "one frame for each of the four things that happened at the start \
             and one for the keystroke — and none at all for the four seconds \
             of waiting between them"
        );
    }

    #[tokio::test]
    async fn a_copied_selection_reaches_the_terminals_own_clipboard() {
        let mut harness = Harness::new();
        // Enough transcript to scroll back through.
        let frames: Vec<_> = (1..=30)
            .map(|i| {
                frame(
                    i,
                    Event::ItemCompleted {
                        item: assistant(&format!("itm_{i}"), "All green.", ItemStatus::Completed),
                    },
                )
            })
            .collect();
        // Read back, take the first line, copy it.
        let script = vec![
            key(KeyCode::PageUp),
            typed('v'),
            key(KeyCode::Down),
            typed('y'),
            ctrl('d'),
        ];
        harness.go(frames, script, None).await;
        assert_eq!(
            harness.recorder.copies,
            vec![b"\x1b]52;c;QWxsIGdyZWVuLgo=\x07".to_vec()],
            "OSC 52 with `All green.\\n` as base64"
        );
    }

    #[test]
    fn a_selection_the_terminal_will_not_take_is_said_out_loud() {
        let mut run = Run {
            host: TestHost::with(vec![]).0,
            data_dir: std::path::PathBuf::new(),
            session: Attached::new(
                state(),
                SessionHandle(std::sync::Arc::new(TestSession::default())),
            ),
            ui: Ui::new(Vec::new(), Instant::now()),
            mine: HashSet::new(),
            replies: mpsc::channel(1).0,
            clipboard: Some("x".repeat(crate::select::LIMIT)),
            exit: None,
        };
        let mut recorder = Recorder::default();
        run.hand_over(&mut recorder)
            .expect("the notice is not an error");
        assert!(recorder.copies.is_empty(), "nothing was handed over");
        assert!(
            run.ui.notices.iter().any(|n| n.text.contains("100 KiB")),
            "the refusal names the size: {:?}",
            run.ui.notices
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
