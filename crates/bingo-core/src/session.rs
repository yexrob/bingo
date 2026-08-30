//! The session actor: one mailbox, one journal, one snapshot, one running
//! turn. Every producer — clients, the turn loop, tools — sends a `Msg`; the
//! actor mints `seq`, persists durable frames, folds them with the one reducer
//! and fans them out to bounded subscriber channels. It never awaits a client.

mod commands;
mod inputs;
mod interactions;
mod mailbox;
mod queue;
mod spawn;
mod subscribers;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use bingo_sdk::*;
use futures::FutureExt;
use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use commands::Commands;
pub use commands::Services;
pub use interactions::INTERACTION_GUARD_MS;
use interactions::Pending;
pub use mailbox::Mailbox;
use mailbox::{Msg, TurnMail};
use queue::Queue;
pub use spawn::{head_summary, resume, spawn};
pub use subscribers::SUBSCRIBER_CAPACITY;
use subscribers::Subscribers;

use crate::gate::hook_applies;
use crate::turn::{TurnConfig, TurnKind, TurnOutcome, TurnRun, run_turn};

/// How long a stopping actor waits for the work it spawned after its turns
/// (ADR-0008 §7) before it lets go.
pub const AFTER_TURN_DEADLINE: Duration = Duration::from_secs(30);

/// The start hooks, still running.
type Starting = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

struct Running {
    turn: TurnId,
    cancel: CancellationToken,
    task: JoinHandle<()>,
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
    subscribers: Subscribers,
    running: Option<Running>,
    queue: Queue,
    commands: Commands,
    /// Work that outlives a turn: the hooks that run after `TurnCompleted`.
    tracker: TaskTracker,
    /// Every frame but the deltas for the hooks that observe the session, in
    /// order, on a task of their own; `None` when no hook asked (ADR-0009 §4).
    observed: Option<mpsc::UnboundedSender<(Frame, HookContext)>>,
    /// Flipped when the actor is done, for whoever waits on the mailbox.
    done: watch::Sender<bool>,
    pending: HashMap<InteractionId, Pending>,
    generation: u64,
    /// A close that waits for the running turn to wind down.
    closing: Option<CloseReason>,
    progress_n: u32,
}

impl Actor {
    async fn run(mut self) {
        let starting = self.open().await;
        let held = self.serve_start(starting).await;
        let mut flow = Flow::Continue;
        for msg in held {
            if self.handle(msg).await == Flow::Stop {
                flow = Flow::Stop;
                break;
            }
        }
        while flow == Flow::Continue {
            let Some(msg) = self.rx.recv().await else {
                break;
            };
            flow = self.handle(msg).await;
        }
        if let Some(running) = self.running.take() {
            running.cancel.cancel();
            running.task.abort();
        }
        drop(self.observed.take());
        self.tracker.close();
        if tokio::time::timeout(AFTER_TURN_DEADLINE, self.tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!(session = %self.id, "post-turn work did not finish in time");
        }
        let _ = self.done.send(true);
    }

    async fn handle(&mut self, msg: Msg) -> Flow {
        match msg {
            Msg::Submit { intent, input } => self.submit(intent, input).await,
            Msg::Deliver {
                intent,
                input,
                delivery,
            } => self.deliver(intent, input, delivery).await,
            Msg::Extend {
                plugin,
                kind,
                payload,
            } => self.extend(plugin, kind, payload).await,
            Msg::Interrupt { intent, scope } => self.interrupt(intent, scope).await,
            Msg::Answer(answered) => self.answer(answered).await,
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
            Msg::Emit { turn, event } => self.emit(turn, *event).await,
            Msg::Ask {
                item,
                kind,
                answers,
                reply,
            } => self.open_interaction(item, kind, answers, reply).await,
            Msg::Absorb { turn, reply } => {
                let _ = reply.send(self.absorb(&turn).await);
            }
            Msg::TurnFinished { turn, outcome } => return self.turn_finished(turn, outcome).await,
            Msg::Record { body, reply } => {
                let id = self.record(body).await;
                let _ = reply.send(id);
            }
            Msg::Progress { item, tail } => self.progress(item, tail).await,
            Msg::CommandFinished { intent, outcome } => {
                self.command_finished(intent, outcome).await
            }
            Msg::Reconfigure { config } => self.reconfigure(config).await,
            Msg::Compact {
                instructions,
                reply,
            } => drop(reply.send(self.compact(instructions).await)),
            Msg::Close { reason } => return self.close(reason).await,
        }
        Flow::Continue
    }

