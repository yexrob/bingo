//! The console's local projection of the core.
//!
//! The core pushes; the renderer pulls. Between them stands this: a client-side
//! reducer that takes one snapshot cut and the ordered `AppEvent` stream that
//! follows it, and materializes them into state a key handler and a render pass
//! can read *synchronously*, which is the only shape those two can read at all.
//!
//! It holds no rules. Every field here is something the core said, stored under
//! the identity the core gave it; a question whose answer needs a judgement —
//! whose turn it is, whether an entry may steer, who owes whom a reply — is
//! asked of the core and never of this. A GUI writes the same object in
//! JavaScript, which is the point: the console reads what a client reads.
//!
//! **Recovery is the protocol's, not ours** (spec "Snapshots and recovery").
//! Events carry a gapless `seq` within one server epoch. When the next frame's
//! `seq` is not the one after the last applied — a frame was dropped because
//! this side fell behind, or the link was replaced — the store does not patch
//! and does not guess: it reads a fresh cut and replaces its state whole. That
//! is the same move a wire client makes, for the same reason.
//!
//! The reducer and the recovery are complete and live: the console attaches on
//! its way into the event loop and folds every frame. The read face is being
//! moved across seam by seam — a reader here without a caller yet is one the
//! console still asks the registries for, and the campaign record (D149) lists
//! which and why.
#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::app::command::{AppCommand, AppQuery};
use crate::app::conversation::ConvKey;
use crate::app::event::{AppEvent, AppEventPayload};
use crate::app::ids::{ConversationId, InteractionId, TurnId};
use crate::app::snapshot::{
    AgentResource, BackgroundCommandResource, Collection, ConfigSnapshot, ConversationKind,
    ConversationSummary, DeliveryResource, Feedback, Interaction, McpServerState, Operation,
    QueueEntry, RoomResource, ServerCapabilities, SessionSnapshot, SessionSummary, TaskResource,
    Turn,
};
use crate::app::{
    AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, RequestId,
};

/// What the core last said, materialized.
///
/// Every collection replaces its named member whole (spec invariant #13), so
/// applying an event is an insert or a removal — never a merge that could differ
/// from what the core holds.
#[derive(Debug, Clone)]
pub struct View {
    pub session: Option<SessionSummary>,
    pub capabilities: Option<ServerCapabilities>,
    pub config: Option<ConfigSnapshot>,
    /// Conversations by the identity the core minted, plus the console's own key
    /// for each, so a page can be looked up either way.
    conversations: BTreeMap<ConversationId, ConversationSummary>,
    keys: BTreeMap<ConversationId, ConvKey>,
    ids: BTreeMap<ConvKey, ConversationId>,
    /// Turns that have not reached their terminal state.
    turns: BTreeMap<TurnId, Turn>,
    /// Prompts waiting for an answer, oldest first.
    interactions: Vec<Interaction>,
    /// Accepted work that is not a turn.
    operations: Vec<Operation>,
    agents: Collection<AgentResource>,
    rooms: Collection<RoomResource>,
    tasks: Collection<TaskResource>,
    deliveries: Collection<DeliveryResource>,
    commands: Collection<BackgroundCommandResource>,
    mcp: Vec<McpServerState>,
    feedback: Vec<Feedback>,
    /// Each conversation's queue as the core reports it, in order.
    queues: BTreeMap<ConversationId, Queue>,
}

/// One conversation's input queue, as the events describe it.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    pub revision: u64,
    pub entries: Vec<QueueEntry>,
}

fn empty<T>() -> Collection<T> {
    Collection {
        revision: 0,
        count: 0,
        active: Vec::new(),
    }
}

impl Default for View {
    fn default() -> Self {
        Self {
            session: None,
            capabilities: None,
            config: None,
            conversations: BTreeMap::new(),
            keys: BTreeMap::new(),
            ids: BTreeMap::new(),
            turns: BTreeMap::new(),
            interactions: Vec::new(),
            operations: Vec::new(),
            agents: empty(),
            rooms: empty(),
            tasks: empty(),
            deliveries: empty(),
            commands: empty(),
            mcp: Vec::new(),
            feedback: Vec::new(),
            queues: BTreeMap::new(),
        }
    }
}

