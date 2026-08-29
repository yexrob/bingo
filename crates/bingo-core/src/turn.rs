//! The turn state machine. It knows providers, tools, the policy, hooks and
//! contributors as traits and nothing feature-shaped: no inbox, no
//! reminders, no roster. Those are contributors and hooks a plugin registers.
//!
//! ```text
//! Assembling → Streaming → Deciding → Gating → Executing → Barrier → Assembling
//!                 │  overflow → Compacting → Streaming      └ stop / interrupt → Closing
//!                 └ cancel → Closing
//! ```

mod config;
mod stream;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;

pub use config::{ModelChoice, TurnBudget, TurnConfig};
use stream::Streamed;
pub use stream::{MAX_RETRY_DELAY, MAX_SERVER_RETRY_DELAY, backoff};

use crate::accumulator::Finished;
use crate::context::{ContextView, estimate_tokens, splice_compaction};
use crate::executor::{self, Gate, PendingCall};
use crate::gate::{GateInput, gate_call, hook_applies};
use crate::models::vision;

pub const INTERRUPTED_MARKER: &str = "[Request interrupted by user]";
pub const CONTINUE_PROMPT: &str = "Continue from where you left off.";
pub const MAX_LENGTH_RECOVERIES: u32 = 3;

/// The session actor as the turn sees it: a place to publish, a way to ask, a queue to absorb.
#[async_trait]
pub trait TurnHost: Send + Sync {
    fn emit(&self, event: Event);
    async fn ask(
        &self,
        item: Option<ItemId>,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError>;
    /// Queued inputs eligible for steering, removed from the queue.
    async fn absorb(&self) -> Vec<(IntentId, Input)>;
}

#[derive(Debug)]
pub struct TurnRun {
    pub turn: TurnId,
    /// The durable journal so far; the turn folds its context from it.
    pub history: Vec<Frame>,
    pub generation: u64,
    pub cancel: CancellationToken,
}

#[derive(Debug, PartialEq)]
pub struct TurnOutcome {
    pub status: TurnStatus,
    pub usage: Usage,
}

struct Turn<'a> {
    cfg: &'a TurnConfig,
    host: &'a dyn TurnHost,
    id: TurnId,
    cancel: CancellationToken,
    items: Vec<Item>,
    round: u32,
    retries: u32,
    recoveries: u32,
    empty_retry_used: bool,
    overflow_compacted: bool,
    generation: u64,
    usage: Usage,
    hook_cx: HookContext,
}

enum Step {
    Assembling,
    Closing(TurnStatus),
}

/// What assembling produced: something to send, or a compaction that has to be
/// assembled around before anything is sent.
enum Assembled {
    Request {
        request: ModelRequest,
        usage: ContextUsage,
    },
    Compacted,
}

pub async fn run_turn(cfg: &TurnConfig, run: TurnRun, host: &dyn TurnHost) -> TurnOutcome {
    let items = ContextView::items(&run.history);
    let hook_cx = HookContext {
        session: cfg.session.id.clone(),
        turn: Some(run.turn.clone()),
        cwd: cfg.cwd.clone(),
    };
    let mut turn = Turn {
        cfg,
        host,
        id: run.turn,
        cancel: run.cancel,
        items,
        round: 0,
        retries: 0,
        recoveries: 0,
        empty_retry_used: false,
        overflow_compacted: false,
        generation: run.generation,
        usage: Usage::default(),
        hook_cx,
    };
    for hook in turn.hooks(HookPoint::Turn) {
        hook.on_turn(Phase::Start, &turn.id, &turn.items, &turn.hook_cx)
            .await;
    }
    let mut step = Step::Assembling;
    let status = loop {
        step = match step {
            Step::Assembling => turn.round_trip().await,
            Step::Closing(status) => break status,
        };
    };
    for hook in turn.hooks(HookPoint::Turn) {
        hook.on_turn(Phase::End, &turn.id, &turn.items, &turn.hook_cx)
            .await;
    }
    TurnOutcome {
        status,
        usage: turn.usage,
    }
}

