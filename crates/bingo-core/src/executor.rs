//! Runs gated tool calls: consecutive concurrency-safe calls in parallel,
//! everything else one at a time; an interrupt drops `Cancel` tools, lets
//! `Block` tools finish, and keeps every completed result.

use std::sync::Arc;
use std::time::Instant;

use bingo_sdk::*;
use futures::future::join_all;

pub const MAX_CONCURRENCY: usize = 10;
pub const MAX_RESULT_CHARS: usize = 50_000;
pub const INTERRUPTED_MARKER: &str = "[Request interrupted by user for tool use]";

/// Whether the gate let the call through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Allowed,
    Denied { message: String },
}

pub struct PendingCall {
    pub item: ItemId,
    pub call: ToolCall,
    pub tool: Option<Arc<dyn Tool>>,
    pub traits: ToolTraits,
    pub gate: Gate,
}

impl std::fmt::Debug for PendingCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingCall")
            .field("item", &self.item)
            .field("call", &self.call)
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub item: ItemId,
    pub call_id: String,
    pub output: ToolOutput,
    pub status: ItemStatus,
    pub duration_ms: u64,
}

/// Execute in order. `context` builds the per-call context; `on_done` sees
/// each outcome as it lands. The returned list is in input order.
pub async fn execute<C, F>(
    calls: Vec<PendingCall>,
    cancel: &CancellationToken,
    mut context: C,
    mut on_done: F,
) -> Vec<Outcome>
where
    C: FnMut(&PendingCall) -> ToolContext,
    F: FnMut(&Outcome),
{
    let mut outcomes = Vec::with_capacity(calls.len());
    let mut i = 0;
    while i < calls.len() {
        if cancel.is_cancelled() {
            for pc in &calls[i..] {
                let o = interrupted(pc);
                on_done(&o);
                outcomes.push(o);
            }
            break;
        }
        let safe = calls[i].traits.concurrency_safe && calls[i].gate == Gate::Allowed;
        let end = if safe {
            let mut j = i + 1;
            while j < calls.len()
                && j - i < MAX_CONCURRENCY
                && calls[j].traits.concurrency_safe
                && calls[j].gate == Gate::Allowed
            {
                j += 1;
            }
            j
        } else {
            i + 1
        };
        let batch = &calls[i..end];
        let contexts: Vec<ToolContext> = batch.iter().map(&mut context).collect();
        let futs = batch
            .iter()
            .zip(contexts)
            .map(|(pc, cx)| run_one(pc, cx, cancel));
        for o in join_all(futs).await {
            on_done(&o);
            outcomes.push(o);
        }
        i = end;
    }
    outcomes
}

fn interrupted(pc: &PendingCall) -> Outcome {
    Outcome {
        item: pc.item.clone(),
        call_id: pc.call.call_id.clone(),
        output: ToolOutput::error(INTERRUPTED_MARKER),
        status: ItemStatus::Interrupted,
        duration_ms: 0,
    }
}

async fn run_one(pc: &PendingCall, cx: ToolContext, cancel: &CancellationToken) -> Outcome {
    let started = Instant::now();
    let finish = |output: ToolOutput, status: ItemStatus| Outcome {
        item: pc.item.clone(),
        call_id: pc.call.call_id.clone(),
        output,
        status,
        duration_ms: started.elapsed().as_millis() as u64,
    };
    if let Gate::Denied { message } = &pc.gate {
        return finish(ToolOutput::error(message.clone()), ItemStatus::Failed);
    }
    let Some(tool) = &pc.tool else {
        return finish(
            ToolOutput::error(format!("tool not found: {}", pc.call.name)),
            ItemStatus::Failed,
        );
    };
    let result = match pc.traits.interrupt {
        Interrupt::Block => tool.call(pc.call.input.clone(), &cx).await,
        Interrupt::Cancel => tokio::select! {
            r = tool.call(pc.call.input.clone(), &cx) => r,
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
        },
    };
    match result {
        Ok(mut output) => {
            if pc.traits.result_limit == ResultLimit::Global {
                clip(&mut output);
            }
            let status = if output.is_error {
                ItemStatus::Failed
            } else {
                ItemStatus::Completed
            };
            finish(output, status)
        }
        Err(ToolError::Cancelled) => finish(
            ToolOutput::error(INTERRUPTED_MARKER),
            ItemStatus::Interrupted,
        ),
        Err(e) => finish(ToolOutput::error(e.to_string()), ItemStatus::Failed),
    }
}

