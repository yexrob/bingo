//! The one reducer. The kernel maintains its snapshot with it; every client
//! folds frames with it. A client's view is always `apply(snapshot, frames
//! since snapshot.seq)`, so no surface needs rules of its own.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::ids::{IntentId, InteractionId, ItemId, Seq, TurnId};
use crate::model::Usage;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    /// The last frame applied. Frames at or below this are duplicates.
    pub seq: Seq,
    pub summary: SessionSummary,
    #[serde(default)]
    pub config: ConfigView,
    #[serde(default)]
    pub history_generation: u64,
    /// Items in transcript order, including every non-terminal one.
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<LiveTurn>,
    #[serde(default)]
    pub queue: Vec<QueueEntry>,
    /// Open interactions, in the order they were opened.
    #[serde(default)]
    pub interactions: Vec<Interaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<TurnStatus>,
    /// A turn ended and nobody has looked since. Cleared by `mark_read`.
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LiveTurn {
    pub id: TurnId,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    pub origin: TurnOrigin,
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrying: Option<u32>,
}

/// What a frame changed, so a renderer can redraw only that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Already applied or older than the snapshot.
    Stale,
    Nothing,
    Session,
    Config,
    Turn,
    Item(ItemId),
    Queue,
    Interaction(InteractionId),
    Intent(IntentId),
    History,
    Notice,
    Extension,
    Lagged,
}

impl SessionState {
    pub fn new(summary: SessionSummary) -> Self {
        Self {
            seq: Seq::ZERO,
            summary,
            config: ConfigView::default(),
            history_generation: 0,
            items: Vec::new(),
            turn: None,
            queue: Vec::new(),
            interactions: Vec::new(),
            context: None,
            last_turn: None,
            unread: false,
            closed: false,
        }
    }

    /// Derived, never stored: something needs a person.
    pub fn attention(&self) -> bool {
        !self.interactions.is_empty() || (self.turn.is_none() && self.unread)
    }

    pub fn mark_read(&mut self) {
        self.unread = false;
    }

