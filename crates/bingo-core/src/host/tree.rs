//! A tree attachment (ADR-0010 §3): the root's frames and every descendant's
//! on one stream, and a port that answers an interaction wherever in the tree
//! it was opened.
//!
//! A tree has two authorities. The descendants this host runs are followed
//! live; the ones only the store knows — a resume revives the root alone —
//! are replayed from their journals onto the same stream, read-only, so a
//! client folds the whole tree from frames whether or not this process runs
//! it. A replayed session that later wakes here is followed on from where its
//! replay stopped.
//!
//! The forwarder is the one subscriber of each session in the tree; the
//! client reads a channel the forwarder fills. A session whose stream lags is
//! re-subscribed from the last frame forwarded, so this stream never carries
//! a `Lagged` marker and each session's `seq` stays contiguous for the
//! client; what a lag loses is what it loses everywhere — the deltas and
//! notices the journal never keeps.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use futures::stream::{self, SelectAll, Stream, StreamExt};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

use super::Host;
use crate::session::{Mailbox, SUBSCRIBER_CAPACITY};

/// Which session opened each interaction the client has seen open.
type Owners = Arc<Mutex<HashMap<InteractionId, Mailbox>>>;

/// What one followed stream yields: a frame, or the end of that session's stream.
enum Tagged {
    Frame(Box<Frame>),
    End(SessionId),
}

type TaggedStream = Pin<Box<dyn Stream<Item = Tagged> + Send>>;

fn tagged(session: SessionId, frames: FrameStream) -> TaggedStream {
    let end = stream::once(async move { Tagged::End(session) });
    Box::pin(frames.map(|f| Tagged::Frame(Box::new(f))).chain(end))
}

pub(super) async fn attach(
    host: Weak<Host>,
    gateway: &broadcast::Sender<GatewayEvent>,
    root: Mailbox,
    who: ClientIdentity,
) -> Result<Attachment, KernelError> {
    // Subscribed before the descendants are listed, so a creation cannot fall between.
    let gateway = gateway.subscribe();
    let (snapshot, events) = root.attach().await?;
    let owners: Owners = Arc::default();
    let (out, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
    let mut forwarder = Forwarder {
        host,
        root: root.id().clone(),
        followed: HashMap::new(),
        streams: SelectAll::new(),
        owners: Arc::clone(&owners),
        gateway: Some(gateway),
        out,
    };
    forwarder.follow_live(root.clone(), events);
    forwarder.adopt_descendants().await;
    tokio::spawn(forwarder.run());
    let events = stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|f| (f, rx)) });
    Ok(Attachment {
        session: root.id().clone(),
        snapshot,
        events: Box::pin(events),
        handle: SessionHandle(Arc::new(TreePort {
            root: root.port(who.clone()),
            owners,
            who,
        })),
    })
}

struct Forwarder {
    host: Weak<Host>,
    root: SessionId,
    /// Every session followed, root included.
    followed: HashMap<SessionId, Followed>,
    streams: SelectAll<TaggedStream>,
    owners: Owners,
    /// `None` once the host is gone; the streams end on their own then.
    gateway: Option<broadcast::Receiver<GatewayEvent>>,
    out: mpsc::Sender<Frame>,
}

/// One session this attachment carries.
struct Followed {
    /// The last `seq` forwarded, where a healed or adopted stream resumes;
    /// `ZERO` until the first frame goes out.
    last: Seq,
    source: Source,
}

/// Where a followed session's frames come from.
enum Source {
    /// It runs on this host: its mailbox heals a lag and owns its
    /// interactions.
    Live(Mailbox),
    /// A stored journal still draining onto the stream. Nothing is followed
    /// over it: the live stream would repeat what it has not reached yet.
    Replaying,
    /// Nothing is arriving from it — it never ran here, or it stopped. Its
    /// `last` is kept, so a session that comes back is followed on from
    /// there rather than from its head.
    Absent,
}

