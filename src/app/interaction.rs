//! Pending permission prompts and questions, and who is allowed to answer them.
//!
//! An interaction is a run stopped on something only a person can settle. The
//! run's half is one `await`; this side holds the other end of it, which is why
//! the registry lives in the actor rather than in whichever surface happens to be
//! drawing the prompt: a prompt recovered from a snapshot must stay answerable,
//! and a prompt nobody answers must fail closed rather than hang.
//!
//! Three rules are enforced here and nowhere else (spec "Server-initiated
//! interactions", invariant #12):
//!
//! - **Advertised decisions.** Exactly what the prompt offers is valid.
//!   `allowSession` is refused unless the server itself derived and verified the
//!   rule behind it, so "don't ask again" is a promise the gate can keep.
//! - **The confirmation guard (D81).** A dialog that appears under a keystroke
//!   already on its way must not answer it. The guard holds back *keyboard
//!   approval of a permission prompt* and nothing else: a pointer, a denial, a
//!   cancellation and every non-confirming key stay immediate.
//! - **Answered once.** The first valid, non-premature response wins; a late or
//!   repeated one is refused by name and cannot reach a later prompt.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, watch};

use crate::app::answer::Answer;
use crate::app::controller::Control;
use crate::app::conversation::ConvKey;
use crate::app::ids::{IdMint, InteractionId, ItemId, TurnId, now_millis};
use crate::app::snapshot::{
    ActivationKind, Interaction, InteractionCancelReason, InteractionDecision, InteractionPrompt,
    Item, ItemBody, ItemStatus, PermissionDecisionKind,
};
use crate::app_server::protocol::error::ProtocolErrorKind;

/// How long a permission prompt ignores keyboard confirmation after it opens.
///
/// It runs from the moment the prompt exists, which is the moment the keystroke
/// already in flight would land on it (D81, CC's confirm delay).
pub const CONFIRM_GUARD: Duration = Duration::from_millis(400);

/// What a run gets back. It is always one of these, including when nobody
/// answered: a closed prompt fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    AllowSession,
    Deny {
        feedback: Option<String>,
    },
    Answer {
        option: Option<usize>,
        text: Option<String>,
    },
    Confirm,
    /// Dismissed, interrupted, or the session closed. Fails closed.
    Cancelled,
}

/// What to ask.
#[derive(Debug, Clone)]
pub struct OpenPrompt {
    pub conversation: ConvKey,
    pub turn: Option<TurnId>,
    pub item: Option<ItemId>,
    pub prompt: InteractionPrompt,
}

/// One prompt as a surface sees it: the contract's shape plus the instant it
/// opened, because the guard is a duration and a snapshot is a moment.
#[derive(Debug, Clone)]
pub struct Pending {
    pub interaction: Interaction,
    opened: Instant,
}

impl Pending {
    /// What is left of the confirmation guard at `at`.
    ///
    /// The instant is the caller's because the key that would answer was pressed
    /// there: a surface reads the guard when it draws, and answers with the
    /// moment the keystroke landed.
    pub fn remaining_guard_at(&self, at: Instant) -> Duration {
        if !matches!(
            self.interaction.prompt,
            InteractionPrompt::Permission { .. }
        ) {
            return Duration::ZERO;
        }
        CONFIRM_GUARD.saturating_sub(at.saturating_duration_since(self.opened))
    }

    /// What is left of it right now.
    pub fn remaining_guard(&self) -> Duration {
        self.remaining_guard_at(Instant::now())
    }

    /// The contract's view of it, with `remainingGuardMs` recomputed for this
    /// moment rather than repeated from the one it opened at.
    pub fn resource(&self) -> Interaction {
        let mut interaction = self.interaction.clone();
        interaction.remaining_guard_ms =
            self.remaining_guard().as_millis().min(u128::from(u64::MAX)) as u64;
        interaction
    }
}

/// Everything open right now.
#[derive(Debug, Default)]
pub struct InteractionView {
    open: Vec<Pending>,
}

