//! The loop. It selects over four things — the terminal, the frame stream, a
//! tick, and the results of the few host calls it makes — and it never awaits
//! the kernel: `submit`, `interrupt` and `answer` return nothing, and `open`,
//! `events_since`, `sessions` and `catalog` are spawned and mailed back. A key
//! press can therefore never wait for a turn.
//!
//! The attachment carries the whole tree (ADR-0010 §3): one stream, every
//! frame stamped with its own session, folded into one reducer state each by
//! [`crate::tree::Tree`].

use std::collections::HashMap;
use std::pin::Pin;
use std::time::{Duration, Instant};

use bingo_sdk::{
    Attachment, CatalogKind, ClientIdentity, CloseReason, CommandSpec, Event, Exit, FrameStream,
    GatewayEvent, GatewayStream, HostHandle, Image, Input, IntentId, IntentOutcome, InterruptScope,
    KernelError, Level, OpenOptions, Origin, SessionFilter, SessionHandle, SessionId,
    SessionSelector, SessionState, SessionSummary, SurfaceOptions, View,
};
use crossterm::event::Event as Term;
use futures::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::clock::{self, Now};
use crate::effect::Effect;
use crate::graphics::{Cell, Graphics, Picture, Stored, Transport};
use crate::terminal::{Notification, Screen};
use crate::tree::{self, Tree};
use crate::ui::{Open, Picker, Ui};
use crate::{SURFACE_ID, clipboard, commands, complete, history, input, pictures, pointer};

/// How often a frame is redrawn *while something is moving*: thirty a second
/// (§6). Nothing moves when nothing is happening, and then there is no tick at
/// all — an idle surface draws zero frames.
const TICK: Duration = clock::FRAME;

/// A draw that costs this much is itself the latency a person feels, and is
/// owned up to once per run (`slow_draw`).
const SLOW_DRAW: Duration = Duration::from_millis(100);
/// Sessions the `/resume` picker lists.
const RECENT: usize = 20;
/// The catalogues whose ids a command's argument may name, by the source name
/// its `ArgSpec::Catalog` gives.
const CATALOGUES: &[(&str, CatalogKind)] = &[
    ("models", CatalogKind::Models),
    ("providers", CatalogKind::Providers),
];
/// What a write says while the mailbox of the session in view is still on its
/// way: it is refused, never held.
pub const NOT_YET: &str = "still opening that session — try again";
/// Rows of the screen kept back when the transcript is printed on the way
/// out: the shell's own prompt needs somewhere to land.
const KEPT_BACK: u16 = 2;

pub(crate) type Keys = Pin<Box<dyn Stream<Item = Term> + Send>>;

/// The results of the host calls the loop spawns.
enum Reply {
    Attached(Box<Attachment>),
    Resynced(FrameStream),
    /// A child's mailbox, so a line typed in its view reaches it. The
    /// attachment's own stream is dropped: the tree's carries the child.
    Handle(SessionId, SessionHandle),
    Sessions(Vec<SessionSummary>),
    /// What the store holds, for the switcher's stored rows.
    Stored(Vec<SessionSummary>),
    Commands(Vec<CommandSpec>),
    /// One catalogue's ids, by the source name a command's argument gives.
    Catalogue(String, Vec<String>),
    /// A line whose pictures have been read in (ADR-0041): the loop asked for
    /// them off its own thread, so a slow web server costs it no frame.
    Mentioned(Box<Mentioned>),
    Failed(KernelError),
}

/// A submitted line waiting on the pictures it named. It carries the mailbox
/// it was typed at, so a person who switches session while a picture is in
/// flight still sends it where they wrote it.
struct Mentioned {
    handle: SessionHandle,
    text: String,
    origin: Origin,
    /// The composer's own pictures with the mentions read in after them, or
    /// the first mention that did not read, in the words a person is shown.
    images: Result<Vec<Image>, String>,
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

/// Why the loop woke, and so whether what it did is drawn now or on the next
/// frame. Everything that happens — a keystroke, a reply, a frame from the
/// kernel — is folded as fast as it arrives and drawn on the animation clock,
/// so a thousand deltas or a trackpad's thousand notches a second cost thirty
/// draws and not a thousand (§6). What is owed is never owed for longer than
/// one tick: an event that is not drawn where it happens arms the frame
/// boundary, and the boundary is the wake that always draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Event,
    Frame,
}

struct Run {
    host: HostHandle,
    data_dir: std::path::PathBuf,
    session: Attached,
    ui: Ui,
    /// The intents this client minted and the line each carried, so an ack
    /// meant for another client is not reported here and a refusal can name
    /// what was refused.
    mine: HashMap<IntentId, Option<String>>,
    replies: mpsc::Sender<Reply>,
    /// A selection a key asked for, handed to the terminal between frames.
    clipboard: Option<String>,
    /// The pictures the terminal is holding, which the frames that draw them
    /// keep in step (design §5). Empty on every run: a terminal taken afresh
    /// holds nothing this surface put there.
    stored: Stored,
    /// When the last frame was painted, and whether anything has happened
    /// since that has not been.
    painted: Instant,
    behind: bool,
    /// Whether this run has already owned up to slow drawing.
    sluggish: bool,
    exit: Option<Exit>,
}

pub(crate) fn identity() -> ClientIdentity {
    ClientIdentity {
        name: SURFACE_ID.into(),
        surface: SURFACE_ID.into(),
    }
}

/// How a run ended: the code, and the screenful of transcript the shell gets
/// back once the alternate screen is gone (design §3).
pub(crate) struct Farewell {
    pub exit: Exit,
    pub screen: Vec<String>,
}