impl Turn<'_> {
    fn hooks(&self, point: HookPoint) -> Vec<Arc<dyn Hook>> {
        self.cfg
            .hooks
            .iter()
            .filter(|h| hook_applies(&h.matcher(), point, None))
            .cloned()
            .collect()
    }

    fn fresh(&self, body: ItemBody, status: ItemStatus) -> Item {
        let now = Timestamp::now();
        Item {
            id: ItemId::mint(),
            turn: Some(self.id.clone()),
            round: self.round,
            status,
            started_at: now,
            completed_at: status.is_terminal().then_some(now),
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn record(&mut self, body: ItemBody) -> ItemId {
        let item = self.fresh(body, ItemStatus::Completed);
        let id = item.id.clone();
        self.items.push(item.clone());
        self.host.emit(Event::ItemCompleted { item });
        id
    }

    fn upsert(&mut self, item: &Item) {
        match self.items.iter_mut().find(|i| i.id == item.id) {
            Some(slot) => *slot = item.clone(),
            None => self.items.push(item.clone()),
        }
    }

    fn user_piece(&mut self, parts: Vec<ContentPart>, surface: String) {
        self.record(ItemBody::User {
            parts,
            origin: Origin {
                surface,
                principal: None,
                conversation: None,
            },
        });
    }

    async fn contribute(
        &mut self,
        want: impl Fn(Placement) -> bool,
        usage: &ContextUsage,
    ) -> Vec<SystemBlock> {
        let mut system = Vec::new();
        let contributors: Vec<Arc<dyn ContextContributor>> = self
            .cfg
            .contributors
            .iter()
            .filter(|c| want(c.placement()))
            .cloned()
            .collect();
        let mut ordered: Vec<(i32, Arc<dyn ContextContributor>)> = contributors
            .into_iter()
            .map(|c| {
                (
                    match c.placement() {
                        Placement::System { order } => order,
                        _ => 0,
                    },
                    c,
                )
            })
            .collect();
        ordered.sort_by_key(|(order, _)| *order);
        for (_, c) in ordered {
            let query = ContextQuery {
                session: &self.cfg.session,
                turn: &self.id,
                round: self.round,
                items: &self.items,
                usage,
                capabilities: &self.cfg.model.capabilities,
                cwd: &self.cfg.cwd,
            };
            let pieces = match c.contribute(query).await {
                Ok(p) => p,
                Err(e) => {
                    self.host.emit(Event::Notice {
                        level: Level::Warn,
                        code: "CONTRIBUTOR_FAILED".into(),
                        text: format!("{}: {e}", c.id()),
                    });
                    continue;
                }
            };
            for piece in pieces {
                match piece {
                    ContextPiece::System(block) => system.push(block),
                    ContextPiece::User { parts, .. } => {
                        self.user_piece(parts, format!("contributor:{}", c.id()))
                    }
                }
            }
        }
        system
    }

    fn tool_specs(&self) -> Vec<ToolSpec> {
        self.cfg.tools.iter().map(|t| t.spec()).collect()
    }

    fn measure(&self, system: &[SystemBlock], messages: &[Message]) -> ContextUsage {
        let used = estimate_tokens(system, messages, &self.tool_specs());
        let window = self.cfg.model.capabilities.context_window;
        let trigger = self
            .cfg
            .compactor
            .as_ref()
            .map(|c| c.threshold(&self.cfg.model.capabilities))
            .unwrap_or(window);
        ContextUsage {
            used,
            window,
            trigger,
        }
    }

    /// One round: assemble, stream, decide, gate, execute, barrier.
    async fn round_trip(&mut self) -> Step {
        if self.cancel.is_cancelled() {
            return self.interrupted();
        }
        if self.round >= self.cfg.budget.max_rounds {
            return Step::Closing(TurnStatus::Failed {
                error: KernelError::new(
                    ErrorCode::TurnBudgetExhausted,
                    format!("{} rounds", self.round),
                ),
            });
        }
        let (request, usage) = match self.assemble().await {
            Assembled::Request { request, usage } => (request, usage),
            Assembled::Compacted => return Step::Assembling,
        };
        let finished = match self.stream(request).await {
            Streamed::Done(f) => f,
            Streamed::Cancelled => return self.interrupted(),
            Streamed::Failed(error, dropped) => {
                return self.failed_stream(error, dropped, usage).await;
            }
        };
        self.account(&finished, usage);
        match self.decide(&finished).await {
            Some(step) => step,
            None => self.act(&finished).await,
        }
    }

    /// Let the contributors speak, then measure. A first round that is already
    /// over the compaction threshold compacts before it sends anything.
    async fn assemble(&mut self) -> Assembled {
        let preliminary = self.measure(&self.cfg.system, &ContextView::fold_items(&self.items));
        let extra_system = self
            .contribute(
                |p| matches!(p, Placement::System { .. } | Placement::RoundStart),
                &preliminary,
            )
            .await;
        let mut system = self.cfg.system.clone();
        system.extend(extra_system);
        let messages = self.without_images(ContextView::fold_items(&self.items));
        let usage = self.measure(&system, &messages);
        if let Some(compactor) = self.cfg.compactor.clone()
            && usage.used >= compactor.threshold(&self.cfg.model.capabilities)
            && self.round == 0
        {
            self.compact(compactor.as_ref(), CompactReason::Threshold, usage)
                .await;
            return Assembled::Compacted;
        }
        Assembled::Request {
            request: ModelRequest {
                model: self.cfg.model.id.clone(),
                max_tokens: self.cfg.model.max_tokens,
                system,
                messages,
                tools: self.tool_specs(),
                reasoning: self.cfg.model.reasoning,
                provider_options: ProviderMetadata::new(),
            },
            usage,
        }
    }

    /// A model without vision never sees an image part; the note stands in on
    /// the wire only, and the items keep the image.
    fn without_images(&self, messages: Vec<Message>) -> Vec<Message> {
        if self.cfg.model.capabilities.images {
            return messages;
        }
        vision::project_images_out(&messages, &vision::omitted_note(&self.cfg.model.id))
            .unwrap_or(messages)
    }

    /// Bill the round to the turn. What the provider counted as input beats
    /// the estimate the assembler made.
    fn account(&mut self, finished: &Finished, usage: ContextUsage) {
        self.usage.add(finished.usage);
        let context = ContextUsage {
            used: finished.usage.input_total().max(usage.used),
            ..usage
        };
        self.host.emit(Event::TurnUsage {
            turn: self.id.clone(),
            usage: finished.usage,
            context,
        });
    }

    /// What the response means when it asks for no tool: `None` means it does,
    /// and the round goes on to act.
    async fn decide(&mut self, finished: &Finished) -> Option<Step> {
        if finished.items.is_empty() {
            // One empty response is a provider hiccup; two is an answer.
            if !self.empty_retry_used {
                self.empty_retry_used = true;
                return Some(Step::Assembling);
            }
            return Some(Step::Closing(TurnStatus::Completed));
        }
        if !finished.tool_calls.is_empty() {
            return None;
        }
        if finished.finish_reason.as_ref().map(|r| r.unified) == Some(UnifiedFinish::Length)
            && self.recoveries < MAX_LENGTH_RECOVERIES
        {
            self.recoveries += 1;
            self.user_piece(vec![ContentPart::text(CONTINUE_PROMPT)], "kernel".into());
            self.round += 1;
            return Some(Step::Assembling);
        }
        Some(self.stop_hooks().await)
    }

    /// A `Stop` hook may push the turn into another round instead of ending it.
    async fn stop_hooks(&mut self) -> Step {
        for hook in self.hooks(HookPoint::Stop) {
            if let HookOutcome::Block { reason } = hook.on_stop(&self.hook_cx).await {
                self.user_piece(
                    vec![ContentPart::text(reason)],
                    format!("hook:{}", hook.id()),
                );
                self.round += 1;
                return Step::Assembling;
            }
        }
        Step::Closing(TurnStatus::Completed)
    }

    /// Gate the calls, run them, then hold the barrier before the next round.
    async fn act(&mut self, finished: &Finished) -> Step {
        let calls = self.gate(finished).await;
        let stop_after = self.execute(calls).await;
        if self.cancel.is_cancelled() {
            return self.interrupted();
        }
        if stop_after {
            return Step::Closing(TurnStatus::Completed);
        }
        self.barrier().await;
        self.round += 1;
        Step::Assembling
    }

    /// The barrier: steering queued during the round joins the transcript, then
    /// the barrier contributors see the round that just happened.
    async fn barrier(&mut self) {
        for (_intent, input) in self.host.absorb().await {
            if let Input::Text { text, origin, .. } = input {
                self.record(ItemBody::User {
                    parts: vec![ContentPart::text(text)],
                    origin,
                });
            }
        }
        let usage = self.measure(&self.cfg.system, &ContextView::fold_items(&self.items));
        let _ = self
            .contribute(|p| matches!(p, Placement::Barrier), &usage)
            .await;
    }

    fn interrupted(&mut self) -> Step {
        self.record(ItemBody::Interruption {
            marker: INTERRUPTED_MARKER.into(),
        });
        Step::Closing(TurnStatus::Interrupted {
            reason: InterruptReason::UserCancel,
        })
    }

    async fn compact(
        &mut self,
        compactor: &dyn Compactor,
        reason: CompactReason,
        usage: ContextUsage,
    ) {
        let started = std::time::Instant::now();
        let cx = CompactContext {
            items: &self.items,
            usage,
            capabilities: &self.cfg.model.capabilities,
            provider: self.cfg.model.provider.clone(),
            model: &self.cfg.model.id,
            cancel: self.cancel.child_token(),
        };
        let compacted = compactor.compact(cx, reason).await;
        match compacted {
            Ok(c) => self.absorb_compaction(c, started.elapsed()),
            Err(e) => self.host.emit(Event::Notice {
                level: Level::Warn,
                code: "COMPACTION_FAILED".into(),
                text: e.to_string(),
            }),
        }
    }

    /// Record the summary, announce the cut, and apply it to the transcript.
    fn absorb_compaction(&mut self, c: Compaction, took: std::time::Duration) {
        let replaced = self
            .items
            .iter()
            .take_while(|i| i.id != c.boundary)
            .filter(|i| !c.kept.contains(&i.id))
            .count() as u32;
        let summary = self.record(ItemBody::Compaction {
            summary: c.summary.clone(),
            replaced,
            before: c.before,
            after: c.after,
            duration_ms: took.as_millis() as u64,
        });
        self.generation += 1;
        self.host.emit(Event::Compacted {
            generation: self.generation,
            boundary: c.boundary.clone(),
            summary: summary.clone(),
            kept: c.kept.clone(),
        });
        let cut = self
            .items
            .iter()
            .position(|i| i.id == c.boundary)
            .unwrap_or(0);
        splice_compaction(&mut self.items, cut, &c.kept, &summary);
    }

    async fn gate(&mut self, finished: &Finished) -> Vec<PendingCall> {
        let mut calls = Vec::with_capacity(finished.tool_calls.len());
        for (item_id, call) in &finished.tool_calls {
            let tool = self
                .cfg
                .tools
                .iter()
                .find(|t| t.spec().name == call.name)
                .cloned();
            let prompter = AskVia {
                host: self.host,
                item: item_id.clone(),
            };
            let gated = gate_call(
                GateInput {
                    session: &self.cfg.session.id,
                    cwd: &self.cfg.cwd,
                    item: item_id,
                    call: call.clone(),
                    tool: tool.clone(),
                    policy: self.cfg.policy.as_ref(),
                    hooks: &self.cfg.hooks,
                    hook_cx: &self.hook_cx,
                },
                &prompter,
            )
            .await;
            if let Some(receipt) = gated.receipt {
                self.record(receipt);
            }
            if gated.gate == Gate::Allowed
                && let Some(item) = self.items.iter_mut().find(|i| &i.id == item_id)
            {
                item.status = ItemStatus::Running;
                if let ItemBody::ToolCall { input, .. } = &mut item.body {
                    *input = gated.call.input.clone();
                }
                self.host.emit(Event::ItemUpdated { item: item.clone() });
            }
            calls.push(PendingCall {
                item: item_id.clone(),
                call: gated.call,
                tool,
                traits: gated.traits,
                gate: gated.gate,
            });
        }
        calls
    }

    /// Returns whether an `after_tool` hook asked to end the turn after this round.
    async fn execute(&mut self, calls: Vec<PendingCall>) -> bool {
        let outcomes = self.run_tools(calls).await;
        self.after_tool_hooks(&outcomes).await
    }

    /// Run the gated calls and fold each result back into the item that made it.
    async fn run_tools(&mut self, calls: Vec<PendingCall>) -> Vec<executor::Outcome> {
        let cfg = self.cfg;
        let session = cfg.session.id.clone();
        let turn = self.id.clone();
        let cancel = self.cancel.clone();
        let host = self.host;
        let mut completed: Vec<Item> = Vec::new();
        let outcomes = executor::execute(
            calls,
            &cancel,
            |pc| ToolContext {
                call_id: pc.call.call_id.clone(),
                session: session.clone(),
                turn: turn.clone(),
                item: pc.item.clone(),
                cwd: cfg.cwd.clone(),
                cancel: cancel.child_token(),
                env: cfg.env.clone(),
                host: cfg.tool_host.clone(),
            },
            |o| {
                if let Some(mut item) = self.items.iter().find(|i| i.id == o.item).cloned() {
                    item.status = o.status;
                    item.completed_at = Some(Timestamp::now());
                    if let ItemBody::ToolCall {
                        output,
                        duration_ms,
                        progress,
                        ..
                    } = &mut item.body
                    {
                        *output = Some(o.output.clone());
                        *duration_ms = Some(o.duration_ms);
                        *progress = None;
                    }
                    host.emit(Event::ItemCompleted { item: item.clone() });
                    completed.push(item);
                }
            },
        )
        .await;
        for item in &completed {
            self.upsert(item);
        }
        outcomes
    }

    /// Let the `AfterTool` hooks see each result; one of them may stop the turn.
    async fn after_tool_hooks(&self, outcomes: &[executor::Outcome]) -> bool {
        let mut stop_after = false;
        for o in outcomes {
            let Some(item) = self.items.iter().find(|i| i.id == o.item) else {
                continue;
            };
            let ItemBody::ToolCall {
                call_id,
                name,
                input,
                ..
            } = &item.body
            else {
                continue;
            };
            let call = ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                input: input.clone(),
            };
            for hook in self
                .cfg
                .hooks
                .iter()
                .filter(|h| hook_applies(&h.matcher(), HookPoint::AfterTool, Some(name)))
            {
                if let HookOutcome::Block { .. } =
                    hook.after_tool(&call, &o.output, &self.hook_cx).await
                {
                    stop_after = true;
                }
            }
        }
        stop_after
    }
}

/// The permission prompter a gate uses: it opens the interaction on the turn host.
struct AskVia<'a> {
    host: &'a dyn TurnHost,
    item: ItemId,
}

#[async_trait]
impl Prompter for AskVia<'_> {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.host.ask(Some(self.item.clone()), kind, answers).await
    }
}

#[cfg(test)]
mod tests;