/// The key a summary belongs to.
///
/// The wire names an agent conversation by the agent's opaque id, which a client
/// cannot turn back into a name once the instance is gone; the title is the
/// stable half, and it is spelled exactly as [`ConvKey::title`] spells it. A test
/// keeps the two ends of that round trip together.
fn key_of(summary: &ConversationSummary) -> ConvKey {
    match &summary.kind {
        ConversationKind::Main => ConvKey::Main,
        ConversationKind::Agent { .. } => ConvKey::Agent(
            summary
                .title
                .strip_prefix('@')
                .unwrap_or(&summary.title)
                .to_string(),
        ),
        ConversationKind::Room { .. } => ConvKey::Room(
            summary
                .title
                .strip_prefix('#')
                .unwrap_or(&summary.title)
                .to_string(),
        ),
    }
}

impl View {
    /// Replace everything with one atomic cut.
    fn replace(&mut self, snapshot: SessionSnapshot) {
        let SessionSnapshot {
            session,
            capabilities,
            conversations,
            active_turns,
            interactions,
            operations,
            collections,
            feedback,
            config,
            event_cursor: _,
        } = snapshot;
        self.session = Some(session);
        self.capabilities = Some(capabilities);
        self.config = Some(config);
        self.conversations.clear();
        self.keys.clear();
        self.ids.clear();
        for summary in conversations.active {
            self.remember(summary);
        }
        self.turns = active_turns
            .into_iter()
            .map(|turn| (turn.id.clone(), turn))
            .collect();
        self.interactions = interactions;
        self.operations = operations;
        self.agents = collections.agents;
        self.rooms = collections.rooms;
        self.tasks = collections.tasks;
        self.deliveries = collections.deliveries;
        self.commands = collections.background_commands;
        self.mcp = collections.mcp_servers;
        self.feedback = feedback;
        // The queues are not in the session cut — a conversation carries its
        // revision and its count there, and the rows themselves come from
        // `conversation/readQueue`. Dropping them is honest: what is left is a
        // count this side can still render, and a page that wants the rows asks.
        self.queues.clear();
    }

    fn remember(&mut self, summary: ConversationSummary) {
        let key = key_of(&summary);
        self.keys.insert(summary.id.clone(), key.clone());
        self.ids.insert(key, summary.id.clone());
        self.conversations.insert(summary.id.clone(), summary);
    }

    fn forget(&mut self, id: &ConversationId) {
        if let Some(key) = self.keys.remove(id) {
            self.ids.remove(&key);
        }
        self.conversations.remove(id);
        self.queues.remove(id);
    }

