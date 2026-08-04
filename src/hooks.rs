use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::permission::PermissionBehavior;
use crate::settings::{HookRule, HooksConfig};

/// 普通 hook 超时。
const HOOK_TIMEOUT: Duration = Duration::from_secs(60);
/// SessionEnd 快速收尾超时（对标 Claude Code SESSION_END_HOOK_TIMEOUT_MS_DEFAULT 1.5s）。
const SESSION_END_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Error)]
pub enum HookError {
    #[error("hook failed: {0}")]
    Failed(String),
}

/// PreToolUse hook 输出（对标 Claude Code hook 输出契约）。
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct PreToolUseOutput {
    pub decision: Option<String>,
    pub reason: Option<String>,
    #[serde(rename = "updatedInput")]
    pub updated_input: Option<serde_json::Value>,
}

/// 发送给 hook 的输入（对标 Claude Code hook 输入契约的最小面）。
#[derive(Debug, Serialize)]
struct HookInput<'a> {
    hook_event_name: &'a str,
    tool_name: &'a str,
    tool_input: &'a serde_json::Value,
    permission_mode: &'a str,
}

fn matched<'a>(rules: &'a [HookRule], tool_name: &str) -> Vec<&'a HookRule> {
    rules
        .iter()
        .filter(|rule| rule.matcher.is_empty() || rule.matcher == tool_name)
        .collect()
}

/// 匹配到的 hook 命令列表。tool_name 为 None 表示事件不针对工具。
fn commands_for<'a>(config: &'a HooksConfig, event: &'a str, tool_name: &str) -> Vec<&'a str> {
    let rules = match event {
        "PreToolUse" => matched(&config.pre_tool_use, tool_name),
        "PostToolUse" => matched(&config.post_tool_use, tool_name),
        "PreCompact" => matched(&config.pre_compact, tool_name),
        "PostCompact" => matched(&config.post_compact, tool_name),
        "UserPromptSubmit" => matched(&config.user_prompt_submit, tool_name),
        "Stop" => matched(&config.stop, tool_name),
        "SessionStart" => matched(&config.session_start, tool_name),
        "SessionEnd" => matched(&config.session_end, tool_name),
        _ => return Vec::new(),
    };
    rules
        .iter()
        .flat_map(|rule| rule.hooks.iter())
        .filter(|h| h.kind == "command")
        .map(|h| h.command.as_str())
        .collect()
}

/// 执行一条 hook：stdin 喂 JSON，stdout 解析 JSON。
/// 返回 (退出码, stdout JSON, stderr)。退出码语义对标 Claude Code：
/// 0 = 成功；2 = blocking（stderr 注入模型）；其他非零 = 仅用户可见。
async fn run_hook_with_timeout(
    command: &str,
    input: &serde_json::Value,
    timeout: Duration,
) -> Result<(i32, serde_json::Value, String), HookError> {
    let mut child = tokio::process::Command::new("/bin/zsh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| HookError::Failed(format!("spawn failed: {e}")))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| HookError::Failed("no stdin".into()))?;
    let input_text = input.to_string();
    use tokio::io::AsyncWriteExt;
    let mut stdin = stdin;
    if let Err(e) = stdin.write_all(input_text.as_bytes()).await {
        eprintln!("[bingo] warning: failed to write hook stdin: {e}");
    }
    drop(stdin);

    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| HookError::Failed(format!("hook timed out: {command}")))?;
    let output = output.map_err(|e| HookError::Failed(format!("wait failed: {e}")))?;

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = if stdout.trim().is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Err(HookError::Failed(format!("bad hook output JSON: {e}")));
            }
        }
    };
    Ok((code, parsed, stderr.trim().to_string()))
}

async fn run_hook(
    command: &str,
    input: &serde_json::Value,
) -> Result<(i32, serde_json::Value, String), HookError> {
    run_hook_with_timeout(command, input, HOOK_TIMEOUT).await
}