    /// A plugin's state, whole, as a durable frame (ADR-0011 §2).
    async fn extend(&mut self, plugin: String, kind: String, payload: Value) {
        let event = Event::Extension {
            plugin,
            kind,
            payload,
        };
        self.publish(event, None).await;
    }

    /// What a barrier may steer the running turn with: the held prose, and
    /// nothing for a turn that is not the one running.
    async fn absorb(&mut self, turn: &TurnId) -> Vec<(IntentId, Input)> {
        if self.is_running(turn) {
            self.take_queue().await
        } else {
            Vec::new()
        }
    }

    /// The head of this segment of the journal: what the session is now.
    /// The start hooks it hands back run while the session opens for reads.
    async fn open(&mut self) -> Starting {
        let summary = SessionSummary {
            busy: false,
            updated_at: Timestamp::now(),
            ..self.state.summary.clone()
        };
        self.observe_journal();
        self.publish(Event::SessionUpdated { summary }, None).await;
        self.restate_extensions().await;
        self.refresh_config().await;
        self.recover().await;
        let cx = self.hook_context();
        Box::pin(run_session_hooks(self.session_hook_set(), Phase::Start, cx))
    }

    /// While the start hooks run the session answers its summary and holds
    /// everything else: a hook may list the tree it is seating — the host
    /// asks every live actor for its summary, this one included — and what
    /// it seats is there before the first message is read.
    async fn serve_start(&mut self, mut hooks: Starting) -> Vec<Msg> {
        let mut held = Vec::new();
        loop {
            tokio::select! {
                () = &mut hooks => return held,
                msg = self.rx.recv() => match msg {
                    Some(msg) if msg.reads() => {
                        self.handle(msg).await;
                    }
                    Some(msg) => held.push(msg),
                    None => return held,
                },
            }
        }
    }

    /// The head of a segment restates the plugin state the journal already
    /// holds (ADR-0011 §2), so an observer that joins here — a hook after a
    /// restart — folds the same state a client is handed in its snapshot.
    async fn restate_extensions(&mut self) {
        let held: Vec<(String, String, Value)> = self
            .state
            .extensions
            .iter()
            .flat_map(|(plugin, kinds)| {
                kinds
                    .iter()
                    .map(move |(kind, payload)| (plugin.clone(), kind.clone(), payload.clone()))
            })
            .collect();
        for (plugin, kind, payload) in held {
            self.extend(plugin, kind, payload).await;
        }
    }

    /// What a client may read of this session's configuration: the kernel's
    /// own keys (ADR-0008 §4) and what the policy says of itself (ADR-0009 §5).
    /// Published when it differs from what the clients already hold.
    async fn refresh_config(&mut self) {
        let policy = &self.config.policy;
        let mut plugins = std::collections::BTreeMap::new();
        let described = policy.describe(&self.id);
        if !described.is_null() {
            plugins.insert(policy.id().to_string(), described);
        }
        let config = ConfigView {
            kernel: json!({ "thinking": self.config.model.as_ref().and_then(|m| m.reasoning) }),
            plugins,
        };
        if config != self.state.config {
            self.publish(Event::ConfigChanged { config }, None).await;
        }
    }

    fn session_hook_set(&self) -> Vec<Arc<dyn Hook>> {
        self.config
            .hooks
            .iter()
            .filter(|h| hook_applies(&h.matcher(), HookPoint::Session, None))
            .cloned()
            .collect()
    }

    /// The end-of-session hooks, off the stopping actor's path.
    fn session_hooks(&self, phase: Phase) {
        let hooks = self.session_hook_set();
        if hooks.is_empty() {
            return;
        }
        let cx = self.hook_context();
        self.tracker.spawn(run_session_hooks(hooks, phase, cx));
    }

