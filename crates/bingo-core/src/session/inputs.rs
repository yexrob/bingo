//! What reaches the actor from outside: a client's submit — a command or
//! prose — a peer's delivery, an interrupt, the queue that holds what cannot
//! run yet (ADR-0008 §2, ADR-0010 §1, ADR-0011 §1), and a tool call handed
//! into the running turn from outside the model (ADR-0036 §2), which is
//! served beside the turn by [`super::invoke`].

use std::sync::Arc;

use bingo_sdk::*;
use serde_json::json;

use jiff::Timestamp;
use serde_json::Value;

use super::queue::Unit;
use super::{Actor, commands, invoke, validate};
use crate::turn::TurnKind;

impl Actor {
    pub(super) async fn submit(&mut self, intent: IntentId, input: Input) {
        if self.state.closed || self.closing.is_some() {
            return self
                .reject(intent, ErrorCode::SessionClosed, "the session is closed")
                .await;
        }
        match commands::parse(&input) {
            Some(parsed) => self.submit_command(intent, input, parsed).await,
            None => self.submit_prose(intent, input).await,
        }
    }

    /// A command line: run now if instant, else behind whatever is running.
    async fn submit_command(&mut self, intent: IntentId, input: Input, parsed: commands::Parsed) {
        let Some(command) = self.commands.find(&parsed.name).await else {
            return self
                .reject(
                    intent,
                    ErrorCode::InvalidInput,
                    format!("unknown command: {}", commands::spelled(&parsed.name)),
                )
                .await;
        };
        let instant = command.spec().instant;
        if !instant && self.busy() {
            return self.enqueue(intent, input).await;
        }
        let invocation = commands::Invocation::of(&input, &parsed);
        self.run_command(intent, invocation, command, parsed.args, !instant)
            .await;
    }

    async fn run_command(
        &mut self,
        intent: IntentId,
        invocation: commands::Invocation,
        command: Arc<dyn Command>,
        args: String,
        holds: bool,
    ) {
        let spawned = self.commands.spawn(commands::Run {
            intent: intent.clone(),
            invocation,
            command,
            args,
            holds,
        });
        if let Err(e) = spawned {
            self.reject(intent, e.code, e.message).await;
        }
    }

    /// The command's outcome becomes its ack (ADR-0008 §3); then the queue
    /// may move.
    pub(super) async fn command_finished(
        &mut self,
        intent: IntentId,
        outcome: Result<CommandOutcome, KernelError>,
    ) {
        let Some((invocation, held)) = self.commands.finish(&intent) else {
            tracing::warn!(session = %self.id, %intent, "completion from a command that is not running");
            return;
        };
        if held {
            self.cancel_command_interactions().await;
        }
        match outcome {
            Ok(CommandOutcome::Applied { message }) => {
                let result = match message {
                    Some(message) => json!({ "message": message }),
                    None => json!({}),
                };
                self.applied(intent, result).await;
            }
            Ok(CommandOutcome::View { view }) => {
                self.applied(intent, json!({ "view": view })).await
            }
            Ok(CommandOutcome::Record { body }) => {
                let item = self.record(body).await;
                self.applied(intent, json!({ "item": item })).await;
            }
            Ok(CommandOutcome::Prompt { text }) => {
                let text = invocation.prompt(&text);
                self.submit_prose(intent, Input::text(text, invocation.origin))
                    .await;
            }
            Err(e) => self.reject(intent, e.code, e.message).await,
        }
        self.refresh_config().await;
        self.drain_queue().await;
    }

    async fn submit_prose(&mut self, intent: IntentId, input: Input) {
        let mut input = input;
        if let Err(message) = validate(&input) {
            return self.reject(intent, ErrorCode::InvalidInput, message).await;
        }
        let cx = self.hook_context();
        for hook in self.config.hooks.at(HookPoint::Submit, None).await {
            match hook.on_submit(&mut input, &cx).await {
                HookOutcome::Continue | HookOutcome::Ask { .. } => {}
                HookOutcome::Deny { reason } | HookOutcome::Block { reason } => {
                    return self
                        .reject(intent, ErrorCode::PermissionDenied, reason)
                        .await;
                }
                HookOutcome::Redirect { session } => {
                    return self.redirect(intent, session, input).await;
                }
            }
        }
        if !self.answers() {
            return self.log_input(intent, input).await;
        }
        if self.busy() {
            return self.enqueue(intent, input).await;
        }
        let inputs = self.held_then(intent, input).await;
        self.start_turn(inputs, TurnOrigin::Submit, TurnKind::Respond)
            .await;
    }

