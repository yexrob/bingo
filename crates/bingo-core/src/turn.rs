//! The turn state machine. It knows providers, tools, the policy, hooks and
//! contributors as traits and nothing feature-shaped: no inbox, no
//! reminders, no roster. Those are contributors and hooks a plugin registers.
//!
//! ```text
//! Assembling → Streaming → Deciding → Gating → Executing → Barrier → Assembling
//!                 │  overflow → Compacting → Streaming      └ stop / interrupt → Closing
//!                 └ cancel → Closing
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::*;
use futures::StreamExt;
use jiff::Timestamp;

use crate::accumulator::{Accumulator, Emit, Finished};
use crate::context::{ContextView, estimate_tokens};
use crate::executor::{self, Gate, PendingCall};
use crate::gate::{GateInput, gate_call, hook_applies, summarize};

pub const INTERRUPTED_MARKER: &str = "[Request interrupted by user]";
pub const CONTINUE_PROMPT: &str = "Continue from where you left off.";
pub const MAX_LENGTH_RECOVERIES: u32 = 3;
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(32);
pub const MAX_SERVER_RETRY_DELAY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnBudget {
    pub max_rounds: u32,
    pub max_retries: u32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_rounds: 100,
            max_retries: 10,
        }
    }
}

/// Everything a turn reads. Built by the host per session; plugins are already resolved.
pub struct TurnConfig {
    pub session: SessionSummary,
    pub cwd: PathBuf,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub capabilities: ModelCapabilities,
    pub max_tokens: u32,
    pub reasoning: Option<Effort>,
    pub system: Vec<SystemBlock>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub policy: Arc<dyn PermissionPolicy>,
    pub hooks: Vec<Arc<dyn Hook>>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub compactor: Option<Arc<dyn Compactor>>,
    pub budget: TurnBudget,
    pub env: Arc<Env>,
    pub tool_host: Arc<dyn ToolHost>,
}

impl std::fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnConfig")
            .field("session", &self.session.id)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

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