    /// Fold one event in. Nothing here decides anything: a named resource is
    /// replaced, a keyed one removed, a list member appended in the order the
    /// core sequenced it.
    fn apply(&mut self, event: &AppEvent) {
        match &event.payload {
            AppEventPayload::SessionUpdated(changed) => {
                self.session = Some(changed.session.clone());
            }
            AppEventPayload::SessionClosed(_) | AppEventPayload::SessionDeleted(_) => {}
            AppEventPayload::ConversationCreated(changed)
            | AppEventPayload::ConversationUpdated(changed) => {
                self.remember(changed.conversation.clone());
            }
            AppEventPayload::ConversationRemoved(removed) => {
                self.forget(&removed.conversation_id);
            }
            AppEventPayload::TurnStarted(changed) => {
                self.turns
                    .insert(changed.turn.id.clone(), changed.turn.clone());
            }
            AppEventPayload::TurnCompleted(changed) => {
                self.turns.remove(&changed.turn.id);
            }
            AppEventPayload::TurnRoundStarted(round) => {
                if let Some(turn) = self.turns.get_mut(&round.turn_id) {
                    turn.round = round.round;
                }
            }
            AppEventPayload::TurnRetrying(retry) => {
                if let Some(turn) = self.turns.get_mut(&retry.turn_id) {
                    turn.round = retry.round;
                }
            }
            AppEventPayload::TurnRoundCompleted(round) => {
                if let Some(turn) = self.turns.get_mut(&round.turn_id)
                    && let Some(usage) = &round.usage
                {
                    turn.usage = Some(*usage);
                }
            }
            AppEventPayload::TurnUsageUpdated(usage) => {
                if let Some(turn) = self.turns.get_mut(&usage.turn_id) {
                    turn.usage = Some(usage.usage);
                }
            }
            // Items are the transcript's, and the transcript is not projected
            // here: the console builds its rows from the conversation stores it
            // already owns. What the store carries about history is the summary
            // the core publishes beside it (`last_item_id`, `unread`).
            AppEventPayload::ItemStarted(_)
            | AppEventPayload::ItemTextDelta(_)
            | AppEventPayload::ItemReasoningDelta(_)
            | AppEventPayload::ItemCommandTailUpdated(_)
            | AppEventPayload::ItemUpdated(_)
            | AppEventPayload::ItemCompleted(_) => {}
            AppEventPayload::QueueItemAdded(added) => {
                let queue = self
                    .queues
                    .entry(added.conversation_id.clone())
                    .or_default();
                queue.revision = added.revision;
                let at = (added.position as usize).min(queue.entries.len());
                queue.entries.insert(at, added.entry.clone());
            }
            AppEventPayload::QueueItemRemoved(removed) => {
                if let Some(queue) = self.queues.get_mut(&removed.conversation_id) {
                    queue.revision = removed.revision;
                    queue.entries.retain(|entry| entry.id != removed.queue_id);
                }
            }
            AppEventPayload::QueueItemAbsorbed(absorbed) => {
                if let Some(queue) = self.queues.get_mut(&absorbed.conversation_id) {
                    queue.revision = absorbed.revision;
                    queue.entries.retain(|entry| entry.id != absorbed.queue_id);
                }
            }
            AppEventPayload::InteractionOpened(opened) => {
                self.interactions
                    .retain(|open| open.id != opened.interaction.id);
                self.interactions.push(opened.interaction.clone());
            }
            AppEventPayload::InteractionResolved(resolved) => {
                self.interactions
                    .retain(|open| open.id != resolved.interaction_id);
            }
            AppEventPayload::InteractionCancelled(cancelled) => {
                self.interactions
                    .retain(|open| open.id != cancelled.interaction_id);
            }
            AppEventPayload::AgentChanged(changed) => {
                replace_by(&mut self.agents, changed.agent.clone(), |agent| {
                    agent.id.clone()
                });
            }
            AppEventPayload::RoomChanged(changed) => {
                replace_by(&mut self.rooms, changed.room.clone(), |room| {
                    room.id.clone()
                });
            }
            AppEventPayload::TaskChanged(changed) => {
                replace_by(&mut self.tasks, changed.task.clone(), |task| {
                    task.id.clone()
                });
            }
            AppEventPayload::TaskRemoved(removed) => {
                self.tasks.active.retain(|task| task.id != removed.task_id);
                self.tasks.count = self.tasks.active.len() as u32;
            }
            AppEventPayload::DeliveryChanged(changed) => {
                replace_by(&mut self.deliveries, changed.delivery.clone(), |row| {
                    row.id.clone()
                });
            }
            AppEventPayload::CommandChanged(changed) => {
                replace_by(&mut self.commands, changed.command.clone(), |row| {
                    row.id.clone()
                });
            }
            AppEventPayload::OperationStarted(changed) => {
                self.operations
                    .retain(|open| open.id != changed.operation.id);
                self.operations.push(changed.operation.clone());
            }
            AppEventPayload::OperationProgress(progressed) => {
                if let Some(operation) = self
                    .operations
                    .iter_mut()
                    .find(|open| open.id == progressed.operation_id)
                {
                    operation.progress = Some(progressed.progress.clone());
                }
            }
            AppEventPayload::OperationCompleted(changed) => {
                self.operations
                    .retain(|open| open.id != changed.operation.id);
            }
            AppEventPayload::ConfigChanged(changed) => {
                self.config = Some(changed.config.clone());
            }
            // A catalog is read on demand and paginated; the notice says only
            // that a re-read would answer differently.
            AppEventPayload::CatalogChanged(_) | AppEventPayload::AssetAvailable(_) => {}
            AppEventPayload::FeedbackRaised(raised) => {
                self.feedback.retain(|open| open.id != raised.feedback.id);
                self.feedback.push(raised.feedback.clone());
            }
            AppEventPayload::FeedbackCleared(cleared) => {
                self.feedback.retain(|open| open.id != cleared.feedback_id);
            }
        }
    }

