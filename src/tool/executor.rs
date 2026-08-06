use futures_util::future::join_all;
use tokio::sync::watch;

use super::{Tool, ToolContext, ToolError, ToolResult};

pub const MAX_CONCURRENCY: usize = 10;

/// 一轮中待执行的工具调用（已通过权限门）。
pub struct PendingCall<'a> {
    pub tool_use_id: String,
    pub tool: &'a dyn Tool,
    pub input: serde_json::Value,
}

pub struct ExecOutcome {
    pub tool_use_id: String,
    pub result: Result<ToolResult, ToolError>,
    /// 工具执行耗时（毫秒）。
    pub duration_ms: u64,
}

/// 执行队列（D7）：
/// 连续 concurrency-safe 的工具一批并行（上限 MAX_CONCURRENCY），
/// 非 safe 工具单独串行；结果保持入队顺序。
/// cancel：Some 时每批与中断信号竞争——信号到达立即停止（正在执行的
/// future drop 即取消），已完成的保留返回；调用方丢弃整轮（不回填）。
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
            rest = &rest[batch.len()..];        } else {
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

/// 一批并行执行；cancel 命中返回 `(已完结果, 中断)`。
async fn run_batch<'a>(
    batch: &[PendingCall<'a>],
    ctx: &ToolContext,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> (Vec<ExecOutcome>, bool) {
    let fut = join_all(batch.iter().map(|c| async move {
        let start = std::time::Instant::now();
        let result = c.tool.call(c.input.clone(), ctx).await;
        (c.tool_use_id.clone(), result, start.elapsed().as_millis() as u64)
    }));
    futures_util::pin_mut!(fut);
    let (executed, aborted) = loop {
        match cancel.as_deref_mut() {
            Some(cancel) => match tokio::select! {
                executed = &mut fut => Some((executed, false)),
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        Some((Vec::new(), true))
                    } else {
                        None
                    }
                }
            } {
                Some(v) => break v,
                None => continue,
            },
            None => break (fut.await, false),
        }
    };
    (
        executed
            .into_iter()
            .map(|(tool_use_id, result, duration_ms)| ExecOutcome {
                tool_use_id,
                result,
                duration_ms,
            })
            .collect(),
        aborted,
    )
}

/// 单个工具执行；cancel 命中返回 None（中断）。
async fn call_or_cancel<'a>(
    call: &PendingCall<'a>,
    ctx: &ToolContext,
    mut cancel: Option<&mut watch::Receiver<bool>>,
) -> Option<Result<ToolResult, ToolError>> {
    let fut = call.tool.call(call.input.clone(), ctx);
    futures_util::pin_mut!(fut);
    loop {
        match cancel.as_deref_mut() {
            Some(cancel) => match tokio::select! {
                result = &mut fut => Some(Some(result)),
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        Some(None)
                    } else {
                        None
                    }
                }
            } {
                Some(v) => return v,
                None => continue,
            },
            None => return Some(fut.await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
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
        let (outcomes, _interrupted) = execute_calls(calls, &ToolContext {
            cwd: Default::default(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }, None).await;
        let elapsed = start.elapsed();

        assert!(elapsed < Duration::from_millis(250), "not parallel: {elapsed:?}");
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
        let (outcomes, _interrupted) = execute_calls(calls, &ToolContext {
            cwd: Default::default(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }, None).await;
        let elapsed = start.elapsed();

        assert!(elapsed >= Duration::from_millis(90), "not serial: {elapsed:?}");
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
            PendingCall { tool_use_id: "r1".into(), tool: &read, input: serde_json::json!({}) },
            PendingCall { tool_use_id: "r2".into(), tool: &read, input: serde_json::json!({}) },
            PendingCall { tool_use_id: "b1".into(), tool: &bash, input: serde_json::json!({}) },
            PendingCall { tool_use_id: "r3".into(), tool: &read, input: serde_json::json!({}) },
        ];

        let (outcomes, _interrupted) =
            execute_calls(calls, &ToolContext {
            cwd: Default::default(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }, None).await;

        let ids: Vec<&str> = outcomes.iter().map(|o| o.tool_use_id.as_str()).collect();
        assert_eq!(ids, vec!["r1", "r2", "b1", "r3"]);
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
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
        }
    }

    #[tokio::test]
    async fn cancel_aborts_in_flight_and_skips_rest() {
        let counter = Arc::new(AtomicUsize::new(0));
        let count_clone = counter.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(async move {
            let tool = FakeTool {
                name: "slow",
                safe: false,
                delay_ms: 200,
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
            (outcomes.len(), aborted)
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
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
        tokio::time::sleep(Duration::from_millis(60)).await;
        tx.send(true).unwrap();
        let (outcomes, aborted) = handle.await.unwrap();
        assert!(aborted);
        assert_eq!(outcomes.len(), 1, "已完成的保留，执行中的取消，未开始的跳过");
        assert_eq!(outcomes[0].tool_use_id, "tu_0");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