    /// One ordered task feeds every frame but the deltas to the hooks that
    /// observe the session; publishing never waits on them.
    fn observe_journal(&mut self) {
        let hooks: Vec<Arc<dyn Hook>> = self
            .config
            .hooks
            .iter()
            .filter(|h| hook_applies(&h.matcher(), HookPoint::Event, None))
            .cloned()
            .collect();
        if hooks.is_empty() {
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<(Frame, HookContext)>();
        self.observed = Some(tx);
        self.tracker.spawn(async move {
            while let Some((frame, cx)) = rx.recv().await {
                for hook in &hooks {
                    hook.on_event(&frame, &cx).await;
                }
            }
        });
    }

    /// The next turn runs on a config the host rebuilt; the running one
    /// keeps its own. The summary says what changed.
    async fn reconfigure(&mut self, config: Arc<TurnConfig>) {
        self.config = config;
        let model = self.config.model.as_ref();
        let summary = SessionSummary {
            model: model.map(|m| m.id.clone()),
            provider: model.map(|m| m.provider.id().to_string()),
            updated_at: Timestamp::now(),
            ..self.state.summary.clone()
        };
        self.publish(Event::SessionUpdated { summary }, None).await;
        self.refresh_config().await;
    }

    /// A turn that only compacts (ADR-0008 §4): refused while one runs, so
    /// that a queued `/compact` never races the turn ahead of it.
    async fn compact(&mut self, instructions: Option<String>) -> Result<(), KernelError> {
        if !self.answers() {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "a log session has nothing to compact",
            ));
        }
        if self.running.is_some() {
            return Err(KernelError::new(ErrorCode::NotReady, "a turn is running"));
        }
        self.start_turn(
            Vec::new(),
            TurnOrigin::Auto,
            TurnKind::Compact { instructions },
        )
        .await;
        Ok(())
    }

    fn hook_context(&self) -> HookContext {
        HookContext {
            host: self.config.host.clone(),
            session: self.id.clone(),
            turn: self.running.as_ref().map(|r| r.turn.clone()),
            cwd: self.config.cwd.clone(),
            provider: self.config.model.as_ref().map(|m| m.provider.clone()),
            model: self.config.model.as_ref().map(|m| m.id.clone()),
        }
    }

    /// Busy for the queue's purposes: a turn, or a command that holds it.
    fn busy(&self) -> bool {
        self.running.is_some() || self.commands.busy()
    }

    /// Whether a model answers this session. A `Log` session (ADR-0011 §1)
    /// has none: what it is told is the journal's at once, and no turn opens.
    fn answers(&self) -> bool {
        self.config.model.is_some()
    }

    /// A resumed journal may end inside a turn the old process never
    /// finished. Its questions can no longer be answered and its turn is
    /// lost; both are said before anything new happens.
    async fn recover(&mut self) {
        let open: Vec<InteractionId> = self
            .state
            .interactions
            .iter()
            .map(|i| i.id.clone())
            .collect();
        for id in open {
            self.publish(
                Event::InteractionCancelled {
                    id,
                    reason: CancelReason::SessionClosed,
                },
                None,
            )
            .await;
        }
        if let Some(turn) = self.state.turn.clone() {
            self.publish(
                Event::TurnCompleted {
                    turn: turn.id,
                    status: TurnStatus::Failed {
                        error: KernelError::new(
                            ErrorCode::TurnLost,
                            "the process ended during this turn",
                        ),
                    },
                    usage: turn.usage,
                },
                None,
            )
            .await;
        }
    }

    fn is_running(&self, turn: &TurnId) -> bool {
        self.running.as_ref().is_some_and(|r| &r.turn == turn)
    }

    /// An event from the running turn. One from a turn that already ended is
    /// dropped: its seq would land after the `TurnCompleted` that closed it.
    /// A permission receipt means the policy may have installed a rule.
    async fn emit(&mut self, turn: TurnId, event: Event) {
        if !self.is_running(&turn) {
            tracing::warn!(session = %self.id, %turn, "event from a turn that is not running");
            return;
        }
        let receipt = matches!(
            &event,
            Event::ItemCompleted { item } if matches!(item.body, ItemBody::PermissionReceipt { .. })
        );
        self.publish(event, None).await;
        if receipt {
            self.refresh_config().await;
        }
    }

    /// A running tool's own tail, numbered per session so clients can order it.
    async fn progress(&mut self, item: ItemId, tail: String) {
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
        // Observers see notices too, which the journal does not keep; only
        // the deltas, which are volume and nothing else, are spared them.
        if let Some(observed) = &self.observed
            && !matches!(frame.event, Event::ItemDelta { .. })
        {
            let _ = observed.send((frame.clone(), self.hook_context()));
        }
        self.state.apply(&frame);
        self.subscribers.fanout(&frame);
        frame.seq
    }

    fn subscribe(&mut self, since: Seq) -> FrameStream {
        let replay: Vec<Frame> = self
            .journal
            .iter()
            .filter(|f| f.seq > since)
            .cloned()
            .collect();
        self.subscribers.add(self.id.clone(), replay)
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

    async fn start_turn(
        &mut self,
        inputs: Vec<(IntentId, Input)>,
        origin: TurnOrigin,
        kind: TurnKind,
    ) {
        let turn = TurnId::mint();
        let (ids, acks) = self.record_inputs(&turn, inputs).await;
        self.publish(
            Event::TurnStarted {
                turn: turn.clone(),
                inputs: ids,
                origin,
            },
            None,
        )
        .await;
        // A queued intent was acknowledged `Queued` when it waited; the turn
        // that runs it acknowledges it again, so a client learns which turn
        // is its own without matching items.
        self.ack_turn_started(&turn, acks).await;
        let running = self.spawn_turn(turn, CancellationToken::new(), kind);
        // Registered after the spawn: the task's first mail is handled only
        // once this function returns, so nothing can race the registration.
        self.running = Some(running);
    }

    /// Journal one user item per text input; the item ids open the turn and
    /// the intents behind them are the ones to acknowledge.
    async fn record_inputs(
        &mut self,
        turn: &TurnId,
        inputs: Vec<(IntentId, Input)>,
    ) -> (Vec<ItemId>, Vec<IntentId>) {
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
        (ids, acks)
    }

    async fn ack_turn_started(&mut self, turn: &TurnId, intents: Vec<IntentId>) {
        for intent in intents {
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

    /// The turn loop runs in its own task and reports back by mail; a panic in
    /// it becomes a failed turn rather than a lost session.
    fn spawn_turn(&self, turn: TurnId, cancel: CancellationToken, kind: TurnKind) -> Running {
        let run = TurnRun {
            turn: turn.clone(),
            history: self.journal.clone(),
            generation: self.generation,
            cancel: cancel.clone(),
            kind,
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
        Running { turn, cancel, task }
    }

    async fn turn_finished(&mut self, turn: TurnId, outcome: Result<TurnOutcome, String>) -> Flow {
        if !self.is_running(&turn) {
            tracing::warn!(session = %self.id, %turn, "completion from a turn that is not running");
            return Flow::Continue;
        }
        self.cancel_interactions(CancelReason::TurnEnded).await;
        let (status, usage, items) = match outcome {
            Ok(outcome) => (outcome.status, outcome.usage, outcome.items),
            Err(panic) => (
                TurnStatus::Failed {
                    error: KernelError::new(
                        ErrorCode::TurnLost,
                        format!("the turn loop panicked: {panic}"),
                    ),
                },
                Usage::default(),
                Vec::new(),
            ),
        };
        self.running = None;
        self.publish(
            Event::TurnCompleted {
                turn: turn.clone(),
                status,
                usage,
            },
            None,
        )
        .await;
        self.after_turn(turn, items);
        if let Some(reason) = self.closing.take() {
            return self.finish_close(reason).await;
        }
        self.drain_queue().await;
        Flow::Continue
    }

    /// The turn-end hooks run after the terminal event (ADR-0008 §7), on a
    /// task the actor waits for before it stops.
    fn after_turn(&self, turn: TurnId, items: Vec<Item>) {
        let hooks: Vec<Arc<dyn Hook>> = self
            .config
            .hooks
            .iter()
            .filter(|h| hook_applies(&h.matcher(), HookPoint::Turn, None))
            .cloned()
            .collect();
        if hooks.is_empty() {
            return;
        }
        let cx = HookContext {
            turn: Some(turn.clone()),
            ..self.hook_context()
        };
        self.tracker.spawn(async move {
            for hook in hooks {
                hook.on_turn(Phase::End, &turn, &items, &cx).await;
            }
        });
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
        for (intent, _) in self.queue.drain_all() {
            self.reject(intent, ErrorCode::SessionClosed, "the session is closed")
                .await;
        }
        self.publish(Event::SessionClosed { reason }, None).await;
        self.subscribers.clear();
        self.session_hooks(Phase::End);
        Flow::Stop
    }
}

async fn run_session_hooks(hooks: Vec<Arc<dyn Hook>>, phase: Phase, cx: HookContext) {
    for hook in hooks {
        hook.on_session(phase, &cx).await;
    }
}

/// What the kernel accepts as prose: text, for now without attachments.
pub(super) fn validate(input: &Input) -> Result<(), String> {
    match input {
        Input::Text {
            text, attachments, ..
        } => {
            if text.trim().is_empty() {
                return Err("empty input".into());
            }
            if !attachments.is_empty() {
                return Err("attachments are not supported".into());
            }
            Ok(())
        }
        Input::Action { action } => Err(format!("unknown action: {}", action.name)),
    }
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