enum Streamed {
    Done(Finished),
    Failed(ProviderError, Vec<ItemId>),
    Cancelled,
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
                capabilities: &self.cfg.capabilities,
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
        let window = self.cfg.capabilities.context_window;
        let trigger = self
            .cfg
            .compactor
            .as_ref()
            .map(|c| c.threshold(&self.cfg.capabilities))
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
        let preliminary = self.measure(&self.cfg.system, &ContextView::fold_items(&self.items));
        let extra_system = self
            .contribute(
                |p| matches!(p, Placement::System { .. } | Placement::RoundStart),
                &preliminary,
            )
            .await;
        let mut system = self.cfg.system.clone();
        system.extend(extra_system);
        let messages = ContextView::fold_items(&self.items);
        let usage = self.measure(&system, &messages);
        if let Some(compactor) = self.cfg.compactor.clone()
            && usage.used >= compactor.threshold(&self.cfg.capabilities)
            && self.round == 0
        {
            self.compact(compactor.as_ref(), CompactReason::Threshold, usage)
                .await;
            return Step::Assembling;
        }
        let request = ModelRequest {
            model: self.cfg.model.clone(),
            max_tokens: self.cfg.max_tokens,
            system,
            messages,
            tools: self.tool_specs(),
            reasoning: self.cfg.reasoning,
            provider_options: ProviderMetadata::new(),
        };
        let finished = match self.stream(request).await {
            Streamed::Done(f) => f,
            Streamed::Cancelled => return self.interrupted(),
            Streamed::Failed(error, dropped) => {
                return self.failed_stream(error, dropped, usage).await;
            }
        };
        self.usage.add(finished.usage);
        let context = ContextUsage {
            used: finished.usage.input_tokens.max(usage.used),
            ..usage
        };
        self.host.emit(Event::TurnUsage {
            turn: self.id.clone(),
            usage: finished.usage,
            context,
        });
        if finished.items.is_empty() {
            if !self.empty_retry_used {
                self.empty_retry_used = true;
                return Step::Assembling;
            }
            return Step::Closing(TurnStatus::Completed);
        }
        if finished.tool_calls.is_empty() {
            if finished.finish_reason.as_ref().map(|r| r.unified) == Some(UnifiedFinish::Length)
                && self.recoveries < MAX_LENGTH_RECOVERIES
            {
                self.recoveries += 1;
                self.user_piece(vec![ContentPart::text(CONTINUE_PROMPT)], "kernel".into());
                self.round += 1;
                return Step::Assembling;
            }
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
            return Step::Closing(TurnStatus::Completed);
        }
        let calls = self.gate(&finished).await;
        let stop_after = self.execute(calls).await;
        if self.cancel.is_cancelled() {
            return self.interrupted();
        }
        if stop_after {
            return Step::Closing(TurnStatus::Completed);
        }
        let absorbed = self.host.absorb().await;
        for (_intent, input) in absorbed {
            if let Input::Text { text, origin, .. } = input {
                self.record(ItemBody::User {
                    parts: vec![ContentPart::text(text)],
                    origin,
                });
            }
        }
        let usage_now = self.measure(&self.cfg.system, &ContextView::fold_items(&self.items));
        let _ = self
            .contribute(|p| matches!(p, Placement::Barrier), &usage_now)
            .await;
        self.round += 1;
        Step::Assembling
    }

    fn interrupted(&mut self) -> Step {
        self.record(ItemBody::Interruption {
            marker: INTERRUPTED_MARKER.into(),
        });
        Step::Closing(TurnStatus::Interrupted {
            reason: InterruptReason::UserCancel,
        })
    }

    async fn stream(&mut self, request: ModelRequest) -> Streamed {
        let mut stream = match self
            .cfg
            .provider
            .stream(request, self.cancel.child_token())
            .await
        {
            Ok(s) => s,
            Err(e) => return Streamed::Failed(e, Vec::new()),
        };
        let mut acc = Accumulator::new(self.id.clone(), self.round);
        let mut cancelled = false;
        let mut error: Option<ProviderError> = None;
        loop {
            tokio::select! {
                next = stream.next() => match next {
                    Some(Ok(event)) => {
                        for emit in acc.push(event) {
                            self.publish(emit);
                        }
                    }
                    Some(Err(e)) => { error = Some(e); break; }
                    None => break,
                },
                _ = self.cancel.cancelled() => { cancelled = true; break; }
            }
        }
        let dropped = acc.item_ids();
        let (emits, mut finished) = acc.finish(cancelled);
        for emit in emits {
            self.publish(emit);
        }
        if cancelled {
            return Streamed::Cancelled;
        }
        if let Some(e) = error.or_else(|| finished.error.take()) {
            return Streamed::Failed(e, dropped);
        }
        Streamed::Done(finished)
    }

    fn publish(&mut self, emit: Emit) {
        match emit {
            Emit::Started(item) => {
                self.upsert(&item);
                self.host.emit(Event::ItemStarted { item });
            }
            Emit::Delta {
                item,
                n,
                kind,
                data,
            } => self.host.emit(Event::ItemDelta {
                item,
                n,
                kind,
                data,
            }),
            Emit::Completed(item) => {
                self.upsert(&item);
                self.host.emit(Event::ItemCompleted { item });
            }
        }
    }

    async fn failed_stream(
        &mut self,
        error: ProviderError,
        dropped: Vec<ItemId>,
        usage: ContextUsage,
    ) -> Step {
        self.items.retain(|i| !dropped.contains(&i.id));
        if let ProviderError::ContextOverflow { .. } = &error
            && let Some(compactor) = self.cfg.compactor.clone()
            && !self.overflow_compacted
        {
            self.overflow_compacted = true;
            self.host.emit(Event::TurnRetrying {
                turn: self.id.clone(),
                attempt: self.retries,
                max: self.cfg.budget.max_retries,
                delay_ms: 0,
                dropped,
                reason: error.to_string(),
            });
            self.compact(
                compactor.as_ref(),
                CompactReason::Overflow {
                    message: error.to_string(),
                },
                usage,
            )
            .await;
            return Step::Assembling;
        }
        if error.retryable() && self.retries < self.cfg.budget.max_retries {
            self.retries += 1;
            let delay = backoff(self.retries, error.retry_after_ms());
            self.host.emit(Event::TurnRetrying {
                turn: self.id.clone(),
                attempt: self.retries,
                max: self.cfg.budget.max_retries,
                delay_ms: delay.as_millis() as u64,
                dropped,
                reason: error.to_string(),
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = self.cancel.cancelled() => return self.interrupted(),
            }
            return Step::Assembling;
        }
        Step::Closing(TurnStatus::Failed {
            error: KernelError::new(error.code(), error.to_string()),
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
            capabilities: &self.cfg.capabilities,
            provider: self.cfg.provider.clone(),
            model: &self.cfg.model,
            cancel: self.cancel.child_token(),
        };
        match compactor.compact(cx, reason).await {
            Ok(c) => {
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
                    duration_ms: started.elapsed().as_millis() as u64,
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
                let summary_item = self
                    .items
                    .iter()
                    .position(|i| i.id == summary)
                    .map(|p| self.items.remove(p));
                let cut = cut.min(self.items.len());
                let (head, tail) = self.items.split_at(cut);
                let mut next: Vec<Item> = head
                    .iter()
                    .filter(|i| c.kept.contains(&i.id))
                    .cloned()
                    .collect();
                next.extend(summary_item);
                next.extend(tail.iter().cloned());
                self.items = next;
            }
            Err(e) => self.host.emit(Event::Notice {
                level: Level::Warn,
                code: "COMPACTION_FAILED".into(),
                text: e.to_string(),
            }),
        }
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
        let cfg = self.cfg;
        let session = cfg.session.id.clone();
        let turn = self.id.clone();
        let cancel = self.cancel.clone();
        let started: std::collections::HashMap<ItemId, Timestamp> = self
            .items
            .iter()
            .map(|i| (i.id.clone(), i.started_at))
            .collect();
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
                    let _ = started.get(&o.item);
                    host.emit(Event::ItemCompleted { item: item.clone() });
                    completed.push(item);
                }
            },
        )
        .await;
        for item in &completed {
            self.upsert(item);
        }
        let mut stop_after = false;
        for o in &outcomes {
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

/// 500 ms doubling, capped at 32 s; a server-stated delay wins, capped at 60 s.
pub fn backoff(attempt: u32, retry_after_ms: Option<u64>) -> Duration {
    if let Some(ms) = retry_after_ms {
        return Duration::from_millis(ms).min(MAX_SERVER_RETRY_DELAY);
    }
    let exp = attempt.saturating_sub(1).min(6);
    Duration::from_millis(500 * (1u64 << exp)).min(MAX_RETRY_DELAY)
}

pub fn summarize_call(call: &ToolCall) -> String {
    summarize(call)
}

#[cfg(test)]
mod tests;