    pub fn busy(&self) -> bool {
        self.turn.is_some()
    }

    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.items.iter().rev().find(|i| &i.id == id)
    }

    fn item_mut(&mut self, id: &ItemId) -> Option<&mut Item> {
        self.items.iter_mut().rev().find(|i| &i.id == id)
    }

    fn upsert(&mut self, item: &Item) {
        match self.item_mut(&item.id) {
            Some(slot) => *slot = item.clone(),
            None => self.items.push(item.clone()),
        }
    }

    pub fn apply(&mut self, frame: &Frame) -> Applied {
        // A lag marker is transport, not history: it must not move `seq`, so
        // the client's next `events_since(seq)` replays what it missed.
        if matches!(frame.event, Event::Lagged { .. }) {
            return Applied::Lagged;
        }
        if frame.seq <= self.seq && self.seq != Seq::ZERO {
            return Applied::Stale;
        }
        self.seq = frame.seq;
        match &frame.event {
            Event::SessionUpdated { summary } => {
                self.summary = summary.clone();
                Applied::Session
            }
            Event::SessionClosed { .. } => {
                self.closed = true;
                self.turn = None;
                self.interactions.clear();
                Applied::Session
            }
            Event::TurnStarted { turn, origin, .. } => {
                self.turn = Some(LiveTurn {
                    id: turn.clone(),
                    started_at: frame.ts,
                    origin: *origin,
                    round: 0,
                    usage: Usage::default(),
                    retrying: None,
                });
                self.summary.busy = true;
                Applied::Turn
            }
            Event::TurnRetrying {
                attempt, dropped, ..
            } => {
                self.items.retain(|i| !dropped.contains(&i.id));
                if let Some(t) = self.turn.as_mut() {
                    t.retrying = Some(*attempt);
                }
                Applied::Turn
            }
            Event::TurnUsage { usage, context, .. } => {
                if let Some(t) = self.turn.as_mut() {
                    t.usage.add(*usage);
                    t.round += 1;
                    t.retrying = None;
                }
                self.summary.usage.add(*usage);
                self.context = Some(*context);
                Applied::Turn
            }
            Event::TurnCompleted { status, .. } => {
                self.turn = None;
                self.summary.busy = false;
                self.last_turn = Some(status.clone());
                self.unread = true;
                Applied::Turn
            }
            Event::ItemStarted { item }
            | Event::ItemUpdated { item }
            | Event::ItemCompleted { item } => {
                self.upsert(item);
                Applied::Item(item.id.clone())
            }
            Event::ItemDelta {
                item, kind, data, ..
            } => {
                let Some(target) = self.item_mut(item) else {
                    return Applied::Nothing;
                };
                if target.is_terminal() {
                    return Applied::Nothing;
                }
                match (&mut target.body, kind) {
                    (ItemBody::Assistant { text }, DeltaKind::Text) => text.push_str(data),
                    (ItemBody::Reasoning { text, .. }, DeltaKind::Reasoning) => text.push_str(data),
                    (ItemBody::ToolCall { progress, .. }, DeltaKind::Tail) => {
                        *progress = Some(data.clone())
                    }
                    _ => return Applied::Nothing,
                }
                Applied::Item(item.clone())
            }
            Event::QueueChanged { entries, .. } => {
                self.queue = entries.clone();
                Applied::Queue
            }
            Event::InteractionOpened { interaction } => {
                self.interactions.retain(|i| i.id != interaction.id);
                self.interactions.push(interaction.clone());
                Applied::Interaction(interaction.id.clone())
            }
            Event::InteractionResolved { id, .. } | Event::InteractionCancelled { id, .. } => {
                self.interactions.retain(|i| &i.id != id);
                Applied::Interaction(id.clone())
            }
            Event::IntentAck { intent, .. } => Applied::Intent(intent.clone()),
            Event::Compacted { generation, .. } => {
                self.history_generation = *generation;
                Applied::History
            }
            Event::Rewound {
                generation,
                dropped,
                ..
            } => {
                self.history_generation = *generation;
                self.items.retain(|i| !dropped.contains(&i.id));
                Applied::History
            }
            Event::ConfigChanged { config } => {
                self.config = config.clone();
                Applied::Config
            }
            Event::CatalogChanged { .. } => Applied::Nothing,
            Event::Notice { .. } => Applied::Notice,
            Event::Extension { .. } => Applied::Extension,
            Event::Lagged { .. } => Applied::Lagged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::SessionId;
    use crate::model::ContentPart;

    fn ts() -> Timestamp {
        Timestamp::from_second(1_700_000_000).unwrap()
    }

    fn summary() -> SessionSummary {
        SessionSummary {
            id: SessionId::from_raw("ses_1"),
            key: None,
            title: None,
            cwd: "/tmp".into(),
            parent: None,
            model: None,
            provider: None,
            created_at: ts(),
            updated_at: ts(),
            usage: Usage::default(),
            busy: false,
        }
    }

    fn frame(seq: u64, event: Event) -> Frame {
        Frame {
            seq: Seq(seq),
            ts: ts(),
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event,
        }
    }

    fn assistant(id: &str, text: &str, status: ItemStatus) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: Some(TurnId::from_raw("trn_1")),
            round: 0,
            status,
            started_at: ts(),
            completed_at: None,
            intent: None,
            body: ItemBody::Assistant { text: text.into() },
            meta: Default::default(),
        }
    }

    #[test]
    fn deltas_accumulate_until_the_authoritative_completion() {
        let mut st = SessionState::new(summary());
        st.apply(&frame(
            1,
            Event::TurnStarted {
                turn: TurnId::from_raw("trn_1"),
                inputs: vec![],
                origin: TurnOrigin::Submit,
            },
        ));
        assert!(st.busy());
        st.apply(&frame(
            2,
            Event::ItemStarted {
                item: assistant("itm_1", "", ItemStatus::Running),
            },
        ));
        st.apply(&frame(
            3,
            Event::ItemDelta {
                item: ItemId::from_raw("itm_1"),
                n: 0,
                kind: DeltaKind::Text,
                data: "Hel".into(),
            },
        ));
        st.apply(&frame(
            4,
            Event::ItemDelta {
                item: ItemId::from_raw("itm_1"),
                n: 1,
                kind: DeltaKind::Text,
                data: "lo".into(),
            },
        ));
        assert_eq!(
            st.item(&ItemId::from_raw("itm_1")).unwrap().body,
            ItemBody::Assistant {
                text: "Hello".into()
            }
        );
        st.apply(&frame(
            5,
            Event::ItemCompleted {
                item: assistant("itm_1", "Hello!", ItemStatus::Completed),
            },
        ));
        let late = st.apply(&frame(
            6,
            Event::ItemDelta {
                item: ItemId::from_raw("itm_1"),
                n: 2,
                kind: DeltaKind::Text,
                data: "??".into(),
            },
        ));
        assert_eq!(late, Applied::Nothing);
        assert_eq!(
            st.item(&ItemId::from_raw("itm_1")).unwrap().body,
            ItemBody::Assistant {
                text: "Hello!".into()
            }
        );
        st.apply(&frame(
            7,
            Event::TurnCompleted {
                turn: TurnId::from_raw("trn_1"),
                status: TurnStatus::Completed,
                usage: Usage::default(),
            },
        ));
        assert!(!st.busy());
        assert!(
            st.attention(),
            "a finished turn nobody looked at needs attention"
        );
        st.mark_read();
        assert!(!st.attention());
    }

    #[test]
    fn stale_and_duplicate_frames_are_ignored() {
        let mut st = SessionState::new(summary());
        st.apply(&frame(
            5,
            Event::ConfigChanged {
                config: ConfigView::default(),
            },
        ));
        assert_eq!(
            st.apply(&frame(5, Event::CatalogChanged { kind: "x".into() })),
            Applied::Stale
        );
        assert_eq!(
            st.apply(&frame(3, Event::CatalogChanged { kind: "x".into() })),
            Applied::Stale
        );
        assert_eq!(st.seq, Seq(5));
    }

    #[test]
    fn a_lag_marker_leaves_seq_where_the_last_applied_frame_put_it() {
        let mut st = SessionState::new(summary());
        st.apply(&frame(
            3,
            Event::ConfigChanged {
                config: ConfigView::default(),
            },
        ));
        assert_eq!(
            st.apply(&frame(
                9,
                Event::Lagged {
                    from: Seq(4),
                    to: Seq(9)
                }
            )),
            Applied::Lagged
        );
        assert_eq!(
            st.seq,
            Seq(3),
            "resync must start from the last applied frame"
        );
        assert_ne!(
            st.apply(&frame(4, Event::CatalogChanged { kind: "x".into() })),
            Applied::Stale
        );
    }

    #[test]
    fn interactions_open_and_close_by_id() {
        let mut st = SessionState::new(summary());
        let interaction = Interaction {
            id: InteractionId::from_raw("int_1"),
            session: SessionId::from_raw("ses_1"),
            turn: None,
            item: None,
            opened_at: ts(),
            guard_until: None,
            expires_at: None,
            kind: InteractionKind::Confirm {
                title: "t".into(),
                detail: "d".into(),
            },
            answers: vec![AnswerSpec::Confirm, AnswerSpec::Cancel],
        };
        st.apply(&frame(1, Event::InteractionOpened { interaction }));
        assert!(st.attention());
        st.apply(&frame(
            2,
            Event::InteractionResolved {
                id: InteractionId::from_raw("int_1"),
                answer: Answer::Confirm,
                by: ResolvedBy::Kernel,
            },
        ));
        assert!(st.interactions.is_empty());
        assert!(!st.attention());
    }

    #[test]
    fn rewind_drops_items_and_bumps_the_generation() {
        let mut st = SessionState::new(summary());
        st.apply(&frame(
            1,
            Event::ItemCompleted {
                item: Item {
                    body: ItemBody::User {
                        parts: vec![ContentPart::text("a")],
                        origin: Origin::surface("tui"),
                    },
                    ..assistant("itm_1", "", ItemStatus::Completed)
                },
            },
        ));
        st.apply(&frame(
            2,
            Event::ItemCompleted {
                item: assistant("itm_2", "b", ItemStatus::Completed),
            },
        ));
        st.apply(&frame(
            3,
            Event::Rewound {
                generation: 1,
                to_turn: TurnId::from_raw("trn_1"),
                dropped: vec![ItemId::from_raw("itm_2")],
                files_restored: vec![],
            },
        ));
        assert_eq!(st.items.len(), 1);
        assert_eq!(st.history_generation, 1);
    }
}
