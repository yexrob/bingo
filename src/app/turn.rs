//! Turn lifecycle: one run, one terminal state, whatever kills the run.
//!
//! A turn is opened by the actor, reported to by the engine task that runs it,
//! and closed exactly once. "Exactly once" is the whole point of putting it here:
//! a run can end by returning, by failing, by being interrupted, or by having its
//! task aborted out from under it, and the last of those executes no code of its
//! own. [`TurnGuard`] closes the turn from `Drop`, and the actor — not the guard —
//! decides that the first close wins. An error raised on the way out is a second
//! fact about the same turn, never a substitute for its terminal event (spec
//! invariant #5).
//!
//! The registry also owns the items one attempt produced, because a transparent
//! stream retry has to withdraw exactly them: `turn/retrying` carries the
//! identifiers it removed, so a client never guesses which text to roll back
//! (invariant #7).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use crate::app::answer::Answer;
use crate::app::controller::Control;
use crate::app::conversation::ConvKey;
use crate::app::ids::{IdMint, ItemId, TurnId, now_millis};
use crate::app::snapshot::{
    ContextUsage, Item, ItemBody, ItemStatus, Turn, TurnError, TurnOrigin, TurnStatus, TurnUsage,
};
use crate::engine::events::EngineEvent;
use crate::query::ToolCallStatus;

/// What a run reports, asks, or ends with.
pub(crate) enum TurnMsg {
    Open {
        conversation: ConvKey,
        origin: TurnOrigin,
        input_items: Vec<ItemId>,
        reply: oneshot::Sender<Option<TurnId>>,
    },
    /// One run, one host: the first claim on a turn wins and every later one is
    /// refused, so two runs cannot report into the same turn's item stream.
    Claim {
        turn: TurnId,
        reply: oneshot::Sender<bool>,
    },
    Report {
        turn: TurnId,
        event: Box<EngineEvent>,
    },
    Close {
        turn: TurnId,
        status: TurnStatus,
        error: Option<TurnError>,
    },
    Active {
        conversation: ConvKey,
        reply: oneshot::Sender<Option<TurnId>>,
    },
    /// An item the core created outside a run — the input a turn opens with, or
    /// a queue entry a barrier absorbed.
    Commit {
        conversation: ConvKey,
        turn: Option<TurnId>,
        body: Box<ItemBody>,
        reply: oneshot::Sender<ItemId>,
    },
    #[cfg(test)]
    Probe {
        turn: TurnId,
        reply: oneshot::Sender<Option<Turn>>,
    },
}

/// What one turn's state change asks the actor to publish. The registry names
/// conversations by key; the actor is what turns a key into the identifier a
/// client sees.
pub(crate) enum TurnChange {
    Started {
        conversation: ConvKey,
        turn: Turn,
    },
    RoundStarted {
        conversation: ConvKey,
        turn: TurnId,
        round: u32,
    },
    Retrying {
        conversation: ConvKey,
        turn: TurnId,
        round: u32,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        removed: Vec<ItemId>,
        code: Option<String>,
        reason: Option<String>,
    },
    RoundCompleted {
        conversation: ConvKey,
        turn: TurnId,
        round: u32,
        usage: Option<TurnUsage>,
    },
    Usage {
        conversation: ConvKey,
        turn: TurnId,
        usage: TurnUsage,
        context: Option<ContextUsage>,
    },
    Completed {
        conversation: ConvKey,
        turn: Turn,
    },
    ItemStarted {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: Box<Item>,
    },
    ItemTextDelta {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: ItemId,
        delta_seq: u64,
        delta: String,
    },
    ItemReasoningDelta {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: ItemId,
        delta_seq: u64,
        delta: String,
    },
    ItemUpdated {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: Box<Item>,
    },
    ItemCompleted {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: Box<Item>,
    },
    /// Prose entering this conversation from outside its own turn: the prompt a
    /// run opened with, and every batch of mail it absorbed at a round boundary.
    /// The actor reads it with the one walker and commits the items it names,
    /// which is why the text travels rather than an item — attribution is
    /// `app::projection`'s, not this registry's.
    Inbound {
        conversation: ConvKey,
        turn: TurnId,
        text: String,
        /// This is the task the instance was created for, the one shape a
        /// continuation's mail can never be.
        first: bool,
    },
    /// Something went wrong that did not end the run.
    Warning {
        conversation: ConvKey,
        turn: TurnId,
        text: String,
    },
    /// The foreground command's output so far. It replaces the slot rather than
    /// appending to it, and it never becomes the item's final output.
    ItemCommandTail {
        conversation: ConvKey,
        turn: TurnId,
        item: ItemId,
        tail: crate::app::snapshot::CommandTail,
    },
}

/// What a reader can see of the turns in flight, without asking.
#[derive(Debug, Default)]
pub struct TurnView {
    active: HashMap<ConvKey, TurnId>,
    /// The turns somebody has asked to stop. The request is the core's; the
    /// stopping is the runner's, which is what watches this.
    interrupting: std::collections::HashSet<TurnId>,
}

impl TurnView {
    /// The turn running on this conversation, if one is.
    pub fn active(&self, conversation: &ConvKey) -> Option<&TurnId> {
        self.active.get(conversation)
    }

    pub fn is_busy(&self, conversation: &ConvKey) -> bool {
        self.active.contains_key(conversation)
    }

    /// How many turns are running anywhere. What a shutdown reports it stopped.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Whether this turn has been asked to stop.
    pub fn is_interrupted(&self, turn: &TurnId) -> bool {
        self.interrupting.contains(turn)
    }
}

/// What `turn/interrupt` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupted {
    /// It is running, and has been asked to stop.
    Asked,
    /// It had already reached its terminal state; interrupting is idempotent
    /// and a late one must not cancel the next turn.
    Already,
    /// No turn of that name in this epoch.
    Unknown,
}

