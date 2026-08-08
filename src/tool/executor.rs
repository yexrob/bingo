use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::sync::watch;

use super::{Tool, ToolContext, ToolError, ToolResult};

pub const MAX_CONCURRENCY: usize = 10;

/// Wait for the next "cancel" signal.
///
/// Two must-haves (otherwise the select branches idle-spin or misfire):
/// 1. Call `borrow_and_update` first to clear the version, so `changed()` only becomes ready
///    on a genuinely new signal — otherwise a receiver subscribed after `send_replace` is
///    immediately ready, select picks a branch at random, and roughly half the turns would
///    drop the already-sent request and resend it;
/// 2. After the sender is dropped, `changed()` keeps returning Err immediately: treat it as
///    "no longer cancellable" and park, never let the caller keep looping to rebuild work.
pub(crate) async fn cancel_requested(cancel: &mut watch::Receiver<bool>) -> bool {
    loop {
        if cancel.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        if *cancel.borrow_and_update() {
            return true;
        }
    }
}

/// Tool calls pending execution in a turn (already past the permission gate).
pub struct PendingCall<'a> {
    pub tool_use_id: String,
    pub tool: &'a dyn Tool,
    pub input: serde_json::Value,
}

pub struct ExecOutcome {
    pub tool_use_id: String,
    pub result: Result<ToolResult, ToolError>,
    /// Tool execution duration in milliseconds.
    pub duration_ms: u64,
}

/// Execution queue (D7):
/// consecutive concurrency-safe tools run in parallel batches (capped at MAX_CONCURRENCY),
/// non-safe tools run serially on their own; results keep insertion order.
/// cancel: when Some, each batch races the interruption signal — on arrival, stop immediately
/// (in-flight futures are cancelled by dropping), completed results are kept and returned;
/// the caller discards the whole turn (no backfill).
pub async fn execute_calls<'a>(
    calls: Vec<PendingCall<'a>>,
    ctx: &ToolContext,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> (Vec<ExecOutcome>, bool) {
    let mut outcomes = Vec::with_capacity(calls.len());
    let mut rest = calls.as_slice();
    while !rest.is_empty() {
        let safe_count = rest
            .iter()
            .take_while(|c| c.tool.is_concurrency_safe(&c.input))
            .count();
        if safe_count > 0 {
            let batch = &rest[..safe_count.min(MAX_CONCURRENCY)];
            let executed = run_batch(batch, ctx, cancel.as_deref_mut()).await;
            outcomes.extend(executed.0);
            if executed.1 {
                return (outcomes, true);
            }
            rest = &rest[batch.len()..];
        } else {
            let head = &rest[0];
            let start = std::time::Instant::now();
            let result = call_or_cancel(head, ctx, cancel.as_deref_mut()).await;
            match result {
                Some(result) => {
                    outcomes.push(ExecOutcome {
                        tool_use_id: head.tool_use_id.clone(),
                        result,
                        duration_ms: start.elapsed().as_millis() as u64,
                    });
                }
                None => return (outcomes, true),
            }
            rest = &rest[1..];
        }
    }
    (outcomes, false)
}

