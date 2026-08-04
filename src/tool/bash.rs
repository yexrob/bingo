use std::time::Duration;

use serde::Deserialize;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// 周期命令默认检查间隔（无显式 -n 时）。
#[allow(dead_code)] // 阶段 3 后台化接入后移除
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;

/// 周期命令检查间隔识别：`watch -n N cmd` → N 秒；`watch cmd` /
/// while/until/for 循环 / `tail -f` → 默认间隔。其余返回 None。
#[allow(dead_code)] // 阶段 3 后台化接入后移除
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

#[cfg(test)]
mod tests {
    use super::*;

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