impl InteractionView {
    /// The oldest open prompt: one surface, one question at a time.
    pub fn head(&self) -> Option<&Pending> {
        self.open.first()
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Pending> {
        self.open.iter()
    }
}

pub(crate) enum InteractionMsg {
    Open {
        request: Box<OpenPrompt>,
        answer: oneshot::Sender<Verdict>,
        reply: oneshot::Sender<InteractionId>,
    },
    Respond {
        id: InteractionId,
        activation: ActivationKind,
        /// When the answer was given. A keystroke is answered at the moment it
        /// landed, not at the moment the actor got round to it.
        at: Instant,
        decision: InteractionDecision,
        reply: oneshot::Sender<Result<(), ProtocolErrorKind>>,
    },
    /// Close prompts without answering them. `abandoned_only` keeps a live
    /// background question out of a foreground turn's cleanup: what belongs to a
    /// turn that ended is the prompt whose run is already gone.
    CancelAll {
        reason: InteractionCancelReason,
        abandoned_only: bool,
        reply: oneshot::Sender<bool>,
    },
}

/// What one interaction change asks the actor to publish.
pub(crate) enum InteractionChange {
    Opened {
        conversation: ConvKey,
        interaction: Box<Interaction>,
    },
    /// The ordered item a resolution committed, before execution continued.
    Committed {
        conversation: ConvKey,
        turn: Option<TurnId>,
        item: Box<Item>,
    },
    Resolved {
        conversation: ConvKey,
        id: InteractionId,
        decision: InteractionDecision,
        item: Option<ItemId>,
    },
    Cancelled {
        conversation: ConvKey,
        id: InteractionId,
        reason: InteractionCancelReason,
    },
}

/// How the rest of the process opens and answers prompts.
#[derive(Clone)]
pub struct InteractionHandle {
    control: mpsc::UnboundedSender<Control>,
    view: watch::Receiver<Arc<InteractionView>>,
}

impl std::fmt::Debug for InteractionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InteractionHandle")
    }
}

impl InteractionHandle {
    pub fn view(&self) -> Arc<InteractionView> {
        self.view.borrow().clone()
    }

    /// Ask, and wait for the answer. The run that calls this is stopped until
    /// somebody settles it or the session does.
    pub async fn ask(&self, request: OpenPrompt) -> Verdict {
        let (answer, verdict) = oneshot::channel();
        let (reply, opened) = oneshot::channel();
        if self
            .control
            .send(Control::Interaction(InteractionMsg::Open {
                request: Box::new(request),
                answer,
                reply,
            }))
            .is_err()
        {
            return Verdict::Cancelled;
        }
        // The identifier is not the caller's business — the surface reads it
        // from the view — but waiting for it keeps the open ordered against the
        // wait below.
        let _ = opened.await;
        verdict.await.unwrap_or(Verdict::Cancelled)
    }

    /// Open a prompt and hand back the channel its answer arrives on, for a
    /// caller standing in for the run that would be waiting on it.
    #[cfg(test)]
    pub fn open(&self, request: OpenPrompt) -> oneshot::Receiver<Verdict> {
        let (answer, verdict) = oneshot::channel();
        let (reply, opened) = oneshot::channel();
        let _ = self
            .control
            .send(Control::Interaction(InteractionMsg::Open {
                request: Box::new(request),
                answer,
                reply,
            }));
        // The open is applied by the time the identifier comes back, so the view
        // a caller reads next already holds the prompt.
        let _ = Answer::new(opened, InteractionId::new("")).now();
        verdict
    }

    /// Answer one prompt.
    pub fn respond(
        &self,
        id: InteractionId,
        activation: ActivationKind,
        decision: InteractionDecision,
    ) -> Answer<Result<(), ProtocolErrorKind>> {
        self.respond_at(id, activation, Instant::now(), decision)
    }