/// A run's hold on its turn.
///
/// Dropping it closes the turn. That is the point: an instance is stopped by
/// aborting its task, which unwinds the run without executing another line, and a
/// turn left running forever is a spinner that never stops. A run that ends
/// normally calls [`TurnGuard::finish`] first, and the actor keeps the first
/// terminal state it was given.
pub struct TurnGuard {
    turn: TurnId,
    control: mpsc::UnboundedSender<Control>,
    /// What `Drop` closes with when nothing else did. Only an abandoned run
    /// reaches it, which is a failure of the run rather than of the model.
    fallback: TurnStatus,
}

impl TurnGuard {
    pub fn turn(&self) -> &TurnId {
        &self.turn
    }

    /// Close the turn with the state the run actually reached.
    pub fn finish(self, status: TurnStatus, error: Option<TurnError>) {
        self.close(status, error);
    }

    fn close(&self, status: TurnStatus, error: Option<TurnError>) {
        let _ = self.control.send(Control::Turn(TurnMsg::Close {
            turn: self.turn.clone(),
            status,
            error,
        }));
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.close(
            self.fallback,
            Some(TurnError {
                code: crate::error::TURN_LOST.to_string(),
                message: "The run ended without closing its turn.".to_string(),
            }),
        );
    }
}

impl std::fmt::Debug for TurnGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnGuard")
            .field("turn", &self.turn)
            .finish()
    }
}

/// How the rest of the process reaches the turns the actor owns.
#[derive(Clone)]
pub struct TurnHandle {
    control: mpsc::UnboundedSender<Control>,
    view: watch::Receiver<Arc<TurnView>>,
}

impl std::fmt::Debug for TurnHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TurnHandle")
    }
}

impl TurnHandle {
    fn report(&self, message: TurnMsg) {
        let _ = self.control.send(Control::Turn(message));
    }

    fn ask<T>(&self, gone: T, build: impl FnOnce(oneshot::Sender<T>) -> TurnMsg) -> Answer<T> {
        let (reply, answer) = oneshot::channel();
        let _ = self.control.send(Control::Turn(build(reply)));
        Answer::new(answer, gone)
    }

    /// What is running right now, for a surface that only wants to look.
    pub fn view(&self) -> Arc<TurnView> {
        self.view.borrow().clone()
    }

    /// Open a turn on a conversation, or refuse because one is already running
    /// there. At most one turn writes a conversation at a time (spec "Turn and
    /// round"). The caller turns the identifier into a [`TurnGuard`] on the task
    /// that will run it, which is where the guard has to live.
    pub fn open(
        &self,
        conversation: ConvKey,
        origin: TurnOrigin,
        input_items: Vec<ItemId>,
    ) -> Answer<Option<TurnId>> {
        self.ask(None, move |reply| TurnMsg::Open {
            conversation,
            origin,
            input_items,
            reply,
        })
    }

    /// A turn the caller already owns, rebuilt as a guard. Used where the run and
    /// the turn are opened in different places.
    pub fn guard(&self, turn: TurnId, fallback: TurnStatus) -> TurnGuard {
        TurnGuard {
            turn,
            control: self.control.clone(),
            fallback,
        }
    }

    /// Claim a turn for one run. `false` means somebody already did.
    pub fn claim(&self, turn: TurnId) -> Answer<bool> {
        self.ask(false, move |reply| TurnMsg::Claim { turn, reply })
    }

    /// Report one thing that happened inside a run.
    pub fn report_event(&self, turn: TurnId, event: EngineEvent) {
        self.report(TurnMsg::Report {
            turn,
            event: Box::new(event),
        });
    }

    pub fn close(&self, turn: TurnId, status: TurnStatus, error: Option<TurnError>) {
        self.report(TurnMsg::Close {
            turn,
            status,
            error,
        });
    }

    pub fn active(&self, conversation: ConvKey) -> Answer<Option<TurnId>> {
        self.ask(None, move |reply| TurnMsg::Active {
            conversation,
            reply,
        })
    }

    /// Commit one completed item the core produced itself. The identifier comes
    /// back because a turn's input items are named in its snapshot.
    pub fn commit_item(
        &self,
        conversation: ConvKey,
        turn: Option<TurnId>,
        body: ItemBody,
    ) -> Answer<ItemId> {
        self.ask(ItemId::new(""), move |reply| TurnMsg::Commit {
            conversation,
            turn,
            body: Box::new(body),
            reply,
        })
    }
}

/// One turn as the actor holds it.
struct Record {
    conversation: ConvKey,
    turn: Turn,
    /// The run that reports into this turn has claimed it.
    claimed: bool,
    /// Items the attempt in flight produced, in order. A retry withdraws exactly
    /// these.
    live: Live,
    /// The next round has not announced itself yet: the first content that
    /// belongs to it does.
    round_pending: bool,
    /// An inbound block has already arrived on this turn, so the next one is a
    /// continuation's mail rather than the task the run opened with.
    saw_inbound: bool,
}

/// The items one attempt is building.
#[derive(Default)]
struct Live {
    /// Content-block index → the item it is building.
    blocks: HashMap<usize, ItemId>,
    /// Tool-call id → the item it is building.
    tools: HashMap<String, ItemId>,
    order: Vec<ItemId>,
    items: HashMap<ItemId, Item>,
    deltas: HashMap<ItemId, u64>,
}

impl Live {
    fn take_order(&mut self) -> Vec<ItemId> {
        self.blocks.clear();
        self.tools.clear();
        self.items.clear();
        self.deltas.clear();
        std::mem::take(&mut self.order)
    }
}

/// The turns of one session, owned by the actor.
pub(crate) struct TurnRegistry {
    turns: HashMap<TurnId, Record>,
    active: HashMap<ConvKey, TurnId>,
    /// The turns somebody asked to stop, kept for the epoch so a runner that
    /// checks late still sees the request.
    interrupting: std::collections::HashSet<TurnId>,
    view: watch::Sender<Arc<TurnView>>,
}

