//! The input queue, and the one race a tool barrier and a pull-back can have.
//!
//! A line typed while a turn is working waits here. It leaves one of three ways:
//! the turn ends and the front of the queue starts the next one; a tool barrier
//! absorbs the eligible prefix into the turn that is running (D83); or the user
//! pulls the newest entry back into the composer.
//!
//! The last two are a race, and it has exactly one winner because both happen
//! inside the actor. Whatever the barrier took is already on its way to the model,
//! so a pull-back that arrives after it finds nothing to pull — and the composer
//! neither shows it as pending nor lets `↑` bring it back (spec invariant #9).
//!
//! Eligibility is a *prefix*, not a filter. A command runs on this side and cannot
//! travel to a turn; a message carrying an attachment mounts at turn start. Either
//! blocks everything queued behind it, because letting a later message jump the
//! barrier would run two lines in the opposite order from the one they were typed
//! in (invariant #10).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use crate::app::answer::Answer;
use crate::app::controller::Control;
use crate::app::conversation::ConvKey;
use crate::app::ids::{AssetId, IdMint, ItemId, QueueId, TurnId, UnixMillis, now_millis};
use crate::app::snapshot::{Item, ItemBody, ItemStatus, QueueRemovalReason};

/// The line a steered message arrives under in the model's context.
///
/// It reaches the model inside the same user message as the tool results, where an
/// unlabelled paragraph would read as tool output rather than as the user speaking.
/// The wording follows the marker family already in use — `[DM from user]` (D64),
/// `[Request interrupted by user]` (D76): a bracketed statement of fact, no
/// instruction attached.
pub const STEER_MARKER: &str = "[Message from user, sent while you were working]";

/// Prefix of the line a steered message leaves in the transcript flow.
///
/// The glyph is what tells a reader this message was injected mid-turn rather than
/// typed at an idle prompt — the reply above it was written without it, the reply
/// below it with it.
pub const STEER_FLOW_PREFIX: &str = "↪ ";

/// What a queued line is. A command dispatches on the core's side; prose starts a
/// turn or rides along at a barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedKind {
    Prose,
    /// A slash command. It must never reach the model as literal text.
    Command,
    /// Shell mode's line. It runs the console's shell rather than reaching the
    /// model, so it cannot travel to a running turn either.
    Shell,
}

/// One queued input.
///
/// The page it was typed on is immutable (D135a): the queue drains at turn end and
/// the screen may be somewhere else by then, which for `/compact` would mean
/// summarising the history of an agent the user never pointed it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInput {
    pub id: QueueId,
    pub text: String,
    pub kind: QueuedKind,
    /// The page it was typed on.
    pub on: ConvKey,
    pub attachments: Vec<AssetId>,
    /// It mounts attachments at turn start, so it cannot travel to a running turn.
    pub carries_attachments: bool,
    pub queued_at: UnixMillis,
}

impl QueuedInput {
    pub fn is_command(&self) -> bool {
        self.kind == QueuedKind::Command
    }

    /// Whether this entry could ride along at a barrier at all — before asking
    /// whether everything ahead of it could too.
    fn steerable(&self) -> bool {
        self.kind == QueuedKind::Prose && !self.carries_attachments
    }

    pub fn is_shell(&self) -> bool {
        self.kind == QueuedKind::Shell
    }
}

/// One queued plain message offered to the running turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerItem {
    /// The queue entry it was, so an absorbed item is identified rather than
    /// matched by its text — two identical messages are two messages.
    pub id: QueueId,
    /// What the user typed.
    pub text: String,
}

impl SteerItem {
    /// The user content block this item contributes at the barrier.
    pub fn block_text(&self) -> String {
        format!("{STEER_MARKER}\n{}", self.text)
    }
}

