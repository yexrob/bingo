//! A tree attachment (ADR-0010 §3): the root's frames and every live
//! descendant's on one stream, and a port that answers an interaction
//! wherever in the tree it was opened.
//!
//! The forwarder is the one subscriber of each session in the tree; the
//! client reads a channel the forwarder fills. A session whose stream lags is
//! re-subscribed from the last frame forwarded, so this stream never carries
//! a `Lagged` marker and each session's `seq` stays contiguous for the
//! client; what a lag loses is what it loses everywhere — the deltas and
//! notices the journal never keeps.

use std::collections::HashMap;
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
        sessions: HashMap::new(),
        last: HashMap::new(),
        streams: SelectAll::new(),
        owners: Arc::clone(&owners),
        gateway: Some(gateway),
        out,
    };
    forwarder.follow(root.clone(), events);
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
    sessions: HashMap<SessionId, Mailbox>,
    /// The last `seq` forwarded per session, where a healed stream resumes.
    last: HashMap<SessionId, Seq>,
    streams: SelectAll<TaggedStream>,
    owners: Owners,
    /// `None` once the host is gone; the streams end on their own then.
    gateway: Option<broadcast::Receiver<GatewayEvent>>,
    out: mpsc::Sender<Frame>,
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
                        if self.ended(session) {
                            return;
                        }
                    }
                    None => return,
                },
                event = gateway_next(&mut self.gateway) => self.gateway_event(event).await,
            }
        }
    }

    fn follow(&mut self, mailbox: Mailbox, frames: FrameStream) {
        let id = mailbox.id().clone();
        self.sessions.insert(id.clone(), mailbox);
        self.streams.push(tagged(id, frames));
    }

    /// Returns whether the client is still there.
    async fn forward(&mut self, frame: Frame) -> bool {
        if matches!(frame.event, Event::Lagged { .. }) {
            self.heal(frame.session).await;
            return true;
        }
        self.last.insert(frame.session.clone(), frame.seq);
        self.note_interaction(&frame);
        self.out.send(frame).await.is_ok()
    }

    /// A lagged stream ended at its marker; resume it from the journal.
    async fn heal(&mut self, session: SessionId) {
        let Some(mailbox) = self.sessions.get(&session).cloned() else {
            return;
        };
        let since = self.last.get(&session).copied().unwrap_or(Seq::ZERO);
        if let Ok(frames) = mailbox.events_since(since).await {
            self.streams.push(tagged(session, frames));
        }
    }

    /// Returns whether the whole attachment is over.
    fn ended(&mut self, session: SessionId) -> bool {
        if session == self.root {
            return true;
        }
        // A healed stream's old half ends too; only a session that is gone
        // is forgotten.
        let live = self
            .host
            .upgrade()
            .is_some_and(|host| host.live(&session).is_ok());
        if !live {
            self.sessions.remove(&session);
            self.last.remove(&session);
        }
        false
    }

    fn note_interaction(&self, frame: &Frame) {
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        match &frame.event {
            Event::InteractionOpened { interaction } => {
                if let Some(mailbox) = self.sessions.get(&frame.session) {
                    owners.insert(interaction.id.clone(), mailbox.clone());
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
                    .is_some_and(|p| self.sessions.contains_key(&p.session));
                if under_tree {
                    self.adopt(&summary.id).await;
                }
            }
            Ok(_) => {}
            // Something may have been created unseen; the list says what.
            Err(RecvError::Lagged(_)) => self.adopt_descendants().await,
            Err(RecvError::Closed) => self.gateway = None,
        }
    }

    async fn adopt_descendants(&mut self) {
        let Some(host) = self.host.upgrade() else {
            return;
        };
        for (id, _) in host.descendants(&self.root) {
            self.adopt(&id).await;
        }
    }

    /// Follow a session from its head; a client folds it from nothing.
    async fn adopt(&mut self, id: &SessionId) {
        if self.sessions.contains_key(id) {
            return;
        }
        let Some(host) = self.host.upgrade() else {
            return;
        };
        let Ok(live) = host.live(id) else {
            return;
        };
        if let Ok(frames) = live.mailbox.events_since(Seq::ZERO).await {
            self.follow(live.mailbox, frames);
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