/// Build the registry and the handle everything reaches it by.
pub(crate) fn attach(control: mpsc::UnboundedSender<Control>) -> (TurnRegistry, TurnHandle) {
    let (view, reader) = watch::channel(Arc::new(TurnView::default()));
    (
        TurnRegistry {
            turns: HashMap::new(),
            active: HashMap::new(),
            interrupting: std::collections::HashSet::new(),
            view,
        },
        TurnHandle {
            control,
            view: reader,
        },
    )
}

impl TurnRegistry {
    /// Apply one message and say what it changed.
    pub(crate) fn handle(&mut self, message: TurnMsg, mint: &mut IdMint) -> Vec<TurnChange> {
        let changes = self.apply(message, mint);
        self.publish();
        changes
    }

    /// Everything still running, closed with one status. The shutdown path: an
    /// interrupted turn is still a turn that ended, and a client that saw
    /// `turn/started` is owed its terminal event.
    pub(crate) fn close_all(&mut self, status: TurnStatus) -> Vec<TurnChange> {
        let running: Vec<TurnId> = self.active.values().cloned().collect();
        let mut changes = Vec::new();
        for turn in running {
            changes.extend(self.close(&turn, status, None));
        }
        self.publish();
        changes
    }

    pub(crate) fn is_busy(&self, conversation: &ConvKey) -> bool {
        self.active.contains_key(conversation)
    }

    /// Ask a running turn to stop. The terminal event still comes from whoever
    /// closes it, so a client that saw `turn/started` is owed exactly one end
    /// whether it was interrupted or not.
    pub(crate) fn interrupt(&mut self, turn: &TurnId) -> Interrupted {
        if !self.turns.contains_key(turn) {
            return Interrupted::Unknown;
        }
        if !self.active.values().any(|active| active == turn) {
            return Interrupted::Already;
        }
        self.interrupting.insert(turn.clone());
        self.publish();
        Interrupted::Asked
    }

    pub(crate) fn active_turns(&self) -> Vec<Turn> {
        self.active
            .values()
            .filter_map(|id| self.turns.get(id))
            .map(|record| record.turn.clone())
            .collect()
    }

    fn publish(&mut self) {
        let view = TurnView {
            active: self.active.clone(),
            interrupting: self.interrupting.clone(),
        };
        let _ = self.view.send(Arc::new(view));
    }

    fn apply(&mut self, message: TurnMsg, mint: &mut IdMint) -> Vec<TurnChange> {
        match message {
            TurnMsg::Open {
                conversation,
                origin,
                input_items,
                reply,
            } => {
                if self.active.contains_key(&conversation) {
                    let _ = reply.send(None);
                    return Vec::new();
                }
                let id: TurnId = mint.mint();
                let turn = Turn {
                    id: id.clone(),
                    // The actor answers in keys and fills the identifier in when
                    // it publishes; `conversation_id` is stamped there.
                    conversation_id: crate::app::ids::ConversationId::new(""),
                    status: TurnStatus::Running,
                    origin,
                    round: 0,
                    input_item_ids: input_items,
                    started_at: now_millis(),
                    completed_at: None,
                    usage: None,
                    error: None,
                };
                self.turns.insert(
                    id.clone(),
                    Record {
                        conversation: conversation.clone(),
                        turn: turn.clone(),
                        claimed: false,
                        live: Live::default(),
                        round_pending: true,
                        saw_inbound: false,
                    },
                );
                self.active.insert(conversation.clone(), id.clone());
                self.publish();
                let _ = reply.send(Some(id));
                vec![TurnChange::Started { conversation, turn }]
            }
            TurnMsg::Claim { turn, reply } => {
                let claimed = match self.turns.get_mut(&turn) {
                    Some(record) if !record.claimed => {
                        record.claimed = true;
                        true
                    }
                    _ => false,
                };
                let _ = reply.send(claimed);
                Vec::new()
            }
            TurnMsg::Report { turn, event } => self.report(&turn, *event, mint),
            TurnMsg::Close {
                turn,
                status,
                error,
            } => self.close(&turn, status, error),
            TurnMsg::Active {
                conversation,
                reply,
            } => {
                let _ = reply.send(self.active.get(&conversation).cloned());
                Vec::new()
            }
            TurnMsg::Commit {
                conversation,
                turn,
                body,
                reply,
            } => {
                let id: ItemId = mint.mint();
                let now = now_millis();
                let item = Item {
                    id: id.clone(),
                    status: ItemStatus::Completed,
                    turn_id: turn.clone(),
                    started_at: Some(now),
                    completed_at: Some(now),
                    body: *body,
                };
                let _ = reply.send(id);
                vec![TurnChange::ItemCompleted {
                    conversation,
                    turn,
                    item: Box::new(item),
                }]
            }
            #[cfg(test)]
            TurnMsg::Probe { turn, reply } => {
                let _ = reply.send(self.turns.get(&turn).map(|record| record.turn.clone()));
                Vec::new()
            }
        }
    }

    /// The first terminal state wins, and every later one — including the guard's
    /// `Drop` after a normal finish — is dropped rather than published.
    fn close(
        &mut self,
        turn: &TurnId,
        status: TurnStatus,
        error: Option<TurnError>,
    ) -> Vec<TurnChange> {
        let Some(record) = self.turns.get_mut(turn) else {
            return Vec::new();
        };
        if record.turn.status.is_terminal() {
            return Vec::new();
        }
        let mut changes = Vec::new();
        // A turn does not end with items still streaming: whatever the run left
        // open is closed before the turn's own terminal event.
        changes.extend(finish_live(record, ItemStatus::Cancelled));
        record.turn.status = status;
        record.turn.completed_at = Some(now_millis());
        record.turn.error = error.filter(|_| status == TurnStatus::Failed);
        self.active.remove(&record.conversation);
        changes.push(TurnChange::Completed {
            conversation: record.conversation.clone(),
            turn: record.turn.clone(),
        });
        changes
    }

