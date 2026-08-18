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

use crate::app::command::{AppCommand, AppQuery};
use crate::app::conversation::{ConvKey, Conversations};
use crate::app::event::{
    AgentChanged, AppEvent, AppEventPayload, EventMeta, ItemChanged, ItemDelta, QueueItemAbsorbed,
    QueueItemAdded, QueueItemRemoved, RoomChanged, TurnChanged, TurnRetrying, TurnRoundCompleted,
    TurnRoundStarted, TurnUsageUpdated,
};
use crate::app::ids::{
    AgentId, ConversationId, EpochId, IdMint, OperationId, RoomId, SessionId, now_millis,
};
use crate::app::snapshot::{
    AgentKind, AgentResource, AgentState, Collection, ConfigSnapshot, QueueEntry, RoomMode,
    RoomResource, RuntimeCollections, ServerCapabilities, SessionSnapshot, SessionState,
    SessionSummary, ThinkingLevel,
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
    /// One submission, read and routed.
    Submit {
        request: Box<crate::app::submit::SubmitRequest>,
        reply: oneshot::Sender<crate::app::submit::Route>,
    },
    /// Answered once everything queued ahead of it has been applied.
    Settle {
        reply: oneshot::Sender<()>,
    },
}

/// What the actor hands out at startup: one handle per registry it owns.
pub(crate) struct Registries {
    pub watch: crate::watch::WatchHandle,
    pub channels: crate::channels::ChannelHandle,
    pub agents: crate::agents::AgentHandle,
    pub turns: crate::app::turn::TurnHandle,
    pub queue: crate::app::queue::QueueHandle,
    pub submit: crate::app::submit::SubmitHandle,
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
pub(super) fn spawn(setup: SessionSetup) -> (mpsc::UnboundedSender<Control>, Registries) {
    let (control, inbox) = mpsc::unbounded_channel();
    let (watch, watch_handle) = crate::watch::attach(control.clone());
    let (channels, channel_handle) = crate::channels::attach(control.clone(), setup.channel_limits);
    let (agents, agent_handle) = crate::agents::attach(control.clone());
    let (turns, turn_handle) = crate::app::turn::attach(control.clone());
    let (queue, queue_handle) = crate::app::queue::attach(control.clone());
    let control_for_submit = control.clone();
    // The actor holds a weak handle to its own inbox: it hands strong clones to
    // the attachments it spawns, and a strong one of its own would keep the
    // queue open forever, so the loop could never end.
    let controller = Controller::new(
        setup,
        control.downgrade(),
        watch,
        channels,
        agents,
        turns,
        queue,
    );
    std::thread::Builder::new()
        .name("bingo-session".to_string())
        .spawn(move || controller.run(inbox))
        .unwrap_or_else(|error| panic!("the session actor could not start: {error}"));
    (
        control,
        Registries {
            watch: watch_handle,
            channels: channel_handle,
            agents: agent_handle,
            turns: turn_handle,
            queue: queue_handle,
            submit: crate::app::submit::SubmitHandle::new(control_for_submit),
        },
    )
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
    /// Which conversations exist and what each one is called on the wire.
    conversations: Conversations,
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
}

impl Controller {
    fn new(
        setup: SessionSetup,
        control: mpsc::WeakUnboundedSender<Control>,
        watch: crate::watch::WatchRegistry,
        channels: crate::channels::ChannelRegistry,
        agents: crate::agents::AgentRegistry,
        turns: crate::app::turn::TurnRegistry,
        queue: crate::app::queue::InputQueue,
    ) -> Self {
        let epoch = EpochId::mint();
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
            // Settings layers, permission rules, and MCP state are read by
            // `config/read`, which lands with B5. An empty list here says "not
            // read yet" and never says "none configured": nothing reads it.
            permissions: Vec::new(),
            layers: Vec::new(),
            mcp_servers: Vec::new(),
        };
        Self {
            mint,
            session,
            capabilities: setup.capabilities,
            config,
            seq: 0,
            attachments: Vec::new(),
            next_attachment: 0,
            control,
            watch,
            channels,
            agents,
            turns,
            queue,
            conversations,
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
                Control::Watch(message) => self.watch.handle(message),
                Control::Channels(message) => {
                    self.channels.handle(message);
                    self.announce_rooms();
                }
                Control::Agents(message) => {
                    self.agents.handle(message);
                    self.announce_agents();
                }
                Control::Turn(message) => {
                    let changes = self.turns.handle(message, &mut self.mint);
                    self.announce_turn(changes);
                }
                Control::Queue(message) => {
                    let changes = self.queue.handle(message, &mut self.mint);
                    self.announce_queue(changes);
                }
                Control::Submit { request, reply } => {
                    let route = self.submit(*request);
                    let _ = reply.send(route);
                }
                Control::Settle { reply } => {
                    let _ = reply.send(());
                }
            }
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
        Err(AppError::Unserved(match command {
            AppCommand::StartSession { .. } => "session/start",
            AppCommand::ResumeSession { .. } => "session/resume",
            AppCommand::CloseSession => "session/close",
            AppCommand::DeleteSession { .. } => "session/delete",
            AppCommand::Execute { .. } => "action/execute",
            AppCommand::Interrupt { .. } => "turn/interrupt",
            AppCommand::RespondInteraction { .. } => "interaction/respond",
            AppCommand::MarkRead { .. } => "conversation/markRead",
            AppCommand::ReclaimQueueTail { .. } => "queue/reclaimTail",
            AppCommand::RegisterAsset { .. } => "asset/registerPath",
            AppCommand::Shutdown => "shutdown",
            // Answered above; the compiler is what keeps this exhaustive.
            AppCommand::Submit { .. } => "conversation/submit",
        }))
    }