/// What to put on a conversation's queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enqueue {
    /// Whose queue this joins. Today that is always the console's: a message to
    /// somebody else waits in their inbox, which is the domain's queue and not
    /// this one.
    pub conversation: ConvKey,
    /// The page it was typed on.
    pub origin: ConvKey,
    pub text: String,
    pub kind: QueuedKind,
    pub attachments: Vec<AssetId>,
    /// Resolved by the composer: the text names images the core has not been
    /// handed as assets yet. B5's `asset/registerPath` makes `attachments` the
    /// only answer and this field goes with it.
    pub carries_attachments: bool,
}

/// Where an accepted entry landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub id: QueueId,
    pub position: u32,
    pub steer_eligible: bool,
    pub revision: u64,
}

/// What the composer's pull-back found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reclaim {
    /// It was still pending and is now out of the queue: the composer owns it.
    Pulled(Box<QueuedInput>),
    /// The barrier took it first. The pull-back is a no-op — the text is in the
    /// request already.
    Absorbed,
    /// There was nothing to pull.
    Empty,
}

/// What one queue change asks the actor to publish.
pub(crate) enum QueueChange {
    Added {
        conversation: ConvKey,
        revision: u64,
        position: u32,
        entry: QueuedInput,
        steer_eligible: bool,
    },
    Removed {
        conversation: ConvKey,
        revision: u64,
        id: QueueId,
        reason: QueueRemovalReason,
    },
    /// The item an absorbed entry became. It is ordered before the absorption
    /// that names it, because the item is what the model read.
    AbsorbedItem {
        conversation: ConvKey,
        item: Box<Item>,
    },
    Absorbed {
        conversation: ConvKey,
        revision: u64,
        id: QueueId,
        turn: TurnId,
        item: ItemId,
    },
}

pub(crate) enum QueueMsg {
    Enqueue {
        request: Box<Enqueue>,
        reply: oneshot::Sender<Placement>,
    },
    /// The tool barrier's atomic take: the eligible prefix, in order, gone from
    /// the queue by the time this answers.
    Absorb {
        conversation: ConvKey,
        turn: TurnId,
        reply: oneshot::Sender<Vec<SteerItem>>,
    },
    ReclaimTail {
        conversation: ConvKey,
        reply: oneshot::Sender<Reclaim>,
    },
    DrainFront {
        conversation: ConvKey,
        reply: oneshot::Sender<Option<QueuedInput>>,
    },
    Clear {
        conversation: ConvKey,
    },
}

/// One conversation's queue, as a reader sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationQueue {
    pub revision: u64,
    pub entries: Vec<QueuedInput>,
    /// Entries a barrier took and the composer has not been told about yet.
    /// Kept so a pull-back racing the barrier can say which one won.
    absorbed: Vec<QueueId>,
}

impl ConversationQueue {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// What every queue in the session holds, without asking.
#[derive(Debug, Default)]
pub struct QueueView {
    conversations: HashMap<ConvKey, ConversationQueue>,
}

impl QueueView {
    /// One conversation's queue. A conversation nothing was ever queued on has an
    /// empty one rather than none.
    pub fn of(&self, conversation: &ConvKey) -> ConversationQueue {
        self.conversations
            .get(conversation)
            .cloned()
            .unwrap_or_default()
    }
}

/// How the rest of the process reaches the queues the actor owns.
#[derive(Clone)]
pub struct QueueHandle {
    control: mpsc::UnboundedSender<Control>,
    view: watch::Receiver<Arc<QueueView>>,
}

impl std::fmt::Debug for QueueHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QueueHandle")
    }
}

impl QueueHandle {
    fn ask<T>(&self, gone: T, build: impl FnOnce(oneshot::Sender<T>) -> QueueMsg) -> Answer<T> {
        let (reply, answer) = oneshot::channel();
        let _ = self.control.send(Control::Queue(build(reply)));
        Answer::new(answer, gone)
    }

    pub fn view(&self) -> Arc<QueueView> {
        self.view.borrow().clone()
    }

    /// One conversation's queue right now.
    pub fn of(&self, conversation: &ConvKey) -> ConversationQueue {
        self.view().of(conversation)
    }