/// PreToolUse：按顺序执行所有匹配 hook；任一返回 deny/ask 即生效，
/// updatedInput 累积覆盖输入。exit 2 → 阻塞（stderr 作为原因）；其他失败不阻断。
pub async fn run_pre_tool_use(
    config: &HooksConfig,
    tool_name: &str,
    tool_input: &serde_json::Value,
    permission_mode: &str,
) -> (PermissionBehavior, String, serde_json::Value) {
    let commands = commands_for(config, "PreToolUse", tool_name);
    if commands.is_empty() {
        return (PermissionBehavior::Allow, String::new(), tool_input.clone());
    }
    let mut input = tool_input.clone();
    for command in commands {
        let hook_input = HookInput {
            hook_event_name: "PreToolUse",
            tool_name,
            tool_input: &input,
            permission_mode,
        };
        let (code, output, stderr) =
            match run_hook(command, &serde_json::to_value(hook_input).unwrap()).await {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("[bingo] warning: PreToolUse hook failed: {e}");
                    continue;
                }
            };
        if code == 2 {
            // blocking：stderr 注入模型（对标 Claude Code exit 2）。
            let reason = if stderr.is_empty() {
                "blocked by PreToolUse hook".to_string()
            } else {
                stderr
            };
            return (PermissionBehavior::Deny, reason, input);
        }
        if code != 0 {
            eprintln!("[bingo] PreToolUse hook exited {code}: {stderr}");
            continue;
        }
        let parsed: PreToolUseOutput = match serde_json::from_value(output) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[bingo] warning: PreToolUse hook returned bad JSON: {e}");
                continue;
            }
        };
        if let Some(updated) = parsed.updated_input {
            input = updated;
        }
        match parsed.decision.as_deref() {
            Some("deny") => {
                return (
                    PermissionBehavior::Deny,
                    parsed.reason.unwrap_or_else(|| "denied by PreToolUse hook".into()),
                    input,
                );
            }
            Some("ask") => return (PermissionBehavior::Ask, parsed.reason.unwrap_or_default(), input),
            _ => {}
        }
    }
    (PermissionBehavior::Allow, String::new(), input)
}

/// PreCompact：执行匹配 hook（无工具名）。失败不阻断。
pub async fn run_pre_compact(config: &HooksConfig, permission_mode: &str) {
    let commands = commands_for(config, "PreCompact", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "PreCompact",
            "permission_mode": permission_mode,
        });
        if let Err(e) = run_hook(command, &input).await {
            eprintln!("[bingo] PreCompact hook warning: {e}");
        }
    }
}

/// PostCompact：执行匹配 hook。失败不阻断。
pub async fn run_post_compact(config: &HooksConfig, permission_mode: &str) {
    let commands = commands_for(config, "PostCompact", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "PostCompact",
            "permission_mode": permission_mode,
        });
        if let Err(e) = run_hook(command, &input).await {
            eprintln!("[bingo] PostCompact hook warning: {e}");
        }
    }
}

/// PostToolUse：执行匹配 hook。返回 true = exit 2（阻断继续，对标 Claude Code blocking error）。
pub async fn run_post_tool_use(
    config: &HooksConfig,
    tool_name: &str,
    tool_input: &serde_json::Value,
    tool_result: &serde_json::Value,
    permission_mode: &str,
) -> bool {
    let commands = commands_for(config, "PostToolUse", tool_name);
    let mut blocked = false;
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": tool_name,
            "tool_input": tool_input,
            "tool_response": tool_result,
            "permission_mode": permission_mode,
        });
        match run_hook(command, &input).await {
            Ok((2, _, stderr)) => {
                eprintln!("[bingo] PostToolUse hook blocked continuation: {stderr}");
                blocked = true;
            }
            Ok((code, _, stderr)) if code != 0 => {
                eprintln!("[bingo] PostToolUse hook exited {code}: {stderr}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[bingo] PostToolUse hook warning: {e}");
            }
        }
    }
    blocked
}

/// UserPromptSubmit：用户提交消息时执行。exit 2 / JSON decision=block → 阻止本次提交。
pub async fn run_user_prompt_submit(
    config: &HooksConfig,
    prompt: &str,
    permission_mode: &str,
) -> bool {
    let commands = commands_for(config, "UserPromptSubmit", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "user_prompt": prompt,
            "permission_mode": permission_mode,
        });
        match run_hook(command, &input).await {
            Ok((2, _, stderr)) => {
                eprintln!("[bingo] UserPromptSubmit hook blocked: {stderr}");
                return true;
            }
            Ok((code, output, stderr)) => {
                if code != 0 {
                    eprintln!("[bingo] UserPromptSubmit hook exited {code}: {stderr}");
                    continue;
                }
                let blocked = output
                    .get("decision")
                    .and_then(|d| d.as_str())
                    .is_some_and(|d| d == "block");
                if blocked {
                    eprintln!("[bingo] UserPromptSubmit hook blocked the prompt");
                    return true;
                }
            }
            Err(e) => {
                eprintln!("[bingo] UserPromptSubmit hook warning: {e}");
            }
        }
    }
    false
}