impl Forwarder {
    async fn run(mut self) {
        loop {
            tokio::select! {
                next = self.streams.next() => match next {
                    Some(Tagged::Frame(frame)) => {
                        if !self.forward(*frame).await {
                            return;
                        }
                    }
                    Some(Tagged::End(session)) => {
                        if self.ended(session).await {
                            return;
                        }
                    }
                    None => return,
                },
                event = gateway_next(&mut self.gateway) => self.gateway_event(event).await,
            }
        }
    }

    /// Put one session's frames on the client's stream, keeping wherever an
    /// earlier stream of the same session left off.
    fn follow(&mut self, id: SessionId, frames: FrameStream, source: Source) {
        let last = self.since(&id);
        self.followed.insert(id.clone(), Followed { last, source });
        self.streams.push(tagged(id, frames));
    }

    fn follow_live(&mut self, mailbox: Mailbox, frames: FrameStream) {
        let id = mailbox.id().clone();
        self.follow(id, frames, Source::Live(mailbox));
    }

    /// Where this attachment left a session: `events_since` and the store's
    /// `replay` both give what is *after* it, so a stream resumed here
    /// repeats nothing.
    fn since(&self, session: &SessionId) -> Seq {
        self.followed.get(session).map_or(Seq::ZERO, |f| f.last)
    }

    fn mailbox(&self, session: &SessionId) -> Option<Mailbox> {
        match self.followed.get(session).map(|f| &f.source) {
            Some(Source::Live(mailbox)) => Some(mailbox.clone()),
            _ => None,
        }
    }

    /// Whether this session's frames are already on their way: it runs here,
    /// or a replay of its journal is still draining.
    fn arriving(&self, session: &SessionId) -> bool {
        matches!(
            self.followed.get(session).map(|f| &f.source),
            Some(Source::Live(_) | Source::Replaying)
        )
    }

    /// Returns whether the client is still there.
    async fn forward(&mut self, frame: Frame) -> bool {
        if matches!(frame.event, Event::Lagged { .. }) {
            self.heal(frame.session).await;
            return true;
        }
        if let Some(followed) = self.followed.get_mut(&frame.session) {
            followed.last = frame.seq;
        }
        self.note_interaction(&frame);
        self.out.send(frame).await.is_ok()
    }

    /// A lagged stream ended at its marker; resume it from the journal.
    async fn heal(&mut self, session: SessionId) {
        let Some(mailbox) = self.mailbox(&session) else {
            return;
        };
        let since = self.since(&session);
        if let Ok(frames) = mailbox.events_since(since).await {
            self.follow(session, frames, Source::Live(mailbox));
        }
    }

    /// Returns whether the whole attachment is over.
    async fn ended(&mut self, session: SessionId) -> bool {
        if session == self.root {
            return true;
        }
        if self.replaying(&session) {
            self.replayed(&session).await;
        } else if !self.lives(&session) {
            // A healed stream's old half ends while its session runs on;
            // this one does not, and nothing more is coming from it.
            self.absent(&session);
        }
        false
    }

    fn replaying(&self, session: &SessionId) -> bool {
        matches!(
            self.followed.get(session).map(|f| &f.source),
            Some(Source::Replaying)
        )
    }

    /// A stored journal is out whole. If the session has woken on this host
    /// meanwhile, its own stream carries on from where the replay stopped.
    async fn replayed(&mut self, session: &SessionId) {
        self.absent(session);
        self.adopt(session).await;
    }

    fn absent(&mut self, session: &SessionId) {
        if let Some(followed) = self.followed.get_mut(session) {
            followed.source = Source::Absent;
        }
    }

    fn lives(&self, session: &SessionId) -> bool {
        self.host
            .upgrade()
            .is_some_and(|host| host.live(session).is_ok())
    }

    fn note_interaction(&self, frame: &Frame) {
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        match &frame.event {
            Event::InteractionOpened { interaction } => {
                if let Some(mailbox) = self.mailbox(&frame.session) {
                    owners.insert(interaction.id.clone(), mailbox);
                }
            }
            Event::InteractionResolved { id, .. } | Event::InteractionCancelled { id, .. } => {
                owners.remove(id);
            }
            _ => {}
        }
    }