    pub fn enqueue(&self, request: Enqueue) -> Answer<Placement> {
        let gone = Placement {
            id: QueueId::new(""),
            position: 0,
            steer_eligible: false,
            revision: 0,
        };
        self.ask(gone, move |reply| QueueMsg::Enqueue {
            request: Box::new(request),
            reply,
        })
    }

    /// The tool barrier. Whatever comes back is committed: the caller is about to
    /// append it to the request it is assembling.
    pub fn absorb(&self, conversation: ConvKey, turn: TurnId) -> Answer<Vec<SteerItem>> {
        self.ask(Vec::new(), move |reply| QueueMsg::Absorb {
            conversation,
            turn,
            reply,
        })
    }

    /// The composer taking the newest entry back (`↑`).
    pub fn reclaim_tail(&self, conversation: ConvKey) -> Answer<Reclaim> {
        self.ask(Reclaim::Empty, move |reply| QueueMsg::ReclaimTail {
            conversation,
            reply,
        })
    }

    /// The next entry, for the turn about to run it.
    pub fn drain_front(&self, conversation: ConvKey) -> Answer<Option<QueuedInput>> {
        self.ask(None, move |reply| QueueMsg::DrainFront {
            conversation,
            reply,
        })
    }

    pub fn clear(&self, conversation: ConvKey) {
        let _ = self
            .control
            .send(Control::Queue(QueueMsg::Clear { conversation }));
    }
}

/// The input queues of one session, owned by the actor.
pub(crate) struct InputQueue {
    conversations: HashMap<ConvKey, ConversationQueue>,
    view: watch::Sender<Arc<QueueView>>,
}

pub(crate) fn attach(control: mpsc::UnboundedSender<Control>) -> (InputQueue, QueueHandle) {
    let (view, reader) = watch::channel(Arc::new(QueueView::default()));
    (
        InputQueue {
            conversations: HashMap::new(),
            view,
        },
        QueueHandle {
            control,
            view: reader,
        },
    )
}

impl InputQueue {
    /// Apply one message and say what it changed.
    ///
    /// The view is published before the changes are returned, for the same reason
    /// the other registries publish before they announce: a surface woken by the
    /// event and then reading the view can never read a world older than the event
    /// that woke it.
    pub(crate) fn handle(&mut self, message: QueueMsg, mint: &mut IdMint) -> Vec<QueueChange> {
        let changes = self.apply(message, mint);
        self.publish();
        changes
    }

    /// Every conversation that has a queue, for a sweep that must not miss one.
    pub(crate) fn conversations(&self) -> Vec<ConvKey> {
        self.conversations.keys().cloned().collect()
    }

    pub(crate) fn count(&self, conversation: &ConvKey) -> u32 {
        self.conversations
            .get(conversation)
            .map_or(0, |queue| queue.entries.len() as u32)
    }

    fn publish(&mut self) {
        let _ = self.view.send(Arc::new(QueueView {
            conversations: self.conversations.clone(),
        }));
    }

    fn queue(&mut self, conversation: &ConvKey) -> &mut ConversationQueue {
        self.conversations.entry(conversation.clone()).or_default()
    }

    /// Put one entry on a queue. The submission path calls this directly, because
    /// it has already decided that the line waits.
    pub(crate) fn enqueue(
        &mut self,
        request: Enqueue,
        mint: &mut IdMint,
    ) -> (Placement, Vec<QueueChange>) {
        let (reply, mut answer) = oneshot::channel();
        let changes = self.handle(
            QueueMsg::Enqueue {
                request: Box::new(request),
                reply,
            },
            mint,
        );
        let placement = answer.try_recv().unwrap_or_else(|_| Placement {
            id: QueueId::new(""),
            position: 0,
            steer_eligible: false,
            revision: 0,
        });
        (placement, changes)
    }