    /// The same answer, given at the instant the key landed.
    pub fn respond_at(
        &self,
        id: InteractionId,
        activation: ActivationKind,
        at: Instant,
        decision: InteractionDecision,
    ) -> Answer<Result<(), ProtocolErrorKind>> {
        let (reply, answer) = oneshot::channel();
        let _ = self
            .control
            .send(Control::Interaction(InteractionMsg::Respond {
                id,
                activation,
                at,
                decision,
                reply,
            }));
        Answer::new(answer, Err(ProtocolErrorKind::InteractionClosed))
    }

    /// Close what is open without answering it. Returns whether anything was.
    pub fn cancel_all(
        &self,
        reason: InteractionCancelReason,
        abandoned_only: bool,
    ) -> Answer<bool> {
        let (reply, answer) = oneshot::channel();
        let _ = self
            .control
            .send(Control::Interaction(InteractionMsg::CancelAll {
                reason,
                abandoned_only,
                reply,
            }));
        Answer::new(answer, false)
    }
}

/// The prompts of one session, owned by the actor.
pub(crate) struct InteractionRegistry {
    open: Vec<Held>,
    view: watch::Sender<Arc<InteractionView>>,
}

struct Held {
    pending: Pending,
    conversation: ConvKey,
    answer: oneshot::Sender<Verdict>,
}

pub(crate) fn attach(
    control: mpsc::UnboundedSender<Control>,
) -> (InteractionRegistry, InteractionHandle) {
    let (view, reader) = watch::channel(Arc::new(InteractionView::default()));
    (
        InteractionRegistry {
            open: Vec::new(),
            view,
        },
        InteractionHandle {
            control,
            view: reader,
        },
    )
}

impl InteractionRegistry {
    /// Apply one message and say what it changed.
    ///
    /// Each arm publishes the reader view *before* it answers, which is what a
    /// caller depends on: a surface that woke on the reply and then read the view
    /// must never read a world older than the answer it was given.
    pub(crate) fn handle(
        &mut self,
        message: InteractionMsg,
        mint: &mut IdMint,
    ) -> Vec<InteractionChange> {
        let changes = self.apply(message, mint);
        self.publish();
        changes
    }

    /// Everything open, for a snapshot.
    pub(crate) fn pending(&self) -> Vec<Interaction> {
        self.open
            .iter()
            .map(|held| held.pending.resource())
            .collect()
    }

    fn publish(&mut self) {
        let _ = self.view.send(Arc::new(InteractionView {
            open: self.open.iter().map(|held| held.pending.clone()).collect(),
        }));
    }

