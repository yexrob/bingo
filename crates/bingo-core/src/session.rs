//! The session actor: one mailbox, one journal, one snapshot, one running
//! turn. Every producer — clients, the turn loop, tools — sends a `Msg`; the
//! actor mints `seq`, persists durable frames, folds them with the one reducer
//! and fans them out to bounded subscriber channels. It never awaits a client.

mod interactions;
mod mailbox;
mod subscribers;

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use bingo_sdk::*;
use futures::FutureExt;
use jiff::Timestamp;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use interactions::INTERACTION_GUARD_MS;
use interactions::Pending;
pub use mailbox::Mailbox;
use mailbox::{Msg, TurnMail};
pub use subscribers::SUBSCRIBER_CAPACITY;
use subscribers::Subscribers;

use crate::gate::hook_applies;
use crate::turn::{TurnConfig, TurnOutcome, TurnRun, run_turn};

/// Characters of a queued input shown in `QueueChanged`.
const PREVIEW_CHARS: usize = 80;

/// Start an actor. The turn config is built second because its tool host
/// talks back through the mailbox.
pub fn spawn(
    summary: SessionSummary,
    store: Option<Arc<dyn SessionStore>>,
    config: impl FnOnce(&Mailbox) -> Arc<TurnConfig>,
) -> Mailbox {
    let (tx, rx) = mpsc::unbounded_channel();
    let mailbox = Mailbox::new(summary.id.clone(), tx);
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
        subscribers: Subscribers::default(),
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
            Msg::Emit { turn, event } => self.emit(turn, *event).await,
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
            Msg::Progress { item, tail } => self.progress(item, tail).await,
            Msg::Close { reason } => return self.close(reason).await,
        }
        Flow::Continue
    }

    fn is_running(&self, turn: &TurnId) -> bool {
        self.running.as_ref().is_some_and(|r| &r.turn == turn)
    }

    /// An event from the running turn. One from a turn that already ended is
    /// dropped: its seq would land after the `TurnCompleted` that closed it.
    async fn emit(&mut self, turn: TurnId, event: Event) {
        if self.is_running(&turn) {
            self.publish(event, None).await;
        } else {
            tracing::warn!(session = %self.id, %turn, "event from a turn that is not running");
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
        if origin == TurnOrigin::Submit {
            self.ack_turn_started(&turn, acks).await;
        }
        let running = self.spawn_turn(turn, CancellationToken::new());
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
    fn spawn_turn(&self, turn: TurnId, cancel: CancellationToken) -> Running {
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
        Running { turn, cancel, task }
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