    fn apply(&mut self, message: QueueMsg, mint: &mut IdMint) -> Vec<QueueChange> {
        match message {
            QueueMsg::Enqueue { request, reply } => {
                let id: QueueId = mint.mint();
                let entry = QueuedInput {
                    id: id.clone(),
                    text: request.text,
                    kind: request.kind,
                    on: request.origin,
                    attachments: request.attachments,
                    carries_attachments: request.carries_attachments,
                    queued_at: now_millis(),
                };
                let conversation = request.conversation;
                let queue = self.queue(&conversation);
                queue.revision = queue.revision.saturating_add(1);
                queue.entries.push(entry.clone());
                let position = (queue.entries.len() - 1) as u32;
                let revision = queue.revision;
                let steer_eligible = eligible_prefix(&queue.entries) > position as usize;
                self.publish();
                let _ = reply.send(Placement {
                    id,
                    position,
                    steer_eligible,
                    revision,
                });
                vec![QueueChange::Added {
                    conversation,
                    revision,
                    position,
                    entry,
                    steer_eligible,
                }]
            }
            QueueMsg::Absorb {
                conversation,
                turn,
                reply,
            } => {
                let queue = self.queue(&conversation);
                let take = eligible_prefix(&queue.entries);
                if take == 0 {
                    self.publish();
                    let _ = reply.send(Vec::new());
                    return Vec::new();
                }
                let taken: Vec<QueuedInput> = queue.entries.drain(..take).collect();
                queue.revision = queue.revision.saturating_add(1);
                queue.absorbed.extend(taken.iter().map(|e| e.id.clone()));
                let revision = queue.revision;
                let mut changes = Vec::new();
                let mut items = Vec::new();
                for entry in &taken {
                    let item_id: ItemId = mint.mint();
                    let now = now_millis();
                    changes.push(QueueChange::AbsorbedItem {
                        conversation: conversation.clone(),
                        item: Box::new(Item {
                            id: item_id.clone(),
                            status: ItemStatus::Completed,
                            turn_id: Some(turn.clone()),
                            started_at: Some(now),
                            completed_at: Some(now),
                            body: ItemBody::UserMessage {
                                text: entry.text.clone(),
                                attachments: entry.attachments.clone(),
                            },
                        }),
                    });
                    changes.push(QueueChange::Absorbed {
                        conversation: conversation.clone(),
                        revision,
                        id: entry.id.clone(),
                        turn: turn.clone(),
                        item: item_id,
                    });
                    items.push(SteerItem {
                        id: entry.id.clone(),
                        text: entry.text.clone(),
                    });
                }
                self.publish();
                let _ = reply.send(items);
                changes
            }
            QueueMsg::ReclaimTail {
                conversation,
                reply,
            } => {
                let queue = self.queue(&conversation);
                let Some(entry) = queue.entries.pop() else {
                    // Nothing pending. If a barrier just took the tail, the
                    // pull-back lost the race and must be a no-op rather than a
                    // fall-through to the history walk.
                    let lost = !queue.absorbed.is_empty();
                    let _ = reply.send(if lost {
                        Reclaim::Absorbed
                    } else {
                        Reclaim::Empty
                    });
                    return Vec::new();
                };
                queue.revision = queue.revision.saturating_add(1);
                let revision = queue.revision;
                let id = entry.id.clone();
                self.publish();
                let _ = reply.send(Reclaim::Pulled(Box::new(entry)));
                vec![QueueChange::Removed {
                    conversation,
                    revision,
                    id,
                    reason: QueueRemovalReason::Reclaimed,
                }]
            }
            QueueMsg::DrainFront {
                conversation,
                reply,
            } => {
                let queue = self.queue(&conversation);
                if queue.entries.is_empty() {
                    // A drain is also the turn boundary: what a barrier took
                    // belongs to the turn that just ended, and the next turn's
                    // pull-back must not be told it lost a race it never ran.
                    queue.absorbed.clear();
                    self.publish();
                    let _ = reply.send(None);
                    return Vec::new();
                }
                let entry = queue.entries.remove(0);
                queue.absorbed.clear();
                queue.revision = queue.revision.saturating_add(1);
                let revision = queue.revision;
                let id = entry.id.clone();
                self.publish();
                let _ = reply.send(Some(entry));
                vec![QueueChange::Removed {
                    conversation,
                    revision,
                    id,
                    reason: QueueRemovalReason::Drained,
                }]
            }
            QueueMsg::Clear { conversation } => {
                let queue = self.queue(&conversation);
                if queue.entries.is_empty() {
                    queue.absorbed.clear();
                    return Vec::new();
                }
                let dropped: Vec<QueueId> = queue.entries.drain(..).map(|entry| entry.id).collect();
                queue.absorbed.clear();
                queue.revision = queue.revision.saturating_add(1);
                let revision = queue.revision;
                dropped
                    .into_iter()
                    .map(|id| QueueChange::Removed {
                        conversation: conversation.clone(),
                        revision,
                        id,
                        reason: QueueRemovalReason::Cleared,
                    })
                    .collect()
            }
        }
    }
}