    fn apply(&mut self, message: InteractionMsg, mint: &mut IdMint) -> Vec<InteractionChange> {
        match message {
            InteractionMsg::Open {
                request,
                answer,
                reply,
            } => {
                let id: InteractionId = mint.mint();
                let mut prompt = request.prompt;
                // The scope behind "don't ask again" is the server's own derived
                // rule, so its identifier is minted here rather than composed by
                // whoever built the prompt.
                if let InteractionPrompt::Permission {
                    session_scope: Some(scope),
                    ..
                } = &mut prompt
                {
                    scope.id = mint.mint();
                }
                let interaction = Interaction {
                    id: id.clone(),
                    // Stamped where the identifier is, as every conversation
                    // reference is; the key travels beside it.
                    conversation_id: crate::app::ids::ConversationId::new(""),
                    turn_id: request.turn,
                    item_id: request.item,
                    opened_at: now_millis(),
                    remaining_guard_ms: CONFIRM_GUARD.as_millis().min(u128::from(u64::MAX)) as u64,
                    prompt,
                };
                let pending = Pending {
                    interaction: interaction.clone(),
                    opened: Instant::now(),
                };
                let conversation = request.conversation;
                self.open.push(Held {
                    pending: pending.clone(),
                    conversation: conversation.clone(),
                    answer,
                });
                self.publish();
                let _ = reply.send(id);
                vec![InteractionChange::Opened {
                    conversation,
                    interaction: Box::new(pending.resource()),
                }]
            }
            InteractionMsg::Respond {
                id,
                activation,
                at,
                decision,
                reply,
            } => {
                let Some(index) = self
                    .open
                    .iter()
                    .position(|held| held.pending.interaction.id == id)
                else {
                    // A late or repeated response. It is refused by name and
                    // cannot reach a later prompt.
                    let _ = reply.send(Err(ProtocolErrorKind::InteractionClosed));
                    return Vec::new();
                };
                if let Err(refusal) = check(&self.open[index].pending, activation, at, &decision) {
                    let _ = reply.send(Err(refusal));
                    return Vec::new();
                }
                let held = self.open.remove(index);
                self.publish();
                let _ = reply.send(Ok(()));
                let verdict = verdict_of(&held.pending.interaction.prompt, &decision);
                let item = receipt(&held.pending.interaction, &decision).map(|body| {
                    let now = now_millis();
                    Item {
                        id: mint.mint(),
                        status: ItemStatus::Completed,
                        turn_id: held.pending.interaction.turn_id.clone(),
                        started_at: Some(now),
                        completed_at: Some(now),
                        body,
                    }
                });
                let item_id = item.as_ref().map(|item| item.id.clone());
                let mut changes = Vec::new();
                if let Some(item) = item {
                    changes.push(InteractionChange::Committed {
                        conversation: held.conversation.clone(),
                        turn: held.pending.interaction.turn_id.clone(),
                        item: Box::new(item),
                    });
                }
                changes.push(InteractionChange::Resolved {
                    conversation: held.conversation.clone(),
                    id,
                    decision,
                    item: item_id,
                });
                // The run continues only once the ordered item is committed.
                let _ = held.answer.send(verdict);
                changes
            }
            InteractionMsg::CancelAll {
                reason,
                abandoned_only,
                reply,
            } => {
                let (closed, kept): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.open)
                    .into_iter()
                    .partition(|held| !abandoned_only || held.answer.is_closed());
                self.open = kept;
                self.publish();
                let _ = reply.send(!closed.is_empty());
                closed
                    .into_iter()
                    .map(|held| {
                        // Fail closed: the run is told no, not left waiting.
                        let _ = held.answer.send(Verdict::Cancelled);
                        InteractionChange::Cancelled {
                            conversation: held.conversation,
                            id: held.pending.interaction.id,
                            reason,
                        }
                    })
                    .collect()
            }
        }
    }
}

/// Whether this answer is one the prompt advertised, and whether it is early.
fn check(
    pending: &Pending,
    activation: ActivationKind,
    at: Instant,
    decision: &InteractionDecision,
) -> Result<(), ProtocolErrorKind> {
    let advertised = match (&pending.interaction.prompt, decision) {
        (InteractionPrompt::Permission { decisions, .. }, InteractionDecision::AllowOnce) => {
            decisions.contains(&PermissionDecisionKind::AllowOnce)
        }
        (
            InteractionPrompt::Permission {
                decisions,
                session_scope,
                ..
            },
            InteractionDecision::AllowSession { scope_id },
        ) => {
            decisions.contains(&PermissionDecisionKind::AllowSession)
                // The scope is the server's own derived rule. A client that
                // names anything else is asking for a promise the gate cannot
                // keep.
                && session_scope
                    .as_ref()
                    .is_some_and(|scope| &scope.id == scope_id)
        }
        (InteractionPrompt::Permission { decisions, .. }, InteractionDecision::Deny { .. }) => {
            decisions.contains(&PermissionDecisionKind::Deny)
        }
        (InteractionPrompt::Question { .. }, InteractionDecision::Answer { option_id, text }) => {
            match (option_id, text) {
                (Some(id), _) => match &pending.interaction.prompt {
                    InteractionPrompt::Question { options, .. } => {
                        options.iter().any(|option| &option.id == id)
                    }
                    _ => false,
                },
                (None, Some(_)) => matches!(
                    &pending.interaction.prompt,
                    InteractionPrompt::Question {
                        allows_free_text: true,
                        ..
                    }
                ),
                (None, None) => false,
            }
        }
        (InteractionPrompt::Confirmation { .. }, InteractionDecision::Confirm) => true,
        // Dismissing is always available: leaving a prompt is the refusal.
        (_, InteractionDecision::Cancel) => true,
        _ => false,
    };
    if !advertised {
        return Err(ProtocolErrorKind::InteractionInvalidDecision);
    }
    // D81, and exactly D81: the guard holds back keyboard approval of a
    // permission prompt. A wrong AskUserQuestion option costs a round trip; a
    // wrong approval runs a command.
    let approving = matches!(
        decision,
        InteractionDecision::AllowOnce
            | InteractionDecision::AllowSession { .. }
            | InteractionDecision::Confirm
    );
    if approving
        && activation == ActivationKind::Keyboard
        && !pending.remaining_guard_at(at).is_zero()
    {
        return Err(ProtocolErrorKind::InteractionNotReady);
    }
    Ok(())
}

