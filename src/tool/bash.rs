use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::AsyncBufReadExt;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// 周期命令默认检查间隔（无显式 -n 时）。
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;

/// 周期命令检查间隔识别：`watch -n N cmd` → N 秒；`watch cmd` /
/// while/until/for 循环 / `tail -f` → 默认间隔。其余返回 None。
pub fn periodic_bash_interval(command: &str) -> Option<std::time::Duration> {
    let mut parts = command.split_whitespace();
    let first = parts.next()?;
    if first == "watch" {
        let mut args = parts;
        let mut interval = DEFAULT_WATCH_INTERVAL_SECS;
        while let Some(a) = args.next() {
            if a == "-n" {
                if let Some(n) = args.next().and_then(|n| n.parse::<u64>().ok())
                    && n > 0
                {
                    interval = n;
                }
                break;
            }
        }
        return Some(std::time::Duration::from_secs(interval));
    }
    if matches!(first, "while" | "until" | "for" | "tail") {
        return Some(std::time::Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS));
    }
    None
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct BashInput {
    #[schemars(description = "要执行的 shell 命令")]
    command: String,
    #[serde(default)]
    #[schemars(description = "超时秒数，默认 120")]
    timeout: Option<u64>,
}

pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> String {
        "Bash".to_string()
    }

    fn description(&self) -> String {
        "在本地 shell 中执行命令，返回 stdout/stderr 与退出码。".to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<BashInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: BashInput = parse_input(&input)?;
        let timeout = Duration::from_secs(params.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));

        // 周期命令（watch/while/until/for/tail -f）自动后台化：
        // 立即返回 async_launched，后台执行 + 轮次检查 + 完成通知。
        if let Some(interval) = periodic_bash_interval(&params.command) {
            return launch_background(&params, ctx, interval, timeout).await;
        }

        let mut command = tokio::process::Command::new("/bin/zsh");
        command
            .arg("-c")
            .arg(&params.command)
            .current_dir(&ctx.cwd);

        let output = match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ToolError::failed(format!("failed to run command: {e}")));
            }
            Err(_) => {
                return Err(ToolError::failed(format!(
                    "command timed out after {}s",
                    timeout.as_secs()
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut text = format!("$ {}\n", params.command);
        if !stdout.is_empty() {
            text.push_str(&stdout);
        }
        if !stderr.is_empty() {
            text.push_str(&stderr);
        }
        text.push_str(&format!("\n[Exited with code {exit_code}]"));

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// 周期命令后台执行：注册 watchable（interval 轮询）+ spawn 流式执行。
async fn launch_background(
    params: &BashInput,
    ctx: &ToolContext,
    interval: Duration,
    timeout: Duration,
) -> Result<ToolResult, ToolError> {
    let cell = Arc::new(BashCell::new());
    let label = format!("$ {}", params.command);
    let id = ctx.watch.register(Box::new(BashWatch {
        cell: cell.clone(),
        label: label.clone(),
        interval: Some(interval),
    }));
    let watch = ctx.watch.clone();
    let command = params.command.clone();
    let cwd = ctx.cwd.clone();
    tokio::spawn(async move {
        match run_streaming(&command, &cwd, timeout, cell.clone()).await {
            Ok((text, code)) => {
                watch.set_state(
                    id,
                    crate::watch::WatchState::Done,
                    Some(format!("退出码 {code}")),
                    Some(serde_json::json!(text)),
                );
            }
            Err(e) => {
                watch.set_state(id, crate::watch::WatchState::Failed, Some(e), None);
            }
        }
    });
    Ok(ToolResult {
        content: serde_json::Value::String(serde_json::json!({
            "status": "async_launched",
            "task_id": id.0,
            "label": label,
            "note": "周期命令已在后台执行，状态变化与完成通知会到达",
        })
        .to_string()),
        is_error: false,
        diff: None,
    })
}

/// 流式执行：逐行读输出（更新行数统计），命令结束返回全文 + 退出码。
async fn run_streaming(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
    cell: Arc<BashCell>,
) -> Result<(String, i32), String> {
    let mut child = tokio::process::Command::new("/bin/zsh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;
    let buf = Arc::new(Mutex::new(String::new()));
    let mut readers = Vec::new();
    let streams: Vec<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    for stream in streams {
        let cell = cell.clone();
        let buf = buf.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stream);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        cell.record_line();
                        if let Ok(mut b) = buf.lock() {
                            b.push_str(&line);
                        }
                    }
                }
            }
        }));
    }
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(format!("failed to wait: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            return Err(format!("command timed out after {}s", timeout.as_secs()));
        }
    };
    for reader in readers {
        let _ = reader.await;
    }
    let code = status.code().unwrap_or(-1);
    let text = buf.lock().map(|b| b.clone()).unwrap_or_default();
    Ok((text, code))
}

/// 后台 Bash 的共享执行状态：轮次 = 自上次 poll 以来的新输出行。
struct BashCell {
    started: Instant,
    rounds: AtomicUsize,
    line_delta: AtomicUsize,
    total_lines: AtomicUsize,
}

impl BashCell {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            rounds: AtomicUsize::new(0),
            line_delta: AtomicUsize::new(0),
            total_lines: AtomicUsize::new(0),
        }
    }
    fn record_line(&self) {
        self.line_delta.fetch_add(1, Ordering::SeqCst);
        self.total_lines.fetch_add(1, Ordering::SeqCst);
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        let delta = self.line_delta.swap(0, Ordering::SeqCst);
        let total = self.total_lines.load(Ordering::SeqCst);
        if delta > 0 {
            let rounds = self.rounds.fetch_add(1, Ordering::SeqCst) + 1;
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Idle,
                detail: Some(format!("第 {rounds} 轮 · 输出 {delta} 行（累计 {total} 行）")),
                payload: None,
            }
        } else {
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Running,
                detail: Some(format!(
                    "已运行 {}s · 输出 {total} 行",
                    self.started.elapsed().as_secs()
                )),
                payload: None,
            }
        }
    }
}

struct BashWatch {
    cell: Arc<BashCell>,
    label: String,
    interval: Option<Duration>,
}

impl crate::watch::Watchable for BashWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll()
    }
    fn check_interval(&self) -> Option<Duration> {
        self.interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn periodic_command_backgrounds_and_notifies() {
        use crate::watch::WatchState;

        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
            cwd: std::env::temp_dir(),
            watch: watch.clone(),
        };
        let tool = BashTool::new();
        let result = tool
            .call(
                serde_json::json!({"command": "for i in 1 2 3; do echo tick; sleep 0.1; done"}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.contains("async_launched"), "launched: {text}");
        // 后台任务完成 → Done 事件 + 通知含输出。
        let mut rx = watch.subscribe();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut done = false;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                Ok(Ok(ev)) if ev.state == WatchState::Done => {
                    done = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(done, "background bash reaches Done");
        let notes = watch.consume_notifications();
        assert!(
            notes.iter().any(|n| n.contains("tick")),
            "payload in notification: {notes:?}"
        );
    }

    #[test]
    fn periodic_interval_recognition() {
        assert_eq!(
            periodic_bash_interval("watch -n 2 ls"),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(
            periodic_bash_interval("watch ls"),
            Some(std::time::Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS))
        );
        assert_eq!(
            periodic_bash_interval("while true; do echo hi; sleep 1; done"),
            Some(std::time::Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS))
        );
        assert_eq!(
            periodic_bash_interval("tail -f /var/log/sys.log"),
            Some(std::time::Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS))
        );
        assert_eq!(periodic_bash_interval("cargo test"), None);
        assert_eq!(periodic_bash_interval("git status"), None);
    }
}