    // --- reads ------------------------------------------------------------

    /// This conversation's identity, if the core has named it.
    pub fn id_of(&self, key: &ConvKey) -> Option<&ConversationId> {
        self.ids.get(key)
    }

    /// The console's key for a conversation the core named.
    pub fn key_of(&self, id: &ConversationId) -> Option<&ConvKey> {
        self.keys.get(id)
    }

    pub fn conversation(&self, key: &ConvKey) -> Option<&ConversationSummary> {
        self.ids.get(key).and_then(|id| self.conversations.get(id))
    }

    pub fn conversations(&self) -> impl Iterator<Item = &ConversationSummary> {
        self.conversations.values()
    }

    /// The turn running on this page, if one is.
    pub fn active_turn(&self, key: &ConvKey) -> Option<&Turn> {
        let id = self.ids.get(key)?;
        self.turns.values().find(|turn| &turn.conversation_id == id)
    }

    /// Whether a turn is running on this page. The core's answer, not a flag
    /// this side keeps.
    pub fn is_busy(&self, key: &ConvKey) -> bool {
        self.active_turn(key).is_some()
    }

    pub fn turn(&self, id: &TurnId) -> Option<&Turn> {
        self.turns.get(id)
    }

    pub fn agents(&self) -> &[AgentResource] {
        &self.agents.active
    }

    pub fn agent(&self, name: &str) -> Option<&AgentResource> {
        self.agents.active.iter().find(|agent| agent.name == name)
    }

    pub fn rooms(&self) -> &[RoomResource] {
        &self.rooms.active
    }

    pub fn room(&self, name: &str) -> Option<&RoomResource> {
        self.rooms.active.iter().find(|room| room.name == name)
    }

    pub fn tasks(&self) -> &[TaskResource] {
        &self.tasks.active
    }

    pub fn deliveries(&self) -> &[DeliveryResource] {
        &self.deliveries.active
    }

    pub fn commands(&self) -> &[BackgroundCommandResource] {
        &self.commands.active
    }

    pub fn mcp(&self) -> &[McpServerState] {
        &self.mcp
    }