pub(crate) async fn drive(
    host: &HostHandle,
    opts: SurfaceOptions,
    screen: &mut dyn Screen,
    mut keys: Keys,
) -> Result<Farewell, KernelError> {
    let (tx, mut replies) = mpsc::channel(16);
    let (mut run, mut events) = attach(host, opts, tx).await?;
    // The host's own stream, beside the session's: what changed for the whole
    // process rather than for this conversation (ADR-0026 §4).
    let mut gateway = Some(host.gateway_events());
    loop {
        // `biased`, keys first: a person outranks the machine, and a
        // keystroke is never continuously ready, so checking it first
        // starves nobody. Under the default random pick, a frame storm
        // whose draws run longer than a tick makes a pending key lose the
        // coin flip a few times, each loss costing another whole draw —
        // tens of felt milliseconds. Frames stay ahead of the tick, or an
        // animation could shut the stream out while draws run long.
        let wake = tokio::select! {
            biased;
            key = keys.next() => {
                match key {
                    Some(event) => run.terminal_event(event),
                    None => run.exit = Some(Exit { code: 0 }),
                }
                Wake::Event
            },
            Some(reply) = replies.recv() => {
                run.reply(reply, &mut events);
                Wake::Event
            },
            // A handful an hour at most, so it is read before the frames and
            // starves nothing — and a catalogue rebuilt during a frame storm
            // does not wait for the storm to end.
            event = next_gateway(&mut gateway) => {
                match event {
                    Some(GatewayEvent::CatalogChanged { kind }) => run.catalog_changed(kind),
                    Some(_) => {}
                    None => gateway = None,
                }
                Wake::Event
            },
            frame = next_frame(&mut events) => {
                match frame {
                    Some(frame) => run.frame(&frame, screen)?,
                    None => events = None,
                }
                Wake::Event
            },
            () = tick(run.animating(Now::real()), run.painted + TICK) => Wake::Frame,
        };
        let now = Now::real();
        run.ui.expire(now);
        if let Some(exit) = run.exit.take() {
            return run.leave(screen, exit, now).await;
        }
        run.paint(screen, wake, now)?;
    }
}

/// Open the session the options name and take everything the loop holds with
/// it: the tree, the mailboxes, and the first prompt if there was one.
async fn attach(
    host: &HostHandle,
    opts: SurfaceOptions,
    replies: mpsc::Sender<Reply>,
) -> Result<(Run, Option<FrameStream>), KernelError> {
    let attachment = host
        .open(opts.selector, identity(), OpenOptions::with_children())
        .await?;
    let mut run = Run {
        host: host.clone(),
        data_dir: opts.env.data_dir.clone(),
        session: Attached::new(attachment.snapshot, attachment.handle),
        ui: Ui::new(history::load(&opts.env.data_dir), Instant::now()),
        mine: HashMap::new(),
        replies,
        clipboard: None,
        // Older than a frame, on the loop's own clock, so the first thing
        // that happens is drawn.
        painted: older_than_a_frame(),
        stored: Stored::default(),
        behind: false,
        sluggish: false,
        exit: None,
    };
    run.fetch_catalogs();
    crate::opening::notices(&mut run.ui, Instant::now());
    if let Some(prompt) = opts.prompt {
        run.effect(Effect::Submit(bingo_sdk::Input::text(
            prompt,
            bingo_sdk::Origin::surface(SURFACE_ID),
        )));
    }
    Ok((run, Some(attachment.events)))
}

/// The bytes that make the terminal hold the pictures this frame placed, cut
/// to the cells each of them covers.
///
/// A picture the terminal has not got is resolved back to where it came from
/// — the journal, or the composer's own held pictures — and decoded and
/// shrunk once ([`crate::graphics::Decoded`]); one it already has costs
/// nothing at all.
fn placing(
    ui: &Ui,
    state: &SessionState,
    cell: Cell,
    stored: &mut Stored,
    placed: &[Picture],
    transport: Transport,
) -> Vec<u8> {
    let pixels = |picture: &Picture| {
        let image = picture.image_in(state, &ui.pictures)?;
        ui.decoded
            .thumbnail(picture.id(), image, picture.pixels(cell))
    };
    stored.catch_up(placed, pixels, transport)
}

/// An instant one frame in the past, or this one on a machine that has not
/// been running for a whole frame yet.
fn older_than_a_frame() -> Instant {
    let now = Now::real().instant;
    now.checked_sub(TICK).unwrap_or(now)
}

