//! The session actor: one loop, one ordering point.
//!
//! Everything that changes application state passes through this loop, one
//! message at a time. That is what makes a sequence number mean something: `seq`
//! is stamped where the change happens, not where a transport later serializes
//! it, so two frontends reading the same stream read the same history.
//!
//! Two barriers hold the frontends together:
//!
//! - **Attachment.** A new attachment sees no event until it has taken a
//!   snapshot cut. Events before the cut are suppressed rather than buffered:
//!   the snapshot it is about to take already contains them, and replaying them
//!   would state the same fact twice.
//! - **The cut.** A snapshot is built and its `event_cursor` stamped inside one
//!   turn of this loop, so nothing can be sequenced between the snapshot and the
//!   reply frame carrying it. Every event that attachment receives afterwards
//!   has `seq > event_cursor` (spec "Snapshots and recovery").

use tokio::sync::{mpsc, oneshot};

use crate::app::attention::Attention;
use crate::app::command::{AppCommand, AppQuery};
use crate::app::conversation::{ConvKey, Conversations, StalePage};
use crate::app::event::{
    AgentChanged, AppEvent, AppEventPayload, ConversationChanged, DeliveryChanged, EventMeta,
    FeedbackRaised, InteractionCancelled, InteractionOpened, InteractionResolved, ItemChanged,
    ItemDelta, QueueItemAbsorbed, QueueItemAdded, QueueItemRemoved, RoomChanged, SessionClosed,
    TurnChanged, TurnRetrying, TurnRoundCompleted, TurnRoundStarted, TurnUsageUpdated,
};
use crate::app::ids::{
    AgentId, CommandId, ConversationId, DeliveryId, EpochId, FeedbackId, IdMint, ItemId,
    OperationId, RoomId, SessionId, now_millis,
};
use crate::app::snapshot::{
    AgentKind, AgentResource, AgentState, BackgroundCommandResource, BackgroundCommandState,
    Collection, ConfigSnapshot, ConversationKind, ConversationRunState, ConversationSummary,
    DeliveryResource, DeliveryState, Feedback, Item, ItemBody, ItemStatus, NoticeLevel, Obligation,
    Page, QueueEntry, RoomMode, RoomResource, RuntimeCollections, ServerCapabilities,
    SessionCloseReason, SessionSnapshot, SessionState, SessionSummary, ThinkingLevel,
};
use crate::app::turn::TurnChange;
use crate::app::{
    AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest, FRAME_CAPACITY,
    REQUEST_CAPACITY, SessionSetup,
};

/// What reaches the actor. Attachments, their requests, the engine's
/// publications and every registry change are one queue on purpose: the order
/// they arrive in is the order the session happened in.
///
/// The queue is unbounded, which is a decision rather than an oversight. Half the
/// callers cannot wait — a render loop, a synchronous event sink, a `Drop` — and
/// a bounded queue would have to either block them (impossible) or drop their
/// message (a lost state transition). Unbounded means enqueueing never blocks and
/// never fails while the actor lives, which also removes the whole deadlock class
/// the ordering point would otherwise invite: nobody waits to be heard.
pub(crate) enum Control {
    Attach {
        request: AttachRequest,
        /// The runtime the attachment's request forwarder runs on. The actor has
        /// no runtime of its own, and the frontend that attaches always does.
        runtime: tokio::runtime::Handle,
        reply: oneshot::Sender<Result<AppLink, AppError>>,
    },
    Request {
        attachment: AttachmentId,
        request: AppRequest,
    },
    Detach {
        attachment: AttachmentId,
    },
    Publish {
        payload: Box<AppEventPayload>,
        caused_by: Option<OperationId>,
    },
    /// A change to, or a question about, the watch registry.
    Watch(crate::watch::WatchMsg),
    /// A change to, or a question about, the rooms.
    Channels(crate::channels::ChannelMsg),
    /// A change to, or a question about, the subagent instances.
    Agents(crate::agents::AgentMsg),
    /// A turn opening, reporting, or ending.
    Turn(crate::app::turn::TurnMsg),
    /// An input queue accepting, absorbing, draining, or losing an entry.
    Queue(crate::app::queue::QueueMsg),
    /// A prompt opening, being answered, or being closed.
    Interaction(crate::app::interaction::InteractionMsg),
    /// A question about the mail waiting for main.
    Mail(crate::app::mail::MailMsg),
    /// Work that is not a turn, starting, reporting, or ending.
    Operation(crate::app::operation::OperationMsg),
    /// What the MCP manager stands at. Connection state is the manager's, and it
    /// lives outside the actor, so it is reported in rather than read out.
    Mcp(Vec<crate::app::snapshot::McpServerState>),
    /// One submission, read and routed.
    Submit {
        request: Box<crate::app::submit::SubmitRequest>,
        reply: oneshot::Sender<crate::app::submit::Route>,
    },
    /// Answered once everything queued ahead of it has been applied.
    Settle {
        reply: oneshot::Sender<()>,
    },
    /// Close the session: settle what is open and end the loop.
    Close {
        reason: SessionCloseReason,
        reply: oneshot::Sender<()>,
    },
}

/// Proof that one session's thread is still running.
///
/// The thread holds the strong half and drops it on the way out, so a holder of
/// the weak half can tell "the loop has ended" from "the loop is idle" without
/// polling the thread table. A session that closed must leave nothing behind, and
/// this is what says so.
pub(crate) type Alive = std::sync::Weak<()>;

/// Everything the actor owns, handed to it in one piece at startup.
struct State {
    watch: crate::watch::WatchRegistry,
    channels: crate::channels::ChannelRegistry,
    agents: crate::agents::AgentRegistry,
    turns: crate::app::turn::TurnRegistry,
    queue: crate::app::queue::InputQueue,
    interactions: crate::app::interaction::InteractionRegistry,
}

/// What the actor hands out at startup: one handle per registry it owns.
pub(crate) struct Registries {
    pub watch: crate::watch::WatchHandle,
    pub channels: crate::channels::ChannelHandle,
    pub agents: crate::agents::AgentHandle,
    pub turns: crate::app::turn::TurnHandle,
    pub queue: crate::app::queue::QueueHandle,
    pub submit: crate::app::submit::SubmitHandle,
    pub interactions: crate::app::interaction::InteractionHandle,
    pub mail: crate::app::mail::MailHandle,
    pub operations: crate::app::operation::OperationHandle,
}

/// Start the actor and return the handle everything reaches it by.
///
/// The loop runs on a thread of its own rather than on the runtime, and that is
/// the invariant made structural: an actor that is not a future cannot await
/// anything while it holds the session's state, so "the actor never waits on a
/// frontend, an engine task, or the disk" is enforced by the type system instead
/// of by everyone remembering. It costs one thread per session and buys a
/// registry that is reachable from synchronous code — a render loop, a `Drop` —
/// without a runtime in scope.
pub(super) fn spawn(setup: SessionSetup) -> (mpsc::UnboundedSender<Control>, Registries, Alive) {
    let (control, inbox) = mpsc::unbounded_channel();
    let (watch, watch_handle) = crate::watch::attach(control.clone());
    let (channels, channel_handle) = crate::channels::attach(control.clone(), setup.channel_limits);
    let (agents, agent_handle) = crate::agents::attach(control.clone());
    let (turns, turn_handle) = crate::app::turn::attach(control.clone());
    let (queue, queue_handle) = crate::app::queue::attach(control.clone());
    let (interactions, interaction_handle) = crate::app::interaction::attach(control.clone());
    let control_for_submit = control.clone();
    // The actor holds a weak handle to its own inbox: it hands strong clones to
    // the attachments it spawns, and a strong one of its own would keep the
    // queue open forever, so the loop could never end.
    let running = std::sync::Arc::new(());
    let alive = std::sync::Arc::downgrade(&running);
    let controller = Controller::new(
        setup,
        control.downgrade(),
        State {
            watch,
            channels,
            agents,
            turns,
            queue,
            interactions,
        },
    );
    std::thread::Builder::new()
        .name("bingo-session".to_string())
        .spawn(move || {
            let _running = running;
            controller.run(inbox);
        })
        .unwrap_or_else(|error| panic!("the session actor could not start: {error}"));
    (
        control,
        Registries {
            watch: watch_handle,
            channels: channel_handle,
            agents: agent_handle,
            turns: turn_handle,
            queue: queue_handle,
            submit: crate::app::submit::SubmitHandle::new(control_for_submit.clone()),
            interactions: interaction_handle,
            mail: crate::app::mail::MailHandle::new(control_for_submit.clone()),
            operations: crate::app::operation::OperationHandle::new(control_for_submit),
        },
        alive,
    )
}

/// Why the core would not take an asset, in the protocol's own vocabulary.
fn asset_refusal(error: &crate::app::asset::AssetError) -> AppError {
    use crate::app::asset::AssetError;
    use crate::app_server::protocol::error::ProtocolErrorKind;
    AppError::Refused(match error {
        AssetError::NotFound => ProtocolErrorKind::AssetNotFound,
        AssetError::Rejected(_) => ProtocolErrorKind::AssetRejected,
        AssetError::BadArgument(_) => ProtocolErrorKind::BadArgument,
    })
}

/// The permission rules settings declares. Session-scoped grants (D81's
/// `allowSession`) live in the run that granted them and reach the core in B7.
fn rules_of(settings: &crate::settings::Settings) -> Vec<crate::app::snapshot::PermissionRule> {
    use crate::app::snapshot::{PermissionRule, PermissionRuleDecision};
    let mut out = Vec::new();
    for (decision, list) in [
        (PermissionRuleDecision::Allow, &settings.permissions.allow),
        (PermissionRuleDecision::Deny, &settings.permissions.deny),
        (PermissionRuleDecision::Ask, &settings.permissions.ask),
    ] {
        out.extend(list.iter().map(|rule| PermissionRule {
            decision,
            rule: rule.clone(),
            session_scoped: false,
        }));
    }
    out
}

/// Which settings file contributed which keys, so a frontend can say where a
/// value came from without reading the files itself.
fn layers_of(
    catalog: &crate::app::catalog::CatalogSource,
) -> Vec<crate::app::snapshot::ConfigLayer> {
    crate::settings::layer_paths(catalog.user_dir(), catalog.cwd())
        .into_iter()
        .map(|path| crate::app::snapshot::ConfigLayer {
            keys: crate::settings::layer_keys(&path),
            path,
        })
        .collect()
}

/// The transcript the open session is reading, when its locator names a path.
fn open_path(locator: &crate::app::snapshot::SessionLocator) -> std::path::PathBuf {
    match locator {
        crate::app::snapshot::SessionLocator::Path { path } => path.clone(),
        _ => std::path::PathBuf::new(),
    }
}

/// When a file was last written, in the same unit every other timestamp uses.
fn modified_millis(path: &std::path::Path) -> crate::app::ids::UnixMillis {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis() as u64)
}

/// Wait until everything already sent has been applied.
pub(crate) async fn settle(control: &mpsc::UnboundedSender<Control>) {
    let (reply, answer) = oneshot::channel();
    if control.send(Control::Settle { reply }).is_ok() {
        let _ = answer.await;
    }
}

/// The same barrier for a caller with no runtime to await on — a synchronous
/// test, or one of the terminal front end's synchronous seams.
#[cfg(test)]
pub(crate) fn settle_now(control: &mpsc::UnboundedSender<Control>) {
    let (reply, answer) = oneshot::channel();
    let _ = control.send(Control::Settle { reply });
    // The same wait `Answer::now` uses, and safe for the same reason: the actor
    // is on a thread of its own and never waits back.
    crate::app::answer::Answer::new(answer, ()).now();
}

/// One attached frontend, as the actor knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttachmentId(u64);

struct Attachment {
    id: AttachmentId,
    /// What this frontend calls itself; it appears in diagnostics only.
    #[allow(dead_code)]
    label: String,
    frames: mpsc::Sender<AppFrame>,
    /// The cut this attachment reads events against. `None` until it has taken
    /// one, and until then it receives no event at all.
    cursor: Option<u64>,
}