    fn query(&mut self, attachment: AttachmentId, query: AppQuery) -> Result<AppReply, AppError> {
        match query {
            AppQuery::ReadSession => {
                let snapshot = self.session_snapshot();
                self.cut(attachment, snapshot.event_cursor);
                Ok(AppReply::Session(Box::new(snapshot)))
            }
            AppQuery::ListSessions { .. } => Err(AppError::Unserved("session/list")),
            AppQuery::ListConversations { .. } => Err(AppError::Unserved("conversation/list")),
            AppQuery::ReadConversation { .. } => Err(AppError::Unserved("conversation/read")),
            AppQuery::ReadQueue { .. } => Err(AppError::Unserved("queue/read")),
            AppQuery::ListActions { .. } => Err(AppError::Unserved("action/list")),
            AppQuery::ReadConfig => Err(AppError::Unserved("config/read")),
            AppQuery::ReadCatalog { .. } => Err(AppError::Unserved("catalog/read")),
            AppQuery::ReadResource { .. } => Err(AppError::Unserved("resource/read")),
            AppQuery::ReadAssetChunk { .. } => Err(AppError::Unserved("asset/readChunk")),
        }
    }

    /// The session as it stands, valid through the sequence number it was cut
    /// at. The collections are empty because the skeleton owns nothing else yet
    /// — not because the session has nothing.
    fn session_snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            session: self.session.clone(),
            capabilities: self.capabilities,
            conversations: empty_collection(),
            active_turns: Vec::new(),
            interactions: Vec::new(),
            operations: Vec::new(),
            collections: RuntimeCollections {
                agents: empty_collection(),
                rooms: empty_collection(),
                tasks: empty_collection(),
                deliveries: empty_collection(),
                background_commands: empty_collection(),
                mcp_servers: Vec::new(),
            },
            feedback: Vec::new(),
            config: self.config.clone(),
            event_cursor: self.seq,
        }
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

    /// Publish one event per instance whose state moved, and one per instance
    /// that is new. An instance that went away is not announced yet: `agent/gone`
    /// is B4's, and inventing a shape for it here would be deciding the contract
    /// from the implementation.
    fn announce_agents(&mut self) {
        let facts = self.agents.facts();
        let mut changed = Vec::new();
        for fact in facts {
            let summary = AgentSummary {
                state: agent_state(fact.state),
                pending: fact.pending,
                unacked: fact.unacked,
            };
            let known = self.told.agents.get(&fact.name);
            if known.is_some_and(|(_, told)| told == &summary) {
                continue;
            }
            let id = match known {
                Some((id, _)) => id.clone(),
                None => self.mint.mint(),
            };
            self.told
                .agents
                .insert(fact.name.clone(), (id.clone(), summary));
            changed.push(AgentResource {
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
                // The instance's own thinking level and its conversation arrive
                // with B4, which is where an agent gets a conversation at all.
                thinking: ThinkingLevel::Off,
                cwd: fact.cwd,
                conversation_id: None,
                pending: fact.pending,
                unacked: fact.unacked,
                elapsed_ms: fact.elapsed_ms,
                output_tokens: fact.output_tokens,
                tool_uses: fact.tool_uses,
                last_active_at: now_millis(),
            });
        }
        for agent in changed {
            self.publish(
                Box::new(AppEventPayload::AgentChanged(AgentChanged { agent })),
                None,
            );
        }
    }

    /// Publish one event per room whose roster or head moved.
    fn announce_rooms(&mut self) {
        let facts = self.channels.facts();
        let mut changed = Vec::new();
        for fact in facts {
            let summary = RoomSummary {
                members: fact.members.clone(),
                last_seq: fact.last_seq,
            };
            let known = self.told.rooms.get(&fact.name);
            if known.is_some_and(|(_, told)| told == &summary) {
                continue;
            }
            let id = match known {
                Some((id, _)) => id.clone(),
                None => self.mint.mint(),
            };
            self.told
                .rooms
                .insert(fact.name.clone(), (id.clone(), summary));
            changed.push(RoomResource {
                id,
                name: fact.name,
                topic: None,
                mode: match fact.mode {
                    crate::channels::ChannelMode::Serial => RoomMode::Relay,
                    crate::channels::ChannelMode::Free => RoomMode::Broadcast,
                },
                user_is_member: fact.members.iter().any(|m| m == crate::channels::USER_NAME),
                members: fact.members,
                conversation_id: None,
                message_count: fact.message_count,
                last_seq: fact.last_seq,
                // Attention is the user's, and the user's cursors land with B4.
                unread: 0,
                mentions: 0,
            });
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
            let payload = match change {
                TurnChange::Started { conversation, turn } => {
                    let conversation_id = self.conversation_id(&conversation);
                    let mut turn = turn;
                    turn.conversation_id = conversation_id.clone();
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
                } => AppEventPayload::TurnRetrying(TurnRetrying {
                    conversation_id: self.conversation_id(&conversation),
                    turn_id: turn,
                    round,
                    attempt,
                    max_attempts,
                    delay_ms,
                    removed_item_ids: removed,
                    code,
                    reason,
                }),
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
                TurnChange::ItemCompleted {
                    conversation,
                    turn,
                    item,
                } => AppEventPayload::ItemCompleted(ItemChanged {
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
                } => AppEventPayload::QueueItemRemoved(QueueItemRemoved {
                    conversation_id: self.conversation_id(&conversation),
                    revision,
                    queue_id: id,
                    reason,
                }),
                QueueChange::AbsorbedItem { conversation, item } => {
                    let turn_id = item.turn_id.clone();
                    AppEventPayload::ItemCompleted(ItemChanged {
                        conversation_id: self.conversation_id(&conversation),
                        turn_id,
                        item: *item,
                    })
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
        assert!(
            snapshot.conversations.active.is_empty() && snapshot.active_turns.is_empty(),
            "the skeleton owns no conversations yet, and says so rather than inventing one"
        );
    }

    /// A mutation the core cannot serve yet is refused by name. The reply is
    /// still a reply: the request is answered, never dropped.
    #[tokio::test]
    async fn the_skeleton_refuses_by_name_what_it_does_not_serve_yet() {
        let core = AppCore::start(SessionSetup::default());
        let (mut link, _) = attached(&core, "test").await;
        link.request(AppRequest::Command {
            id: RequestId(2),
            command: AppCommand::Shutdown,
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
        match link.recv().await {
            Some(AppFrame::Reply { id, result }) => {
                assert_eq!(id, RequestId(2));
                assert_eq!(result, Err(AppError::Unserved("shutdown")));
            }
            other => panic!("expected a refusal, got {other:?}"),
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