/// A stream that has ended never wakes the loop again.
async fn next_frame(events: &mut Option<FrameStream>) -> Option<bingo_sdk::Frame> {
    match events.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// The same for the host's stream: a run whose host has stopped announcing
/// carries on with the session it already has.
async fn next_gateway(gateway: &mut Option<GatewayStream>) -> Option<GatewayEvent> {
    match gateway.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// The animation clock: a wake at the next frame boundary while something
/// moves, and nothing at all while nothing does — the one place a redraw can
/// happen without an event.
///
/// The deadline is measured from the last frame painted rather than from this
/// moment, so a storm of events cannot keep pushing it back: a new `sleep` on
/// every pass of the loop would be cancelled by the next delta and the screen
/// would go still exactly when it has the most to say.
async fn tick(animating: bool, next: Instant) {
    match animating {
        true => tokio::time::sleep_until(next.into()).await,
        false => std::future::pending().await,
    }
}

/// The complaint a run earns the first time a draw costs several frames;
/// `None` while drawing keeps up, or once it has already been said.
fn slow_draw(took: Duration, already: bool) -> Option<String> {
    (!already && took >= SLOW_DRAW).then(|| {
        format!(
            "drawing took {}ms; a debug build or a slow terminal does this",
            took.as_millis()
        )
    })
}

impl Run {
    /// Whether the next frame would differ from the one on the screen: a turn
    /// breathes, a block is still settling, the transcript eases where a key
    /// sent it, a layer is arriving or leaving, a notice is holding the status
    /// line until its time is up, an armed exit is holding its hint — or
    /// something happened faster than it can be drawn and what it did is not
    /// on the screen yet. That last one is what holds the promise a person
    /// feels: an event held at the gate leaves `behind` set, so the frame
    /// boundary is armed and draws it within one tick.
    ///
    /// Every clock here is read at the instant that frame was drawn, not at
    /// this one: a draw costs time, and an animation whose remaining frames
    /// all fall due during a slow one is over before the loop looks again —
    /// leaving a sheet halfway up the screen until the next keystroke.
    fn animating(&self, now: Now) -> bool {
        let screen = Now {
            instant: self.painted,
            ..now
        };
        self.behind
            || self.session.tree.sessions().any(SessionState::busy)
            || self.ui.scroll.moving(screen.instant)
            || self.ui.layer_moving(screen)
            || !self.ui.notices.is_empty()
            || self.ui.crossfading(screen)
            || self.ui.painted.borrow().blocks.moving()
            || self.ui.exit_armed(screen.instant)
    }

    /// The way out. Whatever arrived in the last tick is drawn first, so the
    /// screenful handed back to the shell is the one a person saw.
    async fn leave(
        &mut self,
        screen: &mut dyn Screen,
        exit: Exit,
        now: Now,
    ) -> Result<Farewell, KernelError> {
        self.paint(screen, Wake::Frame, now)?;
        self.forget_pictures(screen)?;
        let root = self.session.tree.root_id().clone();
        let _ = self.host.close(&root, CloseReason::Client).await;
        Ok(Farewell {
            exit,
            screen: self.farewell(screen.rows()),
        })
    }

    /// The last screenful of the transcript, as plain text, through the block
    /// cache's own degrade: what the alternate screen would otherwise take
    /// away with it.
    fn farewell(&self, rows: u16) -> Vec<String> {
        let rows = usize::from(rows.saturating_sub(KEPT_BACK));
        self.ui.painted.borrow().blocks.tail(rows)
    }

    /// Draw, unless the frame on the screen is younger than one tick: what
    /// happened is held until the frame boundary instead, which `animating`
    /// then owes and [`tick`] wakes for. Neither the kernel's pace nor a
    /// trackpad's is the screen's, and nothing waits longer than a frame.
    fn paint(&mut self, screen: &mut dyn Screen, wake: Wake, now: Now) -> Result<(), KernelError> {
        if wake == Wake::Event && now.since(self.painted) < TICK {
            self.behind = true;
            return Ok(());
        }
        self.behind = false;
        self.painted = now.instant;
        self.hand_over(screen)?;
        screen.title(&title(&self.session.tree)).map_err(stdio)?;
        let began = Instant::now();
        screen
            .draw(&self.session.tree, &self.ui, now)
            .map_err(stdio)?;
        self.grumble(began.elapsed());
        self.hand_pictures(screen)
    }

    /// Give the terminal its memory back on the way out: a picture it is
    /// holding for this surface outlives the run otherwise, and nothing will
    /// ever place it again.
    fn forget_pictures(&mut self, screen: &mut dyn Screen) -> Result<(), KernelError> {
        let transport = crate::graphics::chosen().transport();
        let bytes = self.stored.catch_up(&[], |_| None, transport);
        if bytes.is_empty() {
            return Ok(());
        }
        screen.place(&bytes).map_err(stdio)
    }

    /// The pictures the frame drew placeholders for, after the frame: the
    /// cells are on the screen and these are what the terminal draws into
    /// them, so they go out of band as the title and the clipboard do.
    ///
    /// A terminal that draws no pictures is asked for none, and neither is a
    /// frame whose pictures the terminal is already holding: the whole cost
    /// of a redraw is one walk of the blocks.
    fn hand_pictures(&mut self, screen: &mut dyn Screen) -> Result<(), KernelError> {
        let Graphics::Kitty { cell, transport } = crate::graphics::chosen() else {
            return Ok(());
        };
        let placed = self.ui.painted.borrow().placed();
        let state = self.session.tree.viewed();
        let bytes = placing(&self.ui, state, cell, &mut self.stored, &placed, transport);
        if bytes.is_empty() {
            return Ok(());
        }
        screen.place(&bytes).map_err(stdio)
    }

    /// Own up, once per run, when drawing itself is the latency a person
    /// feels: a debug build, a huge transcript or a slow terminal all land
    /// here, and no scheduling can hide a draw that costs several frames.
    fn grumble(&mut self, took: Duration) {
        if let Some(text) = slow_draw(took, self.sluggish) {
            self.sluggish = true;
            self.ui.notify(Level::Warn, text, Instant::now());
        }
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
            Event::InteractionOpened { .. } => {
                screen.bell().map_err(stdio)?;
                self.announce(Notification::NeedsYou, screen)?;
            }
            // A child finishing is the parent's business, not the desktop's:
            // what a person came back for is the turn they started.
            Event::TurnCompleted { .. } if self.session.tree.is_root(&frame.session) => {
                self.announce(Notification::Done, screen)?;
            }
            Event::Notice { level, text, .. } => {
                self.ui.notify(*level, text.clone(), Instant::now())
            }
            Event::IntentAck { intent, outcome } => self.ack(intent, outcome),
            Event::SessionClosed { .. } => self.closed(&frame.session),
            _ => {}
        }
        Ok(())
    }

    /// Say it where the desktop can see it — but only to a window nobody is
    /// looking at (§6). A focused screen is its own notification.
    fn announce(&self, what: Notification, screen: &mut dyn Screen) -> Result<(), KernelError> {
        if self.ui.focused {
            return Ok(());
        }
        screen
            .notify(&crate::terminal::notification(what))
            .map_err(stdio)
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
    /// business. A refusal names the line it refused, so a person sees which
    /// of theirs came back.
    fn ack(&mut self, intent: &IntentId, outcome: &IntentOutcome) {
        let Some(about) = self.mine.remove(intent) else {
            return;
        };
        match outcome {
            IntentOutcome::Rejected { error } => self.rejected(&error.message, about),
            IntentOutcome::Applied { result } => self.applied(result),
            _ => {}
        }
    }

    fn rejected(&mut self, message: &str, about: Option<String>) {
        let now = Instant::now();
        match about {
            Some(text) => self
                .ui
                .notify_about(Level::Error, message.to_string(), text, now),
            None => self.ui.notify(Level::Error, message.to_string(), now),
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
                let effects =
                    pointer::on_mouse(&mut self.ui, &self.session.tree, mouse, Now::real());
                self.apply(effects);
            }
            Term::Paste(text) => input::on_paste(&mut self.ui, &text),
            // A window nobody is looking at is the one that may interrupt a
            // person somewhere else on their desktop.
            Term::FocusGained => self.ui.focused = true,
            Term::FocusLost => self.ui.focused = false,
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
            Effect::ListStored => self.list_stored(),
            Effect::Copy(text) => self.clipboard = Some(text),
            Effect::PasteImage => self.paste_image(),
            Effect::Exit => self.exit = Some(Exit { code: 0 }),
        }
    }

    /// A picture on the clipboard becomes `[image N]` in the line and is held
    /// under that token; a clipboard with none leaves the line alone.
    fn paste_image(&mut self) {
        let Some(bytes) = clipboard::image() else {
            return;
        };
        match bingo_sdk::Image::from_bytes("image/png", &bytes) {
            Ok(image) => {
                let n = self.ui.pictures.hold(self.ui.composer.text(), image);
                self.ui.composer.insert(&pictures::placeholder(n));
                self.ui.edited();
            }
            Err(error) => {
                self.ui
                    .notify(Level::Warn, format!("clipboard: {error}"), Instant::now())
            }
        }
    }

    fn submit(&mut self, input: Input) {
        let Some(handle) = self.session.writer() else {
            return self.not_yet();
        };
        match input {
            Input::Text {
                text,
                images,
                origin,
            } => self.submit_text(handle, text, images, origin),
            action => {
                let intent = self.mint(None);
                handle.submit(intent, action);
            }
        }
    }

    /// A line goes as soon as the pictures it names are in hand. A line that
    /// names none goes now; the rest are read on their own task — a path off
    /// this machine's disk, a URL this machine fetches (ADR-0041 §3) — and
    /// come back as a reply, so no key press waits on a web server.
    fn submit_text(
        &mut self,
        handle: SessionHandle,
        text: String,
        images: Vec<Image>,
        origin: Origin,
    ) {
        let mentions = complete::attachments(&text);
        if mentions.is_empty() {
            return self.send_text(handle, text, images, origin);
        }
        let cwd = std::path::PathBuf::from(&self.session.tree.root().summary.cwd);
        self.spawn(async move {
            let images = read_mentions(mentions, &cwd, images).await;
            Ok(Reply::Mentioned(Box::new(Mentioned {
                handle,
                text,
                origin,
                images,
            })))
        });
    }

    /// The pictures are in hand: the words go with them, or the line comes
    /// back with the reason it did not — nothing is sent then, and what was
    /// typed is not lost.
    fn mentioned(&mut self, waiting: Mentioned) {
        let Mentioned {
            handle,
            text,
            origin,
            images,
        } = waiting;
        match images {
            Ok(images) => self.send_text(handle, text, images, origin),
            Err(why) => {
                self.ui.notify(Level::Warn, why, Instant::now());
                self.ui.composer.set(&text);
            }
        }
    }

    /// The words, their pictures, and the line they leave in the history.
    fn send_text(
        &mut self,
        handle: SessionHandle,
        text: String,
        images: Vec<Image>,
        origin: Origin,
    ) {
        history::append(&self.data_dir, &text);
        self.ui.pictures.clear();
        let intent = self.mint(Some(text.clone()));
        handle.submit(
            intent,
            Input::Text {
                text,
                images,
                origin,
            },
        );
    }

    fn interrupt(&mut self) {
        let Some(handle) = self.session.writer() else {
            return self.not_yet();
        };
        let intent = self.mint(None);
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
        let intent = self.mint(None);
        handle.answer(intent, interaction, answer, activation);
    }

    fn not_yet(&mut self) {
        self.ui.notify(Level::Warn, NOT_YET, Instant::now());
    }

    /// Paint another session of the tree, and fetch its mailbox the first
    /// time, so what is typed there reaches it.
    fn show(&mut self, session: SessionId) {
        self.session.tree.show(&session);
        // Somewhere else is somewhere else: the transcript comes back up out
        // of dim so the change of place is seen and not just noticed (§6).
        self.ui.switched = Some(Instant::now());
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

    /// An intent of this client's own, and the line it carried when it had
    /// one — which is what a refusal of it says it was about.
    fn mint(&mut self, about: Option<String>) -> IntentId {
        let intent = IntentId::mint();
        self.mine.insert(intent.clone(), about);
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

    /// The `/resume` picker's candidates. The cut to the card's size happens
    /// after the roots are kept, not in the filter: a project full of agents
    /// must not crowd the person's own sessions off the card.
    fn list_sessions(&mut self) {
        let host = self.host.clone();
        let filter = SessionFilter {
            cwd: Some(std::path::PathBuf::from(
                &self.session.tree.root().summary.cwd,
            )),
            parent: None,
            limit: None,
        };
        self.spawn(async move {
            host.sessions(filter)
                .await
                .map(|sessions| Reply::Sessions(Self::recent_roots(sessions)))
        });
    }

    /// The person's own sessions, newest first as the host answers: a child
    /// or a room hangs under a root and is reached through it — the switcher
    /// (M31) — never resumed on its own.
    fn recent_roots(mut sessions: Vec<SessionSummary>) -> Vec<SessionSummary> {
        sessions.retain(|session| session.parent.is_none());
        sessions.truncate(RECENT);
        sessions
    }

    /// What the switcher lists besides the tree: everything the host knows of,
    /// unfiltered — which of them hang under this root is the roster's own
    /// question, and a child need not work where its root does. One read per
    /// opening; nothing watches the store.
    fn list_stored(&mut self) {
        let host = self.host.clone();
        self.spawn(async move {
            host.sessions(SessionFilter::default())
                .await
                .map(Reply::Stored)
        });
    }

    /// The catalogues are read at start and again whenever the host says one
    /// of them was rebuilt: the dropdown follows, it does not poll.
    fn fetch_catalogs(&mut self) {
        let host = self.host.clone();
        self.spawn(async move {
            host.catalog(CatalogKind::Commands)
                .await
                .map(|c| Reply::Commands(commands::specs_from(&c)))
        });
        for (source, kind) in CATALOGUES {
            self.fetch_catalogue(source, *kind);
        }
    }

    /// One catalogue's ids, on the loop's own host-call lane.
    fn fetch_catalogue(&mut self, source: &'static str, kind: CatalogKind) {
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

    /// A catalogue the host rebuilt — a provider's endpoint answered what it
    /// serves (ADR-0026 §4) — is read again, and only that one.
    fn catalog_changed(&mut self, kind: CatalogKind) {
        let Some((source, _)) = CATALOGUES.iter().find(|(_, listed)| *listed == kind) else {
            return;
        };
        self.fetch_catalogue(source, kind);
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
            Reply::Stored(sessions) => self.fill_switcher(sessions),
            Reply::Commands(specs) => self.ui.catalogs.commands = specs,
            Reply::Catalogue(source, ids) => {
                self.ui.catalogs.values.insert(source, ids);
            }
            Reply::Mentioned(waiting) => self.mentioned(*waiting),
            Reply::Failed(error) => {
                self.ui.opening = false;
                self.ui.notify(Level::Error, error.message, Instant::now());
            }
        }
    }

    /// The store's answer lands after the card is already up. It fills the
    /// card while that is still what is open, and is dropped when it is not.
    fn fill_switcher(&mut self, sessions: Vec<SessionSummary>) {
        if let Open::Switcher(open) = &mut self.ui.layer.open {
            open.stored = sessions;
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

/// `images` with the pictures `mentions` name read in after them, in the
/// order they were written; the first that does not read is what comes back
/// instead, in the words the notice will carry. Off the loop's thread: a
/// path is read from disk and a URL is fetched by this machine (ADR-0041 §3).
async fn read_mentions(
    mentions: Vec<String>,
    cwd: &std::path::Path,
    mut images: Vec<Image>,
) -> Result<Vec<Image>, String> {
    for word in mentions {
        match bingo_pictures::load(&bingo_pictures::Source::parse(&word, cwd)).await {
            Ok(image) => images.push(image),
            Err(error) => return Err(format!("{word}: {error}")),
        }
    }
    Ok(images)
}

/// `bingo — <directory>`, and the session in view when it is not the root.
/// The mark is the whole tree's: a child that asks is one a person must come
/// back to.
fn title(tree: &Tree) -> String {
    let cwd = tree::directory(&tree.root().summary.cwd);
    let mark = match tree.attention() {
        true => format!("{} ", crate::theme::spark()),
        false => String::new(),
    };
    let child = match tree.viewing() {
        Some(child) => format!(" — in {}", tree::name(child)),
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
            (exit.exit, session)
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
        assert!(screen.contains("⏺ reviewer(review it)"), "{screen}");
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
            harness.recorder.last().contains("Edit · reviewer"),
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
        let ended = drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(script),
        )
        .await
        .expect("the loop ran");
        assert_eq!(ended.exit, Exit { code: 0 });
        assert_eq!(
            child.submitted(),
            vec![Input::text("hi", bingo_sdk::Origin::surface("tui"))]
        );
        assert!(root.submitted().is_empty(), "the root was not written to");
    }

    /// M35: a provider's endpoint answered what it serves, the host says the
    /// models catalogue was rebuilt, and the completions follow — on the same
    /// spawned lane, so no keystroke waited for the kernel.
    #[tokio::test]
    async fn the_completions_follow_a_catalogue_the_host_rebuilt() {
        let mut harness = Harness::new();
        let (host, _) = TestHost::announcing(vec![bingo_sdk::GatewayEvent::CatalogChanged {
            kind: CatalogKind::Models,
        }]);
        // ctrl+c empties the composer, so ctrl+d can end the run at all.
        let typing = "/model fake".chars().map(typed);
        let script: Vec<_> = typing.chain([ctrl('c'), ctrl('d')]).collect();
        let ended = drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(script),
        )
        .await
        .expect("the loop ran");
        assert_eq!(ended.exit, Exit { code: 0 });
        let offered = |id: &str| {
            harness
                .recorder
                .frames
                .iter()
                .any(|frame| frame.contains(id))
        };
        assert!(offered("fake-1"), "{}", harness.recorder.last());
        assert!(
            offered("fake-2"),
            "the model the refresh found is offered: {}",
            harness.recorder.last()
        );
    }

    /// M31 §2: opening the switcher spawns one `sessions` read on the loop's
    /// own host-call lane — no key waits on it — and what comes back fills
    /// the card that is already on the screen.
    #[tokio::test]
    async fn opening_the_switcher_reads_the_store_and_the_card_fills() {
        let mut harness = Harness::new();
        let (host, _) = TestHost::with_stored(vec![], vec![stored_summary("ses_7", "scout")]);
        let ended = drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(vec![ctrl('g'), ctrl('d')]),
        )
        .await
        .expect("the loop ran");
        assert_eq!(ended.exit, Exit { code: 0 });
        let screen = harness.recorder.last();
        assert!(screen.contains("⏺ scout"), "{screen}");
        assert!(screen.contains("stored"), "{screen}");
    }

    /// A child and a room hang under a root and are reached through it —
    /// the switcher — so the `/resume` picker never offers one, and the cut
    /// to the card's size happens after they are gone.
    #[test]
    fn the_resume_picker_offers_roots_and_never_a_child_or_a_room() {
        let mut sessions = vec![summary(), stored_summary("ses_7", "scout")];
        sessions.extend((0..RECENT).map(|at| SessionSummary {
            id: SessionId::from_raw(format!("ses_r{at}")),
            ..summary()
        }));
        let offered = Run::recent_roots(sessions);
        assert_eq!(offered.len(), RECENT, "the cut comes after the keep");
        assert!(
            offered.iter().all(|s| s.parent.is_none()),
            "only the person's own sessions are offered"
        );
        assert!(
            !offered.iter().any(|s| s.id == SessionId::from_raw("ses_7")),
            "the child was never a candidate"
        );
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
            4,
            "one frame for the first of the four things that happened at the \
             start and one at the frame boundary for the three that landed \
             inside it, one for the keystroke and one on the way out — and \
             none at all for the four seconds of waiting between them"
        );
    }

    /// A turn that answers and finishes, then `wait` of nothing at all before
    /// ctrl+d. Answers with how many frames the run drew.
    async fn frames_of_a_flash(wait: Duration) -> usize {
        let mut harness = Harness::new();
        let frames = vec![
            frame(1, started("trn_1")),
            frame(
                2,
                Event::ItemStarted {
                    item: assistant("itm_1", "All gr", ItemStatus::Running),
                },
            ),
            frame(
                3,
                Event::ItemCompleted {
                    item: assistant("itm_1", "All green.", ItemStatus::Completed),
                },
            ),
            frame(4, completed("trn_1", TurnStatus::Completed)),
        ];
        // A frame apart, so one is drawn while the answer is still running: a
        // completion only flashes where the rendering it replaces was seen.
        let (host, _) = TestHost::paced(frames, TICK * 2);
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys_after(wait, vec![ctrl('d')]),
        )
        .await
        .expect("the loop ran");
        assert!(
            harness.recorder.last().contains("All green."),
            "it comes to rest on the finished answer: {}",
            harness.recorder.last()
        );
        harness.recorder.frames.len()
    }

    /// The idle measure again, over a run that had motion in it. The waiting
    /// is what must cost nothing: a cue that went on reporting itself after
    /// its window had lapsed would draw through it at the tick rate, thirty
    /// frames a wasted second, and the surface would never be idle again.
    #[tokio::test(start_paused = true)]
    async fn the_seconds_after_a_flash_cost_no_frames_at_all() {
        assert_eq!(
            frames_of_a_flash(Duration::from_secs(1)).await,
            frames_of_a_flash(Duration::from_secs(3)).await,
            "two more seconds of waiting drew two more seconds of frames"
        );
    }

    /// A window nobody is looking at is the one that may interrupt a person
    /// somewhere else on their desktop (§6).
    async fn unfocused(
        harness: &mut Harness,
        mut frames: Vec<bingo_sdk::Frame>,
        focus: bool,
    ) -> usize {
        // The window says where it is first; the session closing is what ends
        // the run, so no key has to race the frames.
        frames.push(closed(900));
        let (host, _) = TestHost::paced(frames, Duration::from_millis(50));
        let script = match focus {
            true => Vec::new(),
            false => vec![Term::FocusLost],
        };
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            terminal_events(script, Duration::from_millis(5)),
        )
        .await
        .expect("the loop ran");
        harness.recorder.notifications.len()
    }

    #[tokio::test]
    async fn a_question_that_opens_on_a_window_nobody_watches_says_so() {
        let mut harness = Harness::new();
        let asked = vec![frame(1, opened(permission(Some("Edit(src/)"), None)))];
        assert_eq!(
            unfocused(&mut harness, asked.clone(), false).await,
            1,
            "exactly one notification, and the bell as well"
        );
        assert_eq!(harness.recorder.bells, 1);
        let bytes = harness.recorder.notifications[0].clone();
        assert!(bytes.starts_with(b"\x1b"), "{bytes:?}");
        assert!(
            String::from_utf8_lossy(&bytes).contains("needs you"),
            "{bytes:?}"
        );

        let mut watched = Harness::new();
        assert_eq!(
            unfocused(&mut watched, asked, true).await,
            0,
            "and none at all while the window is being looked at"
        );
        assert_eq!(watched.recorder.bells, 1, "the bell stays either way");
    }

    #[tokio::test]
    async fn a_turn_that_finishes_on_a_window_nobody_watches_says_so() {
        let mut harness = Harness::new();
        let done = vec![
            frame(1, started("trn_1")),
            frame(2, completed("trn_1", TurnStatus::Completed)),
        ];
        assert_eq!(unfocused(&mut harness, done.clone(), false).await, 1);
        assert!(
            String::from_utf8_lossy(&harness.recorder.notifications[0]).contains("done"),
            "{:?}",
            harness.recorder.notifications
        );

        let mut child = Harness::new();
        let elsewhere = vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, started("trn_9")),
            child_frame(3, completed("trn_9", TurnStatus::Completed)),
        ];
        assert_eq!(
            unfocused(&mut child, elsewhere, false).await,
            0,
            "a child finishing is the parent's business, not the desktop's"
        );
    }

    /// §6's budget: the kernel's own pace is not the screen's.
    #[tokio::test(start_paused = true)]
    async fn a_storm_of_deltas_costs_one_draw_a_frame_and_no_more() {
        let mut harness = Harness::new();
        let mut frames = vec![
            frame(1, started("trn_1")),
            frame(
                2,
                Event::ItemStarted {
                    item: assistant("itm_1", "", ItemStatus::Running),
                },
            ),
        ];
        // A thousand deltas a second, for a second.
        frames.extend((0..1_000).map(|i| {
            frame(
                3 + i,
                Event::ItemDelta {
                    item: bingo_sdk::ItemId::from_raw("itm_1"),
                    n: i as u32,
                    kind: bingo_sdk::DeltaKind::Text,
                    data: "x".into(),
                },
            )
        }));
        frames.push(closed(1_100));
        let (host, _) = TestHost::paced(frames, Duration::from_millis(1));
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(vec![]),
        )
        .await
        .expect("the loop ran");
        let drawn = harness.recorder.frames.len();
        assert!(
            (10..=40).contains(&drawn),
            "a second of storm is about thirty frames, not a thousand: {drawn}"
        );
        assert!(
            harness.recorder.last().contains(&"x".repeat(20)),
            "and the last of them is up to date: {}",
            harness.recorder.last()
        );
    }

    /// The same budget from the other side: a trackpad sends its notches far
    /// faster than the screen can draw them, and a terminal event is not a
    /// reason to paint out of turn. Where the notches take the transcript is
    /// `scroll.rs`'s own test; what this one watches is the cost.
    #[tokio::test(start_paused = true)]
    async fn a_storm_of_wheel_events_costs_one_draw_a_frame_and_no_more() {
        let mut harness = Harness::new();
        let frames: Vec<_> = (1..=30)
            .map(|i| {
                frame(
                    i,
                    Event::ItemCompleted {
                        item: assistant(
                            &format!("itm_{i}"),
                            &format!("answer {i}"),
                            ItemStatus::Completed,
                        ),
                    },
                )
            })
            .collect();
        let (host, _) = TestHost::with(frames);
        // A thousand notches a second, for a second, and then the way out.
        let mut script: Vec<Term> = (0..1_000)
            .map(|_| Term::Mouse(wheel(true, 10, 5)))
            .collect();
        script.push(Term::Key(ctrl('d')));
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            terminal_events(script, Duration::from_millis(1)),
        )
        .await
        .expect("the loop ran");
        let drawn = harness.recorder.frames.len();
        assert!(
            (10..=40).contains(&drawn),
            "a second of notches is about thirty frames, not a thousand: {drawn}"
        );
        assert!(
            !harness.recorder.last().contains("answer 30"),
            "and the transcript is somewhere else than the foot it started at: \
             {}",
            harness.recorder.last()
        );
    }

    #[tokio::test]
    async fn leaving_hands_the_last_screenful_of_the_transcript_back() {
        let mut harness = Harness::new();
        let frames: Vec<_> = (1..=30)
            .map(|i| {
                frame(
                    i,
                    Event::ItemCompleted {
                        item: assistant(
                            &format!("itm_{i}"),
                            &format!("answer {i}"),
                            ItemStatus::Completed,
                        ),
                    },
                )
            })
            .collect();
        let (host, _) = TestHost::with(frames);
        let ended = drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys(vec![ctrl('d')]),
        )
        .await
        .expect("the loop ran");
        assert_eq!(
            ended.screen.len(),
            22,
            "a 24-row terminal, less the two the shell wants back"
        );
        assert_eq!(
            ended.screen.last().map(String::as_str),
            Some("⏺ answer 30"),
            "{:?}",
            ended.screen
        );
        assert!(
            ended.screen.iter().all(|line| !line.contains('\u{1b}')),
            "plain text, through the block cache's own degrade"
        );
    }

    #[tokio::test(start_paused = true)]
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
        // Read back, take the first line, copy it. A key acts on the frame the
        // person pressing it is looking at, so each press gets one of its own:
        // the loop draws at most once a tick, and what the keys read — the
        // transcript's height, the rows it was given — is what that draw left.
        let script = vec![
            key(KeyCode::PageUp),
            typed('v'),
            key(KeyCode::Down),
            typed('y'),
            ctrl('d'),
        ];
        let (host, _) = TestHost::with(frames);
        drive(
            &host,
            options(None, harness.home.path()),
            &mut harness.recorder,
            keys_after(TICK + Duration::from_millis(5), script),
        )
        .await
        .expect("the loop ran");
        // What is inside the run is `select`'s to say; what the loop owes is
        // one OSC 52 sequence, out of band, carrying it as base64.
        let [copied] = harness.recorder.copies.as_slice() else {
            panic!("one sequence: {:?}", harness.recorder.copies)
        };
        assert!(copied.starts_with(b"\x1b]52;c;"), "{copied:?}");
        assert!(copied.ends_with(b"\x07"), "{copied:?}");
        let payload = &copied[7..copied.len() - 1];
        assert!(
            !payload.is_empty()
                && payload
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'='),
            "base64: {payload:?}"
        );
    }

    /// A run with nothing happening in it, whose one painted frame is `at`.
    fn idle(at: Instant) -> Run {
        idle_in(state(), std::sync::Arc::new(TestSession::default()), at)
    }

    /// `idle`, over a session of one's choosing — its directory is where a
    /// mentioned picture is looked for, and its handle is what is submitted.
    fn idle_in(state: SessionState, session: std::sync::Arc<TestSession>, at: Instant) -> Run {
        Run {
            stored: Stored::default(),
            host: TestHost::with(vec![]).0,
            data_dir: std::path::PathBuf::new(),
            session: Attached::new(state, SessionHandle(session)),
            ui: Ui::new(Vec::new(), at),
            mine: HashMap::new(),
            replies: mpsc::channel(1).0,
            clipboard: None,
            painted: at,
            behind: false,
            sluggish: false,
            exit: None,
        }
    }

    /// A line, the directory a mention is read from, and the lane the read
    /// comes back on — the loop's own, kept open here so a test waits for a
    /// picture exactly as the loop does.
    fn mentioning() -> (
        tempfile::TempDir,
        Run,
        std::sync::Arc<TestSession>,
        mpsc::Receiver<Reply>,
    ) {
        let dir = tempfile::tempdir().expect("a directory");
        let mut state = state();
        state.summary.cwd = dir.path().to_string_lossy().into_owned();
        let session = std::sync::Arc::new(TestSession::default());
        let mut run = idle_in(state, std::sync::Arc::clone(&session), Instant::now());
        let (replies, waiting) = mpsc::channel(4);
        run.replies = replies;
        // A submit appends to the prompt history; that belongs in the
        // scratch directory, not beside the crate's sources.
        run.data_dir = dir.path().to_path_buf();
        (dir, run, session, waiting)
    }

    /// The hop the loop makes: the pictures are read on a task and folded
    /// back in when they land.
    async fn settle(run: &mut Run, waiting: &mut mpsc::Receiver<Reply>) {
        let reply = waiting.recv().await.expect("the mentions came back");
        run.reply(reply, &mut None);
    }

    fn prose(text: &str) -> Input {
        Input::text(text, bingo_sdk::Origin::surface(SURFACE_ID))
    }

    /// The one picture a submitted line carried, whatever brought it.
    fn one_picture(session: &TestSession, line: &str) -> Image {
        let submitted = session.submitted();
        let [Input::Text { text, images, .. }] = submitted.as_slice() else {
            panic!("one submission: {submitted:?}");
        };
        assert_eq!(text, line, "the words go as typed");
        let [image] = images.as_slice() else {
            panic!("one picture: {images:?}");
        };
        image.clone()
    }

    #[tokio::test]
    async fn a_mentioned_picture_is_read_from_the_sessions_directory() {
        let (dir, mut run, session, mut waiting) = mentioning();
        std::fs::write(
            dir.path().join("shot.png"),
            bingo_pictures::testing::png_bytes(3, 4),
        )
        .expect("a picture");
        run.submit(prose("look at @shot.png"));
        settle(&mut run, &mut waiting).await;
        let image = one_picture(&session, "look at @shot.png");
        assert_eq!(image.media_type, "image/png");
        assert!(run.ui.notices.is_empty());
    }

    /// A format no provider takes is one a person still has: it is decoded
    /// here and journaled as PNG (ADR-0041 §2).
    #[tokio::test]
    async fn a_mentioned_bmp_reaches_the_session_as_a_png() {
        let (dir, mut run, session, mut waiting) = mentioning();
        std::fs::write(
            dir.path().join("shot.bmp"),
            bingo_pictures::testing::drawn(5, 6, bingo_pictures::testing::ImageFormat::Bmp),
        )
        .expect("a picture");
        run.submit(prose("@shot.bmp"));
        settle(&mut run, &mut waiting).await;
        assert_eq!(one_picture(&session, "@shot.bmp").media_type, "image/png");
    }

    /// A picture on the web is read by this machine and journaled as bytes;
    /// no provider is ever handed the URL (ADR-0041 §3).
    #[tokio::test]
    async fn a_mentioned_url_is_fetched_by_this_machine() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/y.jpg"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(
                bingo_pictures::testing::drawn(4, 4, bingo_pictures::testing::ImageFormat::Jpeg),
            ))
            .mount(&server)
            .await;
        let (_dir, mut run, session, mut waiting) = mentioning();
        let line = format!("look at @{}/y.jpg", server.uri());
        run.submit(prose(&line));
        settle(&mut run, &mut waiting).await;
        assert_eq!(one_picture(&session, &line).media_type, "image/jpeg");
        assert!(run.ui.notices.is_empty());
    }

    #[tokio::test]
    async fn a_mention_that_does_not_read_keeps_the_line_and_says_so() {
        let (_dir, mut run, session, mut waiting) = mentioning();
        run.submit(prose("look at @missing.png"));
        settle(&mut run, &mut waiting).await;
        assert!(session.submitted().is_empty(), "nothing was sent");
        assert_eq!(run.ui.composer.text(), "look at @missing.png");
        assert!(
            run.ui
                .notices
                .iter()
                .any(|n| n.text.starts_with("missing.png: ") && n.text.contains("could not")),
            "the notice names the mention and says why: {:?}",
            run.ui.notices
        );
    }

    /// A URL that answers with a web page is the one a person pastes by
    /// mistake; it must say so rather than hang on the body.
    #[tokio::test]
    async fn a_url_that_is_not_a_picture_keeps_the_line_and_says_so() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<!doctype html><html>a page</html>"),
            )
            .mount(&server)
            .await;
        let (_dir, mut run, session, mut waiting) = mentioning();
        let line = format!("@{}/page", server.uri());
        run.submit(prose(&line));
        settle(&mut run, &mut waiting).await;
        assert!(session.submitted().is_empty(), "nothing was sent");
        assert_eq!(run.ui.composer.text(), line);
        assert!(
            run.ui
                .notices
                .iter()
                .any(|n| n.text.contains("not a picture")),
            "{:?}",
            run.ui.notices
        );
    }

    /// A picture goes to the terminal once, however many frames draw it: the
    /// cells are redrawn from the block cache and the bytes behind them are
    /// already there (design §5).
    #[test]
    fn a_picture_is_handed_to_the_terminal_once_and_not_again() {
        let picture = bingo_pictures::testing::png(100, 200);
        let read = tool(
            "itm_1",
            "Read",
            serde_json::json!({ "file_path": "shot.png" }),
            Some(bingo_sdk::ToolOutput {
                parts: vec![bingo_sdk::ContentPart::Image(picture)],
                display: None,
                is_error: false,
            }),
            ItemStatus::Completed,
        );
        let at = Instant::now();
        let mut run = idle_in(
            folded(vec![frame(1, Event::ItemCompleted { item: read })]),
            std::sync::Arc::new(TestSession::default()),
            at,
        );
        let mut recorder = Recorder::default();
        let now = crate::test_support::scene().1;
        crate::graphics::with(crate::graphics::drawing(), || {
            run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
            run.paint(&mut recorder, Wake::Frame, now).expect("another");
        });
        let sent: Vec<String> = recorder
            .places
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect();
        assert_eq!(sent.len(), 1, "one write, not one per frame: {sent:?}");
        assert!(sent[0].starts_with("\x1b_Ga=T,f=100,q=2,U=1,"), "{sent:?}");
        assert!(sent[0].contains("c=10,r=10"), "{sent:?}");
    }

    /// The pictures behind the composer's line go to the terminal like any
    /// others — at the strip's own small size, and forgotten the moment their
    /// token leaves the line (M48 bricks 3 and 4).
    #[test]
    fn a_carried_picture_is_sent_small_and_forgotten_with_its_token() {
        use base64::Engine;
        let mut run = idle(Instant::now());
        let mut recorder = Recorder::default();
        let now = crate::test_support::scene().1;
        crate::graphics::with(crate::graphics::drawing(), || {
            let token = run
                .ui
                .pictures
                .hold("", bingo_pictures::testing::png(400, 300));
            run.ui.composer.insert(&pictures::placeholder(token));
            run.paint(&mut recorder, Wake::Frame, now).expect("a frame");
            // What a submit leaves behind: the line taken and the pictures
            // let go (`input::submit`, `Run::send_text`).
            run.ui.composer.take();
            run.ui.pictures.clear();
            run.paint(&mut recorder, Wake::Frame, now).expect("another");
        });
        let sent: Vec<String> = recorder
            .places
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect();
        assert_eq!(sent.len(), 2, "one send, one forget: {sent:?}");
        // 400×300 pixels of a 10×20 cell is 40 by 15 cells, and the strip's
        // three rows cut that to eight by three.
        assert!(sent[0].starts_with("\x1b_Ga=T,f=100,q=2,U=1,"), "{sent:?}");
        assert!(sent[0].contains("c=8,r=3"), "{sent:?}");
        let png = base64::engine::general_purpose::STANDARD
            .decode(
                sent[0]
                    .split_once(';')
                    .expect("a payload")
                    .1
                    .trim_end_matches("\x1b\\"),
            )
            .expect("base64");
        assert_eq!(
            bingo_pictures::png_size(&png),
            Some((80, 60)),
            "the pixels of eight cells by three, not the picture's own"
        );
        assert!(sent[1].contains("a=d,d=I"), "{sent:?}");
    }

    /// A terminal that draws no pictures is handed none, whatever the
    /// transcript holds.
    #[test]
    fn a_terminal_that_draws_no_pictures_is_handed_nothing() {
        let mut run = idle(Instant::now());
        let mut recorder = Recorder::default();
        run.paint(&mut recorder, Wake::Frame, crate::test_support::scene().1)
            .expect("a frame");
        assert!(recorder.places.is_empty());
    }

    #[test]
    fn a_selection_the_terminal_will_not_take_is_said_out_loud() {
        let mut run = idle(Instant::now());
        run.clipboard = Some("x".repeat(crate::select::LIMIT));
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

    /// A draw costs time, and the clock does not stop while it runs. So the
    /// question the loop asks between frames is about the frame that is *on
    /// the screen*, not about this instant: a sheet whose four frames all fell
    /// due during one slow draw is over by the time the loop looks, and the
    /// quarter of it that was painted would stay on the screen until the next
    /// keystroke.
    #[test]
    fn a_frame_is_owed_while_the_one_on_the_screen_is_still_arriving() {
        let (_, opened) = scene();
        let mut run = idle(opened.instant);
        run.ui.layer.show(Open::Help, opened.instant);
        let after = later(opened, 200);
        assert!(
            run.animating(after),
            "the sheet on the screen is a quarter of the way up: the rest is owed"
        );
        run.painted = after.instant;
        assert!(
            !run.animating(after),
            "and once the whole of it has been painted, the loop may sleep"
        );
    }

    #[test]
    fn a_slow_draw_is_owned_up_to_once_and_a_quick_one_never() {
        assert_eq!(slow_draw(Duration::from_millis(3), false), None);
        let said = slow_draw(SLOW_DRAW, false).expect("several frames is worth a word");
        assert!(said.contains("ms"), "{said}");
        assert_eq!(slow_draw(Duration::from_secs(1), true), None, "said once");
    }
}