struct Controller {
    mint: IdMint,
    session: SessionSummary,
    capabilities: ServerCapabilities,
    config: ConfigSnapshot,
    /// Settings, the two directories and the endpoint table, as the catalogs and
    /// the effective configuration are read from them (B5).
    catalog: crate::app::catalog::CatalogSource,
    /// What the MCP manager last reported. Empty until something has connected,
    /// which is the honest answer for a session that has not.
    mcp: Vec<crate::app::snapshot::McpServerState>,
    /// The bytes this session owns: attachments, and output too large for an
    /// item. They go when the session does.
    assets: crate::app::asset::AssetStore,
    /// The last sequence number stamped. Strictly increasing, gapless, scoped to
    /// this epoch.
    seq: u64,
    attachments: Vec<Attachment>,
    next_attachment: u64,
    control: mpsc::WeakUnboundedSender<Control>,
    /// Background work — commands, agent runs, room operations — as a state
    /// machine. Actor-private since B2b: what used to be an `Arc<Mutex<…>>` every
    /// task could reach is now reachable only through this loop.
    watch: crate::watch::WatchRegistry,
    /// The rooms, and the main agent's inbox. Actor-private since B2b.
    channels: crate::channels::ChannelRegistry,
    /// The subagent instances: their state machine, their inboxes, their
    /// delivery records. Actor-private since B2b.
    agents: crate::agents::AgentRegistry,
    /// The turns in flight and the items they are producing (B3).
    turns: crate::app::turn::TurnRegistry,
    /// The input queues, and the barrier/pull-back race they arbitrate (B3).
    queue: crate::app::queue::InputQueue,
    /// The prompts a run is stopped on, and the guard that decides when a
    /// keystroke may answer one (B3).
    interactions: crate::app::interaction::InteractionRegistry,
    /// Which conversations exist, what each one holds, and what each is called
    /// on the wire.
    conversations: Conversations,
    /// How far the user has read each conversation, and what it owes them.
    attention: Attention,
    /// Warnings raised outside any single turn, still standing.
    feedback: Vec<Feedback>,
    /// The debounce that decides when main reads its mail (乙案).
    mail: crate::app::mail::MailWake,
    /// The standing "mail is waiting" notice, so it can be cleared by identity.
    mail_notice: Option<FeedbackId>,
    /// Accepted work that is not a turn: a team coming up, a login, a share.
    operations: crate::app::operation::OperationRegistry,
    /// Conversations whose summary may have moved this turn of the loop. Batched
    /// because one message can move several facts about one conversation, and a
    /// summary published per fact would say the same thing three times.
    dirty: std::collections::BTreeSet<ConvKey>,
    /// A request is being answered: what it publishes waits for its reply.
    serving: bool,
    /// What the request in flight published, in the order it published it.
    deferred: Vec<(Box<AppEventPayload>, Option<OperationId>)>,
    /// What was last said about each instance and each room, so a change can be
    /// told from a repeat. Identifiers are minted once per name and kept for the
    /// epoch: a client that saw `agent_3` twice saw the same instance twice.
    told: Told,
}

/// The last thing published about each collaboration resource.
#[derive(Default)]
struct Told {
    agents: std::collections::HashMap<String, (AgentId, AgentSummary)>,
    rooms: std::collections::HashMap<String, (RoomId, RoomSummary)>,
    /// Identifiers minted for a name before anything was published about it —
    /// a conversation's `kind` names its instance or its room, and it may be the
    /// first thing to ask.
    agent_ids: std::collections::HashMap<String, AgentId>,
    room_ids: std::collections::HashMap<String, RoomId>,
    /// The last summary published for each conversation, so a change can be told
    /// from a repeat.
    conversations: std::collections::HashMap<ConvKey, ConversationSummary>,
    /// Where each direct message was last reported to stand.
    deliveries: std::collections::HashMap<u64, (DeliveryId, DeliveryState, u32)>,
    /// Where each background command was last reported to stand.
    commands: std::collections::HashMap<u64, (CommandId, BackgroundCommandState, Option<String>)>,
}

/// What decides whether an instance's change is worth an event. Progress
/// counters are deliberately out: a token count moving is not a state
/// transition, and B4's usage events are where a live figure belongs.
#[derive(PartialEq, Eq)]
struct AgentSummary {
    state: AgentState,
    pending: u32,
    unacked: u32,
}

#[derive(PartialEq, Eq)]
struct RoomSummary {
    members: Vec<String>,
    last_seq: u64,
    unread: u32,
    mentions: u32,
}

impl Controller {
    fn new(setup: SessionSetup, control: mpsc::WeakUnboundedSender<Control>, state: State) -> Self {
        let epoch = EpochId::mint();
        let assets = crate::app::asset::AssetStore::new(
            &crate::storage::data_dir(setup.catalog.home()),
            epoch.as_str(),
        );
        let mut mint = IdMint::new(epoch.clone());
        let id: SessionId = mint.mint();
        let conversations = Conversations::new(&mut mint);
        let now = now_millis();
        let session = SessionSummary {
            id,
            epoch,
            title: setup.title,
            state: SessionState::Active,
            cwd: setup.cwd.clone(),
            locator: setup.locator,
            provider: setup.provider.clone(),
            model: setup.model.clone(),
            thinking: setup.thinking,
            permission_mode: setup.permission_mode,
            created_at: now,
            updated_at: now,
            resumed: setup.resumed,
        };
        let config = ConfigSnapshot {
            revision: 1,
            model: setup.model,
            provider: setup.provider,
            thinking: setup.thinking,
            permission_mode: setup.permission_mode,
            theme: setup.theme,
            cwd: setup.cwd,
            shell: setup.shell,
            shell_dialect: setup.shell_dialect,
            permissions: rules_of(setup.catalog.settings()),
            layers: layers_of(&setup.catalog),
            mcp_servers: setup.catalog.mcp_servers(None),
        };
        Self {
            mint,
            session,
            capabilities: setup.capabilities,
            config,
            assets,
            catalog: setup.catalog,
            mcp: Vec::new(),
            seq: 0,
            attachments: Vec::new(),
            next_attachment: 0,
            control,
            watch: state.watch,
            channels: state.channels,
            agents: state.agents,
            turns: state.turns,
            queue: state.queue,
            interactions: state.interactions,
            conversations,
            attention: Attention::default(),
            feedback: Vec::new(),
            mail: crate::app::mail::MailWake::default(),
            mail_notice: None,
            operations: crate::app::operation::OperationRegistry::default(),
            dirty: std::collections::BTreeSet::new(),
            serving: false,
            deferred: Vec::new(),
            told: Told::default(),
        }
    }

    fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Control>) {
        while let Some(message) = inbox.blocking_recv() {
            match message {
                Control::Attach {
                    request,
                    runtime,
                    reply,
                } => {
                    let _ = reply.send(self.attach(request, runtime));
                }
                Control::Request {
                    attachment,
                    request,
                } => self.serve(attachment, request),
                Control::Detach { attachment } => {
                    self.attachments.retain(|open| open.id != attachment);
                }
                Control::Publish { payload, caused_by } => self.publish(payload, caused_by),
                // The registries publish their own reader snapshots; what the
                // actor adds is the sequenced account of what changed, which is
                // the only ordering a second frontend can rely on.
                Control::Watch(message) => {
                    self.watch.handle(message);
                    self.announce_commands();
                }
                Control::Channels(message) => {
                    // A replayed sidecar carries the user's own cursors, and they
                    // have to be applied *after* the log it measures has become
                    // items — otherwise the restored history would all read as
                    // unread (Amendment #6).
                    let restored = match &message {
                        crate::channels::ChannelMsg::RestoreRooms(replay) => {
                            Some(replay.read_cursors())
                        }
                        _ => None,
                    };
                    self.channels.handle(message);
                    self.absorb_posts();
                    self.absorb_main_mail();
                    if let Some(cursors) = restored {
                        self.restore_attention(cursors);
                    }
                    self.announce_rooms();
                    self.consider_mail();
                }
                Control::Agents(message) => {
                    self.agents.handle(message);
                    self.absorb_deliveries();
                    self.announce_agents();
                    self.announce_deliveries();
                }
                Control::Turn(message) => {
                    let changes = self.turns.handle(message, &mut self.mint);
                    self.announce_turn(changes);
                }
                Control::Queue(message) => {
                    let changes = self.queue.handle(message, &mut self.mint);
                    self.announce_queue(changes);
                }
                Control::Interaction(message) => {
                    let changes = self.interactions.handle(message, &mut self.mint);
                    self.announce_interactions(changes);
                }
                Control::Mail(message) => self.serve_mail(message),
                Control::Operation(message) => {
                    let changes = self.operations.handle(message, &mut self.mint);
                    self.announce_operations(changes);
                }
                Control::Mcp(states) => self.report_mcp(states),
                Control::Submit { request, reply } => {
                    let route = self.submit(*request);
                    let _ = reply.send(route);
                }
                Control::Settle { reply } => {
                    self.announce_conversations();
                    let _ = reply.send(());
                    continue;
                }
                Control::Close { reason, reply } => {
                    self.close(reason);
                    let _ = reply.send(());
                    // Nothing more can happen in a session that is over, and the
                    // loop ending is what releases the last of what it held.
                    return;
                }
            }
            // One summary per conversation per message, after everything that
            // message moved.
            self.announce_conversations();
        }
    }

    fn attach(
        &mut self,
        request: AttachRequest,
        runtime: tokio::runtime::Handle,
    ) -> Result<AppLink, AppError> {
        let Some(control) = self.control.upgrade() else {
            return Err(AppError::Stopped);
        };
        let (frames, incoming) = mpsc::channel(FRAME_CAPACITY);
        let (requests, mut outgoing) = mpsc::channel(REQUEST_CAPACITY);
        let id = AttachmentId(self.next_attachment);
        self.next_attachment = self.next_attachment.saturating_add(1);
        self.attachments.push(Attachment {
            id,
            label: request.label,
            frames,
            cursor: None,
        });
        // One forwarder per attachment: it tags each request with the
        // attachment that sent it, keeps that attachment's requests in the order
        // they were written, and tells the actor when the frontend is gone.
        runtime.spawn(async move {
            while let Some(request) = outgoing.recv().await {
                if control
                    .send(Control::Request {
                        attachment: id,
                        request,
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = control.send(Control::Detach { attachment: id });
        });
        Ok(AppLink {
            requests,
            frames: incoming,
        })
    }

    /// Answer one request, then publish what it caused.
    ///
    /// The order is the invariant (spec #3): an accepted request's response is
    /// written before the first event caused solely by that request. Everything
    /// the handler publishes is held until the reply frame is out, which is also
    /// what makes a snapshot cut taken here valid — nothing can be sequenced
    /// between the snapshot and the frame carrying it.
    fn serve(&mut self, attachment: AttachmentId, request: AppRequest) {
        self.serving = true;
        let (id, result) = match request {
            AppRequest::Command { id, command } => (id, self.command(command)),
            AppRequest::Query { id, query } => (id, self.query(attachment, query)),
        };
        self.serving = false;
        self.deliver(attachment, AppFrame::Reply { id, result });
        for (payload, caused_by) in std::mem::take(&mut self.deferred) {
            self.publish(payload, caused_by);
        }
    }

    /// Mutations land with B3 (turns and queue), B4 (collaboration), and B5
    /// (actions). The skeleton refuses them by name rather than accepting work
    /// it cannot do.
    fn command(&mut self, command: AppCommand) -> Result<AppReply, AppError> {
        if let AppCommand::Submit {
            conversation_id,
            input,
        } = command
        {
            return self.serve_submit(conversation_id, input);
        }
        if let AppCommand::MarkRead {
            conversation_id,
            last_item_id,
            last_room_seq,
            expected_revision,
        } = &command
        {
            return self.serve_mark_read(
                conversation_id,
                last_item_id.as_ref(),
                *last_room_seq,
                *expected_revision,
            );
        }
        if let AppCommand::RegisterAsset {
            path,
            expected_mime,
            expected_sha256,
        } = &command
        {
            return self.serve_register_asset(
                path,
                expected_mime.as_deref(),
                expected_sha256.as_deref(),
            );
        }
        if matches!(command, AppCommand::CloseSession | AppCommand::Shutdown) {
            // The reply goes out before the loop ends, which is why the closing
            // itself is a control message rather than work done here: a request
            // handler cannot both answer and stop the thread that answers.
            if let Some(control) = self.control.upgrade() {
                let (reply, _done) = oneshot::channel();
                let _ = control.send(Control::Close {
                    reason: SessionCloseReason::Requested,
                    reply,
                });
            }
            return Ok(AppReply::Accepted);
        }
        Err(AppError::Unserved(match command {
            AppCommand::StartSession { .. } => "session/start",
            AppCommand::ResumeSession { .. } => "session/resume",
            AppCommand::DeleteSession { .. } => "session/delete",
            AppCommand::Execute { .. } => "action/execute",
            AppCommand::Interrupt { .. } => "turn/interrupt",
            AppCommand::RespondInteraction { .. } => "interaction/respond",

            AppCommand::ReclaimQueueTail { .. } => "queue/reclaimTail",

            // Answered above; the compiler is what keeps this exhaustive.
            AppCommand::RegisterAsset { .. } => "asset/registerPath",
            // Answered above; the compiler is what keeps this exhaustive.
            AppCommand::MarkRead { .. } => "conversation/markRead",
            // Answered above; the compiler is what keeps this exhaustive.
            AppCommand::Submit { .. } => "conversation/submit",
            AppCommand::CloseSession => "session/close",
            AppCommand::Shutdown => "shutdown",
        }))
    }

    fn query(&mut self, attachment: AttachmentId, query: AppQuery) -> Result<AppReply, AppError> {
        match query {
            AppQuery::ReadSession => {
                let snapshot = self.session_snapshot();
                self.cut(attachment, snapshot.event_cursor);
                Ok(AppReply::Session(Box::new(snapshot)))
            }
            AppQuery::ListSessions { cursor, limit } => {
                Ok(AppReply::Sessions(self.session_list(cursor, limit)))
            }
            AppQuery::ListConversations { limit, .. } => {
                let snapshot = self.conversation_list(limit);
                self.cut(attachment, self.seq);
                Ok(AppReply::Conversations(snapshot))
            }
            AppQuery::ReadConversation {
                conversation_id,
                cursor,
                limit,
            } => {
                let snapshot = self.conversation_snapshot(&conversation_id, cursor, limit)?;
                self.cut(attachment, snapshot.event_cursor);
                Ok(AppReply::Conversation(Box::new(snapshot)))
            }
            AppQuery::ReadQueue {
                conversation_id,
                limit,
                ..
            } => {
                let Some(conversation) = self.conversations.key(&conversation_id).cloned() else {
                    return Err(AppError::Refused(
                        crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
                    ));
                };
                let (_, count) = self.queue.stand(&conversation);
                let limit = limit.map_or(DEFAULT_PAGE as usize, |limit| limit.max(1) as usize);
                let Self {
                    queue,
                    conversations,
                    mint,
                    ..
                } = self;
                let entries =
                    queue.page(&conversation, &mut |key| conversations.id(mint, key), limit);
                Ok(AppReply::Queue { entries, count })
            }
            AppQuery::ListActions {
                origin_conversation_id,
            } => {
                if let Some(id) = &origin_conversation_id
                    && self.conversations.key(id).is_none()
                {
                    return Err(AppError::Refused(
                        crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
                    ));
                }
                Ok(AppReply::Actions {
                    actions: crate::app::action::published(self.availability()),
                    revision: self.config.revision,
                })
            }
            AppQuery::ReadConfig => Ok(AppReply::Config(Box::new(self.config_snapshot()))),
            AppQuery::ReadCatalog {
                catalog,
                provider,
                cursor,
                limit,
            } => Ok(AppReply::Catalog(Box::new(self.catalog.page(
                catalog,
                provider.as_deref(),
                cursor.as_deref(),
                limit,
                &crate::app::catalog::Live {
                    mcp: (!self.mcp.is_empty()).then_some(self.mcp.as_slice()),
                    images: &self.assets.images(),
                },
            )))),
            AppQuery::ReadResource {
                resource,
                cursor,
                limit,
            } => Ok(AppReply::Resource(Box::new(self.resource_page(
                resource,
                cursor.as_deref(),
                limit,
            )))),
            AppQuery::ReadAssetChunk {
                asset_id,
                offset,
                length,
            } => match self.assets.read_chunk(&asset_id, offset, length) {
                Ok((data, next_offset, eof)) => Ok(AppReply::AssetChunk {
                    data,
                    next_offset,
                    eof,
                }),
                Err(error) => Err(asset_refusal(&error)),
            },
        }
    }

    /// `asset/registerPath`: take the file into the server's own storage and
    /// announce that the bytes are available.
    fn serve_register_asset(
        &mut self,
        path: &std::path::Path,
        expected_mime: Option<&str>,
        expected_sha256: Option<&str>,
    ) -> Result<AppReply, AppError> {
        let record = self
            .assets
            .register_path(&mut self.mint, path, expected_mime, expected_sha256)
            .map_err(|error| asset_refusal(&error))?;
        let is_image = record.kind == crate::app::snapshot::AssetKind::Image;
        self.publish(
            Box::new(AppEventPayload::AssetAvailable(
                crate::app::event::AssetAvailable {
                    asset: record.clone(),
                },
            )),
            None,
        );
        if is_image {
            let revision = self.assets.len() as u64;
            self.publish(
                Box::new(AppEventPayload::CatalogChanged(
                    crate::app::event::CatalogChanged {
                        catalog: crate::app::snapshot::CatalogKind::Images,
                        revision,
                    },
                )),
                None,
            );
        }
        Ok(AppReply::Asset(Box::new(record)))
    }

    /// Take what the MCP manager reports, and say so once it has changed.
    fn report_mcp(&mut self, states: Vec<crate::app::snapshot::McpServerState>) {
        if self.mcp == states {
            return;
        }
        self.mcp = states;
        self.config.revision = self.config.revision.saturating_add(1);
        let config = self.config_snapshot();
        self.publish(
            Box::new(AppEventPayload::ConfigChanged(
                crate::app::event::ConfigChanged { config },
            )),
            None,
        );
        self.publish(
            Box::new(AppEventPayload::CatalogChanged(
                crate::app::event::CatalogChanged {
                    catalog: crate::app::snapshot::CatalogKind::McpServers,
                    revision: self.config.revision,
                },
            )),
            None,
        );
    }

    /// What decides whether an action can run right now.
    fn availability(&self) -> crate::app::action::Availability {
        crate::app::action::Availability {
            console_busy: self.turns.is_busy(&ConvKey::Main),
            // The engine half — a model, a transcript rewrite, a network round
            // trip — is still the console's until B7 attaches it here.
            engine_attached: false,
        }
    }

    /// The effective configuration, with the parts that are read rather than
    /// held: the settings layers that contributed, and what MCP stands at.
    fn config_snapshot(&self) -> ConfigSnapshot {
        let mut config = self.config.clone();
        config.layers = layers_of(&self.catalog);
        config.mcp_servers = self
            .catalog
            .mcp_servers((!self.mcp.is_empty()).then_some(self.mcp.as_slice()));
        config
    }

    /// One page of one runtime collection. The lists are the same ones a session
    /// snapshot carries; this is how a client reads past the bounded head of them.
    fn resource_page(
        &mut self,
        resource: crate::app::snapshot::ResourceKind,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> crate::app::snapshot::ResourcePage {
        use crate::app::catalog::page;
        use crate::app::snapshot::{ResourceKind, ResourcePage};
        match resource {
            ResourceKind::Agents => {
                ResourcePage::Agents(page(self.agent_resources(), cursor, limit))
            }
            ResourceKind::Rooms => ResourcePage::Rooms(page(self.room_resources(), cursor, limit)),
            ResourceKind::Tasks => ResourcePage::Tasks(page(Vec::new(), cursor, limit)),
            ResourceKind::Deliveries => {
                ResourcePage::Deliveries(page(self.delivery_resources(), cursor, limit))
            }
            ResourceKind::BackgroundCommands => {
                ResourcePage::BackgroundCommands(page(self.command_resources(), cursor, limit))
            }
        }
    }

    /// The sessions on disk, newest first, with the open one marked.
    ///
    /// Reading a directory is the one blocking call this loop makes, and it is
    /// bounded by what `bingo gc` leaves behind.
    fn session_list(
        &self,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Page<crate::app::snapshot::SessionListEntry> {
        use crate::app::snapshot::{SessionListEntry, SessionLocator};
        let home = self.catalog.home();
        let entries: Vec<SessionListEntry> = crate::transcript::list(home)
            .unwrap_or_default()
            .into_iter()
            .map(|transcript| {
                let path = transcript.path().to_path_buf();
                let locator = SessionLocator::Stem {
                    stem: transcript.name(),
                };
                SessionListEntry {
                    open: locator == self.session.locator
                        || path == open_path(&self.session.locator),
                    title: transcript.name(),
                    cwd: self.session.cwd.clone(),
                    updated_at: modified_millis(&path),
                    message_count: transcript.line_count().unwrap_or(0) as u32,
                    locator,
                }
            })
            .collect();
        crate::app::catalog::page(entries, cursor.as_deref(), limit)
    }

    /// The session as it stands, valid through the sequence number it was cut
    /// at.
    ///
    /// The bounded halves are carried inline and the unbounded remainder is read
    /// through the paginated methods (spec "Snapshots and recovery"). Tasks,
    /// background commands, MCP state and operations are still empty: their
    /// registries land with B5.
    fn session_snapshot(&mut self) -> SessionSnapshot {
        let keys: Vec<ConvKey> = self.conversations.keys().to_vec();
        let count = keys.len() as u32;
        let summaries: Vec<ConversationSummary> = keys
            .into_iter()
            .take(DEFAULT_PAGE as usize)
            .map(|key| self.summarize(&key))
            .collect();
        let agents = self.agent_resources();
        let rooms = self.room_resources();
        let deliveries = self.delivery_resources();
        let active_turns = self
            .turns
            .active_turns()
            .into_iter()
            .map(|mut turn| {
                if let Some(key) = self.conversations.key(&turn.conversation_id).cloned() {
                    turn.conversation_id = self.conversations.id(&mut self.mint, &key);
                }
                turn
            })
            .collect();
        SessionSnapshot {
            session: self.session.clone(),
            capabilities: self.capabilities,
            conversations: Collection {
                revision: u64::from(count),
                count,
                active: summaries,
            },
            active_turns,
            interactions: self.interactions.pending(),
            operations: self.operations.running(),
            collections: RuntimeCollections {
                agents: Collection {
                    revision: agents.len() as u64,
                    count: agents.len() as u32,
                    active: agents,
                },
                rooms: Collection {
                    revision: rooms.len() as u64,
                    count: rooms.len() as u32,
                    active: rooms,
                },
                tasks: empty_collection(),
                deliveries: Collection {
                    revision: deliveries.len() as u64,
                    count: deliveries.len() as u32,
                    active: deliveries,
                },
                background_commands: {
                    let commands = self.command_resources();
                    Collection {
                        revision: commands.len() as u64,
                        count: commands.len() as u32,
                        active: commands,
                    }
                },
                mcp_servers: Vec::new(),
            },
            feedback: self.feedback.clone(),
            config: self.config.clone(),
            event_cursor: self.seq,
        }
    }

    /// One page of the session's conversations, in the order they were first
    /// named.
    fn conversation_list(
        &mut self,
        limit: Option<u32>,
    ) -> crate::app::snapshot::Page<ConversationSummary> {
        let limit = limit.unwrap_or(DEFAULT_PAGE) as usize;
        let keys: Vec<ConvKey> = self.conversations.keys().to_vec();
        let count = keys.len();
        let items = keys
            .into_iter()
            .take(limit)
            .map(|key| self.summarize(&key))
            .collect();
        Page {
            items,
            revision: count as u64,
            next_cursor: None,
        }
    }

    /// One conversation as it stands, valid through the sequence number it was
    /// cut at. Reading it marks nothing (spec invariant #14).
    fn conversation_snapshot(
        &mut self,
        id: &ConversationId,
        cursor: Option<crate::app::snapshot::ItemCursor>,
        limit: Option<u32>,
    ) -> Result<crate::app::snapshot::ConversationSnapshot, AppError> {
        let Some(key) = self.conversations.key(id).cloned() else {
            return Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
            ));
        };
        let limit = limit.unwrap_or(DEFAULT_PAGE) as usize;
        let items = self
            .conversations
            .page(&key, cursor.as_ref(), limit)
            .map_err(|StalePage| {
                AppError::Refused(crate::app_server::protocol::error::ProtocolErrorKind::StalePage)
            })?;
        let conversation = self.summarize(&key);
        let history_generation = conversation.history_generation;
        let active_turn = self
            .turns
            .active_turns()
            .into_iter()
            .find(|turn| Some(&key) == self.conversations.key(&turn.conversation_id))
            .map(|mut turn| {
                turn.conversation_id = conversation.id.clone();
                turn
            });
        let mut origins =
            |origin: &ConvKey| -> ConversationId { self.conversations.id(&mut self.mint, origin) };
        let queue = self.queue.page(&key, &mut origins, DEFAULT_PAGE as usize);
        let interactions = self
            .interactions
            .pending()
            .into_iter()
            .map(|mut pending| {
                pending.conversation_id = conversation.id.clone();
                pending
            })
            .collect();
        Ok(crate::app::snapshot::ConversationSnapshot {
            conversation,
            items,
            history_generation,
            active_turn,
            queue,
            interactions,
            context_usage: None,
            event_cursor: self.seq,
        })
    }

    /// `conversation/markRead`, the one thing that advances attention.
    fn serve_mark_read(
        &mut self,
        id: &ConversationId,
        last_item_id: Option<&ItemId>,
        last_room_seq: Option<u64>,
        expected_revision: u64,
    ) -> Result<AppReply, AppError> {
        let Some(key) = self.conversations.key(id).cloned() else {
            return Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
            ));
        };
        let record = self.conversations.record_mut(&mut self.mint, &key);
        // The revision is what makes this safe: a client marking a view it never
        // saw would clear attention for content it has not shown.
        if expected_revision != record.revision {
            return Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::StaleRevision,
            ));
        }
        self.attention
            .mark_read(&key, record, last_item_id, last_room_seq);
        // A room's cursor is the one piece of attention that survives a restart,
        // so it is written down where it moves (Amendment #6).
        if let Some(room) = key.room() {
            let seq = self.attention.read_room_seq(&key);
            if seq > 0 {
                self.channels.log_read(room, seq);
            }
        }
        self.dirty.insert(key);
        Ok(AppReply::Accepted)
    }

    /// Record where an attachment's snapshot cut fell. Everything at or below it
    /// is that attachment's past.
    fn cut(&mut self, attachment: AttachmentId, cursor: u64) {
        if let Some(open) = self
            .attachments
            .iter_mut()
            .find(|open| open.id == attachment)
        {
            open.cursor = Some(cursor);
        }
    }

    /// Stamp one event and hand it to every attachment whose cut it is after.
    fn publish(&mut self, payload: Box<AppEventPayload>, caused_by: Option<OperationId>) {
        if self.serving {
            self.deferred.push((payload, caused_by));
            return;
        }
        self.seq = self.seq.saturating_add(1);
        let event = AppEvent {
            meta: EventMeta {
                seq: self.seq,
                ts: now_millis(),
                session_id: self.session.id.clone(),
                caused_by,
            },
            payload: *payload,
        };
        self.attachments.retain(|open| match open.cursor {
            // Not cut yet, or already covered by the cut it took: the snapshot
            // states this, so the stream does not have to.
            None => true,
            Some(cursor) if event.meta.seq <= cursor => true,
            Some(_) => send(open, AppFrame::Event(Box::new(event.clone()))),
        });
    }

    /// Every instance as the contract names it.
    fn agent_resources(&mut self) -> Vec<AgentResource> {
        let facts = self.agents.facts();
        facts
            .into_iter()
            .map(|fact| {
                let id = self.agent_id(&fact.name);
                let conversation_id = self.conversation_id(&ConvKey::Agent(fact.name.clone()));
                AgentResource {
                    id,
                    name: fact.name,
                    def: fact.def,
                    description: fact.description,
                    kind: match fact.kind {
                        crate::agents::AgentKind::Crew => AgentKind::Crew,
                        crate::agents::AgentKind::Hire => AgentKind::Hire,
                    },
                    state: agent_state(fact.state),
                    model: fact.model,
                    provider: fact.provider,
                    thinking: thinking_level(fact.thinking.as_deref()),
                    cwd: fact.cwd,
                    conversation_id: Some(conversation_id),
                    pending: fact.pending,
                    unacked: fact.unacked,
                    elapsed_ms: fact.elapsed_ms,
                    output_tokens: fact.output_tokens,
                    tool_uses: fact.tool_uses,
                    last_active_at: now_millis(),
                }
            })
            .collect()
    }

    /// Every room as the contract names it, attention included.
    fn room_resources(&mut self) -> Vec<RoomResource> {
        let facts = self.channels.facts();
        facts
            .into_iter()
            .map(|fact| {
                let key = ConvKey::Room(fact.name.clone());
                let id = self.room_id(&fact.name);
                let conversation_id = self.conversation_id(&key);
                let record = self.conversations.record_mut(&mut self.mint, &key);
                self.attention.seed(&key, record);
                let standing = self.attention.standing(&key, record, Vec::new());
                RoomResource {
                    id,
                    name: fact.name,
                    topic: None,
                    mode: match fact.mode {
                        crate::channels::ChannelMode::Serial => RoomMode::Relay,
                        crate::channels::ChannelMode::Free => RoomMode::Broadcast,
                    },
                    user_is_member: fact.members.iter().any(is_user),
                    members: fact.members,
                    conversation_id: Some(conversation_id),
                    message_count: fact.message_count,
                    last_seq: fact.last_seq,
                    unread: standing.unread,
                    mentions: standing.mentions,
                }
            })
            .collect()
    }

    /// Every background command as the contract names it.
    fn command_resources(&self) -> Vec<BackgroundCommandResource> {
        self.watch
            .command_facts()
            .into_iter()
            .map(|fact| BackgroundCommandResource {
                id: CommandId::new(format!("{}{}", CommandId::PREFIX, fact.id.0)),
                label: fact.label,
                command: fact.command,
                state: command_state(fact.state),
                started_at: now_millis().saturating_sub(fact.elapsed_ms),
                duration_ms: fact.elapsed_ms,
                // The watch table records a state and a line about it, not an
                // exit status; saying `0` here would be inventing one.
                exit_code: None,
                conversation_id: None,
                item_id: None,
            })
            .collect()
    }

    /// Publish one event per background command whose state or detail moved.
    ///
    /// A typed resource update rather than a label-only string: the parity
    /// ledger's "agent/task/command watch transitions" row asks for exactly this,
    /// and polling `resource/read` does not satisfy it (B1 review ruling ①).
    fn announce_commands(&mut self) {
        let facts = self.watch.command_facts();
        let mut changed = Vec::new();
        for fact in facts {
            let state = command_state(fact.state);
            let known = self.told.commands.get(&fact.id.0);
            if known.is_some_and(|(_, told, detail)| *told == state && detail == &fact.detail) {
                continue;
            }
            let id = match known {
                Some((id, ..)) => id.clone(),
                None => CommandId::new(format!("{}{}", CommandId::PREFIX, fact.id.0)),
            };
            self.told
                .commands
                .insert(fact.id.0, (id.clone(), state, fact.detail));
            changed.push(BackgroundCommandResource {
                id,
                label: fact.label,
                command: fact.command,
                state,
                started_at: now_millis().saturating_sub(fact.elapsed_ms),
                duration_ms: fact.elapsed_ms,
                exit_code: None,
                conversation_id: None,
                item_id: None,
            });
        }
        for command in changed {
            self.publish(
                Box::new(AppEventPayload::CommandChanged(
                    crate::app::event::CommandChanged { command },
                )),
                None,
            );
        }
    }

    /// Every direct message the session has a record of.
    fn delivery_resources(&mut self) -> Vec<DeliveryResource> {
        self.agents
            .delivery_facts()
            .into_iter()
            .map(|fact| DeliveryResource {
                id: DeliveryId::new(format!("{}{}", DeliveryId::PREFIX, fact.id.0)),
                from: fact.from,
                to: fact.to,
                private: true,
                state: delivery_state(&fact.state),
                message_item_id: None,
                follow_ups: u32::from(fact.follow_ups),
                max_follow_ups: u32::from(crate::agents::MAX_FOLLOW_UPS),
                reason: match &fact.state {
                    crate::agents::AckState::Dropped { reason } => Some(reason.clone()),
                    _ => None,
                },
                updated_at: now_millis(),
            })
            .collect()
    }

    /// Publish one event per instance whose state moved, and one per instance
    /// that is new. An instance that went away is not announced yet: `agent/gone`
    /// is not in the contract, and inventing a shape for it here would be
    /// deciding it from the implementation.
    fn announce_agents(&mut self) {
        let resources = self.agent_resources();
        let mut changed = Vec::new();
        for agent in resources {
            let summary = AgentSummary {
                state: agent.state,
                pending: agent.pending,
                unacked: agent.unacked,
            };
            let known = self.told.agents.get(&agent.name);
            if known.is_some_and(|(_, told)| told == &summary) {
                continue;
            }
            self.told
                .agents
                .insert(agent.name.clone(), (agent.id.clone(), summary));
            self.dirty.insert(ConvKey::Agent(agent.name.clone()));
            changed.push(agent);
        }
        for agent in changed {
            self.publish(
                Box::new(AppEventPayload::AgentChanged(AgentChanged { agent })),
                None,
            );
        }
    }

    /// Publish one event per room whose roster, head or attention moved.
    fn announce_rooms(&mut self) {
        let resources = self.room_resources();
        let mut changed = Vec::new();
        for room in resources {
            let summary = RoomSummary {
                members: room.members.clone(),
                last_seq: room.last_seq,
                unread: room.unread,
                mentions: room.mentions,
            };
            let known = self.told.rooms.get(&room.name);
            if known.is_some_and(|(_, told)| told == &summary) {
                continue;
            }
            self.told
                .rooms
                .insert(room.name.clone(), (room.id.clone(), summary));
            self.dirty.insert(ConvKey::Room(room.name.clone()));
            changed.push(room);
        }
        for room in changed {
            self.publish(
                Box::new(AppEventPayload::RoomChanged(RoomChanged { room })),
                None,
            );
        }
    }

    /// The identifier this conversation answers to, minted the first time it is
    /// named.
    fn conversation_id(&mut self, key: &ConvKey) -> ConversationId {
        self.conversations.id(&mut self.mint, key)
    }

    /// Publish what the turn registry changed, in the order it changed it.
    ///
    /// The registry answers in conversation *keys* because that is what a run and
    /// a page know; the identifier a client sees is stamped here, where the mint
    /// is. Nothing else translates between the two.
    fn announce_turn(&mut self, changes: Vec<TurnChange>) {
        for change in changes {
            // Three of these are not a payload at all: a completed item joins
            // its conversation's log on the way out, inbound prose is read by
            // the one walker before it becomes anything, and a warning is
            // feedback rather than turn state.
            let payload = match change {
                TurnChange::ItemCompleted {
                    conversation,
                    turn,
                    item,
                } => {
                    self.commit(&conversation, turn, *item);
                    continue;
                }
                TurnChange::Inbound {
                    conversation,
                    text,
                    first,
                    ..
                } => {
                    self.absorb_inbound(&conversation, &text, first);
                    continue;
                }
                TurnChange::Warning {
                    conversation, text, ..
                } => {
                    let id = self.conversation_id(&conversation);
                    self.raise_feedback(Some(id), text);
                    continue;
                }
                TurnChange::Started { conversation, turn } => {
                    let conversation_id = self.conversation_id(&conversation);
                    let mut turn = turn;
                    turn.conversation_id = conversation_id.clone();
                    self.dirty.insert(conversation);
                    AppEventPayload::TurnStarted(TurnChanged {
                        conversation_id,
                        turn,
                    })
                }
                TurnChange::RoundStarted {
                    conversation,
                    turn,
                    round,
                } => AppEventPayload::TurnRoundStarted(TurnRoundStarted {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    round,
                }),
                TurnChange::Retrying {
                    conversation,
                    turn,
                    round,
                    attempt,
                    max_attempts,
                    delay_ms,
                    removed,
                    code,
                    reason,
                } => {
                    // The checkpoint is authoritative: whatever the failed
                    // attempt had already committed leaves the log with it, so a
                    // later `conversation/read` cannot page an item the stream
                    // said was withdrawn.
                    self.withdraw(&conversation, &removed);
                    AppEventPayload::TurnRetrying(TurnRetrying {
                        conversation_id: self.conversation_id(&conversation),
                        turn_id: turn,
                        round,
                        attempt,
                        max_attempts,
                        delay_ms,
                        removed_item_ids: removed,
                        code,
                        reason,
                    })
                }
                TurnChange::RoundCompleted {
                    conversation,
                    turn,
                    round,
                    usage,
                } => AppEventPayload::TurnRoundCompleted(TurnRoundCompleted {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    round,
                    usage,
                }),
                TurnChange::Usage {
                    conversation,
                    turn,
                    usage,
                    context,
                } => AppEventPayload::TurnUsageUpdated(TurnUsageUpdated {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    usage,
                    context_usage: context,
                }),
                TurnChange::Completed { conversation, turn } => {
                    let conversation_id = self.conversation_id(&conversation);
                    let mut turn = turn;
                    turn.conversation_id = conversation_id.clone();
                    self.dirty.insert(conversation);
                    AppEventPayload::TurnCompleted(TurnChanged {
                        conversation_id,
                        turn,
                    })
                }
                TurnChange::ItemStarted {
                    conversation,
                    turn,
                    item,
                } => AppEventPayload::ItemStarted(ItemChanged {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    item: *item,
                }),
                TurnChange::ItemUpdated {
                    conversation,
                    turn,
                    item,
                } => AppEventPayload::ItemUpdated(ItemChanged {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    item: *item,
                }),
                TurnChange::ItemTextDelta {
                    conversation,
                    turn,
                    item,
                    delta_seq,
                    delta,
                } => AppEventPayload::ItemTextDelta(ItemDelta {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    item_id: item,
                    delta_seq,
                    delta,
                }),
                TurnChange::ItemReasoningDelta {
                    conversation,
                    turn,
                    item,
                    delta_seq,
                    delta,
                } => AppEventPayload::ItemReasoningDelta(ItemDelta {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    item_id: item,
                    delta_seq,
                    delta,
                }),
            };
            self.publish(Box::new(payload), None);
        }
    }

    /// Close the session: settle everything open, in one order, and let go.
    ///
    /// The order is the contract. A turn that was running still reaches a
    /// terminal state — `interrupted`, because that is what happened — so a
    /// client that saw `turn/started` is never left waiting for its end. Pending
    /// prompts fail closed rather than hanging their runs. Queued input is
    /// dropped as `cleared`, because there is no turn left to drain it into.
    ///
    /// Then the actor lets go of the instances, which is what breaks D29's cycle:
    /// the registry held an `Arc<Session>`, the session holds the handles that
    /// reach this loop, and until one of them goes the inbox can never close.
    fn close(&mut self, reason: SessionCloseReason) {
        let changes = self
            .turns
            .close_all(crate::app::snapshot::TurnStatus::Interrupted);
        self.announce_turn(changes);

        let (reply, _cancelled) = oneshot::channel();
        let changes = self.interactions.handle(
            crate::app::interaction::InteractionMsg::CancelAll {
                reason: crate::app::snapshot::InteractionCancelReason::SessionClosed,
                abandoned_only: false,
                reply,
            },
            &mut self.mint,
        );
        self.announce_interactions(changes);

        for conversation in self.queue.conversations() {
            let changes = self.queue.handle(
                crate::app::queue::QueueMsg::Clear { conversation },
                &mut self.mint,
            );
            self.announce_queue(changes);
        }

        let changes = self.operations.close_all();
        self.announce_operations(changes);

        self.agents.release();
        // Session assets die with the session; what a transcript refers to stays
        // reconstructable from its own durable content.
        self.assets.clear();
        self.session.state = SessionState::Closed;
        self.publish(
            Box::new(AppEventPayload::SessionClosed(SessionClosed {
                session_id: self.session.id.clone(),
                reason,
            })),
            None,
        );
    }

    /// `conversation/submit`, as a client asks for it.
    ///
    /// The routing is the same one the terminal front end reaches; what differs
    /// is who performs the result. A queue entry and a delivery the core does
    /// itself. A turn, a shell run and a slash command are still run by the
    /// console (B7) and dispatched by the action registry (B5), so the core says
    /// so by name rather than answering out of state it does not hold.
    fn serve_submit(
        &mut self,
        conversation_id: ConversationId,
        input: crate::app::command::Submission,
    ) -> Result<AppReply, AppError> {
        use crate::app::command::SubmitDisposition;
        use crate::app::submit::Route;
        let Some(conversation) = self.conversations.key(&conversation_id).cloned() else {
            return Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound,
            ));
        };
        let route = self.submit(crate::app::submit::SubmitRequest {
            conversation,
            input,
            main_busy: None,
            carries_attachments: false,
        });
        match route {
            Route::Queued(placement) => Ok(AppReply::Submitted(SubmitDisposition::Queued {
                queue_id: placement.id,
                position: placement.position,
                steer_eligible: placement.steer_eligible,
            })),
            Route::Nothing => Err(AppError::Refused(
                crate::app_server::protocol::error::ProtocolErrorKind::BadArgument,
            )),
            Route::Deliver { .. } => Err(AppError::Unserved("conversation/submit delivery")),
            Route::Turn { .. } | Route::Shell { .. } => {
                Err(AppError::Unserved("conversation/submit run"))
            }
            Route::Command { .. } => Err(AppError::Unserved("action/execute")),
        }
    }

    /// The one submission path (spec "One submission path").
    ///
    /// Reading the line and routing it both happen here, so the terminal front
    /// end and a GUI cannot disagree about what a leading slash, shell mode or an
    /// `@name` means. What the core can perform itself it performs — a queue
    /// entry is on the queue by the time this returns; a turn, a shell run and a
    /// slash command name work the caller still runs (B5 and B7 take those).
    fn submit(&mut self, request: crate::app::submit::SubmitRequest) -> crate::app::submit::Route {
        use crate::app::submit::{Decision, Route, compose, route};
        let origin = crate::app::submit::Origin {
            page: request.conversation.clone(),
            main_busy: request
                .main_busy
                .unwrap_or_else(|| self.turns.is_busy(&ConvKey::Main)),
        };
        let composed = compose(&request.input, &self.addressable());
        match route(composed, &origin) {
            Decision::Nothing => Route::Nothing,
            Decision::Turn { text } => Route::Turn { text },
            Decision::Shell { command } => Route::Shell { command },
            Decision::Command { line, on } => Route::Command { line, on },
            Decision::Deliver {
                target,
                text,
                addressed,
            } => Route::Deliver {
                target,
                text,
                addressed,
            },
            Decision::Queue(entry) => {
                let mut entry = *entry;
                entry.carries_attachments = request.carries_attachments;
                let (placement, changes) = self.queue.enqueue(entry, &mut self.mint);
                self.announce_queue(changes);
                Route::Queued(placement)
            }
        }
    }

    /// The names a sigil can resolve against, taken from the registries rather
    /// than from an accounting snapshot.
    fn addressable(&self) -> crate::app::submit::Addressable {
        crate::app::submit::Addressable {
            agents: self
                .agents
                .facts()
                .into_iter()
                .map(|fact| fact.name)
                .collect(),
            rooms: self
                .channels
                .facts()
                .into_iter()
                .map(|fact| fact.name)
                .collect(),
        }
    }

    /// Publish what the prompts changed. The ordered item a resolution committed
    /// comes before the resolution that names it: the item is what the model or
    /// the audit trail reads.
    fn announce_interactions(&mut self, changes: Vec<crate::app::interaction::InteractionChange>) {
        use crate::app::interaction::InteractionChange;
        for change in changes {
            let payload = match change {
                InteractionChange::Opened {
                    conversation,
                    interaction,
                } => {
                    let conversation_id = self.conversation_id(&conversation);
                    let mut interaction = *interaction;
                    interaction.conversation_id = conversation_id;
                    self.dirty.insert(conversation);
                    AppEventPayload::InteractionOpened(InteractionOpened { interaction })
                }
                InteractionChange::Committed {
                    conversation,
                    turn,
                    item,
                } => {
                    self.commit(&conversation, turn, *item);
                    continue;
                }
                InteractionChange::Resolved {
                    conversation,
                    id,
                    decision,
                    item,
                } => AppEventPayload::InteractionResolved(InteractionResolved {
                    interaction_id: id,
                    conversation_id: self.conversation_id(&conversation),
                    decision,
                    item_id: item,
                }),
                InteractionChange::Cancelled {
                    conversation,
                    id,
                    reason,
                } => {
                    self.dirty.insert(conversation.clone());
                    AppEventPayload::InteractionCancelled(InteractionCancelled {
                        interaction_id: id,
                        conversation_id: self.conversation_id(&conversation),
                        reason,
                    })
                }
            };
            self.publish(Box::new(payload), None);
        }
    }

    /// Publish what the queue changed. Absorption emits one event per entry in
    /// contiguous sequence order, so a client can page a bounded change rather
    /// than re-reading the whole queue.
    fn announce_queue(&mut self, changes: Vec<crate::app::queue::QueueChange>) {
        use crate::app::queue::QueueChange;
        for change in changes {
            let payload = match change {
                QueueChange::Added {
                    conversation,
                    revision,
                    position,
                    entry,
                    steer_eligible,
                } => {
                    let conversation_id = self.conversation_id(&conversation);
                    self.dirty.insert(conversation);
                    let origin_conversation_id = self.conversation_id(&entry.on);
                    AppEventPayload::QueueItemAdded(QueueItemAdded {
                        conversation_id,
                        revision,
                        position,
                        entry: QueueEntry {
                            id: entry.id,
                            origin_conversation_id,
                            text: entry.text,
                            attachments: entry.attachments,
                            steer_eligible,
                            queued_at: entry.queued_at,
                        },
                    })
                }
                QueueChange::Removed {
                    conversation,
                    revision,
                    id,
                    reason,
                } => {
                    self.dirty.insert(conversation.clone());
                    AppEventPayload::QueueItemRemoved(QueueItemRemoved {
                        conversation_id: self.conversation_id(&conversation),
                        revision,
                        queue_id: id,
                        reason,
                    })
                }
                QueueChange::AbsorbedItem { conversation, item } => {
                    let turn_id = item.turn_id.clone();
                    self.commit(&conversation, turn_id, *item);
                    continue;
                }
                QueueChange::Absorbed {
                    conversation,
                    revision,
                    id,
                    turn,
                    item,
                } => AppEventPayload::QueueItemAbsorbed(QueueItemAbsorbed {
                    conversation_id: self.conversation_id(&conversation),
                    revision,
                    queue_id: id,
                    turn_id: turn,
                    item_id: item,
                }),
            };
            self.publish(Box::new(payload), None);
        }
    }

    // -- Conversations, items and attention ---------------------------------

    /// Put one completed item in its conversation's log and publish it.
    ///
    /// Every item a client ever sees comes through here, whichever registry
    /// produced it: a turn's assistant message, a room post, a colleague's
    /// message, a permission receipt. One door means the log a `conversation/read`
    /// pages is the same history the stream described.
    fn commit(
        &mut self,
        conversation: &ConvKey,
        turn: Option<crate::app::ids::TurnId>,
        item: Item,
    ) {
        let conversation_id = self.conversation_id(conversation);
        let by_user = crate::app::attention::authored_by_user(&item);
        self.conversations
            .append(&mut self.mint, conversation, item.clone());
        if by_user {
            // The user's own words, and their own arrival in a room, are read by
            // definition — the same thing the domain says when a post advances
            // the sender's own cursor and an invite seats a late joiner at the
            // head.
            let record = self.conversations.record_mut(&mut self.mint, conversation);
            self.attention.mark_all_read(conversation, record);
        }
        self.dirty.insert(conversation.clone());
        self.publish(
            Box::new(AppEventPayload::ItemCompleted(ItemChanged {
                conversation_id,
                turn_id: turn,
                item,
            })),
            None,
        );
    }

    /// Take back exactly the items a retry checkpoint withdrew.
    fn withdraw(&mut self, conversation: &ConvKey, removed: &[ItemId]) {
        if removed.is_empty() {
            return;
        }
        let record = self.conversations.record_mut(&mut self.mint, conversation);
        let before = record.items.len();
        record.items.retain(|item| !removed.contains(&item.id));
        if record.items.len() != before {
            self.dirty.insert(conversation.clone());
        }
    }

    /// A completed item the core built itself, minted and committed in one step.
    fn commit_body(&mut self, conversation: &ConvKey, body: ItemBody) -> ItemId {
        let id: ItemId = self.mint.mint();
        let now = now_millis();
        let item = Item {
            id: id.clone(),
            status: ItemStatus::Completed,
            turn_id: None,
            started_at: Some(now),
            completed_at: Some(now),
            body,
        };
        self.commit(conversation, None, item);
        id
    }

    /// The room posts recorded since the last look become items in the room's
    /// conversation.
    ///
    /// A room post is a completed message item with no turn (spec "Item"): there
    /// is no synthetic room turn and no second representation of the same fact.
    /// Membership entries are items too — they are in the room's log, they take a
    /// sequence number, and a client that had to infer a join from a roster diff
    /// would be reading the room twice.
    fn absorb_posts(&mut self) {
        let facts = self.channels.facts();
        for fact in facts {
            let seen = self
                .told
                .rooms
                .get(&fact.name)
                .map_or(0, |(_, told)| told.last_seq);
            if fact.last_seq <= seen {
                continue;
            }
            let room_id = self.room_id(&fact.name);
            let conversation = ConvKey::Room(fact.name.clone());
            for message in self.channels.since(&fact.name, seen) {
                let mentions = crate::channels::mention_tokens(&message.text);
                let id: ItemId = self.mint.mint();
                let at = message.at.saturating_mul(1_000);
                let item = Item {
                    id,
                    status: ItemStatus::Completed,
                    turn_id: None,
                    started_at: Some(at),
                    completed_at: Some(at),
                    body: ItemBody::RoomMessage {
                        room_id: room_id.clone(),
                        from: message.from.clone(),
                        text: message.text.clone(),
                        room_seq: message.seq,
                        mentions,
                    },
                };
                self.commit(&conversation, None, item);
            }
        }
    }

    /// The messages main was handed since the last look become items in main's
    /// conversation, at the moment they arrived (D135).
    fn absorb_main_mail(&mut self) {
        for handed in self.channels.drain_delivered() {
            self.commit_body(
                &ConvKey::Main,
                ItemBody::PeerMessage {
                    from: handed.from,
                    to: None,
                    text: handed.text,
                    delivery_id: None,
                },
            );
        }
    }

    /// The messages an instance was handed since the last look become items in
    /// that instance's conversation, with the delivery record they belong to.
    fn absorb_deliveries(&mut self) {
        for handed in self.agents.drain_delivered() {
            let delivery = DeliveryId::new(format!("{}{}", DeliveryId::PREFIX, handed.id.0));
            let conversation = ConvKey::Agent(handed.to.clone());
            self.commit_body(
                &conversation,
                ItemBody::PeerMessage {
                    from: handed.from,
                    to: Some(handed.to),
                    text: handed.text,
                    delivery_id: Some(delivery),
                },
            );
        }
    }

    /// Publish one event per direct message whose state moved.
    ///
    /// D137 is what the domain enforces and this only reports: a colleague's turn
    /// prose never settles the sender's acknowledgement, so a record only reaches
    /// `answered` when a message came back.
    fn announce_deliveries(&mut self) {
        let facts = self.agents.delivery_facts();
        let mut changed = Vec::new();
        for fact in facts {
            let state = delivery_state(&fact.state);
            let follow_ups = u32::from(fact.follow_ups);
            let known = self.told.deliveries.get(&fact.id.0);
            if known.is_some_and(|(_, told, chases)| *told == state && *chases == follow_ups) {
                continue;
            }
            let id = match known {
                Some((id, ..)) => id.clone(),
                None => DeliveryId::new(format!("{}{}", DeliveryId::PREFIX, fact.id.0)),
            };
            self.told
                .deliveries
                .insert(fact.id.0, (id.clone(), state, follow_ups));
            changed.push(DeliveryResource {
                id,
                from: fact.from,
                to: fact.to,
                private: true,
                state,
                message_item_id: None,
                follow_ups,
                max_follow_ups: u32::from(crate::agents::MAX_FOLLOW_UPS),
                reason: match &fact.state {
                    crate::agents::AckState::Dropped { reason } => Some(reason.clone()),
                    _ => None,
                },
                updated_at: now_millis(),
            });
        }
        for delivery in changed {
            self.publish(
                Box::new(AppEventPayload::DeliveryChanged(DeliveryChanged {
                    delivery,
                })),
                None,
            );
        }
    }

    /// Publish what the operations changed, in the order they changed.
    fn announce_operations(&mut self, changes: Vec<crate::app::operation::OperationChange>) {
        use crate::app::operation::OperationChange;
        for change in changes {
            let payload = match change {
                OperationChange::Started(operation) => {
                    AppEventPayload::OperationStarted(crate::app::event::OperationChanged {
                        operation: *operation,
                    })
                }
                OperationChange::Progressed { id, progress } => {
                    AppEventPayload::OperationProgress(crate::app::event::OperationProgressed {
                        operation_id: id,
                        progress,
                    })
                }
                OperationChange::Completed(operation) => {
                    AppEventPayload::OperationCompleted(crate::app::event::OperationChanged {
                        operation: *operation,
                    })
                }
            };
            self.publish(Box::new(payload), None);
        }
    }

    /// Answer what the frontends ask about the waiting mail.
    fn serve_mail(&mut self, message: crate::app::mail::MailMsg) {
        use crate::app::mail::MailMsg;
        match message {
            MailMsg::Due { interrupted, reply } => {
                // Idle-only, as the per-post wake was: a running turn absorbs the
                // mail at its own next round, and a queued user message goes
                // first. Main's turn and main's queue, whatever page the screen
                // is on.
                let free = !interrupted
                    && !self.turns.is_busy(&ConvKey::Main)
                    && self.queue.count(&ConvKey::Main) == 0;
                let _ = reply.send(free && self.mail.due(std::time::Instant::now()));
            }
            MailMsg::Woke => self.mail.woke(),
            MailMsg::Waiting { reply } => {
                let _ = reply.send(self.mail.is_waiting());
            }
            #[cfg(test)]
            MailMsg::Rewind { by } => self.mail.rewind(by),
        }
    }

    /// Read where main's inbox stands and say so.
    ///
    /// The notice is what a GUI draws its "reading the mail" state from; the
    /// terminal front end has always drawn the same fact from the inbox itself.
    fn consider_mail(&mut self) {
        use crate::app::mail::Waiting;
        let (waiting, urgent) = self.channels.main_mail_stand();
        match self
            .mail
            .observe(std::time::Instant::now(), waiting, urgent)
        {
            Waiting::Unchanged => {}
            Waiting::Cleared => {
                if let Some(id) = self.mail_notice.take() {
                    self.publish(
                        Box::new(AppEventPayload::FeedbackCleared(
                            crate::app::event::FeedbackCleared { feedback_id: id },
                        )),
                        None,
                    );
                }
            }
            Waiting::Started { count, urgent } => {
                if let Some(id) = self.mail_notice.take() {
                    self.publish(
                        Box::new(AppEventPayload::FeedbackCleared(
                            crate::app::event::FeedbackCleared { feedback_id: id },
                        )),
                        None,
                    );
                }
                let id: FeedbackId = self.mint.mint();
                self.mail_notice = Some(id.clone());
                let feedback = Feedback {
                    id,
                    level: NoticeLevel::Info,
                    code: crate::error::MAIL_WAITING.to_string(),
                    message: format!("{count} waiting for main"),
                    detail: urgent.then(|| "urgent".to_string()),
                    conversation_id: Some(self.conversation_id(&ConvKey::Main)),
                    raised_at: now_millis(),
                    expires_at: None,
                };
                self.publish(
                    Box::new(AppEventPayload::FeedbackRaised(FeedbackRaised { feedback })),
                    None,
                );
            }
        }
    }

    /// Put back the read cursors a resumed sidecar remembered.
    ///
    /// The unit is the room's own sequence, which is the one attention has that
    /// outlives a restart: an item identifier dies with its epoch.
    fn restore_attention(&mut self, cursors: Vec<(String, u64)>) {
        for (room, seq) in cursors {
            let key = ConvKey::Room(room);
            let record = self.conversations.record_mut(&mut self.mint, &key);
            let last = record
                .items
                .iter()
                .rev()
                .find(|item| {
                    matches!(&item.body, ItemBody::RoomMessage { room_seq, .. } if *room_seq <= seq)
                })
                .map(|item| item.id.clone());
            self.attention
                .mark_read(&key, record, last.as_ref(), Some(seq));
            self.dirty.insert(key);
        }
    }

    /// Raise one warning as feedback with a stable code.
    fn raise_feedback(&mut self, conversation: Option<ConversationId>, message: String) {
        let feedback = Feedback {
            id: self.mint.mint::<FeedbackId>(),
            level: NoticeLevel::Warning,
            code: crate::error::RUNTIME_WARNING.to_string(),
            message,
            detail: None,
            conversation_id: conversation,
            raised_at: now_millis(),
            expires_at: None,
        };
        self.feedback.push(feedback.clone());
        // Bounded on purpose: a session that warned a thousand times is a session
        // with a problem, not a session whose snapshot should carry a thousand
        // warnings.
        while self.feedback.len() > MAX_FEEDBACK {
            self.feedback.remove(0);
        }
        self.publish(
            Box::new(AppEventPayload::FeedbackRaised(FeedbackRaised { feedback })),
            None,
        );
    }

    /// Read one inbound block with the one walker and commit what it names.
    fn absorb_inbound(&mut self, conversation: &ConvKey, text: &str, first: bool) {
        let who = match conversation {
            ConvKey::Agent(name) => name.clone(),
            _ => crate::channels::MAIN_NAME.to_string(),
        };
        let at = now_millis();
        for filed in crate::app::projection::inbound(&who, text, at / 1_000, first) {
            let code = match filed.target {
                crate::app::projection::Target::Intake => "intake",
                _ => "runtime",
            };
            self.commit_body(
                conversation,
                ItemBody::Notice {
                    code: code.to_string(),
                    level: NoticeLevel::Info,
                    text: filed.post.text,
                },
            );
        }
    }

    /// The identifier this instance answers to, minted the first time it is
    /// named.
    fn agent_id(&mut self, name: &str) -> AgentId {
        if let Some((id, _)) = self.told.agents.get(name) {
            return id.clone();
        }
        if let Some(id) = self.told.agent_ids.get(name) {
            return id.clone();
        }
        let id: AgentId = self.mint.mint();
        self.told.agent_ids.insert(name.to_string(), id.clone());
        id
    }

    /// The identifier this room answers to, minted the first time it is named.
    fn room_id(&mut self, name: &str) -> RoomId {
        if let Some((id, _)) = self.told.rooms.get(name) {
            return id.clone();
        }
        if let Some(id) = self.told.room_ids.get(name) {
            return id.clone();
        }
        let id: RoomId = self.mint.mint();
        self.told.room_ids.insert(name.to_string(), id.clone());
        id
    }

    /// Publish one summary per conversation whose state moved, and one per
    /// conversation that is new.
    ///
    /// The revision is the count of summaries published: a client that carries it
    /// back on `markRead` is naming the exact view it was looking at.
    fn announce_conversations(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        let dirty: Vec<ConvKey> = std::mem::take(&mut self.dirty).into_iter().collect();
        let mut changed = Vec::new();
        for key in dirty {
            let summary = self.summarize(&key);
            let told = self.told.conversations.get(&key);
            if told.is_some_and(|told| same_summary(told, &summary)) {
                continue;
            }
            let created = told.is_none();
            let mut summary = summary;
            summary.revision = told.map_or(1, |told| told.revision.saturating_add(1));
            let record = self.conversations.record_mut(&mut self.mint, &key);
            record.revision = summary.revision;
            record.announced = true;
            self.told.conversations.insert(key.clone(), summary.clone());
            changed.push((created, summary));
        }
        for (created, conversation) in changed {
            let changed = ConversationChanged { conversation };
            self.publish(
                Box::new(if created {
                    AppEventPayload::ConversationCreated(changed)
                } else {
                    AppEventPayload::ConversationUpdated(changed)
                }),
                None,
            );
        }
    }

    /// One conversation as it stands, attention included.
    fn summarize(&mut self, key: &ConvKey) -> ConversationSummary {
        let id = self.conversation_id(key);
        let kind = match key {
            ConvKey::Main => ConversationKind::Main,
            ConvKey::Agent(name) => ConversationKind::Agent {
                agent_id: self.agent_id(name),
            },
            ConvKey::Room(name) => ConversationKind::Room {
                room_id: self.room_id(name),
            },
        };
        let obligations = self.obligations(key);
        let is_member = match key {
            ConvKey::Room(name) => self
                .channels
                .facts()
                .into_iter()
                .any(|room| &room.name == name && room.members.iter().any(is_user)),
            _ => true,
        };
        let active_turn_id = self.turns.active_turns().into_iter().find_map(|turn| {
            (self.conversations.key(&turn.conversation_id) == Some(key)).then_some(turn.id)
        });
        let run_state = match key {
            ConvKey::Room(_) => ConversationRunState::Passive,
            _ if active_turn_id.is_some() => ConversationRunState::Running,
            ConvKey::Agent(name) => match self
                .agents
                .facts()
                .into_iter()
                .find(|fact| &fact.name == name)
            {
                Some(fact) if fact.state == crate::agents::AgentState::Stopped => {
                    ConversationRunState::Stopped
                }
                Some(fact) if fact.state == crate::agents::AgentState::Running => {
                    ConversationRunState::Running
                }
                _ => ConversationRunState::Idle,
            },
            ConvKey::Main => ConversationRunState::Idle,
        };
        let (queue_revision, queue_count) = self.queue.stand(key);
        let pending_interactions = self.interactions.pending_in(key);
        let record = self.conversations.record_mut(&mut self.mint, key);
        let last_item_id = record.last_item_id();
        let history_generation = record.history_generation;
        let last_activity_at = record.last_activity_at;
        let revision = record.revision;
        // A conversation nobody has opened starts read: its past is not news.
        self.attention.seed(key, record);
        let standing = self.attention.standing(key, record, obligations);
        ConversationSummary {
            id,
            kind,
            title: key.title(),
            revision,
            history_generation,
            run_state,
            active_turn_id,
            unread: standing.unread,
            mentions: standing.mentions,
            read_cursor: standing.read_cursor,
            last_item_id,
            obligations: standing.obligations,
            is_member,
            queue_revision,
            queue_count,
            pending_interactions,
            last_activity_at,
        }
    }

    /// What this conversation is waiting on the user for, read from the
    /// registries rather than inferred from prose.
    fn obligations(&self, key: &ConvKey) -> Vec<Obligation> {
        let mut owed = Vec::new();
        match key {
            ConvKey::Room(name) => {
                for (room, mention) in self.channels.open_mentions() {
                    if &room != name {
                        continue;
                    }
                    if mention.to == crate::channels::USER_NAME
                        || mention.to == crate::channels::ALL_NAME
                    {
                        owed.push(crate::app::attention::mention_debt(
                            &mention.from,
                            mention.at.saturating_mul(1_000),
                        ));
                    }
                }
            }
            ConvKey::Agent(name) => {
                for fact in self.agents.delivery_facts() {
                    if &fact.to != name || fact.from != crate::channels::USER_NAME {
                        continue;
                    }
                    if fact.state.is_outstanding() {
                        owed.push(crate::app::attention::unanswered(name, now_millis()));
                    }
                }
            }
            ConvKey::Main => {}
        }
        if self.interactions.pending_in(key) > 0 {
            owed.push(crate::app::attention::awaiting_user(now_millis()));
        }
        owed
    }

    fn deliver(&mut self, attachment: AttachmentId, frame: AppFrame) {
        self.attachments
            .retain(|open| open.id != attachment || send(open, frame.clone()));
    }
}