/// What the run is told.
fn verdict_of(prompt: &InteractionPrompt, decision: &InteractionDecision) -> Verdict {
    match decision {
        InteractionDecision::AllowOnce => Verdict::Allow,
        InteractionDecision::AllowSession { .. } => Verdict::AllowSession,
        InteractionDecision::Deny { feedback } => Verdict::Deny {
            feedback: feedback.clone(),
        },
        InteractionDecision::Confirm => Verdict::Confirm,
        InteractionDecision::Answer { option_id, text } => {
            let option = option_id.as_ref().and_then(|id| match prompt {
                InteractionPrompt::Question { options, .. } => {
                    options.iter().position(|option| &option.id == id)
                }
                _ => None,
            });
            Verdict::Answer {
                option,
                text: text.clone(),
            }
        }
        InteractionDecision::Cancel => Verdict::Cancelled,
    }
}

/// The ordered item a resolution commits.
///
/// A question's answer enters the model's context through the existing answer
/// path; a permission receipt is display and audit state and is explicitly
/// excluded from model input (spec "Item").
fn receipt(interaction: &Interaction, decision: &InteractionDecision) -> Option<ItemBody> {
    match (&interaction.prompt, decision) {
        (InteractionPrompt::Permission { tool, .. }, _) => {
            let (kind, scope_id, feedback) = match decision {
                InteractionDecision::AllowOnce => (PermissionDecisionKind::AllowOnce, None, None),
                InteractionDecision::AllowSession { scope_id } => (
                    PermissionDecisionKind::AllowSession,
                    Some(scope_id.clone()),
                    None,
                ),
                InteractionDecision::Deny { feedback } => {
                    (PermissionDecisionKind::Deny, None, feedback.clone())
                }
                // Dismissing a permission prompt is a denial, and the receipt
                // says so rather than saying nothing happened.
                _ => (PermissionDecisionKind::Deny, None, None),
            };
            Some(ItemBody::PermissionReceipt {
                interaction_id: interaction.id.clone(),
                tool: tool.name.clone(),
                decision: kind,
                scope_id,
                feedback,
            })
        }
        (
            InteractionPrompt::Question {
                question, options, ..
            },
            InteractionDecision::Answer { option_id, text },
        ) => {
            let answer = match (option_id, text) {
                (Some(id), _) => options
                    .iter()
                    .find(|option| &option.id == id)
                    .map(|option| option.label.clone())?,
                (None, Some(text)) => text.clone(),
                (None, None) => return None,
            };
            Some(ItemBody::QuestionAnswer {
                interaction_id: interaction.id.clone(),
                question: question.clone(),
                answer,
                option_id: option_id.clone(),
            })
        }
        _ => None,
    }
}

