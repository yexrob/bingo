//! What the actor says about the collaboration domain.
//!
//! The instances, the rooms, the background commands and the direct messages are
//! four registries with one reporting problem: each of them changes far more
//! often than it changes *meaningfully*, and a client that was told every touch
//! would be re-rendering a roster on every token. So each is projected into the
//! shape the contract names, compared against the last thing said about it, and
//! published only when the answer moved (spec "Unbounded collections are
//! paginated and changed by keyed upsert/removal events").
//!
//! Identifiers are minted once per name and kept for the epoch: a client that
//! saw `agent_3` twice saw the same instance twice.

use crate::app::conversation::ConvKey;
use crate::app::event::{AgentChanged, AppEventPayload, DeliveryChanged, RoomChanged};
use crate::app::ids::{CommandId, DeliveryId, now_millis};
use crate::app::snapshot::{
    AgentKind, AgentResource, AgentState, BackgroundCommandResource, BackgroundCommandState,
    DeliveryResource, DeliveryState, RoomMode, RoomResource, ThinkingLevel,
};

use super::{AgentSummary, RoomSummary};

impl super::Controller {
    /// Every instance as the contract names it.
    pub(super) fn agent_resources(&mut self) -> Vec<AgentResource> {
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
    pub(super) fn room_resources(&mut self) -> Vec<RoomResource> {
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
    pub(super) fn command_resources(&self) -> Vec<BackgroundCommandResource> {
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
    pub(super) fn announce_commands(&mut self) {
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
    pub(super) fn delivery_resources(&mut self) -> Vec<DeliveryResource> {
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
    pub(super) fn announce_agents(&mut self) {
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
    pub(super) fn announce_rooms(&mut self) {
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

    /// Publish one event per direct message whose state moved.
    ///
    /// D137 is what the domain enforces and this only reports: a colleague's turn
    /// prose never settles the sender's acknowledgement, so a record only reaches
    /// `answered` when a message came back.
    pub(super) fn announce_deliveries(&mut self) {
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
}

/// A member entry that is the human.
pub(super) fn is_user(member: &String) -> bool {
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