/// Write one frame, or say the attachment is over.
///
/// The actor never waits on a frontend: it is the whole process's ordering
/// point, and a blocked write here would stop every other conversation. A
/// frontend that has stopped reading loses its attachment and must attach and
/// read again — the transport's own backpressure policy is B6's.
fn send(attachment: &Attachment, frame: AppFrame) -> bool {
    attachment.frames.try_send(frame).is_ok()
}

/// How many standing warnings a snapshot carries. A session that warned a
/// thousand times has a problem; its snapshot should not.
const MAX_FEEDBACK: usize = 32;

/// How much of a collection a read carries when the caller names no limit.
const DEFAULT_PAGE: u32 = 100;

/// A member entry that is the human.
fn is_user(member: &String) -> bool {
    member == crate::channels::USER_NAME
}

/// Where a message stands, in the vocabulary the wire uses.
///
/// The translation is deliberate. The domain's `Queued` means "in the receiver's
/// inbox, unread", which on the wire is **delivered**; the domain's `Delivered`
/// means "folded into the receiver's prompt", which on the wire is **read**.
/// Those are exactly D135's two moments, named for what each one means to the
/// sender. The wire's `queued` — accepted but not yet in an inbox — cannot happen
/// while delivery is one step.
fn delivery_state(state: &crate::agents::AckState) -> DeliveryState {
    match state {
        crate::agents::AckState::Queued => DeliveryState::Delivered,
        crate::agents::AckState::Delivered { .. } => DeliveryState::Read,
        crate::agents::AckState::Answered { .. } => DeliveryState::Answered,
        crate::agents::AckState::Dropped { .. } => DeliveryState::Dropped,
    }
}