    fn report(&mut self, turn: &TurnId, event: EngineEvent, mint: &mut IdMint) -> Vec<TurnChange> {
        let Some(record) = self.turns.get_mut(turn) else {
            return Vec::new();
        };
        if record.turn.status.is_terminal() {
            return Vec::new();
        }
        let conversation = record.conversation.clone();
        let mut changes = Vec::new();
        match event {
            EngineEvent::TextDelta { index, text } => {
                changes.extend(open_round(record));
                let id = block_item(record, mint, index, |_| ItemBody::AssistantMessage {
                    text: String::new(),
                });
                if record.live.items.get(&id).is_some_and(is_fresh) {
                    changes.push(started(record, &id));
                }
                if let Some(Item {
                    body: ItemBody::AssistantMessage { text: body },
                    ..
                }) = record.live.items.get_mut(&id)
                {
                    body.push_str(&text);
                }
                let delta_seq = bump(record, &id);
                changes.push(TurnChange::ItemTextDelta {
                    conversation,
                    turn: Some(turn.clone()),
                    item: id,
                    delta_seq,
                    delta: text,
                });
            }
            EngineEvent::ThinkingDelta { index, thinking } => {
                changes.extend(open_round(record));
                let id = block_item(record, mint, index, |_| ItemBody::Reasoning {
                    text: String::new(),
                });
                if record.live.items.get(&id).is_some_and(is_fresh) {
                    changes.push(started(record, &id));
                }
                if let Some(Item {
                    body: ItemBody::Reasoning { text: body },
                    ..
                }) = record.live.items.get_mut(&id)
                {
                    body.push_str(&thinking);
                }
                let delta_seq = bump(record, &id);
                changes.push(TurnChange::ItemReasoningDelta {
                    conversation,
                    turn: Some(turn.clone()),
                    item: id,
                    delta_seq,
                    delta: thinking,
                });
            }
            // Arguments arriving in pieces are not an item's content: the call is
            // announced when it starts and carries its resolved input when it is
            // ready.
            EngineEvent::ToolInputDelta { .. } => {}
            // The tail belongs to the one shell call that is running. Which one
            // that is needs no guess: the foreground slot holds one command at a
            // time, so at most one shell item is open when a sample arrives, and
            // a sample that arrives with none is a promoted command's last —
            // dropped, because a promoted command owns no rows any more.
            EngineEvent::CommandTail(tail) => {
                if let Some(item) = foreground_item(record) {
                    changes.push(TurnChange::ItemCommandTail {
                        conversation,
                        turn: turn.clone(),
                        item,
                        tail: crate::app::snapshot::CommandTail {
                            lines: tail.lines,
                            total_lines: tail.total_lines as u64,
                        },
                    });
                }
            }
            EngineEvent::ToolUseStarted { index, id, name } => {
                changes.extend(open_round(record));
                let item = tool_item(record, mint, &id, &name);
                record.live.blocks.insert(index, item.clone());
                changes.push(started(record, &item));
            }
            EngineEvent::ToolReady {
                tool_call_id,
                name,
                input,
                standalone: _,
            } => {
                changes.extend(open_round(record));
                let fresh = !record.live.tools.contains_key(&tool_call_id);
                let item = tool_item(record, mint, &tool_call_id, &name);
                if fresh {
                    changes.push(started(record, &item));
                }
                if let Some(Item {
                    body: ItemBody::ToolCall { input: held, .. },
                    ..
                }) = record.live.items.get_mut(&item)
                {
                    *held = input;
                }
                changes.push(updated(record, &item));
            }
            EngineEvent::ToolDone(done) => {
                changes.extend(open_round(record));
                let fresh = !record.live.tools.contains_key(&done.tool_call_id);
                let item = tool_item(record, mint, &done.tool_call_id, &done.name);
                if fresh {
                    changes.push(started(record, &item));
                }
                if let Some(held) = record.live.items.get_mut(&item) {
                    held.status = match done.status {
                        ToolCallStatus::Done => ItemStatus::Completed,
                        ToolCallStatus::Error => ItemStatus::Failed,
                        ToolCallStatus::Interrupted => ItemStatus::Cancelled,
                    };
                    held.completed_at = Some(now_millis());
                    if let ItemBody::ToolCall {
                        summary,
                        output,
                        duration_ms,
                        diff,
                        ..
                    } = &mut held.body
                    {
                        *summary = done.summary;
                        *output = done.output;
                        *duration_ms = done.duration_ms;
                        *diff = done.diff;
                    }
                }
                changes.push(completed(record, &item));
                retire(record, &item);
            }
            EngineEvent::StopReason { output_tokens, .. } => {
                changes.extend(open_round(record));
                if let Some(tokens) = output_tokens {
                    let usage = TurnUsage {
                        output_tokens: tokens,
                        authoritative: true,
                        ..record.turn.usage.unwrap_or_default()
                    };
                    record.turn.usage = Some(usage);
                    changes.push(TurnChange::Usage {
                        conversation,
                        turn: turn.clone(),
                        usage,
                        context: None,
                    });
                }
            }
            EngineEvent::ContextUsage(usage) => {
                let context = ContextUsage {
                    used: usage.used,
                    window: usage.window,
                    trigger: usage.trigger,
                };
                let turn_usage = record.turn.usage.unwrap_or_default();
                changes.push(TurnChange::Usage {
                    conversation,
                    turn: turn.clone(),
                    usage: turn_usage,
                    context: Some(context),
                });
            }
            EngineEvent::StreamRetry {
                attempt,
                max_attempts,
                delay_ms,
                discarded_output: _,
                code,
                reason,
            } => {
                let removed = record.live.take_order();
                changes.push(TurnChange::Retrying {
                    conversation,
                    turn: turn.clone(),
                    round: record.turn.round.max(1),
                    attempt,
                    max_attempts,
                    delay_ms,
                    removed,
                    code,
                    reason,
                });
            }
            EngineEvent::RoundEnd => {
                changes.extend(finish_live(record, ItemStatus::Completed));
                let round = record.turn.round.max(1);
                record.turn.round = round;
                record.round_pending = true;
                changes.push(TurnChange::RoundCompleted {
                    conversation,
                    turn: turn.clone(),
                    round,
                    usage: record.turn.usage,
                });
            }
            EngineEvent::Inbound(text) => {
                let first = !record.saw_inbound && record.turn.round == 0;
                record.saw_inbound = true;
                // A turn opened with an input item already *has* the prompt in
                // the log — the core committed it before the turn existed, which
                // is what let the submission's reply name it. The run reports the
                // same prose back as it starts; committing it again would put the
                // user's line in the conversation twice.
                if first && !record.turn.input_item_ids.is_empty() {
                    return changes;
                }
                changes.push(TurnChange::Inbound {
                    conversation,
                    turn: turn.clone(),
                    text,
                    first,
                });
            }
            EngineEvent::Warning(text) => changes.push(TurnChange::Warning {
                conversation,
                turn: turn.clone(),
                text,
            }),
        }
        changes
    }
}