/// How many entries from the front may ride along at a barrier. The first
/// ineligible entry stops the count, and everything behind it waits with it.
fn eligible_prefix(entries: &[QueuedInput]) -> usize {
    entries.iter().take_while(|entry| entry.steerable()).count()
}

impl std::fmt::Debug for QueueChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Added { entry, .. } => write!(f, "Added({})", entry.id),
            Self::Removed { id, reason, .. } => write!(f, "Removed({id}, {reason:?})"),
            Self::AbsorbedItem { item, .. } => write!(f, "AbsorbedItem({})", item.id),
            Self::Absorbed { id, .. } => write!(f, "Absorbed({id})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;

    fn queue() -> (InputQueue, IdMint) {
        let (control, _inbox) = mpsc::unbounded_channel();
        let (queue, _handle) = attach(control);
        (queue, IdMint::new(EpochId::mint()))
    }

    fn prose(text: &str) -> Enqueue {
        Enqueue {
            conversation: ConvKey::Main,
            origin: ConvKey::Main,
            text: text.to_string(),
            kind: QueuedKind::Prose,
            attachments: Vec::new(),
            carries_attachments: false,
        }
    }

    fn add(queue: &mut InputQueue, mint: &mut IdMint, request: Enqueue) -> Placement {
        let (reply, answer) = oneshot::channel();
        queue.handle(
            QueueMsg::Enqueue {
                request: Box::new(request),
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn absorb(queue: &mut InputQueue, mint: &mut IdMint) -> Vec<SteerItem> {
        let (reply, answer) = oneshot::channel();
        queue.handle(
            QueueMsg::Absorb {
                conversation: ConvKey::Main,
                turn: TurnId::new("turn_1"),
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn reclaim(queue: &mut InputQueue, mint: &mut IdMint) -> Reclaim {
        let (reply, answer) = oneshot::channel();
        queue.handle(
            QueueMsg::ReclaimTail {
                conversation: ConvKey::Main,
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn drain(queue: &mut InputQueue, mint: &mut IdMint) -> Option<QueuedInput> {
        let (reply, answer) = oneshot::channel();
        queue.handle(
            QueueMsg::DrainFront {
                conversation: ConvKey::Main,
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn the_queue_is_fifo_and_the_page_it_was_typed_on_is_immutable() {
        let (mut queue, mut mint) = queue();
        add(&mut queue, &mut mint, prose("first"));
        add(
            &mut queue,
            &mut mint,
            Enqueue {
                origin: ConvKey::Agent("scout".to_string()),
                ..prose("/compact")
            },
        );
        let second = drain(&mut queue, &mut mint);
        assert_eq!(
            drain(&mut queue, &mut mint)
                .unwrap_or_else(|| panic!("two were queued"))
                .on,
            ConvKey::Agent("scout".to_string()),
            "the second entry keeps the page it was typed on"
        );
        assert_eq!(
            second.unwrap_or_else(|| panic!("two were queued")).text,
            "first",
            "the first one typed is the first one out"
        );
    }

    /// A command runs on the core's side and cannot travel to a turn, so nothing
    /// queued behind it may overtake it.
    #[test]
    fn an_ineligible_entry_blocks_everything_behind_it() {
        let (mut queue, mut mint) = queue();
        add(&mut queue, &mut mint, prose("use tabs"));
        let command = add(
            &mut queue,
            &mut mint,
            Enqueue {
                kind: QueuedKind::Command,
                ..prose("/model sonnet")
            },
        );
        let behind = add(&mut queue, &mut mint, prose("and rename it"));
        assert!(!command.steer_eligible);
        assert!(
            !behind.steer_eligible,
            "a message behind a command would otherwise run before it"
        );
        let taken = absorb(&mut queue, &mut mint);
        assert_eq!(
            taken
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["use tabs"],
            "only the eligible prefix rides along"
        );
    }

    #[test]
    fn an_attachment_is_as_ineligible_as_a_command() {
        let (mut queue, mut mint) = queue();
        let placement = add(
            &mut queue,
            &mut mint,
            Enqueue {
                carries_attachments: true,
                ..prose("look at this")
            },
        );
        assert!(!placement.steer_eligible);
        assert!(
            absorb(&mut queue, &mut mint).is_empty(),
            "mounting attachments is the turn's own path"
        );
    }

    /// The barrier and the pull-back are one race with one winner, decided by
    /// which reached the actor first.
    #[test]
    fn absorption_and_tail_reclaim_have_one_winner() {
        let (mut queue, mut mint) = queue();
        add(&mut queue, &mut mint, prose("use tabs"));
        assert_eq!(absorb(&mut queue, &mut mint).len(), 1);
        assert_eq!(
            reclaim(&mut queue, &mut mint),
            Reclaim::Absorbed,
            "the barrier took it: the pull-back must be a no-op"
        );

        add(&mut queue, &mut mint, prose("actually, spaces"));
        match reclaim(&mut queue, &mut mint) {
            Reclaim::Pulled(entry) => assert_eq!(entry.text, "actually, spaces"),
            other => panic!("the composer got there first: {other:?}"),
        }
        assert!(
            absorb(&mut queue, &mut mint).is_empty(),
            "what was pulled back is not in the request"
        );
    }

    /// The absorbed ledger belongs to the turn that filled it: the next turn's
    /// pull-back must not be told it lost a race it never ran.
    #[test]
    fn a_drained_queue_forgets_the_previous_turns_absorptions() {
        let (mut queue, mut mint) = queue();
        add(&mut queue, &mut mint, prose("use tabs"));
        let _ = absorb(&mut queue, &mut mint);
        assert_eq!(reclaim(&mut queue, &mut mint), Reclaim::Absorbed);
        assert_eq!(drain(&mut queue, &mut mint), None);
        assert_eq!(
            reclaim(&mut queue, &mut mint),
            Reclaim::Empty,
            "a new turn starts with no claim on it"
        );
    }

    /// Absorption publishes the item the input became before the absorption that
    /// names it: the item is what the model read.
    #[test]
    fn absorption_publishes_the_item_before_it_names_it() {
        let (mut queue, mut mint) = queue();
        add(&mut queue, &mut mint, prose("use tabs"));
        let (reply, _answer) = oneshot::channel();
        let changes = queue.handle(
            QueueMsg::Absorb {
                conversation: ConvKey::Main,
                turn: TurnId::new("turn_1"),
                reply,
            },
            &mut mint,
        );
        match &changes[..] {
            [
                QueueChange::AbsorbedItem { item, .. },
                QueueChange::Absorbed {
                    item: named, turn, ..
                },
            ] => {
                assert_eq!(&item.id, named);
                assert_eq!(turn, &TurnId::new("turn_1"));
                assert!(matches!(item.body, ItemBody::UserMessage { .. }));
            }
            other => panic!("expected the item then its absorption, got {other:?}"),
        }
    }

    #[test]
    fn the_marker_carries_the_message_rather_than_instructing_the_model() {
        let item = SteerItem {
            id: QueueId::new("queue_1"),
            text: "use tabs".to_string(),
        };
        assert_eq!(item.block_text(), format!("{STEER_MARKER}\nuse tabs"));
    }
}