/// Whether two summaries say the same thing. The revision is excluded because it
/// is what this decides: a summary that changed gets the next one.
fn same_summary(told: &ConversationSummary, fresh: &ConversationSummary) -> bool {
    let mut fresh = fresh.clone();
    fresh.revision = told.revision;
    // The last-activity stamp moves with the log, and the log moving is already
    // a change; comparing the clock itself would make every equal summary differ.
    fresh.last_activity_at = told.last_activity_at;
    &fresh == told
}

/// The domain's thinking selection, as the contract names it. Absent is the
/// level "off" rather than an unknown, which is what makes a snapshot always
/// answer the question.
fn thinking_level(level: Option<&str>) -> ThinkingLevel {
    match level {
        None => ThinkingLevel::Off,
        Some(level) => ThinkingLevel::ALL
            .into_iter()
            .find(|known| known.as_str() == level)
            .unwrap_or(ThinkingLevel::Off),
    }
}

/// The domain's watch state, as the contract names a background command's.
fn command_state(state: crate::watch::WatchState) -> BackgroundCommandState {
    match state {
        crate::watch::WatchState::Running => BackgroundCommandState::Running,
        crate::watch::WatchState::Idle => BackgroundCommandState::Idle,
        crate::watch::WatchState::Done => BackgroundCommandState::Done,
        crate::watch::WatchState::Failed => BackgroundCommandState::Failed,
        crate::watch::WatchState::Cancelled => BackgroundCommandState::Cancelled,
    }
}