/// Announce the round the first content of an attempt belongs to. The engine has
/// no round-start event of its own — a round starts when a request goes out, and
/// what proves it went out is the first thing that came back.
fn open_round(record: &mut Record) -> Option<TurnChange> {
    if !record.round_pending {
        return None;
    }
    record.round_pending = false;
    record.turn.round = record.turn.round.saturating_add(1);
    Some(TurnChange::RoundStarted {
        conversation: record.conversation.clone(),
        turn: record.turn.id.clone(),
        round: record.turn.round,
    })
}

fn is_fresh(item: &Item) -> bool {
    match &item.body {
        ItemBody::AssistantMessage { text } | ItemBody::Reasoning { text } => text.is_empty(),
        _ => false,
    }
}

fn block_item(
    record: &mut Record,
    mint: &mut IdMint,
    index: usize,
    body: impl FnOnce(usize) -> ItemBody,
) -> ItemId {
    if let Some(id) = record.live.blocks.get(&index) {
        return id.clone();
    }
    let id: ItemId = mint.mint();
    record.live.blocks.insert(index, id.clone());
    record.live.order.push(id.clone());
    record.live.items.insert(
        id.clone(),
        Item {
            id: id.clone(),
            status: ItemStatus::Streaming,
            turn_id: Some(record.turn.id.clone()),
            started_at: Some(now_millis()),
            completed_at: None,
            body: body(index),
        },
    );
    id
}

fn tool_item(record: &mut Record, mint: &mut IdMint, call: &str, name: &str) -> ItemId {
    if let Some(id) = record.live.tools.get(call) {
        return id.clone();
    }
    let id: ItemId = mint.mint();
    record.live.tools.insert(call.to_string(), id.clone());
    record.live.order.push(id.clone());
    record.live.items.insert(
        id.clone(),
        Item {
            id: id.clone(),
            status: ItemStatus::Pending,
            turn_id: Some(record.turn.id.clone()),
            started_at: Some(now_millis()),
            completed_at: None,
            body: ItemBody::ToolCall {
                tool_call_id: call.to_string(),
                name: name.to_string(),
                input: serde_json::Value::Null,
                summary: String::new(),
                output: String::new(),
                duration_ms: 0,
                diff: None,
                artifact: None,
            },
        },
    );
    id
}

/// The shell call the foreground tail belongs to: the newest `Bash` item this
/// attempt still has open.
///
/// Newest rather than only, because a retry can leave an earlier attempt's item
/// in `order` — the search is over what is open, and the last one to open is the
/// one that is running.
fn foreground_item(record: &Record) -> Option<ItemId> {
    record.live.order.iter().rev().find_map(|id| {
        let item = record.live.items.get(id)?;
        match &item.body {
            ItemBody::ToolCall { name, .. }
                if name == "Bash"
                    && matches!(item.status, ItemStatus::Pending | ItemStatus::Streaming) =>
            {
                Some(id.clone())
            }
            _ => None,
        }
    })
}

fn snapshot(record: &Record, id: &ItemId) -> Box<Item> {
    Box::new(record.live.items.get(id).cloned().unwrap_or_else(|| Item {
        id: id.clone(),
        status: ItemStatus::Cancelled,
        turn_id: Some(record.turn.id.clone()),
        started_at: None,
        completed_at: Some(now_millis()),
        body: ItemBody::AssistantMessage {
            text: String::new(),
        },
    }))
}

fn started(record: &Record, id: &ItemId) -> TurnChange {
    TurnChange::ItemStarted {
        conversation: record.conversation.clone(),
        turn: Some(record.turn.id.clone()),
        item: snapshot(record, id),
    }
}

fn updated(record: &Record, id: &ItemId) -> TurnChange {
    TurnChange::ItemUpdated {
        conversation: record.conversation.clone(),
        turn: Some(record.turn.id.clone()),
        item: snapshot(record, id),
    }
}

fn completed(record: &Record, id: &ItemId) -> TurnChange {
    TurnChange::ItemCompleted {
        conversation: record.conversation.clone(),
        turn: Some(record.turn.id.clone()),
        item: snapshot(record, id),
    }
}

/// An item that reached its terminal snapshot leaves the live set: a retry
/// withdraws what is still in flight, never what is already final.
fn retire(record: &mut Record, id: &ItemId) {
    record.live.items.remove(id);
    record.live.deltas.remove(id);
    record.live.order.retain(|held| held != id);
    record.live.blocks.retain(|_, held| held != id);
    record.live.tools.retain(|_, held| held != id);
}

fn bump(record: &mut Record, id: &ItemId) -> u64 {
    let counter = record.live.deltas.entry(id.clone()).or_insert(0);
    *counter = counter.saturating_add(1);
    *counter
}