    /// A `Log` session's input (ADR-0011 §1): a user item in the journal at
    /// once, acknowledged as applied, and nothing answers it.
    async fn log_input(&mut self, intent: IntentId, input: Input) {
        let Input::Text {
            text,
            images,
            origin,
        } = input
        else {
            return self
                .reject(intent, ErrorCode::InvalidInput, "a log records text")
                .await;
        };
        let id = self
            .journal_prose(None, intent.clone(), text, images, origin)
            .await;
        self.applied(intent, json!({ "item": id })).await;
    }

    /// `@name` and the like (ADR-0010 §2): the input as the hook left it goes
    /// to another session under an intent of its own; this one says where.
    async fn redirect(&mut self, intent: IntentId, to: SessionId, input: Input) {
        let sent = self
            .config
            .host
            .deliver(&to, IntentId::mint(), input, Delivery::Wake)
            .await;
        match sent {
            Ok(()) => self.applied(intent, json!({ "redirected": to })).await,
            Err(e) => self.reject(intent, e.code, e.message).await,
        }
    }

    /// A peer's message (ADR-0010 §1): prose from another session, past the
    /// command parser and the submit hooks, into the queue or a `Peer` turn.
    pub(super) async fn deliver(&mut self, intent: IntentId, input: Input, delivery: Delivery) {
        if self.state.closed || self.closing.is_some() {
            return self
                .reject(intent, ErrorCode::SessionClosed, "the session is closed")
                .await;
        }
        if !matches!(input, Input::Text { .. }) {
            return self
                .reject(intent, ErrorCode::InvalidInput, "a peer delivers text")
                .await;
        }
        if let Err(message) = validate(&input) {
            return self.reject(intent, ErrorCode::InvalidInput, message).await;
        }
        if !self.answers() {
            return self.log_input(intent, input).await;
        }
        if self.busy() || delivery == Delivery::Hold {
            return self.enqueue(intent, input).await;
        }
        let inputs = self.held_then(intent, input).await;
        self.start_turn(inputs, TurnOrigin::Peer, TurnKind::Respond)
            .await;
    }

    /// Prose held in an idle session's queue goes first, in order.
    async fn held_then(&mut self, intent: IntentId, input: Input) -> Vec<(IntentId, Input)> {
        let mut inputs = self.take_queue().await;
        inputs.push((intent, input));
        inputs
    }

    async fn enqueue(&mut self, intent: IntentId, input: Input) {
        let position = self.queue.push(intent.clone(), input);
        let changed = self.queue.changed();
        self.publish(changed, None).await;
        self.publish(
            Event::IntentAck {
                intent: intent.clone(),
                outcome: IntentOutcome::Queued { position },
            },
            Some(intent),
        )
        .await;
    }

    pub(super) async fn interrupt(&mut self, intent: IntentId, scope: InterruptScope) {
        let Some(running) = &self.running else {
            return self
                .reject(intent, ErrorCode::NotReady, "no turn is running")
                .await;
        };
        if let InterruptScope::Turn { turn } = &scope
            && turn != &running.turn
        {
            return self
                .reject(intent, ErrorCode::NotReady, "that turn is not running")
                .await;
        }
        running.cancel.cancel();
        let turn = running.turn.clone();
        self.cancel_interactions(CancelReason::Interrupted).await;
        self.applied(intent, json!({ "turn": turn })).await;
    }

    // ----- a call handed into the running turn (ADR-0036 §2) -----

    /// Refuse the call, or open its item and hand it to a server beside the
    /// turn. Nothing here waits for the call: the actor goes on reading its
    /// mailbox, which is what keeps the turn streaming and one `esc` heard.
    pub(super) async fn invoke(&mut self, call: ToolCall, reply: invoke::Reply) {
        let serving = match self.serving(&call.name) {
            Ok(serving) => serving,
            Err(refusal) => {
                let _ = reply.send(Err(refusal));
                return;
            }
        };
        let item = self.open_call(&serving.turn, &call).await;
        let bridged = invoke::Bridged {
            config: serving.config,
            mailbox: self.mailbox.clone(),
            turn: serving.turn,
            cancel: serving.cancel,
            tool: serving.tool,
            item,
            call,
        };
        self.tracker.spawn(bridged.serve(reply));
    }