fn clip(output: &mut ToolOutput) {
    let mut budget = MAX_RESULT_CHARS;
    for part in &mut output.parts {
        if let ContentPart::Text { text } = part {
            let n = text.chars().count();
            if n > budget {
                let keep: String = text.chars().take(budget).collect();
                *text = format!("{keep}\n[truncated: {} more characters]", n - budget);
                budget = 0;
            } else {
                budget -= n;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use tokio::sync::Barrier;

    use super::*;

    struct Echo {
        safe: bool,
        delay_ms: u64,
        interrupt: Interrupt,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "Echo".into(),
                description: String::new(),
                input_schema: json!({}),
                meta: Default::default(),
            }
        }
        fn traits(&self, _: &Value) -> ToolTraits {
            ToolTraits {
                concurrency_safe: self.safe,
                interrupt: self.interrupt,
                trusted: true,
                ..ToolTraits::default()
            }
        }
        async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.log.lock().unwrap().push(input["v"].to_string());
            Ok(ToolOutput::text(input["v"].to_string()))
        }
    }

    /// A call that finishes only if another one is running beside it: it
    /// announces its start, then waits for its partner to reach the same
    /// barrier. Two of these complete when they overlap and fail when they do
    /// not, so the pin needs no wall clock — a serialized batch leaves the
    /// first one waiting until the bound below expires.
    struct Rendezvous {
        meet: Arc<Barrier>,
        log: Arc<Mutex<Vec<String>>>,
    }

    /// Long enough that no load can starve a batch that truly runs together,
    /// short enough that a serialized one fails in reasonable time.
    const MEET_BOUND: Duration = Duration::from_secs(10);

    #[async_trait]
    impl Tool for Rendezvous {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "Rendezvous".into(),
                description: String::new(),
                input_schema: json!({}),
                meta: Default::default(),
            }
        }
        fn traits(&self, _: &Value) -> ToolTraits {
            ToolTraits {
                concurrency_safe: true,
                interrupt: Interrupt::Cancel,
                trusted: true,
                ..ToolTraits::default()
            }
        }
        async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
            self.log.lock().unwrap().push(input["v"].to_string());
            match tokio::time::timeout(MEET_BOUND, self.meet.wait()).await {
                Ok(_) => Ok(ToolOutput::text(input["v"].to_string())),
                Err(_) => Ok(ToolOutput::error("nothing else was running")),
            }
        }
    }

    fn rendezvous(log: &Arc<Mutex<Vec<String>>>) -> impl Fn(i32) -> PendingCall {
        let meet = Arc::new(Barrier::new(2));
        let log = log.clone();
        move |v| {
            let tool = Arc::new(Rendezvous {
                meet: meet.clone(),
                log: log.clone(),
            });
            let traits = tool.traits(&json!({}));
            PendingCall {
                item: ItemId::from_raw(format!("i{v}")),
                call: ToolCall {
                    call_id: format!("c{v}"),
                    name: "Rendezvous".into(),
                    input: json!({ "v": v }),
                },
                tool: Some(tool),
                traits,
                gate: Gate::Allowed,
            }
        }
    }

    struct NoHost;
    #[async_trait]
    impl Prompter for NoHost {
        async fn ask(&self, _: InteractionKind, _: Vec<AnswerSpec>) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }
    #[async_trait]
    impl ToolHost for NoHost {
        fn progress(&self, _: &ItemId, _: String) {}
        async fn record(&self, _: ItemBody) -> Result<ItemId, KernelError> {
            Ok(ItemId::mint())
        }
    }

    fn cx(cancel: &CancellationToken) -> ToolContext {
        ToolContext {
            call_id: "c".into(),
            session: SessionId::from_raw("s"),
            turn: TurnId::from_raw("t"),
            item: ItemId::from_raw("i"),
            cwd: PathBuf::from("/tmp"),
            cancel: cancel.child_token(),
            env: Arc::new(Env {
                home: "/tmp".into(),
                config_dir: "/tmp".into(),
                data_dir: "/tmp".into(),
            }),
            host: bingo_sdk::testing::NoHost::handle(),
            call: Arc::new(NoHost),
        }
    }

    fn pending(v: i32, tool: Arc<Echo>, gate: Gate) -> PendingCall {
        let traits = tool.traits(&json!({}));
        PendingCall {
            item: ItemId::from_raw(format!("i{v}")),
            call: ToolCall {
                call_id: format!("c{v}"),
                name: "Echo".into(),
                input: json!({"v": v}),
            },
            tool: Some(tool),
            traits,
            gate,
        }
    }

    #[tokio::test]
    async fn results_come_back_in_input_order_whichever_finishes_first() {
        let log = Arc::new(Mutex::new(vec![]));
        let slow = Arc::new(Echo {
            safe: true,
            delay_ms: 30,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let fast = Arc::new(Echo {
            safe: true,
            delay_ms: 1,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        let out = execute(
            vec![
                pending(1, slow, Gate::Allowed),
                pending(2, fast, Gate::Allowed),
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(
            out.iter().map(|o| o.call_id.as_str()).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["2", "1"],
            "fast one finished first"
        );
    }

    #[tokio::test]
    async fn two_safe_allowed_calls_are_in_flight_at_the_same_moment() {
        let log = Arc::new(Mutex::new(vec![]));
        let call = rendezvous(&log);
        let cancel = CancellationToken::new();
        let out = execute(vec![call(1), call(2)], &cancel, |_| cx(&cancel), |_| {}).await;
        assert!(
            out.iter().all(|o| o.status == ItemStatus::Completed),
            "each call only finishes once it has met the other: {out:?}"
        );
        assert_eq!(log.lock().unwrap().len(), 2, "both calls started");
    }

    #[tokio::test]
    async fn a_call_that_cannot_run_together_splits_the_batch_around_it() {
        let log = Arc::new(Mutex::new(vec![]));
        let safe = |delay_ms| {
            Arc::new(Echo {
                safe: true,
                delay_ms,
                interrupt: Interrupt::Cancel,
                log: log.clone(),
            })
        };
        let alone = Arc::new(Echo {
            safe: false,
            delay_ms: 1,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        execute(
            vec![
                pending(1, safe(30), Gate::Allowed),
                pending(2, alone, Gate::Allowed),
                pending(3, safe(1), Gate::Allowed),
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["1", "2", "3"],
            "three groups, each waiting for the one before"
        );
    }

    #[tokio::test]
    async fn a_denied_call_splits_the_batch_around_it_too() {
        let log = Arc::new(Mutex::new(vec![]));
        let safe = |delay_ms| {
            Arc::new(Echo {
                safe: true,
                delay_ms,
                interrupt: Interrupt::Cancel,
                log: log.clone(),
            })
        };
        let cancel = CancellationToken::new();
        execute(
            vec![
                pending(1, safe(30), Gate::Allowed),
                pending(
                    2,
                    safe(1),
                    Gate::Denied {
                        message: "no".into(),
                    },
                ),
                pending(3, safe(1), Gate::Allowed),
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(log.lock().unwrap().as_slice(), ["1", "3"]);
    }

    #[tokio::test]
    async fn an_interrupt_before_a_batch_runs_none_of_it() {
        let log = Arc::new(Mutex::new(vec![]));
        let call = rendezvous(&log);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = execute(vec![call(1), call(2)], &cancel, |_| cx(&cancel), |_| {}).await;
        assert!(out.iter().all(|o| o.status == ItemStatus::Interrupted));
        assert_eq!(
            out[0].output.parts[0].as_text(),
            Some(INTERRUPTED_MARKER),
            "each one says why"
        );
        assert!(log.lock().unwrap().is_empty(), "neither call started");
    }

    #[tokio::test]
    async fn an_interrupt_keeps_what_a_parallel_batch_already_finished() {
        let log = Arc::new(Mutex::new(vec![]));
        let call = rendezvous(&log);
        let after = Arc::new(Echo {
            safe: false,
            delay_ms: 1,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        let landed = Mutex::new(0);
        let out = execute(
            vec![call(1), call(2), pending(3, after, Gate::Allowed)],
            &cancel,
            |_| cx(&cancel),
            |_| {
                let mut n = landed.lock().unwrap();
                *n += 1;
                if *n == 2 {
                    cancel.cancel();
                }
            },
        )
        .await;
        assert_eq!(out[0].status, ItemStatus::Completed);
        assert_eq!(out[1].status, ItemStatus::Completed, "both met and kept");
        assert_eq!(out[2].status, ItemStatus::Interrupted);
        assert_eq!(log.lock().unwrap().len(), 2, "the third never started");
    }

    #[tokio::test]
    async fn unsafe_calls_run_one_at_a_time() {
        let log = Arc::new(Mutex::new(vec![]));
        let a = Arc::new(Echo {
            safe: false,
            delay_ms: 10,
            interrupt: Interrupt::Block,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        execute(
            vec![
                pending(1, a.clone(), Gate::Allowed),
                pending(2, a, Gate::Allowed),
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(log.lock().unwrap().as_slice(), ["1", "2"]);
    }

    #[tokio::test]
    async fn a_denied_call_fails_without_running_and_a_missing_tool_fails_too() {
        let log = Arc::new(Mutex::new(vec![]));
        let a = Arc::new(Echo {
            safe: true,
            delay_ms: 0,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        let mut missing = pending(2, a.clone(), Gate::Allowed);
        missing.tool = None;
        let out = execute(
            vec![
                pending(
                    1,
                    a,
                    Gate::Denied {
                        message: "no".into(),
                    },
                ),
                missing,
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(out[0].status, ItemStatus::Failed);
        assert!(out[0].output.is_error);
        assert_eq!(out[1].status, ItemStatus::Failed);
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_interrupt_cancels_cancel_tools_blocks_on_block_tools_and_skips_the_rest() {
        let log = Arc::new(Mutex::new(vec![]));
        let block = Arc::new(Echo {
            safe: false,
            delay_ms: 40,
            interrupt: Interrupt::Block,
            log: log.clone(),
        });
        let cancellable = Arc::new(Echo {
            safe: false,
            delay_ms: 40,
            interrupt: Interrupt::Cancel,
            log: log.clone(),
        });
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            c2.cancel();
        });
        let out = execute(
            vec![
                pending(1, block, Gate::Allowed),
                pending(2, cancellable.clone(), Gate::Allowed),
                pending(3, cancellable, Gate::Allowed),
            ],
            &cancel,
            |_| cx(&cancel),
            |_| {},
        )
        .await;
        assert_eq!(
            out[0].status,
            ItemStatus::Completed,
            "a Block tool finishes"
        );
        assert_eq!(
            out[1].status,
            ItemStatus::Interrupted,
            "nothing after the interrupt runs"
        );
        assert_eq!(out[2].status, ItemStatus::Interrupted);
        assert_eq!(log.lock().unwrap().as_slice(), ["1"]);
    }

    #[test]
    fn results_are_clipped_at_the_global_cap() {
        let mut out = ToolOutput::text("x".repeat(MAX_RESULT_CHARS + 5));
        clip(&mut out);
        let text = out.parts[0].as_text().unwrap();
        assert!(text.ends_with("[truncated: 5 more characters]"));
    }
}
