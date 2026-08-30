//! The actor's address and the two views on it: the client port
//! (`SessionPort`) and the turn loop's host (`TurnHost`). Writes are
//! synchronous and never block; reads are oneshot round trips.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::*;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::turn::{TurnConfig, TurnHost, TurnOutcome};

pub(crate) enum Msg {
    Submit {
        intent: IntentId,
        input: Input,
    },
    /// A peer's prose (ADR-0010 §1): no command parse, no submit hooks.
    Deliver {
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    },
    /// A plugin's state, whole, into the journal (ADR-0011 §2).
    Extend {
        plugin: String,
        kind: String,
        payload: Value,
    },
    Interrupt {
        intent: IntentId,
        scope: InterruptScope,
    },
    Answer(Answered),
    Attach {
        reply: oneshot::Sender<(SessionState, FrameStream)>,
    },
    EventsSince {
        since: Seq,
        reply: oneshot::Sender<FrameStream>,
    },
    History {
        page: HistoryPage,
        reply: oneshot::Sender<HistoryChunk>,
    },
    Summary {
        reply: oneshot::Sender<SessionSummary>,
    },
    Emit {
        turn: TurnId,
        event: Box<Event>,
    },
    Ask {
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
        reply: oneshot::Sender<Result<Answer, KernelError>>,
    },
    Absorb {
        turn: TurnId,
        reply: oneshot::Sender<Vec<(IntentId, Input)>>,
    },
    TurnFinished {
        turn: TurnId,
        outcome: Result<TurnOutcome, String>,
    },
    Record {
        body: ItemBody,
        reply: oneshot::Sender<ItemId>,
    },
    Progress {
        item: ItemId,
        tail: String,
    },
    CommandFinished {
        intent: IntentId,
        outcome: Result<CommandOutcome, KernelError>,
    },
    /// The host rebuilt the turn config; the next turn runs on it.
    Reconfigure {
        config: Arc<TurnConfig>,
    },
    /// A turn that only compacts (ADR-0008 §4).
    Compact {
        instructions: Option<String>,
        reply: oneshot::Sender<Result<(), KernelError>>,
    },
    Close {
        reason: CloseReason,
    },
}

impl Msg {
    /// The one message answered while the session is still starting: the
    /// host reads every live actor's summary to list the tree, and a start
    /// hook may list the tree. Everything else — attachments included, so a
    /// snapshot never runs ahead of a write sent before it — waits.
    pub(crate) fn reads(&self) -> bool {
        matches!(self, Msg::Summary { .. })
    }
}

/// A client's answer to an open interaction, with who gave it.
pub(crate) struct Answered {
    pub intent: IntentId,
    pub interaction: InteractionId,
    pub answer: Answer,
    pub activation: Activation,
    pub who: ClientIdentity,
}

/// The actor's address. Cheap to clone; every write is synchronous and
/// never blocks, every read is a oneshot round trip.
#[derive(Clone)]
pub struct Mailbox {
    id: SessionId,
    tx: mpsc::UnboundedSender<Msg>,
    /// `true` once the actor has stopped and its post-turn work is done.
    done: watch::Receiver<bool>,
}

impl Mailbox {
    pub(super) fn new(
        id: SessionId,
        tx: mpsc::UnboundedSender<Msg>,
        done: watch::Receiver<bool>,
    ) -> Self {
        Self { id, tx, done }
    }
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mailbox({})", self.id)
    }
}

fn gone() -> KernelError {
    KernelError::new(ErrorCode::SessionClosed, "the session actor is gone")
}

impl Mailbox {
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub(super) fn send(&self, msg: Msg) {
        // A closed actor drops writes; the reply channel, where there is one,
        // reports it. Fire-and-forget writes have `IntentAck` for that.
        let _ = self.tx.send(msg);
    }