/// The permission gate's prompt, as an interaction.
///
/// This is the whole of what used to be `ui::modal_ask`: the gate asks, the actor
/// holds the answer, and whichever surface is drawing prompts settles it. A
/// verdict always comes back — a session that ends while a run waits denies
/// rather than hanging it.
pub fn permission_ask(
    handle: InteractionHandle,
    conversation: ConvKey,
) -> Arc<crate::query::AskFn> {
    Arc::new(move |ask| {
        let handle = handle.clone();
        let conversation = conversation.clone();
        let scope = ask.scope.map(str::to_string);
        let mut decisions = vec![PermissionDecisionKind::AllowOnce];
        if scope.is_some() {
            decisions.push(PermissionDecisionKind::AllowSession);
        }
        decisions.push(PermissionDecisionKind::Deny);
        let prompt = InteractionPrompt::Permission {
            title: format!("Allow running {}", ask.tool),
            reason: Some(ask.reason.to_string()),
            tool: crate::app::snapshot::ToolRequest {
                name: ask.tool.to_string(),
                input: ask.input.clone(),
            },
            preview: preview(ask),
            decisions,
            // The identifier is stamped by the registry; the label is what the
            // gate derived and verified.
            session_scope: scope.map(|label| crate::app::snapshot::SessionScope {
                id: crate::app::ids::ScopeId::new(""),
                label,
            }),
            allows_feedback: true,
        };
        Box::pin(async move {
            match handle
                .ask(OpenPrompt {
                    conversation,
                    turn: None,
                    item: None,
                    prompt,
                })
                .await
            {
                Verdict::Allow => crate::query::AskOutcome::Allow,
                Verdict::AllowSession => crate::query::AskOutcome::AllowSession,
                Verdict::Deny { feedback } => crate::query::AskOutcome::Deny { feedback },
                // Anything else is not an approval, and a gate that is not
                // approved refuses.
                _ => crate::query::AskOutcome::Deny { feedback: None },
            }
        })
    })
}

/// AskUserQuestion, as an interaction. The options are the model's own; the
/// free-text row is the one CC adds.
pub fn question_ask(
    handle: InteractionHandle,
    conversation: ConvKey,
) -> Arc<crate::query::AskQuestionFn> {
    Arc::new(move |title, question, options| {
        let handle = handle.clone();
        let conversation = conversation.clone();
        let prompt = InteractionPrompt::Question {
            title,
            question,
            options: options
                .into_iter()
                .enumerate()
                .map(
                    |(index, (label, description))| crate::app::snapshot::QuestionOption {
                        id: index.to_string(),
                        label,
                        description,
                    },
                )
                .collect(),
            allows_free_text: true,
        };
        Box::pin(async move {
            match handle
                .ask(OpenPrompt {
                    conversation,
                    turn: None,
                    item: None,
                    prompt,
                })
                .await
            {
                Verdict::Answer {
                    option: Some(index),
                    ..
                } => Some(crate::query::AskAnswer::Option(index)),
                Verdict::Answer {
                    text: Some(text), ..
                } => Some(crate::query::AskAnswer::Other(text)),
                _ => None,
            }
        })
    })
}