    pub fn feedback(&self) -> &[Feedback] {
        &self.feedback
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// The prompts waiting for an answer on this page, oldest first.
    pub fn interactions(&self, key: &ConvKey) -> impl Iterator<Item = &Interaction> {
        let id = self.ids.get(key);
        self.interactions
            .iter()
            .filter(move |open| Some(&open.conversation_id) == id)
    }

    pub fn interaction(&self, id: &InteractionId) -> Option<&Interaction> {
        self.interactions.iter().find(|open| &open.id == id)
    }

    /// The queue rows this side has seen for a page. Empty is not "no queue" —
    /// the count on the summary is authoritative, and the rows are read.
    pub fn queue(&self, key: &ConvKey) -> Option<&Queue> {
        self.ids.get(key).and_then(|id| self.queues.get(id))
    }
}

/// Replace one member of a bounded collection, or add it.
fn replace_by<T, K: PartialEq>(into: &mut Collection<T>, value: T, id: impl Fn(&T) -> K) {
    let key = id(&value);
    match into.active.iter().position(|open| id(open) == key) {
        Some(at) => into.active[at] = value,
        None => into.active.push(value),
    }
    into.count = into.active.len() as u32;
    into.revision = into.revision.saturating_add(1);
}

/// One attachment to the core, and what it has made of it.
///
/// The console owns this the way a browser tab owns its store: it is fed by the
/// event loop and read by everything else.
#[derive(Default)]
pub struct Store {
    /// `None` before the console attaches, and in a test with no runtime to
    /// attach on. A detached store is not a second source of truth — it holds
    /// what it was told and nothing else, which is what an attachment that has
    /// heard nothing yet holds too.
    link: Option<AppLink>,
    view: View,
    /// The last `seq` folded in. `None` before the first cut.
    cursor: Option<u64>,
    /// Replies that arrived before their caller asked for them. A frame channel
    /// carries replies and events in one order, so a reply cannot be waited for
    /// without draining the events queued in front of it.
    replies: BTreeMap<RequestId, Result<AppReply, AppError>>,
    next_request: u64,
    /// The link closed. Nothing more will arrive, and nothing more can be sent.
    closed: bool,
    /// A hole was seen and not yet repaired. Set by the synchronous drain,
    /// cleared by the `async` re-read: the two halves of one recovery.
    stale: bool,
    /// How many holes the stream has shown. Diagnostics: a healthy console
    /// never leaves zero.
    pub gaps: u64,
}

impl Store {
    /// Attach to the core and take the first cut.
    pub async fn open(core: &AppCore, label: &str) -> Result<Self, AppError> {
        let mut store = Self::default();
        store.connect(core, label).await?;
        Ok(store)
    }

    /// Attach an existing store, replacing whatever it holds with a fresh cut.
    pub async fn connect(&mut self, core: &AppCore, label: &str) -> Result<(), AppError> {
        self.link = Some(core.attach(AttachRequest::new(label)).await?);
        self.cursor = None;
        self.closed = false;
        self.stale = false;
        self.resync().await
    }

    /// What the core last said.
    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn is_attached(&self) -> bool {
        self.link.is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Read a fresh cut and replace local state with it.
    ///
    /// Everything at or below the new cursor is that snapshot's past, so events
    /// still in the channel from before it are dropped rather than folded in
    /// twice.
    pub async fn resync(&mut self) -> Result<(), AppError> {
        let id = self.mint();
        let pending = self.ask(AppRequest::Query {
            id,
            query: AppQuery::ReadSession,
        })?;
        match self.settle(pending).await? {
            AppReply::Session(snapshot) => {
                self.cursor = Some(snapshot.event_cursor);
                self.view.replace(*snapshot);
                Ok(())
            }
            // The core answers `ReadSession` with a session and nothing else;
            // anything here would be a core that stopped meaning what it says.
            _ => Err(AppError::Stopped),
        }
    }

    /// Fold in everything waiting, without blocking.
    ///
    /// Returns whether anything changed, so a caller that only redraws on change
    /// can ask one question. A gap sets the resync flag rather than acting on it:
    /// re-reading is `async`, and this is the half a render pass can call.
    pub fn pump(&mut self) -> Pumped {
        let mut pumped = Pumped::default();
        let mut taken = Vec::new();
        if let Some(link) = self.link.as_mut() {
            loop {
                match link.frames.try_recv() {
                    Ok(frame) => taken.push(frame),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.closed = true;
                        break;
                    }
                }
            }
        }
        for frame in taken {
            match frame {
                AppFrame::Reply { id, result } => {
                    self.replies.insert(id, result);
                    pumped.changed = true;
                }
                AppFrame::Event(event) => {
                    if self.fold(&event) {
                        pumped.changed = true;
                    } else {
                        pumped.stale = true;
                    }
                }
            }
        }
        pumped.stale |= self.stale;
        pumped
    }

