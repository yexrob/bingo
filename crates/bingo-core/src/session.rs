//! The session actor: one mailbox, one journal, one snapshot, one running
//! turn. Every producer — clients, the turn loop, tools — sends a `Msg`; the
//! actor mints `seq`, persists durable frames, folds them with the one reducer
//! and fans them out to bounded subscriber channels. It never awaits a client.

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::*;
use futures::{FutureExt, StreamExt};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::gate::hook_applies;
use crate::turn::{TurnConfig, TurnHost, TurnOutcome, TurnRun, run_turn};

/// Frames a subscriber may fall behind by before it is told to resync.
pub const SUBSCRIBER_CAPACITY: usize = 256;

/// A keyboard answer inside this window after opening is a stray keystroke.
pub const INTERACTION_GUARD_MS: i64 = 400;

/// Characters of a queued input shown in `QueueChanged`.
const PREVIEW_CHARS: usize = 80;

pub(crate) enum Msg {
    Submit {
        intent: IntentId,
        input: Input,
    },
    Interrupt {
        intent: IntentId,
        scope: InterruptScope,
    },
    Answer {
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
        who: ClientIdentity,
    },
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
    Close {
        reason: CloseReason,
    },
}

/// The actor's address. Cheap to clone; every write is synchronous and
/// never blocks, every read is a oneshot round trip.
#[derive(Clone)]
pub struct Mailbox {
    id: SessionId,
    tx: mpsc::UnboundedSender<Msg>,
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

    fn send(&self, msg: Msg) {
        // A closed actor drops writes; the reply channel, where there is one,
        // reports it. Fire-and-forget writes have `IntentAck` for that.
        let _ = self.tx.send(msg);
    }