/// The rows a permission prompt shows above its options: a file change shows the
/// diff it would make, anything carrying a shell command shows the command.
fn preview(ask: &crate::query::AskContext<'_>) -> Option<crate::app::snapshot::InteractionPreview> {
    if let Some(diff) = ask.diff {
        return Some(crate::app::snapshot::InteractionPreview::Diff {
            diff: diff.to_string(),
        });
    }
    ask.input
        .get("command")
        .and_then(|value| value.as_str())
        .map(
            |command| crate::app::snapshot::InteractionPreview::Command {
                command: command.to_string(),
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::{EpochId, ScopeId};
    use crate::app::snapshot::{QuestionOption, SessionScope, ToolRequest};

    fn registry() -> (InteractionRegistry, IdMint) {
        let (control, _inbox) = mpsc::unbounded_channel();
        let (registry, _handle) = attach(control);
        (registry, IdMint::new(EpochId::mint()))
    }

    fn permission(scope: bool) -> InteractionPrompt {
        InteractionPrompt::Permission {
            title: "Allow running Bash".to_string(),
            reason: Some("Run the test suite".to_string()),
            tool: ToolRequest {
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cargo test"}),
            },
            preview: None,
            decisions: if scope {
                vec![
                    PermissionDecisionKind::AllowOnce,
                    PermissionDecisionKind::AllowSession,
                    PermissionDecisionKind::Deny,
                ]
            } else {
                vec![
                    PermissionDecisionKind::AllowOnce,
                    PermissionDecisionKind::Deny,
                ]
            },
            session_scope: scope.then(|| SessionScope {
                id: ScopeId::new("scope_1"),
                label: "Bash: cargo test".to_string(),
            }),
            allows_feedback: true,
        }
    }

    fn open(
        registry: &mut InteractionRegistry,
        mint: &mut IdMint,
        prompt: InteractionPrompt,
    ) -> (InteractionId, oneshot::Receiver<Verdict>) {
        let (answer, verdict) = oneshot::channel();
        let (reply, opened) = oneshot::channel();
        registry.handle(
            InteractionMsg::Open {
                request: Box::new(OpenPrompt {
                    conversation: ConvKey::Main,
                    turn: None,
                    item: None,
                    prompt,
                }),
                answer,
                reply,
            },
            mint,
        );
        (
            opened
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            verdict,
        )
    }

    fn respond(
        registry: &mut InteractionRegistry,
        mint: &mut IdMint,
        id: &InteractionId,
        activation: ActivationKind,
        decision: InteractionDecision,
    ) -> (Result<(), ProtocolErrorKind>, Vec<InteractionChange>) {
        let (reply, answer) = oneshot::channel();
        let changes = registry.handle(
            InteractionMsg::Respond {
                id: id.clone(),
                activation,
                at: Instant::now(),
                decision,
                reply,
            },
            mint,
        );
        (
            answer
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            changes,
        )
    }

    /// The guard holds back keyboard approval and nothing else.
    #[test]
    fn the_confirmation_guard_stops_only_a_premature_keyboard_approval() {
        let (mut registry, mut mint) = registry();
        let (id, _verdict) = open(&mut registry, &mut mint, permission(false));
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Keyboard,
                InteractionDecision::AllowOnce
            )
            .0,
            Err(ProtocolErrorKind::InteractionNotReady),
            "a keystroke already in flight approves nothing"
        );
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Pointer,
                InteractionDecision::AllowOnce
            )
            .0,
            Ok(()),
            "a pointer was aimed at the prompt that exists"
        );

        // Denial is immediate however it arrives: the user is leaving.
        let (id, _verdict) = open(&mut registry, &mut mint, permission(false));
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Keyboard,
                InteractionDecision::Deny { feedback: None }
            )
            .0,
            Ok(())
        );
    }

    /// A question has no guard: a wrong option costs a round trip, a wrong
    /// approval runs a command.
    #[test]
    fn a_question_is_answered_the_moment_it_is_asked() {
        let (mut registry, mut mint) = registry();
        let (id, verdict) = open(
            &mut registry,
            &mut mint,
            InteractionPrompt::Question {
                title: "Tech stack".to_string(),
                question: "Which library?".to_string(),
                options: vec![
                    QuestionOption {
                        id: "0".to_string(),
                        label: "A".to_string(),
                        description: None,
                    },
                    QuestionOption {
                        id: "1".to_string(),
                        label: "B".to_string(),
                        description: None,
                    },
                ],
                allows_free_text: true,
            },
        );
        let (result, changes) = respond(
            &mut registry,
            &mut mint,
            &id,
            ActivationKind::Keyboard,
            InteractionDecision::Answer {
                option_id: Some("1".to_string()),
                text: None,
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            verdict
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            Verdict::Answer {
                option: Some(1),
                text: None
            }
        );
        // The answer is committed as an ordered item before the resolution that
        // names it, and it is the answer's *label* the transcript carries.
        match &changes[..] {
            [
                InteractionChange::Committed { item, .. },
                InteractionChange::Resolved {
                    item: named,
                    decision,
                    ..
                },
            ] => {
                assert_eq!(named.as_ref(), Some(&item.id));
                assert!(matches!(
                    &item.body,
                    ItemBody::QuestionAnswer { answer, .. } if answer == "B"
                ));
                assert!(matches!(decision, InteractionDecision::Answer { .. }));
            }
            other => panic!(
                "expected the item then its resolution, got {} changes",
                other.len()
            ),
        }
    }

    /// `allowSession` is only valid against the scope the server derived.
    #[test]
    fn allow_session_names_a_scope_the_server_verified() {
        let (mut registry, mut mint) = registry();
        let (id, _verdict) = open(&mut registry, &mut mint, permission(false));
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Pointer,
                InteractionDecision::AllowSession {
                    scope_id: ScopeId::new("scope_1")
                }
            )
            .0,
            Err(ProtocolErrorKind::InteractionInvalidDecision),
            "no scope was offered, so no session rule can be promised"
        );

        let (id, verdict) = open(&mut registry, &mut mint, permission(true));
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Pointer,
                InteractionDecision::AllowSession {
                    scope_id: ScopeId::new("scope_other")
                }
            )
            .0,
            Err(ProtocolErrorKind::InteractionInvalidDecision),
            "and not against one the client made up"
        );
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Pointer,
                InteractionDecision::AllowSession {
                    scope_id: ScopeId::new("scope_1")
                }
            )
            .0,
            Ok(())
        );
        assert_eq!(
            verdict
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            Verdict::AllowSession
        );
    }

    /// Answered once. The second response is refused by name and changes nothing.
    #[test]
    fn a_late_response_is_refused_rather_than_applied() {
        let (mut registry, mut mint) = registry();
        let (id, _verdict) = open(&mut registry, &mut mint, permission(false));
        assert_eq!(
            respond(
                &mut registry,
                &mut mint,
                &id,
                ActivationKind::Pointer,
                InteractionDecision::Deny { feedback: None }
            )
            .0,
            Ok(())
        );
        let (result, changes) = respond(
            &mut registry,
            &mut mint,
            &id,
            ActivationKind::Pointer,
            InteractionDecision::AllowOnce,
        );
        assert_eq!(result, Err(ProtocolErrorKind::InteractionClosed));
        assert!(changes.is_empty(), "a closed prompt publishes nothing more");
    }

    /// Cancellation fails closed: the run is told no rather than left waiting.
    #[test]
    fn cancellation_fails_closed() {
        let (mut registry, mut mint) = registry();
        let (_id, verdict) = open(&mut registry, &mut mint, permission(true));
        let (reply, cancelled) = oneshot::channel();
        let changes = registry.handle(
            InteractionMsg::CancelAll {
                reason: InteractionCancelReason::Interrupted,
                abandoned_only: false,
                reply,
            },
            &mut mint,
        );
        assert!(
            cancelled
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}"))
        );
        assert!(matches!(
            changes.as_slice(),
            [InteractionChange::Cancelled { .. }]
        ));
        assert_eq!(
            verdict
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}")),
            Verdict::Cancelled
        );
    }

    /// A prompt whose run is still waiting is not a dead turn's leftover, and a
    /// foreground cleanup leaves it alone (D80).
    #[test]
    fn an_abandoned_only_sweep_keeps_the_prompts_still_being_waited_on() {
        let (mut registry, mut mint) = registry();
        let (live_id, _live) = open(&mut registry, &mut mint, permission(false));
        let (_dead_id, dead) = open(&mut registry, &mut mint, permission(false));
        drop(dead);

        let (reply, cancelled) = oneshot::channel();
        let changes = registry.handle(
            InteractionMsg::CancelAll {
                reason: InteractionCancelReason::TurnEnded,
                abandoned_only: true,
                reply,
            },
            &mut mint,
        );
        assert!(
            cancelled
                .blocking_recv()
                .unwrap_or_else(|error| panic!("{error}"))
        );
        assert_eq!(changes.len(), 1, "only the abandoned one closed");
        let view = registry.pending();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].id, live_id);
    }
}