    /// Fold one event in, or refuse it. `false` means the local state is no
    /// longer the core's and has to be re-read.
    fn fold(&mut self, event: &AppEvent) -> bool {
        let Some(cursor) = self.cursor else {
            // No cut yet: the core suppresses events until one is taken, so
            // there is nothing this can legitimately be.
            return false;
        };
        // A merged frame stands for the whole run it names (`coalescedFrom`
        // through `seq`), so continuity is judged from where the run began.
        let begins_at = event.meta.coalesced_from.unwrap_or(event.meta.seq);
        if event.meta.seq <= cursor {
            // Already covered by the cut that was taken after it was sent.
            return true;
        }
        if begins_at != cursor.saturating_add(1) {
            self.gaps = self.gaps.saturating_add(1);
            self.stale = true;
            return false;
        }
        self.cursor = Some(event.meta.seq);
        self.view.apply(event);
        true
    }

    /// The next correlation number.
    fn mint(&mut self) -> RequestId {
        let id = RequestId(self.next_request);
        self.next_request = self.next_request.saturating_add(1);
        id
    }

    /// Send one request and hand back the ticket its reply will carry.
    fn ask(&mut self, request: AppRequest) -> Result<RequestId, AppError> {
        let id = match &request {
            AppRequest::Command { id, .. } | AppRequest::Query { id, .. } => *id,
        };
        self.link
            .as_ref()
            .ok_or(AppError::Stopped)?
            .requests
            .try_send(request)
            .map_err(|_| AppError::Stopped)?;
        Ok(id)
    }

    /// Wait for one reply, folding in every event that arrives in front of it.
    async fn settle(&mut self, id: RequestId) -> Result<AppReply, AppError> {
        loop {
            if let Some(result) = self.replies.remove(&id) {
                return result;
            }
            let Some(link) = self.link.as_mut() else {
                return Err(AppError::Stopped);
            };
            match link.frames.recv().await {
                Some(AppFrame::Reply { id: got, result }) => {
                    self.replies.insert(got, result);
                }
                Some(AppFrame::Event(event)) => {
                    // A resync in flight has no cursor to judge against yet;
                    // the cut that follows states everything anyway.
                    if self.cursor.is_some() {
                        self.fold(&event);
                    }
                }
                None => {
                    self.closed = true;
                    return Err(AppError::Stopped);
                }
            }
        }
    }

    /// Ask the core to do something, and wait for it to say what it did.
    pub async fn command(&mut self, command: AppCommand) -> Result<AppReply, AppError> {
        let id = self.mint();
        let id = self.ask(AppRequest::Command { id, command })?;
        self.settle(id).await
    }

    /// Read something the snapshot does not carry.
    pub async fn query(&mut self, query: AppQuery) -> Result<AppReply, AppError> {
        let id = self.mint();
        let id = self.ask(AppRequest::Query { id, query })?;
        self.settle(id).await
    }

    /// Re-read if the stream showed a hole. Idempotent: a store that is current
    /// does nothing.
    pub async fn reconcile(&mut self) -> Result<bool, AppError> {
        if !self.stale {
            return Ok(false);
        }
        self.stale = false;
        self.resync().await?;
        Ok(true)
    }
}

/// What one non-blocking drain found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pumped {
    /// Something was folded in; a reader would see a different answer.
    pub changed: bool,
    /// A hole was seen: the local state is behind and a re-read is owed.
    pub stale: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::{AgentChanged, ConversationChanged, EventMeta, TurnChanged};
    use crate::app::ids::{AgentId, SessionId};
    use crate::app::snapshot::{
        AgentKind, AgentState, ConversationRunState, ThinkingLevel, TurnOrigin, TurnStatus,
    };

    fn meta(seq: u64) -> EventMeta {
        EventMeta {
            seq,
            ts: 0,
            session_id: SessionId::new("sess_test"),
            caused_by: None,
            coalesced_from: None,
        }
    }