    async fn gateway_event(&mut self, event: Result<GatewayEvent, RecvError>) {
        match event {
            Ok(GatewayEvent::SessionCreated { summary }) => {
                let under_tree = summary
                    .parent
                    .as_ref()
                    .is_some_and(|p| self.followed.contains_key(&p.session));
                if under_tree {
                    self.adopt(&summary.id).await;
                }
            }
            Ok(_) => {}
            // Something may have been created unseen; the lists say what.
            Err(RecvError::Lagged(_)) => self.adopt_descendants().await,
            Err(RecvError::Closed) => self.gateway = None,
        }
    }

    /// Both authorities on what this tree holds: the sessions this host runs,
    /// then the ones only the store knows.
    async fn adopt_descendants(&mut self) {
        if let Some(host) = self.host.upgrade() {
            for (id, _) in host.descendants(&self.root) {
                self.adopt(&id).await;
            }
        }
        self.adopt_stored().await;
    }

    /// Follow a session that runs here, from its head or from the tail of a
    /// replay already forwarded.
    async fn adopt(&mut self, id: &SessionId) {
        if self.arriving(id) {
            return;
        }
        let Some(host) = self.host.upgrade() else {
            return;
        };
        let Ok(live) = host.live(id) else {
            return;
        };
        let since = self.since(id);
        if let Ok(frames) = live.mailbox.events_since(since).await {
            self.follow_live(live.mailbox, frames);
        }
    }

    /// Every descendant the store knows of, breadth-first. One this host runs
    /// is followed live; the rest are replayed, so a resume that revived the
    /// root alone still shows the client a whole tree.
    async fn adopt_stored(&mut self) {
        let Some(store) = self.store() else {
            return;
        };
        let mut seen = HashSet::from([self.root.clone()]);
        let mut frontier = vec![self.root.clone()];
        while let Some(parent) = frontier.pop() {
            for id in stored_children(store.as_ref(), &parent).await {
                if !seen.insert(id.clone()) {
                    continue;
                }
                frontier.push(id.clone());
                self.adopt(&id).await;
                self.replay(store.as_ref(), &id).await;
            }
        }
    }

    /// A descendant this host does not run: its journal, from wherever this
    /// attachment left it. The journal is read, not held — a replay acquires
    /// nothing, so whichever process owns the session keeps it, and what the
    /// client is given here can be folded but not written back.
    async fn replay(&mut self, store: &dyn SessionStore, id: &SessionId) {
        if self.arriving(id) {
            return;
        }
        let Ok(frames) = store.replay(id, self.since(id)).await else {
            return;
        };
        self.follow(
            id.clone(),
            Box::pin(stream::iter(frames)),
            Source::Replaying,
        );
    }

    fn store(&self) -> Option<Arc<dyn SessionStore>> {
        self.host.upgrade()?.registry.store.clone()
    }
}

async fn stored_children(store: &dyn SessionStore, parent: &SessionId) -> Vec<SessionId> {
    let filter = SessionFilter {
        parent: Some(parent.clone()),
        ..SessionFilter::default()
    };
    match store.list(&filter).await {
        Ok(children) => children.into_iter().map(|s| s.id).collect(),
        Err(error) => {
            tracing::debug!(%error, %parent, "the store would not list a tree's children");
            Vec::new()
        }
    }
}

/// The next gateway event, or never once the host is gone.
async fn gateway_next(
    gateway: &mut Option<broadcast::Receiver<GatewayEvent>>,
) -> Result<GatewayEvent, RecvError> {
    match gateway.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The root's port, except that an answer goes to the session that asked.
struct TreePort {
    root: SessionHandle,
    owners: Owners,
    who: ClientIdentity,
}

#[async_trait]
impl SessionPort for TreePort {
    fn submit(&self, intent: IntentId, input: Input) {
        self.root.submit(intent, input);
    }

    fn interrupt(&self, intent: IntentId, scope: InterruptScope) {
        self.root.interrupt(intent, scope);
    }

    fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        let owner = self
            .owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&interaction)
            .cloned();
        match owner {
            Some(mailbox) => {
                mailbox.answer(intent, interaction, answer, activation, self.who.clone())
            }
            None => self.root.answer(intent, interaction, answer, activation),
        }
    }

    async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        self.root.history(page).await
    }

    async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        self.root.events_since(since).await
    }
}