/// Close everything the attempt still holds open, in the order it was created.
fn finish_live(record: &mut Record, status: ItemStatus) -> Vec<TurnChange> {
    let open: Vec<ItemId> = record.live.order.clone();
    let mut changes = Vec::new();
    for id in open {
        if let Some(item) = record.live.items.get_mut(&id) {
            item.status = status;
            item.completed_at = Some(now_millis());
        }
        changes.push(completed(record, &id));
        retire(record, &id);
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;

    fn registry() -> (TurnRegistry, mpsc::UnboundedSender<Control>, IdMint) {
        let (control, _inbox) = mpsc::unbounded_channel();
        let (registry, _handle) = attach(control.clone());
        (registry, control, IdMint::new(EpochId::mint()))
    }

    fn open(registry: &mut TurnRegistry, mint: &mut IdMint) -> TurnId {
        let (reply, answer) = oneshot::channel();
        registry.handle(
            TurnMsg::Open {
                conversation: ConvKey::Main,
                origin: TurnOrigin::User,
                input_items: Vec::new(),
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
            .unwrap_or_else(|| panic!("the conversation was idle"))
    }

    fn terminal(changes: &[TurnChange]) -> Vec<TurnStatus> {
        changes
            .iter()
            .filter_map(|change| match change {
                TurnChange::Completed { turn, .. } => Some(turn.status),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_conversation_runs_one_turn_at_a_time() {
        let (mut registry, _control, mut mint) = registry();
        let first = open(&mut registry, &mut mint);
        let (reply, answer) = oneshot::channel();
        registry.handle(
            TurnMsg::Open {
                conversation: ConvKey::Main,
                origin: TurnOrigin::User,
                input_items: Vec::new(),
                reply,
            },
            &mut mint,
        );
        assert_eq!(
            answer
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            None,
            "a second turn on a busy conversation is refused rather than queued here"
        );
        registry.handle(
            TurnMsg::Close {
                turn: first,
                status: TurnStatus::Completed,
                error: None,
            },
            &mut mint,
        );
        let (reply, answer) = oneshot::channel();
        registry.handle(
            TurnMsg::Open {
                conversation: ConvKey::Main,
                origin: TurnOrigin::User,
                input_items: Vec::new(),
                reply,
            },
            &mut mint,
        );
        assert!(
            answer
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}"))
                .is_some(),
            "the conversation is free again once its turn ended"
        );
    }

    /// The v1 bug this closes: an error was raised and the turn never ended.
    #[test]
    fn a_turn_reaches_exactly_one_terminal_state() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        let first = registry.handle(
            TurnMsg::Close {
                turn: turn.clone(),
                status: TurnStatus::Completed,
                error: None,
            },
            &mut mint,
        );
        assert_eq!(terminal(&first), vec![TurnStatus::Completed]);
        for status in [TurnStatus::Failed, TurnStatus::Interrupted] {
            let again = registry.handle(
                TurnMsg::Close {
                    turn: turn.clone(),
                    status,
                    error: None,
                },
                &mut mint,
            );
            assert!(
                terminal(&again).is_empty(),
                "the first terminal state wins; {status:?} arrived after it"
            );
        }
    }

    #[test]
    fn one_run_claims_a_turn_and_the_next_is_refused() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        let mut claim = |turn: TurnId| {
            let (reply, answer) = oneshot::channel();
            registry.handle(TurnMsg::Claim { turn, reply }, &mut mint);
            answer
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}"))
        };
        assert!(claim(turn.clone()), "the run that opened it claims it");
        assert!(
            !claim(turn),
            "a second run on one host would report into a turn it does not own"
        );
    }

    /// A retry withdraws exactly what the failed attempt produced, and the next
    /// attempt uses new identifiers.
    #[test]
    fn a_retry_withdraws_the_attempt_it_failed_on() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        registry.handle(
            TurnMsg::Report {
                turn: turn.clone(),
                event: Box::new(EngineEvent::TextDelta {
                    index: 0,
                    text: "half a th".to_string(),
                }),
            },
            &mut mint,
        );
        let changes = registry.handle(
            TurnMsg::Report {
                turn: turn.clone(),
                event: Box::new(EngineEvent::StreamRetry {
                    attempt: 1,
                    max_attempts: 10,
                    delay_ms: 200,
                    discarded_output: true,
                    code: None,
                    reason: Some("connection reset".to_string()),
                }),
            },
            &mut mint,
        );
        let removed = changes
            .iter()
            .find_map(|change| match change {
                TurnChange::Retrying { removed, .. } => Some(removed.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a retry says what it withdrew"));
        assert_eq!(removed.len(), 1, "the one item the attempt had started");

        let next = registry.handle(
            TurnMsg::Report {
                turn,
                event: Box::new(EngineEvent::TextDelta {
                    index: 0,
                    text: "a whole thought".to_string(),
                }),
            },
            &mut mint,
        );
        let started = next
            .iter()
            .find_map(|change| match change {
                TurnChange::ItemStarted { item, .. } => Some(item.id.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the next attempt opens an item of its own"));
        assert!(
            !removed.contains(&started),
            "a removed identifier is never reused"
        );
    }

    /// Deltas are contiguous per item, and an item that ends is authoritative.
    #[test]
    fn deltas_are_contiguous_and_the_round_closes_what_it_opened() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        let mut seqs = Vec::new();
        for text in ["a", "b", "c"] {
            let changes = registry.handle(
                TurnMsg::Report {
                    turn: turn.clone(),
                    event: Box::new(EngineEvent::TextDelta {
                        index: 0,
                        text: text.to_string(),
                    }),
                },
                &mut mint,
            );
            for change in &changes {
                if let TurnChange::ItemTextDelta { delta_seq, .. } = change {
                    seqs.push(*delta_seq);
                }
            }
        }
        assert_eq!(seqs, vec![1, 2, 3]);
        let closing = registry.handle(
            TurnMsg::Report {
                turn,
                event: Box::new(EngineEvent::RoundEnd),
            },
            &mut mint,
        );
        let final_text = closing
            .iter()
            .find_map(|change| match change {
                TurnChange::ItemCompleted { item, .. } => match &item.body {
                    ItemBody::AssistantMessage { text } => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("the round closes the message it opened"));
        assert_eq!(
            final_text, "abc",
            "the terminal snapshot is authoritative over every delta before it"
        );
    }

    #[test]
    fn a_round_announces_itself_when_its_first_content_arrives() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        let first = registry.handle(
            TurnMsg::Report {
                turn: turn.clone(),
                event: Box::new(EngineEvent::TextDelta {
                    index: 0,
                    text: "one".to_string(),
                }),
            },
            &mut mint,
        );
        assert!(
            matches!(
                first.first(),
                Some(TurnChange::RoundStarted { round: 1, .. })
            ),
            "the round starts before the content that proves it started"
        );
        registry.handle(
            TurnMsg::Report {
                turn: turn.clone(),
                event: Box::new(EngineEvent::RoundEnd),
            },
            &mut mint,
        );
        let second = registry.handle(
            TurnMsg::Report {
                turn,
                event: Box::new(EngineEvent::TextDelta {
                    index: 1,
                    text: "two".to_string(),
                }),
            },
            &mut mint,
        );
        assert!(
            second
                .iter()
                .any(|change| matches!(change, TurnChange::RoundStarted { round: 2, .. })),
            "the next round is announced by the content that belongs to it"
        );
    }

    /// A turn that is dropped mid-stream still closes its items before its own
    /// terminal event.
    #[test]
    fn closing_a_turn_settles_the_items_it_left_open() {
        let (mut registry, _control, mut mint) = registry();
        let turn = open(&mut registry, &mut mint);
        registry.handle(
            TurnMsg::Report {
                turn: turn.clone(),
                event: Box::new(EngineEvent::TextDelta {
                    index: 0,
                    text: "cut off".to_string(),
                }),
            },
            &mut mint,
        );
        let changes = registry.handle(
            TurnMsg::Close {
                turn,
                status: TurnStatus::Interrupted,
                error: None,
            },
            &mut mint,
        );
        let statuses: Vec<ItemStatus> = changes
            .iter()
            .filter_map(|change| match change {
                TurnChange::ItemCompleted { item, .. } => Some(item.status),
                _ => None,
            })
            .collect();
        assert_eq!(statuses, vec![ItemStatus::Cancelled]);
        assert!(
            matches!(changes.last(), Some(TurnChange::Completed { .. })),
            "the turn's terminal event comes last"
        );
    }
}

/// The same guarantees seen from outside, through a running actor: an abandoned
/// run, an aborted task and a failing run each publish one terminal event and no
/// more.
#[cfg(test)]
mod actor_tests {
    use super::*;
    use crate::app::command::AppQuery;
    use crate::app::event::AppEventPayload;
    use crate::app::snapshot::SessionSnapshot;
    use crate::app::{
        AppCore, AppFrame, AppReply, AppRequest, AttachRequest, RequestId, SessionSetup,
    };

    async fn attached(core: &AppCore) -> (crate::app::AppLink, SessionSnapshot) {
        let mut link = core
            .attach(AttachRequest::new("test"))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        link.request(AppRequest::Query {
            id: RequestId(1),
            query: AppQuery::ReadSession,
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        match link.recv().await {
            Some(AppFrame::Reply {
                result: Ok(AppReply::Session(snapshot)),
                ..
            }) => (link, *snapshot),
            other => panic!("expected a session snapshot, got {other:?}"),
        }
    }

    /// Every turn event this link saw, in order, once the actor has settled.
    /// Everything the core said, minus the conversation summaries.
    ///
    /// A summary is republished whenever anything in the conversation moves, so
    /// it accompanies most of what these tests assert. It belongs to the
    /// attention family rather than the turn's, and it is asserted where it is
    /// the subject (`app::attention`, `app::controller`).
    async fn drain(link: &mut crate::app::AppLink) -> Vec<AppEventPayload> {
        let mut seen = Vec::new();
        while let Ok(Some(frame)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), link.recv()).await
        {
            if let AppFrame::Event(event) = frame
                && !matches!(
                    event.payload,
                    AppEventPayload::ConversationCreated(_)
                        | AppEventPayload::ConversationUpdated(_)
                )
            {
                seen.push(event.payload);
            }
        }
        seen
    }

    fn statuses(events: &[AppEventPayload]) -> Vec<TurnStatus> {
        events
            .iter()
            .filter_map(|event| match event {
                AppEventPayload::TurnCompleted(changed) => Some(changed.turn.status),
                _ => None,
            })
            .collect()
    }

    /// A run whose task is aborted executes no line of its own. The guard's
    /// `Drop` is the only thing left, and it is enough.
    #[tokio::test]
    async fn an_aborted_run_still_closes_its_turn_exactly_once() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core).await;
        let turns = core.turns();
        let id = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        let guard = turns.guard(id.clone(), TurnStatus::Failed);
        let run = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        run.abort();
        let _ = run.await;

        let events = drain(&mut link).await;
        assert_eq!(
            statuses(&events),
            vec![TurnStatus::Failed],
            "one terminal event, and it is the guard's: {events:?}"
        );
        assert!(
            !turns.view().is_busy(&ConvKey::Main),
            "the conversation is free again"
        );
    }

    /// The v1 bug, stated as a test: a run that fails raises an error *and*
    /// closes its turn, and the error is never the closing.
    #[tokio::test]
    async fn an_error_does_not_substitute_for_the_terminal_event() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core).await;
        let turns = core.turns();
        let id = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        let guard = turns.guard(id.clone(), TurnStatus::Failed);
        guard.finish(
            TurnStatus::Failed,
            Some(TurnError {
                code: "API_ERROR".to_string(),
                message: "the provider hung up".to_string(),
            }),
        );
        // The console raises its own error afterwards; the actor hears about the
        // turn once more only through a second close, which changes nothing.
        turns.close(id, TurnStatus::Completed, None);

        let events = drain(&mut link).await;
        assert_eq!(statuses(&events), vec![TurnStatus::Failed]);
        let error = events.iter().find_map(|event| match event {
            AppEventPayload::TurnCompleted(changed) => changed.turn.error.clone(),
            _ => None,
        });
        assert_eq!(
            error.map(|error| error.code),
            Some("API_ERROR".to_string()),
            "the failure travels on the terminal event rather than instead of it"
        );
    }

    /// One host, one run: the second claim on a turn is refused rather than
    /// allowed to interleave a second attempt into one item stream.
    #[tokio::test]
    async fn a_turn_is_claimed_by_one_run() {
        let core = AppCore::start(SessionSetup::default());
        let turns = core.turns();
        let id = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        assert!(turns.claim(id.clone()).await);
        assert!(!turns.claim(id).await);
    }

    /// A stream retry replaces the live tail at its checkpoint: the identifiers
    /// it removed travel with the event, and no removed item is ever reused.
    #[tokio::test]
    async fn a_retry_publishes_the_checkpoint_it_replaced() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core).await;
        let turns = core.turns();
        let id = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        turns.report_event(
            id.clone(),
            EngineEvent::TextDelta {
                index: 0,
                text: "half a th".to_string(),
            },
        );
        turns.report_event(
            id.clone(),
            EngineEvent::StreamRetry {
                attempt: 2,
                max_attempts: 10,
                delay_ms: 250,
                discarded_output: true,
                code: None,
                reason: Some("connection reset".to_string()),
            },
        );
        turns.close(id, TurnStatus::Completed, None);

        let events = drain(&mut link).await;
        let retry = events
            .iter()
            .find_map(|event| match event {
                AppEventPayload::TurnRetrying(retrying) => Some(retrying.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a retry is announced: {events:?}"));
        assert_eq!(
            (retry.attempt, retry.max_attempts, retry.delay_ms),
            (2, 10, 250)
        );
        assert_eq!(retry.removed_item_ids.len(), 1);
        let started: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AppEventPayload::ItemStarted(changed) => Some(changed.item.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            started, retry.removed_item_ids,
            "it withdrew what it started"
        );
    }
    /// The provider's own count and the turn's context measurement both arrive
    /// as `turn/usageUpdated`, and neither is recomputed downstream.
    #[tokio::test]
    async fn usage_and_context_reach_the_turn_that_spent_them() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core).await;
        let turns = core.turns();
        let id = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        turns.report_event(
            id.clone(),
            EngineEvent::StopReason {
                stop_reason: Some("end_turn".to_string()),
                output_tokens: Some(4096),
            },
        );
        turns.report_event(
            id.clone(),
            EngineEvent::ContextUsage(crate::context_usage::ContextUsage::new(
                12_345, 128_000, 100_000,
            )),
        );
        turns.close(id, TurnStatus::Completed, None);

        let events = drain(&mut link).await;
        let usage: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AppEventPayload::TurnUsageUpdated(updated) => Some(updated.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 2, "one per measurement: {events:?}");
        assert_eq!(usage[0].usage.output_tokens, 4096);
        assert!(
            usage[0].usage.authoritative,
            "the provider's own count outranks any local estimate"
        );
        let context = usage[1]
            .context_usage
            .unwrap_or_else(|| panic!("the context measurement travels whole"));
        assert_eq!(
            (context.used, context.window, context.trigger),
            (12_345, 128_000, 100_000)
        );
    }

    /// A compaction is an item in the conversation whose history it replaced,
    /// carrying the compactor's own numbers (plan decision 9).
    #[tokio::test]
    async fn a_compaction_enters_the_conversation_as_an_item() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core).await;
        let id = core
            .turns()
            .commit_item(
                ConvKey::Main,
                None,
                ItemBody::Compaction {
                    before_tokens: 90_000,
                    after_tokens: 12_000,
                    replaced_messages: 42,
                    duration_ms: 1_500,
                },
            )
            .await;
        let events = drain(&mut link).await;
        match events.as_slice() {
            [AppEventPayload::ItemCompleted(changed)] => {
                assert_eq!(changed.item.id, id);
                assert_eq!(changed.item.status, ItemStatus::Completed);
                assert!(matches!(
                    changed.item.body,
                    ItemBody::Compaction {
                        before_tokens: 90_000,
                        after_tokens: 12_000,
                        replaced_messages: 42,
                        duration_ms: 1_500,
                    }
                ));
            }
            other => panic!("expected one completed item, got {other:?}"),
        }
    }

    /// Closing a session settles everything open and leaves nothing running: the
    /// turn reaches a terminal state, the prompt fails closed, and the actor's
    /// thread ends once the handles that kept it reachable are gone (D29's cycle,
    /// which B2b left standing).
    #[tokio::test]
    async fn closing_a_session_settles_what_is_open_and_ends_the_thread() {
        use crate::app::interaction::{OpenPrompt, Verdict};
        use crate::app::snapshot::{InteractionPrompt, PermissionDecisionKind, ToolRequest};

        let core = AppCore::start(SessionSetup::default());
        assert!(core.is_running(), "the session runs on a thread of its own");
        let (mut link, _) = attached(&core).await;
        let turns = core.turns();
        let _turn = turns
            .open(ConvKey::Main, TurnOrigin::User, Vec::new())
            .await
            .unwrap_or_else(|| panic!("main was idle"));
        let verdict = core.interactions().open(OpenPrompt {
            conversation: ConvKey::Main,
            turn: None,
            item: None,
            prompt: InteractionPrompt::Permission {
                title: "Allow running Bash".to_string(),
                reason: None,
                tool: ToolRequest {
                    name: "Bash".to_string(),
                    input: serde_json::Value::Null,
                },
                preview: None,
                decisions: vec![
                    PermissionDecisionKind::AllowOnce,
                    PermissionDecisionKind::Deny,
                ],
                session_scope: None,
                allows_feedback: true,
            },
        });

        core.close().await;
        assert_eq!(
            verdict.await,
            Ok(Verdict::Cancelled),
            "an unanswered prompt fails closed rather than hanging its run"
        );

        let events = drain(&mut link).await;
        assert_eq!(
            statuses(&events),
            vec![TurnStatus::Interrupted],
            "the running turn still reaches a terminal state: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEventPayload::SessionClosed(_))),
            "and the session says it closed"
        );

        // Nothing can be started in a session that is over.
        assert_eq!(
            core.attach(AttachRequest::new("late")).await.err(),
            Some(crate::app::AppError::Stopped)
        );
        drop(link);
        for _ in 0..200 {
            if !core.is_running() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the session actor's thread outlived the session");
    }
}
