use futures_util::future::join_all;

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

/// 执行队列（对标 StreamingToolExecutor / toolOrchestration，D7）：
/// 连续 concurrency-safe 的工具一批并行（上限 MAX_CONCURRENCY），
/// 非 safe 工具单独串行；结果保持入队顺序。
pub async fn execute_calls<'a>(
    calls: Vec<PendingCall<'a>>,
    ctx: &ToolContext,
) -> Vec<ExecOutcome> {
    let mut outcomes = Vec::with_capacity(calls.len());
    let mut rest = calls.as_slice();
    while !rest.is_empty() {
        let safe_count = rest
            .iter()
            .take_while(|c| c.tool.is_concurrency_safe(&c.input))
            .count();
        if safe_count > 0 {
            let batch = &rest[..safe_count.min(MAX_CONCURRENCY)];
            let executed: Vec<(String, Result<ToolResult, ToolError>, u64)> =
                join_all(batch.iter().map(|c| async move {
                    let start = std::time::Instant::now();
                    let result = c.tool.call(c.input.clone(), ctx).await;
                    (c.tool_use_id.clone(), result, start.elapsed().as_millis() as u64)
                }))
                .await;
            for (tool_use_id, result, duration_ms) in executed {
                outcomes.push(ExecOutcome { tool_use_id, result, duration_ms });
            }
            rest = &rest[batch.len()..];
        } else {
            let head = &rest[0];
            let start = std::time::Instant::now();
            let result = head.tool.call(head.input.clone(), ctx).await;
            outcomes.push(ExecOutcome {
                tool_use_id: head.tool_use_id.clone(),
                result,
                duration_ms: start.elapsed().as_millis() as u64,
            });
            rest = &rest[1..];
        }
    }
    outcomes
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
        let outcomes = execute_calls(calls, &ToolContext { cwd: Default::default(), watch: crate::watch::WatchRegistry::new(), http: reqwest::Client::new() }).await;
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
        let outcomes = execute_calls(calls, &ToolContext { cwd: Default::default(), watch: crate::watch::WatchRegistry::new(), http: reqwest::Client::new() }).await;
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

        let outcomes =
            execute_calls(calls, &ToolContext { cwd: Default::default(), watch: crate::watch::WatchRegistry::new(), http: reqwest::Client::new() }).await;

        let ids: Vec<&str> = outcomes.iter().map(|o| o.tool_use_id.as_str()).collect();
        assert_eq!(ids, vec!["r1", "r2", "b1", "r3"]);
    }
}