/// The domain's instance state, as the contract names it.
fn agent_state(state: crate::agents::AgentState) -> AgentState {
    match state {
        crate::agents::AgentState::Running => AgentState::Running,
        crate::agents::AgentState::Idle => AgentState::Idle,
        crate::agents::AgentState::Stopped => AgentState::Stopped,
    }
}

fn empty_collection<T>() -> Collection<T> {
    Collection {
        revision: 0,
        count: 0,
        active: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::CatalogChanged;
    use crate::app::snapshot::CatalogKind;
    use crate::app::{AppCore, AppPublisher, RequestId};

    fn catalog(revision: u64) -> AppEventPayload {
        AppEventPayload::CatalogChanged(CatalogChanged {
            catalog: CatalogKind::Models,
            revision,
        })
    }

    fn revision_of(frame: &AppFrame) -> u64 {
        match frame {
            AppFrame::Event(event) => match &event.payload {
                AppEventPayload::CatalogChanged(changed) => changed.revision,
                other => panic!("expected a catalog event, got {other:?}"),
            },
            other => panic!("expected an event, got {other:?}"),
        }
    }

    fn publish(publisher: &AppPublisher, revision: u64) {
        publisher
            .publish(catalog(revision), None)
            .unwrap_or_else(|error| panic!("{error}"));
    }

    /// Attach, then take the cut every attachment starts from.
    async fn attached(core: &AppCore, label: &str) -> (AppLink, SessionSnapshot) {
        let mut link = core
            .attach(AttachRequest::new(label))
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

    /// The one ordering point has to hold under every producer at once: N agent
    /// runs publishing concurrently still make one history, with no number
    /// skipped and none repeated.
    #[tokio::test]
    async fn concurrent_producers_still_make_one_gapless_sequence() {
        const PRODUCERS: u64 = 8;
        const EACH: u64 = 25;
        let core = AppCore::start(SessionSetup::default());
        let (mut link, snapshot) = attached(&core, "test").await;
        assert_eq!(snapshot.event_cursor, 0, "nothing has happened yet");

        let mut runs = Vec::new();
        for producer in 0..PRODUCERS {
            let publisher = core.publisher();
            runs.push(tokio::spawn(async move {
                for step in 0..EACH {
                    publish(&publisher, producer * EACH + step);
                }
            }));
        }
        for run in runs {
            run.await.unwrap_or_else(|error| panic!("{error}"));
        }

        let mut seen = Vec::new();
        for _ in 0..(PRODUCERS * EACH) {
            match link.recv().await {
                Some(AppFrame::Event(event)) => seen.push(event.meta.seq),
                other => panic!("expected an event, got {other:?}"),
            }
        }
        assert_eq!(
            seen,
            (1..=PRODUCERS * EACH).collect::<Vec<_>>(),
            "sequence numbers are strictly increasing and gapless, in arrival order"
        );
    }

    /// The cut is a barrier, not a hint: what happened before it is in the
    /// snapshot, so the stream starts strictly after it.
    #[tokio::test]
    async fn a_snapshot_cut_suppresses_what_it_already_contains() {
        let core = AppCore::start(SessionSetup::default());
        let publisher = core.publisher();
        let mut link = core
            .attach(AttachRequest::new("test"))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        for revision in 0..3 {
            publish(&publisher, revision);
        }

        link.request(AppRequest::Query {
            id: RequestId(7),
            query: AppQuery::ReadSession,
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let cursor = match link.recv().await {
            Some(AppFrame::Reply {
                id,
                result: Ok(AppReply::Session(snapshot)),
            }) => {
                assert_eq!(id, RequestId(7), "the reply names the request it answers");
                snapshot.event_cursor
            }
            other => panic!("expected the snapshot first, got {other:?}"),
        };
        assert_eq!(cursor, 3, "the cut names the last event it contains");

        publish(&publisher, 99);
        match link.recv().await {
            Some(AppFrame::Event(event)) => {
                assert_eq!(
                    event.meta.seq,
                    cursor + 1,
                    "the first event after a cut is the next one, never a replay"
                );
                assert!(event.meta.ts > 0, "the actor stamps the instant it decided");
            }
            other => panic!("expected the event after the cut, got {other:?}"),
        }
    }

    /// Two frontends attach at different moments and each reads from its own
    /// cut. Neither is told the other's past.
    #[tokio::test]
    async fn two_attachments_read_from_their_own_cursors() {
        let core = AppCore::start(SessionSetup::default());
        let publisher = core.publisher();
        let (mut early, early_snapshot) = attached(&core, "early").await;
        assert_eq!(early_snapshot.event_cursor, 0);
        publish(&publisher, 1);
        publish(&publisher, 2);

        let (mut late, late_snapshot) = attached(&core, "late").await;
        assert_eq!(late_snapshot.event_cursor, 2, "the second cut is later");
        publish(&publisher, 3);

        let mut seen = Vec::new();
        for _ in 0..3 {
            match early.recv().await {
                Some(frame) => seen.push(revision_of(&frame)),
                None => panic!("the early attachment closed"),
            }
        }
        assert_eq!(seen, vec![1, 2, 3], "the early attachment saw all three");
        match late.recv().await {
            Some(frame) => assert_eq!(
                revision_of(&frame),
                3,
                "the late attachment starts after its own cut"
            ),
            None => panic!("the late attachment closed"),
        }
    }

    /// Every identifier comes from the actor, inside one epoch.
    #[tokio::test]
    async fn the_session_is_identified_by_the_epoch_that_minted_it() {
        let core = AppCore::start(SessionSetup {
            title: "Notes".to_string(),
            provider: "default".to_string(),
            model: "sonnet".to_string(),
            ..SessionSetup::default()
        });
        let (_link, snapshot) = attached(&core, "test").await;
        assert!(snapshot.session.id.as_str().starts_with(SessionId::PREFIX));
        assert!(snapshot.session.epoch.as_str().starts_with(EpochId::PREFIX));
        assert_eq!(snapshot.session.title, "Notes");
        assert_eq!(snapshot.config.model, "sonnet");
        assert_eq!(snapshot.session.state, SessionState::Active);
        assert!(snapshot.active_turns.is_empty(), "nothing is running yet");
        match snapshot.conversations.active.as_slice() {
            [main] => {
                assert_eq!(main.kind, crate::app::snapshot::ConversationKind::Main);
                assert_eq!(main.unread, 0);
                assert!(main.obligations.is_empty());
            }
            other => panic!("a session has exactly one conversation to start with: {other:?}"),
        }
    }

    /// A mutation the core cannot serve yet is refused by name. The reply is
    /// still a reply: the request is answered, never dropped.
    #[tokio::test]
    async fn the_skeleton_refuses_by_name_what_it_does_not_serve_yet() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core, "test").await;
        link.request(AppRequest::Command {
            id: RequestId(2),
            command: AppCommand::ReclaimQueueTail {
                conversation_id: crate::app::ids::ConversationId::new("conv_1"),
                expected_revision: None,
            },
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        match link.recv().await {
            Some(AppFrame::Reply { id, result }) => {
                assert_eq!(id, RequestId(2));
                assert_eq!(result, Err(AppError::Unserved("queue/reclaimTail")));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Reading a conversation never marks it read; only `markRead` does, and it
    /// names the revision it believed it was looking at (spec invariant #14).
    #[tokio::test]
    async fn marking_read_names_the_view_it_was_looking_at() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, snapshot) = attached(&core, "test").await;
        let main = snapshot
            .conversations
            .active
            .first()
            .map(|summary| summary.id.clone())
            .unwrap_or_else(|| panic!("main exists"));
        let revision = snapshot
            .conversations
            .active
            .first()
            .map(|summary| summary.revision)
            .unwrap_or_default();

        link.request(AppRequest::Command {
            id: RequestId(2),
            command: AppCommand::MarkRead {
                conversation_id: main.clone(),
                last_item_id: None,
                last_room_seq: None,
                expected_revision: revision.saturating_add(7),
            },
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        match next_reply(&mut link, RequestId(2)).await {
            Err(AppError::Refused(kind)) => assert_eq!(
                kind,
                crate::app_server::protocol::error::ProtocolErrorKind::StaleRevision,
                "a view the client never saw cannot clear attention"
            ),
            other => panic!("expected a stale revision, got {other:?}"),
        }

        link.request(AppRequest::Command {
            id: RequestId(3),
            command: AppCommand::MarkRead {
                conversation_id: main,
                last_item_id: None,
                last_room_seq: None,
                expected_revision: revision,
            },
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            next_reply(&mut link, RequestId(3)).await,
            Ok(AppReply::Accepted)
        );
    }

    /// A core whose settings and directories are real, so the reads have
    /// something to read.
    fn configured(tag: &str) -> (AppCore, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!("bingo-reads-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::create_dir_all(home.join("bingo"));
        let _ = std::fs::write(
            home.join("bingo").join("settings.json"),
            r#"{"apiKey": "sk-test", "permissions": {"allow": ["Bash(cargo test:*)"]},
                "mcpServers": {"docs": {"command": "docs-server"}}}"#,
        );
        let settings = crate::settings::load_settings(&home, &home)
            .unwrap_or_else(|error| panic!("settings: {error}"));
        let core = AppCore::start(SessionSetup {
            model: "sonnet".to_string(),
            provider: "default".to_string(),
            catalog: crate::app::catalog::CatalogSource::load(&home, &home, &home, settings),
            ..SessionSetup::default()
        });
        (core, home)
    }

    async fn read(link: &mut AppLink, id: u64, query: AppQuery) -> Result<AppReply, AppError> {
        link.request(AppRequest::Query {
            id: RequestId(id),
            query,
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        next_reply(link, RequestId(id)).await
    }

    /// `config/read` answers out of settings: the effective selection, the rules
    /// that are in force, and which layer file contributed what.
    #[tokio::test]
    async fn the_configuration_says_where_it_came_from() {
        let (core, home) = configured("config");
        let (mut link, _) = attached(&core, "test").await;
        match read(&mut link, 2, AppQuery::ReadConfig).await {
            Ok(AppReply::Config(config)) => {
                assert_eq!(config.model, "sonnet");
                assert_eq!(
                    config
                        .permissions
                        .iter()
                        .map(|rule| rule.rule.as_str())
                        .collect::<Vec<_>>(),
                    vec!["Bash(cargo test:*)"]
                );
                assert!(
                    config
                        .layers
                        .iter()
                        .any(|layer| layer.keys.iter().any(|key| key == "permissions")),
                    "the layer that carries the rules is named: {:?}",
                    config.layers
                );
                assert_eq!(
                    config
                        .mcp_servers
                        .iter()
                        .map(|server| (server.name.as_str(), server.status))
                        .collect::<Vec<_>>(),
                    vec![("docs", crate::app::snapshot::McpStatus::Disconnected)],
                    "configured is not connected"
                );
            }
            other => panic!("expected a configuration, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `catalog/read` is answerable from the same state, and what MCP reports
    /// lands in both the catalog and the configuration.
    #[tokio::test]
    async fn a_catalog_reads_settings_and_takes_what_mcp_reports() {
        let (core, home) = configured("catalog");
        let (mut link, _) = attached(&core, "test").await;
        match read(
            &mut link,
            2,
            AppQuery::ReadCatalog {
                catalog: CatalogKind::Providers,
                provider: None,
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Catalog(catalog)) => match *catalog {
                crate::app::snapshot::Catalog::Providers(page) => assert_eq!(
                    page.items.first().map(|info| info.name.as_str()),
                    Some("default")
                ),
                other => panic!("expected providers, got {other:?}"),
            },
            other => panic!("expected a catalog, got {other:?}"),
        }

        core.report_mcp(vec![crate::app::snapshot::McpServerState {
            name: "docs".to_string(),
            enabled: true,
            status: crate::app::snapshot::McpStatus::Connected,
            tools: 3,
            error: None,
        }]);
        settle(&core.control).await;
        match read(
            &mut link,
            3,
            AppQuery::ReadCatalog {
                catalog: CatalogKind::McpServers,
                provider: None,
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Catalog(catalog)) => match *catalog {
                crate::app::snapshot::Catalog::McpServers(page) => {
                    assert_eq!(
                        page.items[0].status,
                        crate::app::snapshot::McpStatus::Connected
                    );
                    assert_eq!(page.items[0].tools, 3);
                }
                other => panic!("expected servers, got {other:?}"),
            },
            other => panic!("expected a catalog, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `action/list` publishes the registry, and availability is answered from
    /// the state the core is actually in.
    #[tokio::test]
    async fn the_actions_are_listed_with_what_can_run_now() {
        let (core, home) = configured("actions");
        let (mut link, _) = attached(&core, "test").await;
        match read(
            &mut link,
            2,
            AppQuery::ListActions {
                origin_conversation_id: None,
            },
        )
        .await
        {
            Ok(AppReply::Actions { actions, .. }) => {
                assert_eq!(actions.len(), crate::app::action::ACTIONS.len());
                let theme = actions
                    .iter()
                    .find(|info| info.id.as_str() == "theme.set")
                    .unwrap_or_else(|| panic!("theme.set is published"));
                assert!(theme.available, "a preference needs nothing");
                assert_eq!(
                    theme
                        .arguments
                        .first()
                        .map(|argument| argument.choices.as_slice()),
                    Some(["dark".to_string(), "light".to_string(), "auto".to_string()].as_slice()),
                    "the argument schema comes from the same table"
                );
                let compact = actions
                    .iter()
                    .find(|info| info.id.as_str() == "conversation.compact")
                    .unwrap_or_else(|| panic!("compact is published"));
                assert!(
                    !compact.available && compact.unavailable_reason.is_some(),
                    "an action that needs an engine says so rather than failing later"
                );
            }
            other => panic!("expected actions, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `resource/read` pages the same collections a session snapshot carries a
    /// bounded head of, and `queue/read` reads one conversation's queue.
    #[tokio::test]
    async fn the_runtime_collections_and_the_queue_are_paged() {
        let (core, home) = configured("resources");
        let (mut link, snapshot) = attached(&core, "test").await;
        let main = snapshot
            .conversations
            .active
            .first()
            .map(|summary| summary.id.clone())
            .unwrap_or_else(|| panic!("main exists"));
        match read(
            &mut link,
            2,
            AppQuery::ReadResource {
                resource: crate::app::snapshot::ResourceKind::Rooms,
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Resource(page)) => match *page {
                crate::app::snapshot::ResourcePage::Rooms(rooms) => {
                    assert!(rooms.items.is_empty(), "no rooms yet, and that is a page");
                    assert_eq!(rooms.next_cursor, None);
                }
                other => panic!("expected rooms, got {other:?}"),
            },
            other => panic!("expected a resource page, got {other:?}"),
        }
        match read(
            &mut link,
            3,
            AppQuery::ReadQueue {
                conversation_id: main,
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Queue { entries, count }) => {
                assert_eq!(count, 0);
                assert!(entries.items.is_empty());
            }
            other => panic!("expected a queue, got {other:?}"),
        }
        match read(
            &mut link,
            4,
            AppQuery::ReadQueue {
                conversation_id: crate::app::ids::ConversationId::new("conv_nope"),
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Err(AppError::Refused(kind)) => assert_eq!(
                kind,
                crate::app_server::protocol::error::ProtocolErrorKind::ConversationNotFound
            ),
            other => panic!("expected a refusal, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `asset/registerPath` then `asset/readChunk`: the bytes go into the
    /// server's own storage, the caller's file is no longer needed, and the
    /// image shows up in the catalog that lists them.
    #[tokio::test]
    async fn an_asset_is_registered_and_read_back_through_the_core() {
        let (core, home) = configured("assets");
        let (mut link, _) = attached(&core, "test").await;
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 2, image::Rgba([9, 9, 9, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let source = home.join("shot.png");
        std::fs::write(&source, &png).unwrap_or_else(|error| panic!("{error}"));

        link.request(AppRequest::Command {
            id: RequestId(2),
            command: AppCommand::RegisterAsset {
                path: source.clone(),
                expected_mime: Some("image/png".to_string()),
                expected_sha256: None,
            },
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        let record = match next_reply(&mut link, RequestId(2)).await {
            Ok(AppReply::Asset(record)) => *record,
            other => panic!("expected an asset, got {other:?}"),
        };
        assert_eq!(record.bytes, png.len() as u64);
        assert_eq!((record.width, record.height), (Some(4), Some(2)));
        // The caller's file is the server's business no longer.
        std::fs::remove_file(&source).unwrap_or_else(|error| panic!("{error}"));

        let mut back = Vec::new();
        let mut offset = 0;
        let mut id = 3;
        loop {
            let reply = read(
                &mut link,
                id,
                AppQuery::ReadAssetChunk {
                    asset_id: record.id.clone(),
                    offset,
                    length: 32,
                },
            )
            .await;
            let (data, next, eof) = match reply {
                Ok(AppReply::AssetChunk {
                    data,
                    next_offset,
                    eof,
                }) => (data, next_offset, eof),
                other => panic!("expected a chunk, got {other:?}"),
            };
            use base64::Engine;
            back.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap_or_else(|error| panic!("{error}")),
            );
            offset = next;
            id += 1;
            if eof {
                break;
            }
        }
        assert_eq!(back, png, "byte for byte, through the request path");

        match read(
            &mut link,
            id,
            AppQuery::ReadCatalog {
                catalog: CatalogKind::Images,
                provider: None,
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Catalog(catalog)) => match *catalog {
                crate::app::snapshot::Catalog::Images(page) => {
                    assert_eq!(
                        page.items
                            .iter()
                            .map(|image| image.asset_id.clone())
                            .collect::<Vec<_>>(),
                        vec![record.id.clone()]
                    );
                    assert_eq!(page.items[0].label.as_deref(), Some("shot.png"));
                }
                other => panic!("expected images, got {other:?}"),
            },
            other => panic!("expected a catalog, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `session/list` reads the transcripts on disk and marks the open one.
    #[tokio::test]
    async fn the_sessions_on_disk_are_listed() {
        let (core, home) = configured("sessions");
        let transcript = crate::transcript::create(&home, &home)
            .unwrap_or_else(|error| panic!("transcript: {error}"));
        let _ = transcript.append(&crate::api::types::Message::user_text("hi"));
        let (mut link, _) = attached(&core, "test").await;
        match read(
            &mut link,
            2,
            AppQuery::ListSessions {
                cursor: None,
                limit: None,
            },
        )
        .await
        {
            Ok(AppReply::Sessions(page)) => {
                assert_eq!(
                    page.items
                        .iter()
                        .map(|entry| entry.title.as_str())
                        .collect::<Vec<_>>(),
                    vec![transcript.name().as_str()]
                );
                assert!(page.items[0].message_count >= 1);
            }
            other => panic!("expected sessions, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The reply to `id`, skipping whatever the core said on the way.
    async fn next_reply(link: &mut AppLink, id: RequestId) -> Result<AppReply, AppError> {
        loop {
            match link.recv().await {
                Some(AppFrame::Reply { id: seen, result }) if seen == id => return result,
                Some(_) => {}
                None => panic!("the core closed"),
            }
        }
    }

    /// A frontend that stops reading loses its attachment rather than stalling
    /// the core. It sees the frames it was already handed, then the end.
    #[tokio::test]
    async fn a_frontend_that_stops_reading_loses_its_attachment() {
        let core = AppCore::start(SessionSetup::default());
        let publisher = core.publisher();
        let (mut link, _) = attached(&core, "silent").await;
        // One frame channel over, and then some: enqueueing never blocks, so
        // every one of these is accepted and the actor writes them out until the
        // silent frontend's channel is full.
        let published = (FRAME_CAPACITY + 8) as u64;
        for revision in 0..published {
            publish(&publisher, revision);
        }
        // The barrier is what makes the overflow a fact rather than a race with
        // the reader below: by the time it answers, every publish above has been
        // written or dropped.
        settle(&core.control).await;
        let mut delivered = 0;
        while let Some(frame) = link.recv().await {
            assert_eq!(revision_of(&frame), delivered);
            delivered += 1;
        }
        assert_eq!(
            delivered, FRAME_CAPACITY as u64,
            "what fit was delivered; the rest closed the attachment instead of blocking the core"
        );
    }
}