    async fn call<T>(
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
        self.send(Msg::Answer {
            intent,
            interaction,
            answer,
            activation,
            who,
        });
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
struct TurnMail {
    mailbox: Mailbox,
    turn: TurnId,
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

/// Start an actor. The turn config is built second because its tool host
/// talks back through the mailbox.
pub fn spawn(
    summary: SessionSummary,
    store: Option<Arc<dyn SessionStore>>,
    config: impl FnOnce(&Mailbox) -> Arc<TurnConfig>,
) -> Mailbox {
    let (tx, rx) = mpsc::unbounded_channel();
    let mailbox = Mailbox {
        id: summary.id.clone(),
        tx,
    };
    let config = config(&mailbox);
    let actor = Actor {
        id: summary.id.clone(),
        mailbox: mailbox.clone(),
        rx,
        state: SessionState::new(summary.clone()),
        journal: Vec::new(),
        seq: Seq::ZERO,
        store,
        config,
        subscribers: Vec::new(),
        running: None,
        queue: VecDeque::new(),
        queue_revision: 0,
        pending: HashMap::new(),
        generation: 0,
        closing: None,
        progress_n: 0,
    };
    tokio::spawn(actor.run(summary));
    mailbox
}

/// The gap a subscriber fell behind by: first and last missed seq. Shared
/// with its stream, which turns it into the `Lagged` marker once the frames
/// it did get are drained — and then ends, so the client has to resync.
type Lag = Arc<Mutex<Option<(Seq, Seq)>>>;

struct Subscriber {
    tx: mpsc::Sender<Frame>,
    lag: Lag,
}

struct Running {
    turn: TurnId,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

struct Pending {
    interaction: Interaction,
    reply: oneshot::Sender<Result<Answer, KernelError>>,
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

struct Actor {
    id: SessionId,
    mailbox: Mailbox,
    rx: mpsc::UnboundedReceiver<Msg>,
    /// The kernel's own view, maintained by the one reducer.
    state: SessionState,
    /// Durable frames, in seq order.
    journal: Vec<Frame>,
    seq: Seq,
    store: Option<Arc<dyn SessionStore>>,
    config: Arc<TurnConfig>,
    subscribers: Vec<Subscriber>,
    running: Option<Running>,
    queue: VecDeque<(IntentId, Input)>,
    queue_revision: u64,
    pending: HashMap<InteractionId, Pending>,
    generation: u64,
    /// A close that waits for the running turn to wind down.
    closing: Option<CloseReason>,
    progress_n: u32,
}

impl Actor {
    async fn run(mut self, summary: SessionSummary) {
        // The journal head: what this session is.
        self.publish(Event::SessionUpdated { summary }, None).await;
        while let Some(msg) = self.rx.recv().await {
            if self.handle(msg).await == Flow::Stop {
                break;
            }
        }
        if let Some(running) = self.running.take() {
            running.cancel.cancel();
            running.task.abort();
        }
    }

    async fn handle(&mut self, msg: Msg) -> Flow {
        match msg {
            Msg::Submit { intent, input } => self.submit(intent, input).await,
            Msg::Interrupt { intent, scope } => self.interrupt(intent, scope).await,
            Msg::Answer {
                intent,
                interaction,
                answer,
                activation,
                who,
            } => {
                self.answer(intent, interaction, answer, activation, who)
                    .await
            }
            Msg::Attach { reply } => {
                let snapshot = self.state.clone();
                let stream = self.subscribe(snapshot.seq);
                let _ = reply.send((snapshot, stream));
            }
            Msg::EventsSince { since, reply } => {
                let _ = reply.send(self.subscribe(since));
            }
            Msg::History { page, reply } => {
                let _ = reply.send(self.history(&page));
            }
            Msg::Summary { reply } => {
                let _ = reply.send(self.state.summary.clone());
            }
            Msg::Emit { turn, event } => {
                if self.is_running(&turn) {
                    self.publish(*event, None).await;
                } else {
                    tracing::warn!(session = %self.id, %turn, "event from a turn that is not running");
                }
            }
            Msg::Ask {
                item,
                kind,
                answers,
                reply,
            } => self.open_interaction(item, kind, answers, reply).await,
            Msg::Absorb { turn, reply } => {
                let taken = if self.is_running(&turn) {
                    self.take_queue().await
                } else {
                    Vec::new()
                };
                let _ = reply.send(taken);
            }
            Msg::TurnFinished { turn, outcome } => return self.turn_finished(turn, outcome).await,
            Msg::Record { body, reply } => {
                let id = self.record(body).await;
                let _ = reply.send(id);
            }
            Msg::Progress { item, tail } => {
                self.progress_n += 1;
                self.publish(
                    Event::ItemDelta {
                        item,
                        n: self.progress_n,
                        kind: DeltaKind::Tail,
                        data: tail,
                    },
                    None,
                )
                .await;
            }
            Msg::Close { reason } => return self.close(reason).await,
        }
        Flow::Continue
    }

    fn is_running(&self, turn: &TurnId) -> bool {
        self.running.as_ref().is_some_and(|r| &r.turn == turn)
    }

    // ----- publishing -----

    async fn publish(&mut self, event: Event, cause: Option<IntentId>) -> Seq {
        self.seq = self.seq.next();
        let frame = Frame {
            seq: self.seq,
            ts: Timestamp::now(),
            session: self.id.clone(),
            cause,
            event,
        };
        if frame.event.is_durable() {
            if let Some(store) = &self.store
                && let Err(e) = store.append(&self.id, &frame).await
            {
                tracing::error!(session = %self.id, error = %e, "journal append failed");
            }
            self.journal.push(frame.clone());
        }
        self.state.apply(&frame);
        self.fanout(&frame);
        frame.seq
    }

    fn fanout(&mut self, frame: &Frame) {
        self.subscribers.retain_mut(|s| {
            let mut lag = s.lag.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, to)) = lag.as_mut() {
                // Already behind: the stream ends at the marker, so nothing
                // after the gap is worth queueing.
                *to = frame.seq;
                return !s.tx.is_closed();
            }
            match s.tx.try_send(frame.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    *lag = Some((frame.seq, frame.seq));
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }

    fn subscribe(&mut self, since: Seq) -> FrameStream {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        let lag: Lag = Arc::new(Mutex::new(None));
        self.subscribers.push(Subscriber {
            tx,
            lag: Arc::clone(&lag),
        });
        let replay: Vec<Frame> = self
            .journal
            .iter()
            .filter(|f| f.seq > since)
            .cloned()
            .collect();
        let session = self.id.clone();
        let live = futures::stream::unfold(Some((rx, lag)), move |slot| {
            let session = session.clone();
            async move {
                let (mut rx, lag) = slot?;
                match rx.try_recv() {
                    Ok(frame) => return Some((frame, Some((rx, lag)))),
                    Err(mpsc::error::TryRecvError::Disconnected) => return None,
                    Err(mpsc::error::TryRecvError::Empty) => {}
                }
                let gap = lag.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some((from, to)) = gap {
                    let marker = Frame {
                        seq: to,
                        ts: Timestamp::now(),
                        session,
                        cause: None,
                        event: Event::Lagged { from, to },
                    };
                    // Dropping the receiver ends the subscription on the
                    // actor's side too.
                    return Some((marker, None));
                }
                rx.recv().await.map(|frame| (frame, Some((rx, lag))))
            }
        });
        Box::pin(futures::stream::iter(replay).chain(live))
    }

    async fn reject(&mut self, intent: IntentId, code: ErrorCode, message: impl Into<String>) {
        self.publish(
            Event::IntentAck {
                intent: intent.clone(),
                outcome: IntentOutcome::Rejected {
                    error: KernelError::new(code, message),
                },
            },
            Some(intent),
        )
        .await;
    }

    async fn applied(&mut self, intent: IntentId, result: Value) {
        self.publish(
            Event::IntentAck {
                intent: intent.clone(),
                outcome: IntentOutcome::Applied { result },
            },
            Some(intent),
        )
        .await;
    }

    fn fresh(&self, turn: Option<TurnId>, intent: Option<IntentId>, body: ItemBody) -> Item {
        let now = Timestamp::now();
        Item {
            id: ItemId::mint(),
            turn,
            round: 0,
            status: ItemStatus::Completed,
            started_at: now,
            completed_at: Some(now),
            intent,
            body,
            meta: Default::default(),
        }
    }

    async fn record(&mut self, body: ItemBody) -> ItemId {
        let turn = self.running.as_ref().map(|r| r.turn.clone());
        let item = self.fresh(turn, None, body);
        let id = item.id.clone();
        self.publish(Event::ItemCompleted { item }, None).await;
        id
    }

    fn history(&self, page: &HistoryPage) -> HistoryChunk {
        let items = &self.state.items;
        let end = page
            .before
            .as_ref()
            .and_then(|b| items.iter().position(|i| &i.id == b))
            .unwrap_or(items.len());
        let start = if page.limit == 0 {
            0
        } else {
            end.saturating_sub(page.limit)
        };
        let slice = &items[start..end];
        HistoryChunk {
            items: slice.to_vec(),
            next: (start > 0)
                .then(|| slice.first().map(|i| i.id.clone()))
                .flatten(),
            generation: self.generation,
        }
    }

    // ----- submissions -----

    async fn submit(&mut self, intent: IntentId, input: Input) {
        if self.state.closed || self.closing.is_some() {
            return self
                .reject(intent, ErrorCode::SessionClosed, "the session is closed")
                .await;
        }
        let mut input = input;
        if let Err(message) = validate(&input) {
            return self.reject(intent, ErrorCode::InvalidInput, message).await;
        }
        let cx = HookContext {
            session: self.id.clone(),
            turn: self.running.as_ref().map(|r| r.turn.clone()),
            cwd: self.config.cwd.clone(),
        };
        let hooks: Vec<Arc<dyn Hook>> = self
            .config
            .hooks
            .iter()
            .filter(|h| hook_applies(&h.matcher(), HookPoint::Submit, None))
            .cloned()
            .collect();
        for hook in hooks {
            match hook.on_submit(&mut input, &cx).await {
                HookOutcome::Continue | HookOutcome::Ask { .. } => {}
                HookOutcome::Deny { reason } | HookOutcome::Block { reason } => {
                    return self
                        .reject(intent, ErrorCode::PermissionDenied, reason)
                        .await;
                }
                HookOutcome::Redirect { session } => {
                    return self
                        .reject(
                            intent,
                            ErrorCode::InvalidInput,
                            format!("redirect to {session} is not supported"),
                        )
                        .await;
                }
            }
        }
        if self.running.is_some() {
            self.queue.push_back((intent.clone(), input));
            let position = self.queue.len() as u32;
            self.publish_queue().await;
            self.publish(
                Event::IntentAck {
                    intent: intent.clone(),
                    outcome: IntentOutcome::Queued { position },
                },
                Some(intent),
            )
            .await;
            return;
        }
        self.start_turn(vec![(intent, input)], TurnOrigin::Submit)
            .await;
    }

    async fn start_turn(&mut self, inputs: Vec<(IntentId, Input)>, origin: TurnOrigin) {
        let turn = TurnId::mint();
        let cancel = CancellationToken::new();
        let mut ids = Vec::new();
        let mut acks = Vec::new();
        for (intent, input) in inputs {
            let Input::Text { text, origin, .. } = input else {
                continue;
            };
            let item = self.fresh(
                Some(turn.clone()),
                Some(intent.clone()),
                ItemBody::User {
                    parts: vec![ContentPart::text(text)],
                    origin,
                },
            );
            ids.push(item.id.clone());
            acks.push(intent.clone());
            self.publish(Event::ItemCompleted { item }, Some(intent))
                .await;
        }
        self.publish(
            Event::TurnStarted {
                turn: turn.clone(),
                inputs: ids,
                origin,
            },
            None,
        )
        .await;
        if origin == TurnOrigin::Submit {
            for intent in acks {
                self.publish(
                    Event::IntentAck {
                        intent: intent.clone(),
                        outcome: IntentOutcome::TurnStarted { turn: turn.clone() },
                    },
                    Some(intent),
                )
                .await;
            }
        }
        let run = TurnRun {
            turn: turn.clone(),
            history: self.journal.clone(),
            generation: self.generation,
            cancel: cancel.clone(),
        };
        let cfg = Arc::clone(&self.config);
        let mailbox = self.mailbox.clone();
        let host = TurnMail {
            mailbox: mailbox.clone(),
            turn: turn.clone(),
        };
        let task = tokio::spawn(async move {
            let outcome = AssertUnwindSafe(run_turn(&cfg, run, &host))
                .catch_unwind()
                .await
                .map_err(panic_message);
            mailbox.send(Msg::TurnFinished {
                turn: host.turn.clone(),
                outcome,
            });
        });
        // Registered after the spawn: the task's first mail is handled only
        // once this function returns, so nothing can race the registration.
        self.running = Some(Running { turn, cancel, task });
    }

    async fn turn_finished(&mut self, turn: TurnId, outcome: Result<TurnOutcome, String>) -> Flow {
        if !self.is_running(&turn) {
            tracing::warn!(session = %self.id, %turn, "completion from a turn that is not running");
            return Flow::Continue;
        }
        self.cancel_interactions(CancelReason::TurnEnded).await;
        let (status, usage) = match outcome {
            Ok(outcome) => (outcome.status, outcome.usage),
            Err(panic) => (
                TurnStatus::Failed {
                    error: KernelError::new(
                        ErrorCode::TurnLost,
                        format!("the turn loop panicked: {panic}"),
                    ),
                },
                Usage::default(),
            ),
        };
        self.running = None;
        self.publish(
            Event::TurnCompleted {
                turn,
                status,
                usage,
            },
            None,
        )
        .await;
        if let Some(reason) = self.closing.take() {
            return self.finish_close(reason).await;
        }
        if !self.queue.is_empty() {
            let inputs: Vec<_> = self.queue.drain(..).collect();
            self.publish_queue().await;
            self.start_turn(inputs, TurnOrigin::Queue).await;
        }
        Flow::Continue
    }

    async fn interrupt(&mut self, intent: IntentId, scope: InterruptScope) {
        let Some(running) = &self.running else {
            return self
                .reject(intent, ErrorCode::NotReady, "no turn is running")
                .await;
        };
        if let InterruptScope::Turn { turn } = &scope
            && turn != &running.turn
        {
            return self
                .reject(intent, ErrorCode::NotReady, "that turn is not running")
                .await;
        }
        running.cancel.cancel();
        let turn = running.turn.clone();
        self.cancel_interactions(CancelReason::Interrupted).await;
        self.applied(intent, json!({ "turn": turn })).await;
    }

    // ----- queue -----

    async fn publish_queue(&mut self) {
        self.queue_revision += 1;
        let entries = self
            .queue
            .iter()
            .enumerate()
            .map(|(i, (intent, input))| QueueEntry {
                intent: intent.clone(),
                position: i as u32 + 1,
                preview: preview(input),
                steerable: true,
                origin: match input {
                    Input::Text { origin, .. } => origin.clone(),
                    Input::Action { .. } => Origin::default(),
                },
            })
            .collect();
        self.publish(
            Event::QueueChanged {
                revision: self.queue_revision,
                entries,
            },
            None,
        )
        .await;
    }

    async fn take_queue(&mut self) -> Vec<(IntentId, Input)> {
        if self.queue.is_empty() {
            return Vec::new();
        }
        let taken: Vec<_> = self.queue.drain(..).collect();
        self.publish_queue().await;
        taken
    }

    // ----- interactions -----

    async fn open_interaction(
        &mut self,
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
        reply: oneshot::Sender<Result<Answer, KernelError>>,
    ) {
        let Some(running) = &self.running else {
            let _ = reply.send(Err(KernelError::new(
                ErrorCode::NotReady,
                "no turn is running",
            )));
            return;
        };
        let now = Timestamp::now();
        let interaction = Interaction {
            id: InteractionId::mint(),
            session: self.id.clone(),
            turn: Some(running.turn.clone()),
            item,
            opened_at: now,
            guard_until: now
                .checked_add(SignedDuration::from_millis(INTERACTION_GUARD_MS))
                .ok(),
            expires_at: None,
            kind,
            answers,
        };
        self.pending.insert(
            interaction.id.clone(),
            Pending {
                interaction: interaction.clone(),
                reply,
            },
        );
        self.publish(Event::InteractionOpened { interaction }, None)
            .await;
    }

    async fn answer(
        &mut self,
        intent: IntentId,
        id: InteractionId,
        answer: Answer,
        activation: Activation,
        who: ClientIdentity,
    ) {
        let Some(pending) = self.pending.get(&id) else {
            return self
                .reject(
                    intent,
                    ErrorCode::InteractionClosed,
                    "no such open interaction",
                )
                .await;
        };
        if !pending.interaction.answers.contains(&answer.spec()) {
            return self
                .reject(
                    intent,
                    ErrorCode::InvalidInput,
                    format!("{:?} is not an accepted answer here", answer.spec()),
                )
                .await;
        }
        if activation == Activation::Keyboard
            && let Some(guard) = pending.interaction.guard_until
            && Timestamp::now() < guard
        {
            return self
                .reject(
                    intent,
                    ErrorCode::NotReady,
                    "answered too soon after opening",
                )
                .await;
        }
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        let _ = pending.reply.send(Ok(answer.clone()));
        self.publish(
            Event::InteractionResolved {
                id,
                answer,
                by: ResolvedBy::Client {
                    name: who.name,
                    surface: who.surface,
                },
            },
            Some(intent.clone()),
        )
        .await;
        self.applied(intent, Value::Null).await;
    }

    async fn cancel_interactions(&mut self, reason: CancelReason) {
        let pending: Vec<_> = self.pending.drain().collect();
        for (id, pending) in pending {
            let _ = pending.reply.send(Err(KernelError::new(
                ErrorCode::InteractionClosed,
                format!("cancelled: {reason:?}"),
            )));
            self.publish(Event::InteractionCancelled { id, reason }, None)
                .await;
        }
    }

    // ----- closing -----

    async fn close(&mut self, reason: CloseReason) -> Flow {
        if self.state.closed {
            return Flow::Stop;
        }
        if let Some(running) = &self.running {
            running.cancel.cancel();
            self.cancel_interactions(CancelReason::SessionClosed).await;
            self.closing = Some(reason);
            return Flow::Continue;
        }
        self.finish_close(reason).await
    }

    async fn finish_close(&mut self, reason: CloseReason) -> Flow {
        let queued: Vec<_> = self.queue.drain(..).collect();
        for (intent, _) in queued {
            self.reject(intent, ErrorCode::SessionClosed, "the session is closed")
                .await;
        }
        self.publish(Event::SessionClosed { reason }, None).await;
        self.subscribers.clear();
        Flow::Stop
    }
}

/// What the kernel accepts as a submission in M0: plain text.
fn validate(input: &Input) -> Result<(), String> {
    match input {
        Input::Text {
            text, attachments, ..
        } => {
            if text.trim().is_empty() {
                return Err("empty input".into());
            }
            if text.trim_start().starts_with('/') {
                return Err(format!(
                    "unknown command: {}",
                    text.split_whitespace().next().unwrap_or("/")
                ));
            }
            if !attachments.is_empty() {
                return Err("attachments are not supported".into());
            }
            Ok(())
        }
        Input::Action { action } => Err(format!("unknown action: {}", action.name)),
    }
}

fn preview(input: &Input) -> String {
    let text = match input {
        Input::Text { text, .. } => text.as_str(),
        Input::Action { action } => action.name.as_str(),
    };
    text.chars().take(PREVIEW_CHARS).collect()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

#[cfg(test)]
mod tests;
