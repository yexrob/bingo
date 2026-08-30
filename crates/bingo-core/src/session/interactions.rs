//! Interactions: the actor opens one whenever something needs a person, holds
//! the reply channel until an answer arrives, and cancels the rest when the
//! turn that opened them ends.

use bingo_sdk::*;
use jiff::{SignedDuration, Timestamp};
use serde_json::Value;
use tokio::sync::oneshot;

use super::Actor;
use super::mailbox::Answered;

/// A keyboard answer inside this window after opening is a stray keystroke.
pub const INTERACTION_GUARD_MS: i64 = 400;

pub(super) struct Pending {
    pub(super) interaction: Interaction,
    pub(super) reply: oneshot::Sender<Result<Answer, KernelError>>,
}

impl Actor {
    pub(super) async fn open_interaction(
        &mut self,
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
        reply: oneshot::Sender<Result<Answer, KernelError>>,
    ) {
        // A turn's call asks under the turn; a holding command asks under
        // none (ADR-0012 §5). Nothing else can be running to ask.
        let turn = match (&self.running, self.commands.busy()) {
            (Some(running), _) => Some(running.turn.clone()),
            (None, true) => None,
            (None, false) => {
                let _ = reply.send(Err(KernelError::new(
                    ErrorCode::NotReady,
                    "no turn or command is running",
                )));
                return;
            }
        };
        let now = Timestamp::now();
        let interaction = Interaction {
            id: InteractionId::mint(),
            session: self.id.clone(),
            turn,
            item,
            opened_at: now,
            guard_until: now
                .checked_add(SignedDuration::from_millis(INTERACTION_GUARD_MS))
                .ok(),
            expires_at: None,
            kind,
            answers,
        };
        self.pending.insert(
            interaction.id.clone(),
            Pending {
                interaction: interaction.clone(),
                reply,
            },
        );
        self.publish(Event::InteractionOpened { interaction }, None)
            .await;
    }

    /// Why this answer cannot be taken, if it cannot.
    fn refuse(
        &self,
        id: &InteractionId,
        answer: &Answer,
        activation: Activation,
    ) -> Option<(ErrorCode, String)> {
        let Some(pending) = self.pending.get(id) else {
            return Some((
                ErrorCode::InteractionClosed,
                "no such open interaction".into(),
            ));
        };
        if !pending.interaction.answers.contains(&answer.spec()) {
            return Some((
                ErrorCode::InvalidInput,
                format!("{:?} is not an accepted answer here", answer.spec()),
            ));
        }
        if activation == Activation::Keyboard
            && let Some(guard) = pending.interaction.guard_until
            && Timestamp::now() < guard
        {
            return Some((
                ErrorCode::NotReady,
                "answered too soon after opening".into(),
            ));
        }
        None
    }

    pub(super) async fn answer(&mut self, answered: Answered) {
        let Answered {
            intent,
            interaction: id,
            answer,
            activation,
            who,
        } = answered;
        if let Some((code, message)) = self.refuse(&id, &answer, activation) {
            return self.reject(intent, code, message).await;
        }
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        let _ = pending.reply.send(Ok(answer.clone()));
        self.publish(
            Event::InteractionResolved {
                id,
                answer,
                by: ResolvedBy::Client {
                    name: who.name,
                    surface: who.surface,
                },
            },
            Some(intent.clone()),
        )
        .await;
        self.applied(intent, Value::Null).await;
    }

    pub(super) async fn cancel_interactions(&mut self, reason: CancelReason) {
        let pending: Vec<_> = self.pending.drain().collect();
        self.cancel_each(pending, reason).await;
    }

    /// The holding command that asked is done: what it left open closes,
    /// and a turn's own questions (there is no turn) are not touched.
    pub(super) async fn cancel_command_interactions(&mut self) {
        let ids: Vec<InteractionId> = self
            .pending
            .iter()
            .filter(|(_, p)| p.interaction.turn.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        let pending: Vec<_> = ids
            .into_iter()
            .filter_map(|id| self.pending.remove(&id).map(|p| (id, p)))
            .collect();
        self.cancel_each(pending, CancelReason::CommandEnded).await;
    }

    async fn cancel_each(&mut self, pending: Vec<(InteractionId, Pending)>, reason: CancelReason) {
        for (id, pending) in pending {
            let _ = pending.reply.send(Err(KernelError::new(
                ErrorCode::InteractionClosed,
                format!("cancelled: {reason:?}"),
            )));
            self.publish(Event::InteractionCancelled { id, reason }, None)
                .await;
        }
    }
}