    pub(super) async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Msg,
    ) -> Result<T, KernelError> {
        let (tx, rx) = oneshot::channel();
        self.send(make(tx));
        rx.await.map_err(|_| gone())
    }

    pub fn submit(&self, intent: IntentId, input: Input) {
        self.send(Msg::Submit { intent, input });
    }

    pub fn deliver(&self, intent: IntentId, input: Input, delivery: Delivery) {
        self.send(Msg::Deliver {
            intent,
            input,
            delivery,
        });
    }

    pub fn extend(&self, plugin: String, kind: String, payload: Value) {
        self.send(Msg::Extend {
            plugin,
            kind,
            payload,
        });
    }

    pub fn interrupt(&self, intent: IntentId, scope: InterruptScope) {
        self.send(Msg::Interrupt { intent, scope });
    }

    pub fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
        who: ClientIdentity,
    ) {
        self.send(Msg::Answer(Answered {
            intent,
            interaction,
            answer,
            activation,
            who,
        }));
    }

    /// A snapshot and every frame after it.
    pub async fn attach(&self) -> Result<(SessionState, FrameStream), KernelError> {
        self.call(|reply| Msg::Attach { reply }).await
    }

    pub async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        self.call(|reply| Msg::EventsSince { since, reply }).await
    }

    pub async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        self.call(|reply| Msg::History { page, reply }).await
    }

    pub async fn summary(&self) -> Result<SessionSummary, KernelError> {
        self.call(|reply| Msg::Summary { reply }).await
    }

    /// Write one completed item outside the turn loop (a background result).
    pub async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError> {
        self.call(|reply| Msg::Record { body, reply }).await
    }

    pub fn progress(&self, item: ItemId, tail: String) {
        self.send(Msg::Progress { item, tail });
    }

    /// Open an interaction on the running turn and wait for its answer.
    pub async fn ask(
        &self,
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.call(|reply| Msg::Ask {
            item,
            kind,
            answers,
            reply,
        })
        .await?
    }

    pub fn close(&self, reason: CloseReason) {
        self.send(Msg::Close { reason });
    }

    /// Resolves once the actor has stopped, its post-turn work included.
    pub async fn wait_closed(&self) {
        let mut done = self.done.clone();
        let _ = done.wait_for(|d| *d).await;
    }

    pub fn reconfigure(&self, config: Arc<TurnConfig>) {
        self.send(Msg::Reconfigure { config });
    }

    /// Open a turn that only compacts; refused while a turn runs.
    pub async fn compact(&self, instructions: Option<String>) -> Result<(), KernelError> {
        self.call(|reply| Msg::Compact {
            instructions,
            reply,
        })
        .await?
    }

    /// The client-facing port, stamped with who is holding it.
    pub fn port(&self, who: ClientIdentity) -> SessionHandle {
        SessionHandle(Arc::new(Port {
            mailbox: self.clone(),
            who,
        }))
    }
}

struct Port {
    mailbox: Mailbox,
    who: ClientIdentity,
}

#[async_trait]
impl SessionPort for Port {
    fn submit(&self, intent: IntentId, input: Input) {
        self.mailbox.submit(intent, input);
    }

    fn interrupt(&self, intent: IntentId, scope: InterruptScope) {
        self.mailbox.interrupt(intent, scope);
    }

    fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        self.mailbox
            .answer(intent, interaction, answer, activation, self.who.clone());
    }

    async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        self.mailbox.history(page).await
    }

    async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        self.mailbox.events_since(since).await
    }
}

/// The turn loop's view of the actor: publish, ask, absorb — all by mail.
pub(super) struct TurnMail {
    pub(super) mailbox: Mailbox,
    pub(super) turn: TurnId,
}

#[async_trait]
impl TurnHost for TurnMail {
    fn emit(&self, event: Event) {
        self.mailbox.send(Msg::Emit {
            turn: self.turn.clone(),
            event: Box::new(event),
        });
    }

    async fn ask(
        &self,
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.mailbox.ask(item, kind, answers).await
    }

    async fn absorb(&self) -> Vec<(IntentId, Input)> {
        let turn = self.turn.clone();
        self.mailbox
            .call(|reply| Msg::Absorb { turn, reply })
            .await
            .unwrap_or_default()
    }
}