/// Run a batch in parallel; on cancel hit returns `(completed results, interrupted)`.
/// Collect one by one (FuturesUnordered) rather than join_all: on interruption, tools that
/// already finished have side effects that happened, so results must be kept, otherwise
/// the UI reporting "interrupted" would mislead the user. Results keep insertion order.
async fn run_batch<'a>(
    batch: &[PendingCall<'a>],
    ctx: &ToolContext,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> (Vec<ExecOutcome>, bool) {
    let mut running: FuturesUnordered<_> = batch
        .iter()
        .enumerate()
        .map(|(index, call)| async move {
            let start = std::time::Instant::now();
            let result = call.tool.call(call.input.clone(), ctx).await;
            (index, result, start.elapsed().as_millis() as u64)
        })
        .collect();

    let mut done: Vec<Option<ExecOutcome>> = (0..batch.len()).map(|_| None).collect();
    let mut aborted = false;
    // Acknowledge an already-set signal while clearing the version: a cancel arriving between
    // batches must not be swallowed by borrow_and_update.
    if let Some(cancel) = cancel.as_deref_mut()
        && *cancel.borrow_and_update()
    {
        return (Vec::new(), true);
    }
    loop {
        let finished = match cancel.as_deref_mut() {
            Some(cancel) => tokio::select! {
                finished = running.next() => finished,
                _ = cancel_requested(cancel) => {
                    aborted = true;
                    break;
                }
            },
            None => running.next().await,
        };
        let Some((index, result, duration_ms)) = finished else {
            break;
        };
        done[index] = Some(ExecOutcome {
            tool_use_id: batch[index].tool_use_id.clone(),
            result,
            duration_ms,
        });
    }
    (done.into_iter().flatten().collect(), aborted)
}

/// Run a single tool; on cancel hit returns None (interrupted).
async fn call_or_cancel<'a>(
    call: &PendingCall<'a>,
    ctx: &ToolContext,
    cancel: Option<&mut watch::Receiver<bool>>,
) -> Option<Result<ToolResult, ToolError>> {
    let fut = call.tool.call(call.input.clone(), ctx);
    futures_util::pin_mut!(fut);
    match cancel {
        Some(cancel) => {
            // Acknowledge an already-set signal while clearing the version (see the same note in run_batch).
            if *cancel.borrow_and_update() {
                return None;
            }
            tokio::select! {
                result = &mut fut => Some(result),
                _ = cancel_requested(cancel) => None,
            }
        }
        None => Some(fut.await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct FakeTool {
        name: &'static str,
        safe: bool,
        delay_ms: u64,
        counter: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
        running: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> String {
            self.name.to_string()
        }
        fn description(&self) -> String {
            String::new()
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
            self.safe
        }
        fn is_read_only(&self, _input: &serde_json::Value) -> bool {
            self.safe
        }
        async fn call(
            &self,
            _input: serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                content: serde_json::Value::Null,
                is_error: false,
                diff: None,
            })
        }
    }

    #[tokio::test]
    async fn safe_calls_run_in_parallel() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let tool = FakeTool {
            name: "read",
            safe: true,
            delay_ms: 50,
            counter: counter.clone(),
            max_seen: max_seen.clone(),
            running,
        };
        let calls: Vec<PendingCall> = (0..5)
            .map(|i| PendingCall {
                tool_use_id: format!("tu_{i}"),
                tool: &tool,
                input: serde_json::json!({}),
            })
            .collect();

        let start = Instant::now();
        let (outcomes, _interrupted) = execute_calls(
            calls,
            &ToolContext {
                home: std::env::temp_dir(),
                cwd: Default::default(),
                watch: crate::watch::WatchRegistry::new(),
                http: reqwest::Client::new(),
                tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                    &std::env::temp_dir(),
                    "test",
                )),
                hooks: Default::default(),
                permission_mode: "default".into(),
                expand_tasks: tokio::sync::watch::channel(false).0,
                ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            },
            None,
        )
        .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(250),
            "not parallel: {elapsed:?}"
        );
        assert_eq!(outcomes.len(), 5);
        assert_eq!(counter.load(Ordering::SeqCst), 5);
        assert!(max_seen.load(Ordering::SeqCst) >= 2, "never overlapped");
    }

    #[tokio::test]
    async fn unsafe_calls_are_serial() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let tool = FakeTool {
            name: "bash",
            safe: false,
            delay_ms: 30,
            counter: counter.clone(),
            max_seen: max_seen.clone(),
            running,
        };
        let calls: Vec<PendingCall> = (0..3)
            .map(|i| PendingCall {
                tool_use_id: format!("tu_{i}"),
                tool: &tool,
                input: serde_json::json!({}),
            })
            .collect();

        let start = Instant::now();
        let (outcomes, _interrupted) = execute_calls(
            calls,
            &ToolContext {
                home: std::env::temp_dir(),
                cwd: Default::default(),
                watch: crate::watch::WatchRegistry::new(),
                http: reqwest::Client::new(),
                tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                    &std::env::temp_dir(),
                    "test",
                )),
                hooks: Default::default(),
                permission_mode: "default".into(),
                expand_tasks: tokio::sync::watch::channel(false).0,
                ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            },
            None,
        )
        .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(90),
            "not serial: {elapsed:?}"
        );
        assert_eq!(outcomes.len(), 3);
        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "overlapped");
    }

    #[tokio::test]
    async fn mixed_batches_preserve_order() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let read = FakeTool {
            name: "read",
            safe: true,
            delay_ms: 10,
            counter: counter.clone(),
            max_seen: max_seen.clone(),
            running: running.clone(),
        };
        let bash = FakeTool {
            name: "bash",
            safe: false,
            delay_ms: 10,
            counter: counter.clone(),
            max_seen: max_seen.clone(),
            running,
        };
        let calls = vec![
            PendingCall {
                tool_use_id: "r1".into(),
                tool: &read,
                input: serde_json::json!({}),
            },
            PendingCall {
                tool_use_id: "r2".into(),
                tool: &read,
                input: serde_json::json!({}),
            },
            PendingCall {
                tool_use_id: "b1".into(),
                tool: &bash,
                input: serde_json::json!({}),
            },
            PendingCall {
                tool_use_id: "r3".into(),
                tool: &read,
                input: serde_json::json!({}),
            },
        ];

        let (outcomes, _interrupted) = execute_calls(
            calls,
            &ToolContext {
                home: std::env::temp_dir(),
                cwd: Default::default(),
                watch: crate::watch::WatchRegistry::new(),
                http: reqwest::Client::new(),
                tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                    &std::env::temp_dir(),
                    "test",
                )),
                hooks: Default::default(),
                permission_mode: "default".into(),
                expand_tasks: tokio::sync::watch::channel(false).0,
                ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            },
            None,
        )
        .await;

        let ids: Vec<&str> = outcomes.iter().map(|o| o.tool_use_id.as_str()).collect();
        assert_eq!(ids, vec!["r1", "r2", "b1", "r3"]);
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            cwd: Default::default(),
            home: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }
    }

    #[tokio::test]
    async fn cancel_aborts_in_flight_and_skips_rest() {
        let counter = Arc::new(AtomicUsize::new(0));
        let count_clone = counter.clone();
        let running = Arc::new(AtomicUsize::new(0));
        let running_clone = running.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let tool = FakeTool {
                name: "slow",
                safe: false,
                delay_ms: 200,
                counter: count_clone.clone(),
                max_seen: Arc::new(AtomicUsize::new(0)),
                running: running_clone,
            };
            let calls: Vec<PendingCall> = (0..3)
                .map(|i| PendingCall {
                    tool_use_id: format!("tu_{i}"),
                    tool: &tool,
                    input: serde_json::json!({}),
                })
                .collect();
            let (outcomes, aborted) = execute_calls(calls, &test_ctx(), Some(&mut rx)).await;
            (outcomes.len(), aborted)
        });
        // Wait for the first call to start (not finish: this test cancels mid-flight).
        // No deadline: on overloaded CI (2-core runners, hundreds of parallel test
        // threads) the 50ms call can take seconds to even begin.
        while running.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send(true).unwrap();
        let (done, aborted) = handle.await.unwrap();
        assert!(aborted, "中断后返回 aborted");
        assert_eq!(done, 0, "执行中的工具被取消，无完成结果");
        assert_eq!(counter.load(Ordering::SeqCst), 0, "没有任何工具跑完");
    }

    #[tokio::test]
    async fn cancel_keeps_completed_keeps_in_flight_dropped() {
        let counter = Arc::new(AtomicUsize::new(0));
        let count_clone = counter.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let tool = FakeTool {
                name: "fast",
                safe: false,
                delay_ms: 50,
                counter: count_clone.clone(),
                max_seen: Arc::new(AtomicUsize::new(0)),
                running: Arc::new(AtomicUsize::new(0)),
            };
            let calls: Vec<PendingCall> = (0..3)
                .map(|i| PendingCall {
                    tool_use_id: format!("tu_{i}"),
                    tool: &tool,
                    input: serde_json::json!({}),
                })
                .collect();
            let (outcomes, aborted) = execute_calls(calls, &test_ctx(), Some(&mut rx)).await;
            (outcomes, aborted)
        });
        // Wait for the first 50ms call to finish before cancelling (its result must be
        // kept). No deadline: on overloaded CI (2-core runners, hundreds of parallel
        // test threads) a 50ms delay can take far longer than 5s of wall time.
        while counter.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tx.send(true).unwrap();
        let (outcomes, aborted) = handle.await.unwrap();
        assert!(aborted);
        assert_eq!(
            outcomes.len(),
            1,
            "已完成的保留，执行中的取消，未开始的跳过"
        );
        assert_eq!(outcomes[0].tool_use_id, "tu_0");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// A parallel batch interrupted: results of already-finished tools (whose side effects
    /// have happened) must be kept, not discarded with join_all.
    #[tokio::test]
    async fn cancel_keeps_completed_results_in_parallel_batch() {
        let counter = Arc::new(AtomicUsize::new(0));
        let count_clone = counter.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let fast = FakeTool {
                name: "fast",
                safe: true,
                delay_ms: 20,
                counter: count_clone.clone(),
                max_seen: Arc::new(AtomicUsize::new(0)),
                running: Arc::new(AtomicUsize::new(0)),
            };
            let slow = FakeTool {
                name: "slow",
                safe: true,
                delay_ms: 5_000,
                counter: count_clone.clone(),
                max_seen: Arc::new(AtomicUsize::new(0)),
                running: Arc::new(AtomicUsize::new(0)),
            };
            let calls = vec![
                PendingCall {
                    tool_use_id: "fast_0".into(),
                    tool: &fast,
                    input: serde_json::json!({}),
                },
                PendingCall {
                    tool_use_id: "slow_1".into(),
                    tool: &slow,
                    input: serde_json::json!({}),
                },
                PendingCall {
                    tool_use_id: "fast_2".into(),
                    tool: &fast,
                    input: serde_json::json!({}),
                },
            ];
            execute_calls(calls, &test_ctx(), Some(&mut rx)).await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        tx.send(true).unwrap();
        let (outcomes, aborted) = handle.await.unwrap();

        assert!(aborted, "中断后返回 aborted");
        let ids: Vec<&str> = outcomes.iter().map(|o| o.tool_use_id.as_str()).collect();
        assert_eq!(ids, vec!["fast_0", "fast_2"], "已完成的按入队顺序保留");
        assert_eq!(counter.load(Ordering::SeqCst), 2, "只有两个跑完");
    }

    /// A receiver subscribed after send_replace(false): changed() becomes ready immediately,
    /// but that is not a cancel — the tool must finish normally and must not be misjudged
    /// as interrupted.
    #[tokio::test]
    async fn stale_watch_version_does_not_cancel() {
        let (tx, _seed) = tokio::sync::watch::channel(false);
        tx.send_replace(false);
        let mut rx = tx.subscribe();
        tx.send_replace(false);

        let tool = FakeTool {
            name: "read",
            safe: true,
            delay_ms: 20,
            counter: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            running: Arc::new(AtomicUsize::new(0)),
        };
        let calls = vec![
            PendingCall {
                tool_use_id: "a".into(),
                tool: &tool,
                input: serde_json::json!({}),
            },
            PendingCall {
                tool_use_id: "b".into(),
                tool: &tool,
                input: serde_json::json!({}),
            },
        ];
        let (outcomes, aborted) = execute_calls(calls, &test_ctx(), Some(&mut rx)).await;
        assert!(!aborted, "假信号不得判定中断");
        assert_eq!(outcomes.len(), 2);
    }

    /// Sender dropped: changed() keeps returning Err, which must be treated as "no longer
    /// cancellable" and parked, rather than letting the select branch become ready and
    /// repeatedly rebuild work (tight loop).
    #[tokio::test]
    async fn dropped_sender_never_cancels() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        drop(tx);

        let tool = FakeTool {
            name: "bash",
            safe: false,
            delay_ms: 20,
            counter: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            running: Arc::new(AtomicUsize::new(0)),
        };
        let calls = vec![PendingCall {
            tool_use_id: "a".into(),
            tool: &tool,
            input: serde_json::json!({}),
        }];
        let (outcomes, aborted) = tokio::time::timeout(
            Duration::from_secs(5),
            execute_calls(calls, &test_ctx(), Some(&mut rx)),
        )
        .await
        .expect("sender drop 后不得空转");
        assert!(!aborted);
        assert_eq!(outcomes.len(), 1, "工具照常跑完");
    }

    /// Cancel was already set before entering execution: when borrowing to clear the version,
    /// this signal must be acknowledged, not swallowed.
    #[tokio::test]
    async fn already_set_cancel_aborts_immediately() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();

        let tool = FakeTool {
            name: "read",
            safe: true,
            delay_ms: 50,
            counter: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::new(AtomicUsize::new(0)),
            running: Arc::new(AtomicUsize::new(0)),
        };
        let calls = vec![PendingCall {
            tool_use_id: "a".into(),
            tool: &tool,
            input: serde_json::json!({}),
        }];
        let (outcomes, aborted) = execute_calls(calls, &test_ctx(), Some(&mut rx)).await;
        assert!(aborted, "已置位的取消必须生效");
        assert!(outcomes.is_empty());
    }
}