    fn event(seq: u64, payload: AppEventPayload) -> AppEvent {
        AppEvent {
            meta: meta(seq),
            payload,
        }
    }

    fn summary(id: &str, kind: ConversationKind, title: &str) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId::new(id),
            kind,
            title: title.to_string(),
            revision: 1,
            history_generation: 0,
            run_state: ConversationRunState::Idle,
            active_turn_id: None,
            unread: 0,
            mentions: 0,
            read_cursor: None,
            last_item_id: None,
            obligations: Vec::new(),
            is_member: true,
            queue_revision: 0,
            queue_count: 0,
            pending_interactions: 0,
            last_activity_at: None,
        }
    }

    fn agent(name: &str, state: AgentState) -> AgentResource {
        AgentResource {
            id: AgentId::new(format!("agent_{name}")),
            name: name.to_string(),
            def: None,
            description: String::new(),
            kind: AgentKind::Hire,
            state,
            model: "test-model".to_string(),
            provider: "default".to_string(),
            thinking: ThinkingLevel::Off,
            cwd: std::path::PathBuf::from("/tmp"),
            conversation_id: None,
            pending: 0,
            unacked: 0,
            elapsed_ms: None,
            output_tokens: 0,
            tool_uses: 0,
            last_active_at: 0,
        }
    }

    /// A store with a cut already taken, so the reducer can be exercised without
    /// a link. The cut is the fact the protocol requires before any event is
    /// legible; nothing else here is faked.
    fn cut_at(seq: u64) -> Store {
        Store {
            cursor: Some(seq),
            ..Store::default()
        }
    }

    /// The console's key and the contract's title are two spellings of one
    /// identity. If either end changes alone, a page stops finding itself.
    #[test]
    fn every_key_survives_the_title_the_contract_gives_it() {
        for (key, kind) in [
            (ConvKey::Main, ConversationKind::Main),
            (
                ConvKey::Agent("scout".to_string()),
                ConversationKind::Agent {
                    agent_id: AgentId::new("agent_scout"),
                },
            ),
            (
                ConvKey::Room("lobby".to_string()),
                ConversationKind::Room {
                    room_id: crate::app::ids::RoomId::new("room_lobby"),
                },
            ),
        ] {
            let summary = summary("conv_1", kind, &key.title());
            assert_eq!(key_of(&summary), key);
        }
    }

    /// The reducer keeps identity both ways: a page asks by key, an event
    /// arrives by id, and both find the same conversation.
    #[test]
    fn a_conversation_is_found_by_key_and_by_id() {
        let mut store = cut_at(0);
        store.fold(&event(
            1,
            AppEventPayload::ConversationCreated(ConversationChanged {
                conversation: summary("conv_main", ConversationKind::Main, "@main"),
            }),
        ));
        let view = store.view();
        assert_eq!(
            view.id_of(&ConvKey::Main),
            Some(&ConversationId::new("conv_main"))
        );
        assert_eq!(
            view.key_of(&ConversationId::new("conv_main")),
            Some(&ConvKey::Main)
        );
        assert!(view.conversation(&ConvKey::Main).is_some());
    }

    /// Busy is the core's answer, read off the turns it opened — not a flag the
    /// console keeps beside them.
    #[test]
    fn a_page_is_busy_exactly_while_the_core_holds_a_turn_on_it() {
        let mut store = cut_at(0);
        store.fold(&event(
            1,
            AppEventPayload::ConversationCreated(ConversationChanged {
                conversation: summary("conv_main", ConversationKind::Main, "@main"),
            }),
        ));
        assert!(!store.view().is_busy(&ConvKey::Main));

        let turn = Turn {
            id: TurnId::new("turn_1"),
            conversation_id: ConversationId::new("conv_main"),
            status: TurnStatus::Running,
            origin: TurnOrigin::User,
            round: 0,
            input_item_ids: Vec::new(),
            started_at: 0,
            completed_at: None,
            usage: None,
            error: None,
        };
        store.fold(&event(
            2,
            AppEventPayload::TurnStarted(TurnChanged {
                conversation_id: ConversationId::new("conv_main"),
                turn: turn.clone(),
            }),
        ));
        assert!(store.view().is_busy(&ConvKey::Main));

        let mut done = turn;
        done.status = TurnStatus::Completed;
        store.fold(&event(
            3,
            AppEventPayload::TurnCompleted(TurnChanged {
                conversation_id: ConversationId::new("conv_main"),
                turn: done,
            }),
        ));
        assert!(!store.view().is_busy(&ConvKey::Main));
    }

    /// A bounded collection replaces its named member rather than growing a
    /// second copy of it (spec invariant #13).
    #[test]
    fn a_named_resource_is_replaced_not_appended() {
        let mut store = cut_at(0);
        store.fold(&event(
            1,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("scout", AgentState::Idle),
            }),
        ));
        store.fold(&event(
            2,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("scout", AgentState::Running),
            }),
        ));
        assert_eq!(store.view().agents().len(), 1);
        assert_eq!(
            store.view().agent("scout").map(|agent| agent.state),
            Some(AgentState::Running)
        );
    }

    /// A hole is not patched over. The store stops folding, says it is stale,
    /// and waits for the re-read — the same move a wire client makes.
    #[test]
    fn a_hole_in_the_stream_makes_the_store_stale_rather_than_wrong() {
        let mut store = cut_at(0);
        assert!(store.fold(&event(
            1,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("scout", AgentState::Idle),
            }),
        )));
        // seq 2 never arrives.
        assert!(!store.fold(&event(
            3,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("relay", AgentState::Idle),
            }),
        )));
        assert_eq!(store.gaps, 1);
        assert!(store.pump().stale);
        // The event that could not be trusted was not half-applied.
        assert!(store.view().agent("relay").is_none());
    }

    /// A merged frame stands for the run it names, so a client reading for gaps
    /// still reads a gapless stream (`coalescedFrom`, D147).
    #[test]
    fn a_coalesced_frame_closes_the_span_it_names() {
        let mut store = cut_at(4);
        let mut merged = event(
            8,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("scout", AgentState::Running),
            }),
        );
        merged.meta.coalesced_from = Some(5);
        assert!(store.fold(&merged));
        assert_eq!(store.cursor, Some(8));
        assert_eq!(store.gaps, 0);
    }

    /// Everything at or below a cut is that cut's past: an event still in the
    /// channel from before a re-read is dropped rather than folded in twice.
    #[test]
    fn an_event_the_cut_already_states_is_dropped() {
        let mut store = cut_at(10);
        assert!(store.fold(&event(
            7,
            AppEventPayload::AgentChanged(AgentChanged {
                agent: agent("stale", AgentState::Idle),
            }),
        )));
        assert_eq!(store.cursor, Some(10));
        assert!(store.view().agent("stale").is_none());
    }

    /// The whole loop, against a real core: attach, take a cut, and watch a
    /// change the core made turn up in the local projection.
    #[tokio::test]
    async fn the_console_reads_what_the_core_publishes() {
        let core = AppCore::start(Default::default());
        let mut store = Store::open(&core, "tui-test")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(store.is_attached());
        assert!(store.view().session.is_some());

        core.agents()
            .insert(
                "scout",
                crate::agents::AgentKind::Hire,
                None,
                "a scout".to_string(),
                crate::tui::test_util::test_session(),
            )
            .await;
        // The registries publish on their own settling cadence; the store is
        // read after the actor has said everything it had to say about the
        // insert.
        core.agents().settle_now();
        for _ in 0..200 {
            if store.pump().changed && store.view().agent("scout").is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(
            store.view().agent("scout").map(|agent| agent.name.clone()),
            Some("scout".to_string())
        );
        assert_eq!(store.gaps, 0);
    }
}
