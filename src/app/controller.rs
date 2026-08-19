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

mod resources;
mod run;
#[cfg(test)]
mod tests;

use tokio::sync::{mpsc, oneshot};

use crate::app::attention::Attention;
use crate::app::command::{AppCommand, AppQuery};
use crate::app::conversation::{ConvKey, Conversations, StalePage};
use crate::app::event::{
    AppEvent, AppEventPayload, ConversationChanged, EventMeta, FeedbackRaised,
    InteractionCancelled, InteractionOpened, InteractionResolved, ItemChanged, ItemDelta,
    QueueItemAbsorbed, QueueItemAdded, QueueItemRemoved, SessionClosed, TurnChanged, TurnRetrying,
    TurnRoundCompleted, TurnRoundStarted, TurnUsageUpdated,
};
use crate::app::ids::{
    AgentId, CommandId, ConversationId, DeliveryId, EpochId, FeedbackId, IdMint, ItemId,
    OperationId, RoomId, SessionId, now_millis,
};
use crate::app::snapshot::{
    AgentState, BackgroundCommandState, Collection, ConfigSnapshot, ConversationKind,
    ConversationRunState, ConversationSummary, DeliveryState, Feedback, Item, ItemBody, ItemStatus,
    NoticeLevel, Obligation, Page, QueueEntry, RuntimeCollections, ServerCapabilities,
    SessionCloseReason, SessionSnapshot, SessionState, SessionSummary,
};
use crate::app::turn::TurnChange;
use crate::app::{AppError, AppFrame, AppReply, AppRequest, AttachRequest, SessionSetup};

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
        /// Minted by the attaching side, so attaching needs no answer back.
        attachment: AttachmentId,
        /// The frame channel the frontend already holds the reading half of.
        frames: mpsc::Sender<AppFrame>,
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
    /// The engine this session runs its work on, handed over once on the way up.
    Engine(crate::app::engine::Attached),
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
pub(crate) struct AttachmentId(pub(crate) u64);

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
    /// What runs a turn, a shell line, and the waking half of a delivery. `None`
    /// until something attaches one, which is the difference between a session
    /// that can be read and one that can run (B7).
    engine: Option<crate::app::engine::Attached>,
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
///
/// The task and the latest tool line are in, because they are what the row
/// *says*: a roster whose `Running · Read src/lib.rs` never moved would be
/// reporting the first tool of the run for the rest of it. They move once per
/// tool call and once per run, which is a transition rate, not a token rate.
#[derive(PartialEq, Eq)]
struct AgentSummary {
    state: AgentState,
    pending: u32,
    unacked: u32,
    prompt: String,
    recent_activity: Vec<String>,
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
            engine: None,
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
                    attachment,
                    frames,
                } => self.attach(request, attachment, frames),
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
                Control::Engine(engine) => self.engine = Some(engine),
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
                    // Shut the inbox before answering, not after: a caller that
                    // saw its close finish must not then be able to attach to a
                    // loop that is gone. Closing rejects new sends; what is
                    // already queued dies with the loop, which is what a closed
                    // session means.
                    inbox.close();
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

    /// Take a frontend on. Nothing is created here: the attaching side made the
    /// channel and the number, so this is the loop learning that a reader
    /// exists — and until that reader takes a cut, it is a reader of nothing
    /// (spec "Architecture").
    fn attach(&mut self, request: AttachRequest, id: AttachmentId, frames: mpsc::Sender<AppFrame>) {
        self.attachments.push(Attachment {
            id,
            label: request.label,
            frames,
            cursor: None,
        });
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
        match command {
            AppCommand::Interrupt {
                conversation_id,
                turn_id,
            } => self.serve_interrupt(&conversation_id, turn_id),
            AppCommand::RespondInteraction {
                interaction_id,
                activation,
                decision,
            } => self.serve_respond(interaction_id, activation, decision),
            AppCommand::ReclaimQueueTail {
                conversation_id,
                expected_revision,
            } => self.serve_reclaim(&conversation_id, expected_revision),
            AppCommand::DeleteSession { locator } => self.serve_delete_session(&locator),
            AppCommand::Execute {
                origin_conversation_id,
                precondition,
                action,
            } => self.serve_execute(&origin_conversation_id, precondition, action),
            // Which session this connection has is the transport's to decide:
            // one `AppCore` *is* one session, so starting or resuming another is
            // replacing this actor rather than asking it (spec "Resource model":
            // one session per connection). B6 owns that lifecycle.
            AppCommand::StartSession { .. } => Err(AppError::Unserved("session/start")),
            AppCommand::ResumeSession { .. } => Err(AppError::Unserved("session/resume")),
            // Answered above; the compiler is what keeps this exhaustive.
            AppCommand::RegisterAsset { .. }
            | AppCommand::MarkRead { .. }
            | AppCommand::Submit { .. }
            | AppCommand::CloseSession
            | AppCommand::Shutdown => Ok(AppReply::Accepted),
        }
    }

    /// `action/execute`: one registry decides what an action is called, whether
    /// it can run now, and what happens when it does.
    ///
    /// The origin conversation is carried rather than read off the screen, so a
    /// queued action acts on the page it was typed on (D135a). A precondition is
    /// checked against the live revision, so a stale write fails instead of
    /// overwriting a view somebody else refreshed.
    fn serve_execute(
        &mut self,
        origin: &ConversationId,
        precondition: Option<crate::app::snapshot::ResourceRevision>,
        action: crate::app::command::Action,
    ) -> Result<AppReply, AppError> {
        use crate::app::command::{ActionResult, SubmitDisposition};
        use crate::app_server::protocol::error::ProtocolErrorKind;
        if self.conversations.key(origin).is_none() {
            return Err(AppError::Refused(ProtocolErrorKind::ConversationNotFound));
        }
        let spec = crate::app::action::spec_of(&action);
        if spec.requires.unmet(self.availability()).is_some() {
            return Err(AppError::Refused(ProtocolErrorKind::ActionUnavailable));
        }
        if let Some(expected) = &precondition
            && expected.revision != self.revision_of(expected.scope)
        {
            return Err(AppError::Refused(ProtocolErrorKind::StaleRevision));
        }
        let status = self.apply_action(action)?;
        let revision = spec
            .precondition
            .map(|scope| crate::app::snapshot::ResourceRevision {
                scope,
                revision: self.revision_of(scope),
            });
        Ok(AppReply::Submitted(SubmitDisposition::Applied {
            result: ActionResult {
                status,
                revision,
                message: None,
            },
        }))
    }

    /// Where a revisioned resource stands right now.
    fn revision_of(&self, scope: crate::app::snapshot::RevisionScope) -> u64 {
        use crate::app::snapshot::RevisionScope;
        match scope {
            RevisionScope::Config => self.config.revision,
            RevisionScope::Session => self.session.updated_at,
            RevisionScope::Rooms => self.channels.facts().len() as u64,
            RevisionScope::Agents => self.agents.facts().len() as u64,
            RevisionScope::Conversation | RevisionScope::Queue => {
                self.queue.stand(&ConvKey::Main).0
            }
            RevisionScope::Catalog | RevisionScope::Tasks | RevisionScope::Team => 0,
        }
    }

    /// Perform one action the core owns outright.
    ///
    /// Every arm here changes state this loop holds. The ones that need a model,
    /// a transcript rewrite or a network round trip never reach it: their spec
    /// says they need an engine, and `action/list` says so too rather than
    /// letting a client find out by failing (B7 attaches the engine).
    fn apply_action(
        &mut self,
        action: crate::app::command::Action,
    ) -> Result<crate::app::command::ActionResultStatus, AppError> {
        use crate::app::command::{Action, ActionResultStatus};
        use crate::app_server::protocol::error::ProtocolErrorKind;
        let unavailable = || AppError::Refused(ProtocolErrorKind::ActionUnavailable);
        let user = crate::channels::USER_NAME;
        match action {
            Action::ThemeSet { theme } => {
                if self.config.theme == theme {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.config.theme = theme;
                self.persist(&serde_json::json!({ "theme": theme.as_str() }));
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::ThinkingSelect { level } => {
                if self.config.thinking == level {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.config.thinking = level;
                self.session.thinking = level;
                self.persist(&serde_json::json!({ "thinkingLevel": level.as_str() }));
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::PermissionModeSet { mode } => {
                if self.config.permission_mode == mode {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.config.permission_mode = mode;
                self.session.permission_mode = mode;
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::ModelSelect { model } => {
                if self.config.model == model {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.config.model = model.clone();
                self.session.model = model.clone();
                self.persist(&serde_json::json!({ "model": model }));
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::ProviderSelect { provider } => {
                if !self
                    .catalog
                    .providers()
                    .iter()
                    .any(|known| known.name == provider)
                {
                    return Err(AppError::Refused(ProtocolErrorKind::BadArgument));
                }
                if self.config.provider == provider {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.config.provider = provider.clone();
                self.session.provider = provider.clone();
                self.persist(&serde_json::json!({ "provider": provider }));
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::ProviderLogout { provider } => {
                let store = crate::auth::AuthStore::new(self.catalog.home());
                let held = matches!(store.get(&provider), Ok(Some(_)));
                if !held {
                    return Ok(ActionResultStatus::NoChange);
                }
                store.remove(&provider).map_err(|_| unavailable())?;
                self.config_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::PermissionRuleAdd { decision, rule } => {
                Ok(self.permission_rule(decision, rule, true))
            }
            Action::PermissionRuleRemove { decision, rule } => {
                Ok(self.permission_rule(decision, rule, false))
            }
            Action::McpEnable { server } => self.set_mcp_enabled(&server, true),
            Action::McpDisable { server } => self.set_mcp_enabled(&server, false),
            Action::SessionGarbageCollect => {
                let protected = match &self.session.locator {
                    crate::app::snapshot::SessionLocator::Path { path } => Some(path.as_path()),
                    _ => None,
                };
                let report = crate::storage::cleanup(self.catalog.home(), protected)
                    .map_err(|_| unavailable())?;
                Ok(if report.total() == 0 {
                    ActionResultStatus::NoChange
                } else {
                    ActionResultStatus::Applied
                })
            }
            Action::SessionChangeDirectory { path } => {
                let requested = if path.is_absolute() {
                    path
                } else {
                    self.session.cwd.join(path)
                };
                let resolved = std::fs::canonicalize(&requested)
                    .map_err(|_| AppError::Refused(ProtocolErrorKind::BadArgument))?;
                if !resolved.is_dir() {
                    return Err(AppError::Refused(ProtocolErrorKind::BadArgument));
                }
                if resolved == self.session.cwd {
                    return Ok(ActionResultStatus::NoChange);
                }
                self.session.cwd = resolved.clone();
                self.config.cwd = resolved.clone();
                self.catalog.set_cwd(resolved);
                self.config_changed();
                self.session_changed();
                Ok(ActionResultStatus::Applied)
            }
            Action::RoomJoin { room } => self.room_membership(&room, user, true),
            Action::RoomLeave { room } => self.room_membership(&room, user, false),
            // Which item is in the foreground is the core's answer, so an
            // identifier that names anything else changes nothing rather than
            // backgrounding whatever happens to be running.
            Action::CommandPromote { item_id } => {
                let engine = self.engine()?;
                if self.turns.foreground(&ConvKey::Main) != Some(item_id.clone()) {
                    return Ok(ActionResultStatus::NoChange);
                }
                engine.run(crate::app::engine::Run::Promote { item: item_id });
                Ok(ActionResultStatus::Applied)
            }
            // Everything else needs the engine, and its spec said so before it
            // got here.
            _ => Err(unavailable()),
        }
    }

    /// Join or leave a room, as the user.
    fn room_membership(
        &mut self,
        room: &str,
        member: &str,
        join: bool,
    ) -> Result<crate::app::command::ActionResultStatus, AppError> {
        use crate::app::command::ActionResultStatus;
        use crate::app_server::protocol::error::ProtocolErrorKind;
        let seated = self.channels.facts().into_iter().any(|room_facts| {
            room_facts.name == room && room_facts.members.iter().any(|held| held == member)
        });
        if seated == join {
            return Ok(ActionResultStatus::NoChange);
        }
        let (reply, answer) = oneshot::channel();
        let message = if join {
            crate::channels::ChannelMsg::Invite {
                name: room.to_string(),
                member: member.to_string(),
                reply,
            }
        } else {
            crate::channels::ChannelMsg::Kick {
                name: room.to_string(),
                member: member.to_string(),
                reply,
            }
        };
        self.channels.handle(message);
        self.absorb_posts();
        self.announce_rooms();
        match crate::app::answer::Answer::new(answer, Err(String::new())).now() {
            Ok(()) => Ok(ActionResultStatus::Applied),
            Err(_) => Err(AppError::Refused(ProtocolErrorKind::BadArgument)),
        }
    }

    /// Record an `allowSession` grant (D81) in the effective configuration.
    ///
    /// Never persisted, and marked as what it is: the console's `/permissions`
    /// has always listed these — the grant goes into the live rules table the
    /// gate reads — so a client that could not see them would be reading a
    /// different session from the one that is running (B5 ruling ⑤).
    fn grant_for_session(&mut self, rule: String) {
        use crate::app::snapshot::{PermissionRule, PermissionRuleDecision};
        let held = self
            .config
            .permissions
            .iter()
            .any(|entry| entry.decision == PermissionRuleDecision::Allow && entry.rule == rule);
        if held {
            return;
        }
        self.config.permissions.push(PermissionRule {
            decision: PermissionRuleDecision::Allow,
            rule,
            session_scoped: true,
        });
        self.config_changed();
    }

    /// Add or drop one permission rule, in the core's own table.
    fn permission_rule(
        &mut self,
        decision: crate::app::snapshot::PermissionRuleDecision,
        rule: String,
        add: bool,
    ) -> crate::app::command::ActionResultStatus {
        use crate::app::command::ActionResultStatus;
        use crate::app::snapshot::PermissionRule;
        let held = self
            .config
            .permissions
            .iter()
            .any(|entry| entry.decision == decision && entry.rule == rule);
        if add == held {
            return ActionResultStatus::NoChange;
        }
        if add {
            self.config.permissions.push(PermissionRule {
                decision,
                rule,
                session_scoped: false,
            });
        } else {
            self.config
                .permissions
                .retain(|entry| !(entry.decision == decision && entry.rule == rule));
        }
        let list = |wanted: crate::app::snapshot::PermissionRuleDecision| -> Vec<&str> {
            self.config
                .permissions
                .iter()
                .filter(|entry| entry.decision == wanted && !entry.session_scoped)
                .map(|entry| entry.rule.as_str())
                .collect()
        };
        let patch = serde_json::json!({
            "permissions": {
                "allow": list(crate::app::snapshot::PermissionRuleDecision::Allow),
                "deny": list(crate::app::snapshot::PermissionRuleDecision::Deny),
                "ask": list(crate::app::snapshot::PermissionRuleDecision::Ask),
            }
        });
        self.persist(&patch);
        self.config_changed();
        ActionResultStatus::Applied
    }

    /// Turn one MCP server on or off, in settings and in what the catalogs say.
    fn set_mcp_enabled(
        &mut self,
        server: &str,
        enable: bool,
    ) -> Result<crate::app::command::ActionResultStatus, AppError> {
        use crate::app::command::ActionResultStatus;
        use crate::app_server::protocol::error::ProtocolErrorKind;
        let known = self.catalog.mcp_servers(None);
        let Some(state) = known.iter().find(|state| state.name == server) else {
            return Err(AppError::Refused(ProtocolErrorKind::BadArgument));
        };
        if state.enabled == enable {
            return Ok(ActionResultStatus::NoChange);
        }
        let mut settings = self.catalog.settings().clone();
        if enable {
            settings.disabled_mcp_servers.retain(|held| held != server);
            // A union-merged key cannot be un-set by writing one layer: every
            // layer that lists it has to stop listing it.
            let _ = crate::settings::remove_from_union_lists(
                self.catalog.user_dir(),
                self.catalog.cwd(),
                "disabledMcpServers",
                server,
            );
        } else {
            settings.disabled_mcp_servers.push(server.to_string());
            self.persist(&serde_json::json!({
                "disabledMcpServers": settings.disabled_mcp_servers,
            }));
        }
        self.catalog.reload(settings);
        self.config_changed();
        Ok(ActionResultStatus::Applied)
    }

    /// Write a settings patch where it takes effect. A failure is not a refusal:
    /// the change is in force either way, and the client is told what stands.
    fn persist(&mut self, patch: &serde_json::Value) {
        if self.catalog.user_dir().as_os_str().is_empty() {
            return;
        }
        let _ = crate::settings::upsert_scoped_settings(
            self.catalog.user_dir(),
            self.catalog.cwd(),
            patch,
        );
    }

    /// Say that the configuration moved, once, with its new revision.
    fn config_changed(&mut self) {
        self.config.revision = self.config.revision.saturating_add(1);
        let config = self.config_snapshot();
        self.publish(
            Box::new(AppEventPayload::ConfigChanged(
                crate::app::event::ConfigChanged { config },
            )),
            None,
        );
    }

    fn session_changed(&mut self) {
        self.session.updated_at = now_millis();
        let session = self.session.clone();
        self.publish(
            Box::new(AppEventPayload::SessionUpdated(
                crate::app::event::SessionUpdated { session },
            )),
            None,
        );
    }

    /// `turn/interrupt`: idempotent, and aimed at a turn this epoch minted, so a
    /// late interrupt cannot cancel the next one.
    fn serve_interrupt(
        &mut self,
        conversation_id: &ConversationId,
        turn_id: crate::app::ids::TurnId,
    ) -> Result<AppReply, AppError> {
        use crate::app_server::protocol::error::ProtocolErrorKind;
        if self.conversations.key(conversation_id).is_none() {
            return Err(AppError::Refused(ProtocolErrorKind::ConversationNotFound));
        }
        match self.turns.interrupt(&turn_id) {
            crate::app::turn::Interrupted::Asked => {
                if let Some(engine) = &self.engine {
                    engine.run(crate::app::engine::Run::Interrupt {
                        turn: turn_id.clone(),
                    });
                }
                Ok(AppReply::Interrupted {
                    turn_id,
                    accepted: true,
                })
            }
            crate::app::turn::Interrupted::Already => Ok(AppReply::Interrupted {
                turn_id,
                accepted: false,
            }),
            crate::app::turn::Interrupted::Unknown => {
                Err(AppError::Refused(ProtocolErrorKind::TurnClosed))
            }
        }
    }

    /// `interaction/respond`: the run is stopped on a prompt the actor holds, so
    /// the answer reaches it without a transport request id to correlate.
    fn serve_respond(
        &mut self,
        interaction_id: crate::app::ids::InteractionId,
        activation: crate::app::snapshot::ActivationKind,
        decision: crate::app::snapshot::InteractionDecision,
    ) -> Result<AppReply, AppError> {
        use crate::app::interaction::{InteractionChange, InteractionMsg};
        use crate::app_server::protocol::error::ProtocolErrorKind;
        let (reply, answer) = oneshot::channel();
        let changes = self.interactions.handle(
            InteractionMsg::Respond {
                id: interaction_id,
                activation,
                at: std::time::Instant::now(),
                decision,
                reply,
            },
            &mut self.mint,
        );
        let item_id = changes.iter().find_map(|change| match change {
            InteractionChange::Resolved { item, .. } => item.clone(),
            _ => None,
        });
        self.announce_interactions(changes);
        match crate::app::answer::Answer::new(answer, Err(ProtocolErrorKind::InteractionClosed))
            .now()
        {
            Ok(()) => Ok(AppReply::Responded { item_id }),
            Err(kind) => Err(AppError::Refused(kind)),
        }
    }

    /// `queue/reclaimTail`: pull the newest entry back, or lose the race to the
    /// barrier that already absorbed it. One race, one winner.
    fn serve_reclaim(
        &mut self,
        conversation_id: &ConversationId,
        expected_revision: Option<u64>,
    ) -> Result<AppReply, AppError> {
        use crate::app_server::protocol::error::ProtocolErrorKind;
        use crate::app_server::protocol::requests::ReclaimOutcome;
        let Some(conversation) = self.conversations.key(conversation_id).cloned() else {
            return Err(AppError::Refused(ProtocolErrorKind::ConversationNotFound));
        };
        let (revision, _) = self.queue.stand(&conversation);
        if let Some(expected) = expected_revision
            && expected != revision
        {
            return Err(AppError::Refused(ProtocolErrorKind::StaleRevision));
        }
        let (reply, answer) = oneshot::channel();
        let changes = self.queue.handle(
            crate::app::queue::QueueMsg::ReclaimTail {
                conversation: conversation.clone(),
                reply,
            },
            &mut self.mint,
        );
        self.announce_queue(changes);
        let outcome =
            crate::app::answer::Answer::new(answer, crate::app::queue::Reclaim::Empty).now();
        let (revision, _) = self.queue.stand(&conversation);
        // Reclaim and absorption are one race with one winner, and the outcome
        // names which: the entry that came back, or the identifier of the one a
        // barrier already took.
        let outcome = match outcome {
            crate::app::queue::Reclaim::Pulled(entry) => {
                let origin_conversation_id = self.conversation_id(&entry.on);
                ReclaimOutcome::Reclaimed {
                    entry: QueueEntry {
                        id: entry.id,
                        origin_conversation_id,
                        text: entry.text,
                        attachments: entry.attachments,
                        steer_eligible: false,
                        queued_at: entry.queued_at,
                    },
                    revision,
                }
            }
            crate::app::queue::Reclaim::Absorbed(queue_id) => {
                ReclaimOutcome::AlreadyAbsorbed { queue_id }
            }
            crate::app::queue::Reclaim::Empty => ReclaimOutcome::Empty,
        };
        Ok(AppReply::Reclaimed {
            outcome: Box::new(outcome),
            revision,
        })
    }

    /// `session/delete`: drop a persisted session that is not the open one.
    fn serve_delete_session(
        &mut self,
        locator: &crate::app::snapshot::SessionLocator,
    ) -> Result<AppReply, AppError> {
        use crate::app::snapshot::SessionLocator;
        use crate::app_server::protocol::error::ProtocolErrorKind;
        if locator == &self.session.locator {
            return Err(AppError::Refused(ProtocolErrorKind::BadArgument));
        }
        let home = self.catalog.home();
        let path = match locator {
            SessionLocator::Path { path } => path.clone(),
            SessionLocator::Stem { stem } => {
                crate::transcript::transcripts_dir(home).join(format!("{stem}.jsonl"))
            }
            // "the latest" is not a name for something to delete: a client that
            // means one names it.
            SessionLocator::Latest => {
                return Err(AppError::Refused(ProtocolErrorKind::BadArgument));
            }
        };
        if !path.exists() {
            return Err(AppError::Refused(ProtocolErrorKind::SessionNotFound));
        }
        let deleted = std::fs::remove_file(&path).is_ok();
        if deleted {
            self.publish(
                Box::new(AppEventPayload::SessionDeleted(
                    crate::app::event::SessionDeleted {
                        locator: locator.clone(),
                    },
                )),
                None,
            );
        }
        Ok(AppReply::Deleted {
            locator: locator.clone(),
            deleted,
        })
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
            engine_attached: self.engine.is_some(),
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
        self.dirty.insert(key.clone());
        // The reader's own view comes back with the reply rather than only as
        // the event that follows it: a client that just cleared a badge should
        // not have to wait for a notification to know it is clear.
        Ok(AppReply::Marked(Box::new(self.summarize(&key))))
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
                // Merging is the transport's, never the ordering point's: an
                // event leaves here standing for exactly itself.
                coalesced_from: None,
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
        let mut main_ended = false;
        for change in changes {
            if let TurnChange::Completed { conversation, .. } = &change {
                main_ended |= *conversation == ConvKey::Main;
            }
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
                TurnChange::ItemCommandTail {
                    conversation,
                    turn,
                    item,
                    tail,
                } => AppEventPayload::ItemCommandTailUpdated(
                    crate::app::event::ItemCommandTailUpdated {
                        conversation_id: self.conversation_id(&conversation),
                        turn_id: Some(turn),
                        item_id: item,
                        tail,
                    },
                ),
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
        // The turn's own end is published before the next one starts: a client
        // that reads `turn/completed` and then `turn/started` read them in the
        // order they happened.
        if main_ended {
            self.drain_main();
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
            // The core owns the ledger half of a delivery and of a run; the
            // model, the shell and the loop a deposit wakes are the engine's
            // (B4 ruling ②, B5 ruling ①). `app/controller/run.rs` is the seam.
            Route::Deliver {
                target,
                text,
                addressed,
            } => self.serve_deliver(target, text, addressed),
            Route::Turn { text } => self.start_turn(text, crate::app::snapshot::TurnOrigin::User),
            Route::Shell { command } => {
                self.start_shell(command, crate::app::snapshot::TurnOrigin::Shell)
            }
            // A slash line is the same action a typed call makes, read by the
            // same table (D146).
            Route::Command { line, on } => {
                let origin = self.conversations.id(&mut self.mint, &on);
                self.serve_command_line(&line, &origin)
            }
        }
    }

    /// One command line, submitted through the composer.
    ///
    /// The composer's parser and a GUI's typed call produce the same
    /// [`crate::app::command::Action`], so a leading slash cannot mean one thing
    /// in one client and something else in another. A viewing command changes
    /// nothing and says so: the view itself is a structured read each frontend
    /// renders for itself.
    fn serve_command_line(
        &mut self,
        line: &str,
        origin: &ConversationId,
    ) -> Result<AppReply, AppError> {
        use crate::app::action::{Call, Command};
        use crate::app::command::{ActionResult, ActionResultStatus, SubmitDisposition};
        use crate::app_server::protocol::error::ProtocolErrorKind;
        let skills: Vec<String> = self
            .catalog
            .skills()
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        let unchanged = || {
            Ok(AppReply::Submitted(SubmitDisposition::Applied {
                result: ActionResult {
                    status: ActionResultStatus::NoChange,
                    revision: None,
                    message: None,
                },
            }))
        };
        match crate::app::action::parse_in(line, &skills) {
            Ok(Command::Act(action)) => self.serve_execute(origin, None, action),
            Ok(Command::Read(_)) => unchanged(),
            Ok(Command::Call(Call::Close)) => self.command(AppCommand::CloseSession),
            Ok(Command::Call(Call::Resume(_))) => Err(AppError::Unserved("session/resume")),
            Err(crate::app::action::ParseError::Unknown(_)) => {
                Err(AppError::Refused(ProtocolErrorKind::ActionUnavailable))
            }
            Err(crate::app::action::ParseError::Usage { .. }) => {
                Err(AppError::Refused(ProtocolErrorKind::BadArgument))
            }
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
            // Whose turn it is is the registry's fact, and the registry is
            // here. No caller states it and none can disagree with it.
            main_busy: self.turns.is_busy(&ConvKey::Main),
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
                    granted,
                } => {
                    // D81's grant is configuration from the moment it is made:
                    // it is in the table the gate reads, so it is in the table a
                    // client reads. Marked `sessionScoped` and never persisted —
                    // it lives as long as this session does and no longer.
                    if let Some(rule) = granted {
                        self.grant_for_session(rule);
                    }
                    AppEventPayload::InteractionResolved(InteractionResolved {
                        interaction_id: id,
                        conversation_id: self.conversation_id(&conversation),
                        decision,
                        item_id: item,
                    })
                }
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
        let _ = self.absorb_posts_into();
    }

    /// The same absorption, saying which item each post became.
    ///
    /// The user's own post needs its identifier back — `conversation/submit`
    /// answers `Delivered { messageId }`, and the message is exactly the item
    /// this commits.
    fn absorb_posts_into(&mut self) -> Vec<(ConvKey, ItemId)> {
        let mut committed = Vec::new();
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
                    id: id.clone(),
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
                committed.push((conversation.clone(), id));
            }
        }
        committed
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
        let _ = self.absorb_deliveries_into();
    }

    /// The same absorption, saying which item each message became.
    fn absorb_deliveries_into(&mut self) -> Vec<(ConvKey, ItemId)> {
        let mut committed = Vec::new();
        for handed in self.agents.drain_delivered() {
            let delivery = DeliveryId::new(format!("{}{}", DeliveryId::PREFIX, handed.id.0));
            let conversation = ConvKey::Agent(handed.to.clone());
            let id = self.commit_body(
                &conversation,
                ItemBody::PeerMessage {
                    from: handed.from,
                    to: Some(handed.to),
                    text: handed.text,
                    delivery_id: Some(delivery),
                },
            );
            committed.push((conversation, id));
        }
        committed
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
    ///
    /// Each line is a *message that entered this conversation from outside it* —
    /// the task an instance was dispatched with, a room relay it was handed, a
    /// chase it owes an answer to — so each becomes a `peerMessage` wearing the
    /// name the walker attributed it to. It used to be a `notice` with a code
    /// and no author, which meant a client could render the line and not who
    /// said it; the walker knows, and dropping what it knows on the way into the
    /// log made the log the poorer record of the two.
    fn absorb_inbound(&mut self, conversation: &ConvKey, text: &str, first: bool) {
        let who = match conversation {
            ConvKey::Agent(name) => name.clone(),
            _ => crate::channels::MAIN_NAME.to_string(),
        };
        let at = now_millis();
        for filed in crate::app::projection::inbound(&who, text, at / 1_000, first) {
            self.commit_body(
                conversation,
                ItemBody::PeerMessage {
                    from: filed.post.from,
                    to: None,
                    text: filed.post.text,
                    delivery_id: None,
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
                .any(|room| &room.name == name && room.members.iter().any(resources::is_user)),
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

fn empty_collection<T>() -> Collection<T> {
    Collection {
        revision: 0,
        count: 0,
        active: Vec::new(),
    }
}
