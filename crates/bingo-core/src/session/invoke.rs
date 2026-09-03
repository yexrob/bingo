//! The door into a running turn (ADR-0036 §2): one call, handed in from
//! outside the model, served by the turn's own machinery — its gate, its
//! journal, a cancel token that is a child of its.
//!
//! The seam is here, beside the turn loop, and never inside it. Whoever hands
//! a call in is blocked on the answer before it can go on, so the call must be
//! served while the turn's stream is still open; a door that waited for the
//! stream to end would deadlock the two against each other. What it holds is
//! the executor's pieces — the turn's config, the tool the turn was given, its
//! token, the item it journals under — and never the turn itself, which is
//! busy with its stream.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::*;
use tokio::sync::oneshot;

use super::mailbox::Mailbox;
use crate::executor::{self, Gate, PendingCall};
use crate::gate::{GateInput, gate_call};
use crate::turn::TurnConfig;

/// Where a bridged call's outcome goes, or the refusal in its place. It goes
/// here and nowhere else: the caller already holds it, and a copy in the
/// provider's messages would be a second representation of one call.
pub(super) type Reply = oneshot::Sender<Result<ToolOutcome, KernelError>>;

/// What the running turn offers this call: the pieces the actor hands over
/// when it lets one in ([`super::inputs`]).
pub(super) struct Serving {
    pub(super) turn: TurnId,
    pub(super) cancel: CancellationToken,
    pub(super) config: Arc<TurnConfig>,
    pub(super) tool: Arc<dyn Tool>,
}

/// One call, in flight beside the turn it belongs to.
pub(super) struct Bridged {
    /// The running turn's own, not the session's next: a call is judged by
    /// the policy and the hooks the turn started under.
    pub(super) config: Arc<TurnConfig>,
    pub(super) mailbox: Mailbox,
    pub(super) turn: TurnId,
    /// A child of the turn's: one `esc` ends the turn and drops this call
    /// where it stands.
    pub(super) cancel: CancellationToken,
    pub(super) tool: Arc<dyn Tool>,
    pub(super) item: ItemId,
    pub(super) call: ToolCall,
}

impl Bridged {
    /// Gate it, run it, journal what it came to, then answer — in that order,
    /// so an interrupt reaches the caller as an outcome rather than silence.
    pub(super) async fn serve(self, reply: Reply) {
        let pending = self.gated().await;
        let outcome = executor::execute_one(pending, &self.cancel, self.context()).await;
        self.mailbox.call_finished(outcome.clone());
        let _ = reply.send(Ok(outcome));
    }

    /// The turn's own gate: the same hooks, the same policy, the same person
    /// to ask — under this call's own item, so a question about it is asked
    /// where a question about any call is.
    async fn gated(&self) -> PendingCall {
        let hooks = self.config.hooks.gather().await;
        let hook_cx = self.hook_context();
        let prompter = AskVia {
            mailbox: self.mailbox.clone(),
            item: self.item.clone(),
        };
        let gated = gate_call(
            GateInput {
                session: &self.config.session.id,
                cwd: &self.config.cwd,
                item: &self.item,
                call: self.call.clone(),
                tool: Some(Arc::clone(&self.tool)),
                policy: self.config.policy.as_ref(),
                hooks: &hooks,
                hook_cx: &hook_cx,
            },
            &prompter,
        )
        .await;
        if let Some(receipt) = gated.receipt {
            let _ = self.mailbox.record(receipt).await;
        }
        if gated.gate == Gate::Allowed {
            self.mailbox
                .call_allowed(self.item.clone(), gated.call.input.clone());
        }
        PendingCall {
            item: self.item.clone(),
            call: gated.call,
            tool: Some(Arc::clone(&self.tool)),
            traits: gated.traits,
            gate: gated.gate,
        }
    }

    fn context(&self) -> ToolContext {
        ToolContext {
            call_id: self.call.call_id.clone(),
            session: self.config.session.id.clone(),
            turn: self.turn.clone(),
            item: self.item.clone(),
            cwd: self.config.cwd.clone(),
            cancel: self.cancel.child_token(),
            env: self.config.env.clone(),
            host: self.config.host.clone(),
            call: self.config.tool_host.clone(),
        }
    }

    fn hook_context(&self) -> HookContext {
        HookContext {
            host: self.config.host.clone(),
            session: self.config.session.id.clone(),
            turn: Some(self.turn.clone()),
            cwd: self.config.cwd.clone(),
            provider: self.config.model.as_ref().map(|m| m.provider.clone()),
            model: self.config.model.as_ref().map(|m| m.id.clone()),
        }
    }
}

/// The permission prompter a bridged call's gate uses: the interaction opens
/// on the actor under the call's own item, as a turn's gate opens its own.
struct AskVia {
    mailbox: Mailbox,
    item: ItemId,
}

#[async_trait]
impl Prompter for AskVia {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.mailbox
            .ask(Some(self.item.clone()), kind, answers)
            .await
    }
}