    /// The pieces this call would be served with, or why there are none. Fail
    /// closed twice: no turn in flight is a refusal, and so is a name the
    /// running turn's own offer does not carry — the tools the model was
    /// given are the whole of what may be called.
    fn serving(&self, name: &str) -> Result<invoke::Serving, KernelError> {
        let Some(running) = &self.running else {
            return Err(KernelError::new(
                ErrorCode::NotReady,
                "no turn is in flight in this session",
            ));
        };
        let tool = running
            .tools
            .iter()
            .find(|tool| tool.spec().name == name)
            .cloned()
            .ok_or_else(|| {
                KernelError::new(
                    ErrorCode::ToolNotFound,
                    format!("the running turn was not offered a tool called {name}"),
                )
            })?;
        Ok(invoke::Serving {
            turn: running.turn.clone(),
            cancel: running.cancel.child_token(),
            config: Arc::clone(&running.config),
            tool,
        })
    }

    /// The tools the turn resolved when it started (ADR-0009 §1) — the offer
    /// its requests carry. They are kept as the turn's own objects, not as a
    /// list of names beside them: a door that resolved the set a second time
    /// could serve a tool the model was never given.
    pub(super) fn turn_offers(&mut self, turn: &TurnId, tools: Vec<Arc<dyn Tool>>) {
        if let Some(running) = self.running.as_mut()
            && &running.turn == turn
        {
            running.tools = tools;
        }
    }

    /// The call's own item, journaled under the turn as the model's calls are
    /// and marked as none of the model's: it is the caller's, and the
    /// provider fold skips it (ADR-0036 §2).
    async fn open_call(&mut self, turn: &TurnId, call: &ToolCall) -> ItemId {
        let body = ItemBody::ToolCall {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            output: None,
            progress: None,
            duration_ms: None,
        };
        let mut item = self.fresh(Some(turn.clone()), None, body);
        item.status = ItemStatus::Pending;
        item.completed_at = None;
        item.mark_external();
        let id = item.id.clone();
        self.publish(Event::ItemStarted { item }, None).await;
        id
    }

    /// The gate let it through: the item goes `Running` and carries whatever a
    /// hook rewrote, exactly as a call of the model's own does.
    pub(super) async fn call_allowed(&mut self, item: &ItemId, input: Value) {
        let Some(mut item) = self.item(item) else {
            return;
        };
        item.status = ItemStatus::Running;
        if let ItemBody::ToolCall { input: held, .. } = &mut item.body {
            *held = input;
        }
        self.publish(Event::ItemUpdated { item }, None).await;
    }

    /// What the call came to, folded into its item. The turn it ran under may
    /// already have ended — an `esc` drops a bridged call where it stands —
    /// so this is published on its own account and never as the turn's.
    pub(super) async fn call_finished(&mut self, outcome: ToolOutcome) {
        let Some(mut item) = self.item(&outcome.item) else {
            return;
        };
        item.status = outcome.status;
        item.completed_at = Some(Timestamp::now());
        if let ItemBody::ToolCall {
            output,
            duration_ms,
            progress,
            ..
        } = &mut item.body
        {
            *output = Some(outcome.output);
            *duration_ms = Some(outcome.duration_ms);
            *progress = None;
        }
        self.publish(Event::ItemCompleted { item }, None).await;
    }

    fn item(&self, id: &ItemId) -> Option<Item> {
        self.state.items.iter().find(|item| &item.id == id).cloned()
    }

    // ----- queue -----

    /// The prose a barrier may steer with: up to the first command.
    pub(super) async fn take_queue(&mut self) -> Vec<(IntentId, Input)> {
        let taken = self.queue.take_prose();
        if !taken.is_empty() {
            let changed = self.queue.changed();
            self.publish(changed, None).await;
        }
        taken
    }

    /// An idle session takes the next unit off the queue (ADR-0008 §2).
    pub(super) async fn drain_queue(&mut self) {
        if self.busy() {
            return;
        }
        let Some(unit) = self.queue.take_unit() else {
            return;
        };
        match unit {
            Unit::Prose(inputs) => {
                self.start_turn(inputs, TurnOrigin::Queue, TurnKind::Respond)
                    .await
            }
            Unit::Command(intent, input) => self.run_queued(intent, input).await,
        }
        // Announced after the unit is under way, so no client ever folds an
        // empty queue beside no turn while the next one is about to open.
        let changed = self.queue.changed();
        self.publish(changed, None).await;
    }

    /// A command that waited its turn; the table may have lost it meanwhile.
    async fn run_queued(&mut self, intent: IntentId, input: Input) {
        let parsed = commands::parse(&input);
        let command = match &parsed {
            Some(p) => self.commands.find(&p.name).await,
            None => None,
        };
        match (parsed, command) {
            (Some(parsed), Some(command)) => {
                let invocation = commands::Invocation::of(&input, &parsed);
                self.run_command(intent, invocation, command, parsed.args, true)
                    .await
            }
            _ => {
                self.reject(intent, ErrorCode::InvalidInput, "unknown command")
                    .await
            }
        }
    }
}