/// Stop：回合正常结束时执行。exit 2 → 返回 blocking stderr（调用方注入模型并重试）。
pub async fn run_stop_hooks(
    config: &HooksConfig,
    permission_mode: &str,
) -> Option<String> {
    let commands = commands_for(config, "Stop", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "Stop",
            "permission_mode": permission_mode,
        });
        match run_hook(command, &input).await {
            Ok((2, _, stderr)) => {
                eprintln!("[bingo] Stop hook blocked continuation: {stderr}");
                return Some(stderr);
            }
            Ok((code, _, stderr)) if code != 0 => {
                eprintln!("[bingo] Stop hook exited {code}: {stderr}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[bingo] Stop hook warning: {e}");
            }
        }
    }
    None
}

/// SessionStart：会话开始时执行（无决策语义，失败不阻断）。
pub async fn run_session_start(config: &HooksConfig, permission_mode: &str) {
    let commands = commands_for(config, "SessionStart", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "SessionStart",
            "permission_mode": permission_mode,
        });
        if let Err(e) = run_hook(command, &input).await {
            eprintln!("[bingo] SessionStart hook warning: {e}");
        }
    }
}

/// SessionEnd：会话结束时执行。快速超时（1.5s，对标 Claude Code）。
pub async fn run_session_end(config: &HooksConfig, permission_mode: &str) {
    let commands = commands_for(config, "SessionEnd", "");
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": "SessionEnd",
            "permission_mode": permission_mode,
        });
        if let Err(e) = run_hook_with_timeout(command, &input, SESSION_END_TIMEOUT).await {
            eprintln!("[bingo] SessionEnd hook warning: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(command: &str) -> HooksConfig {
        serde_json::from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": command}]
            }]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn hook_denies_tool() {
        let config = config_with(r#"echo '{"decision":"deny","reason":"no"}'"#);
        let (behavior, reason, _) = run_pre_tool_use(
            &config,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Deny);
        assert_eq!(reason, "no");
    }

    #[tokio::test]
    async fn hook_rewrites_input() {
        let config = config_with(r#"echo '{"updatedInput":{"command":"echo safe"}}'"#);
        let (behavior, _, input) = run_pre_tool_use(
            &config,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Allow);
        assert_eq!(input, serde_json::json!({"command": "echo safe"}));
    }

    #[tokio::test]
    async fn matcher_filters_tools() {
        let config = config_with(r#"echo '{"decision":"deny"}'"#);
        let (behavior, _, _) = run_pre_tool_use(
            &config,
            "Read",
            &serde_json::json!({"file_path": "x"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn failing_hook_is_ignored() {
        let config = config_with("exit 1");
        let (behavior, _, _) = run_pre_tool_use(
            &config,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Allow);
    }

    #[tokio::test]
    async fn exit_two_blocks_with_stderr() {
        let config = config_with(r#"echo "no git" >&2; exit 2"#);
        let (behavior, reason, _) = run_pre_tool_use(
            &config,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Deny);
        assert_eq!(reason, "no git");
    }

    #[tokio::test]
    async fn post_tool_use_exit_two_blocks() {
        let config = serde_json::from_value(serde_json::json!({
            "PostToolUse": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": "exit 2"}]
            }]
        }))
        .unwrap();
        let blocked = run_post_tool_use(
            &config,
            "Bash",
            &serde_json::json!({"command": "ls"}),
            &serde_json::json!("ok"),
            "default",
        )
        .await;
        assert!(blocked);
    }

    #[tokio::test]
    async fn user_prompt_submit_exit_two_blocks_prompt() {
        let config = serde_json::from_value(serde_json::json!({
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": "echo denied >&2; exit 2"}]
            }]
        }))
        .unwrap();
        let blocked = run_user_prompt_submit(&config, "hello", "default").await;
        assert!(blocked);
    }

    #[tokio::test]
    async fn stop_hook_exit_two_returns_blocking_stderr() {
        let config = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "hooks": [{"type": "command", "command": "echo review pending >&2; exit 2"}]
            }]
        }))
        .unwrap();
        let blocking = run_stop_hooks(&config, "default").await;
        assert_eq!(blocking.as_deref(), Some("review pending"));
    }

    #[tokio::test]
    async fn stop_hook_clean_exit_no_blocking() {
        let config = serde_json::from_value(serde_json::json!({
            "Stop": [{
                "hooks": [{"type": "command", "command": "exit 0"}]
            }]
        }))
        .unwrap();
        let blocking = run_stop_hooks(&config, "default").await;
        assert_eq!(blocking, None);
    }
}
