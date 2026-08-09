use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::ErrorCode;
use crate::permission::PermissionBehavior;
use crate::settings::{HookRule, HooksConfig};

/// Ordinary hook timeout.
const HOOK_TIMEOUT: Duration = Duration::from_secs(60);
/// SessionEnd fast-teardown timeout (1.5s).
const SESSION_END_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Error)]
pub enum HookError {
    #[error("hook failed: {0}")]
    Failed(String),
}

impl ErrorCode for HookError {
    fn error_code(&self) -> &'static str {
        match self {
            HookError::Failed(_) => "HOOK_FAILED",
        }
    }
}

/// PreToolUse hook output.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct PreToolUseOutput {
    pub decision: Option<String>,
    pub reason: Option<String>,
    #[serde(rename = "updatedInput")]
    pub updated_input: Option<serde_json::Value>,
}

/// Input sent to hooks (minimal face of the hook input contract).
#[derive(Debug, Serialize)]
struct HookInput<'a> {
    hook_event_name: &'a str,
    tool_name: &'a str,
    tool_input: &'a serde_json::Value,
    permission_mode: &'a str,
}

/// matcher regex cache: compiled once; a compile failure warns once (None = fall back
/// to exact comparison).
static MATCHER_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Option<regex::Regex>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// matcher semantics: whole-string-anchored regex (`Edit|Write`, `mcp__.*` etc.).
/// Empty matcher matches everything; a regex compile failure falls back to exact
/// comparison with one warning.
fn matcher_matches(matcher: &str, tool_name: &str) -> bool {
    if matcher.is_empty() {
        return true;
    }
    let mut cache = MATCHER_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let compiled = cache.entry(matcher.to_string()).or_insert_with(|| {
        match regex::Regex::new(&format!("^(?:{matcher})$")) {
            Ok(re) => Some(re),
            Err(e) => {
                eprintln!(
                    "[bingo] warning: hook matcher {matcher:?} is not a valid regex ({e}); \
                     falling back to exact match"
                );
                None
            }
        }
    });
    match compiled {
        Some(re) => re.is_match(tool_name),
        None => matcher == tool_name,
    }
}

fn matched<'a>(rules: &'a [HookRule], tool_name: &str) -> Vec<&'a HookRule> {
    rules
        .iter()
        .filter(|rule| matcher_matches(&rule.matcher, tool_name))
        .collect()
}

/// Matched hook command list. A None tool_name means the event isn't tool-targeted.
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
        "TaskCreated" => matched(&config.task_created, ""),
        "TaskCompleted" => matched(&config.task_completed, ""),
        _ => return Vec::new(),
    };
    rules
        .iter()
        .flat_map(|rule| rule.hooks.iter())
        .filter(|h| h.kind == "command")
        .map(|h| h.command.as_str())
        .collect()
}

/// Run one hook: feed JSON on stdin, parse JSON from stdout.
/// Returns (exit code, stdout JSON, stderr). Exit-code semantics:
/// 0 = success; 2 = blocking (stderr injected into the model); other non-zero =
/// user-visible only.
async fn run_hook_with_timeout(
    command: &str,
    input: &serde_json::Value,
    timeout: Duration,
) -> Result<(i32, serde_json::Value, String), HookError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut child = crate::platform::shell_command(command, &cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Dropping the future after timeout kills the hook process, no leftovers.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| HookError::Failed(format!("spawn failed: {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| HookError::Failed("no stdin".into()))?;
    let input_text = input.to_string();

    // Writing stdin must share one timeout with wait and run concurrently: with a
    // tool_input larger than the pipe buffer and a hook that doesn't read stdin,
    // sequential writing would block the whole turn forever.
    let output = tokio::time::timeout(timeout, async move {
        use tokio::io::AsyncWriteExt;
        let write = async {
            if let Err(e) = stdin.write_all(input_text.as_bytes()).await {
                eprintln!("[bingo] warning: failed to write hook stdin: {e}");
            }
            // Close the pipe; the hook sees EOF.
            drop(stdin);
        };
        let (_, output) = tokio::join!(write, child.wait_with_output());
        output
    })
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

/// PreToolUse: run all matching hooks in order; any deny/ask takes effect,
/// updatedInput accumulates over the input. exit 2 → blocking (stderr as the reason);
/// other failures don't block.
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
        let (code, output, stderr) = match run_hook(
            command,
            &serde_json::to_value(hook_input).expect("HookInput serialization must not fail"),
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("[bingo] warning: PreToolUse hook failed: {e}");
                continue;
            }
        };
        if code == 2 {
            // blocking: stderr injected into the model.
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
                    parsed
                        .reason
                        .unwrap_or_else(|| "denied by PreToolUse hook".into()),
                    input,
                );
            }
            Some("ask") => {
                return (
                    PermissionBehavior::Ask,
                    parsed.reason.unwrap_or_default(),
                    input,
                );
            }
            _ => {}
        }
    }
    (PermissionBehavior::Allow, String::new(), input)
}

/// PreCompact: run matching hooks (no tool name). Failures don't block.
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

/// PostCompact: run matching hooks. Failures don't block.
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

/// PostToolUse: run matching hooks. Returns true = exit 2 (block continuation).
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

/// UserPromptSubmit: run when the user submits a message. exit 2 / JSON
/// decision=block → block this submission.
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

/// Stop: run when a turn ends normally. exit 2 → return blocking stderr (caller
/// injects it into the model and retries).
pub async fn run_stop_hooks(config: &HooksConfig, permission_mode: &str) -> Option<String> {
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

/// SessionStart: run when the session starts (no decision semantics, failures don't block).
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

/// SessionEnd: run when the session ends. Fast timeout (1.5s).
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

/// Task lifecycle hook common execution: exit-2 stderr collected and returned as
/// blockingError (the caller decides the consequence).
async fn run_task_lifecycle_hooks(
    config: &HooksConfig,
    event: &str,
    task_id: &str,
    subject: &str,
    permission_mode: &str,
) -> Vec<String> {
    let commands = commands_for(config, event, "");
    let mut blocking = Vec::new();
    for command in commands {
        let input = serde_json::json!({
            "hook_event_name": event,
            "task_id": task_id,
            "subject": subject,
            "permission_mode": permission_mode,
        });
        match run_hook(command, &input).await {
            Ok((2, _, stderr)) => {
                let reason = if stderr.is_empty() {
                    format!("blocked by {event} hook")
                } else {
                    stderr
                };
                eprintln!("[bingo] {event} hook blocked: {reason}");
                blocking.push(reason);
            }
            Ok((code, _, stderr)) if code != 0 => {
                eprintln!("[bingo] {event} hook exited {code}: {stderr}");
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("[bingo] {event} hook warning: {e}");
            }
        }
    }
    blocking
}

/// TaskCreated: run after a new task is created. blockingError → the task is revoked
/// (handled by the caller).
pub async fn run_task_created(
    config: &HooksConfig,
    task_id: &str,
    subject: &str,
    permission_mode: &str,
) -> Vec<String> {
    run_task_lifecycle_hooks(config, "TaskCreated", task_id, subject, permission_mode).await
}

/// TaskCompleted: run before a task is marked completed. blockingError → reject
/// completed (handled by the caller).
pub async fn run_task_completed(
    config: &HooksConfig,
    task_id: &str,
    subject: &str,
    permission_mode: &str,
) -> Vec<String> {
    run_task_lifecycle_hooks(config, "TaskCompleted", task_id, subject, permission_mode).await
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

    /// Hook script that writes to stderr and exits 2 (blocking): PowerShell 5.1
    /// doesn't support `>&2`, so use [Console]::Error.WriteLine there.
    fn stderr_exit_2(msg: &str) -> String {
        #[cfg(windows)]
        {
            format!("[Console]::Error.WriteLine(\"{msg}\"); exit 2")
        }
        #[cfg(not(windows))]
        {
            format!("echo \"{msg}\" >&2; exit 2")
        }
    }

    #[tokio::test]
    async fn exit_two_blocks_with_stderr() {
        let config = config_with(&stderr_exit_2("no git"));
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
                "hooks": [{"type": "command", "command": stderr_exit_2("denied")}]
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
                "hooks": [{"type": "command", "command": stderr_exit_2("review pending")}]
            }]
        }))
        .unwrap();
        let blocking = run_stop_hooks(&config, "default").await;
        assert_eq!(blocking.as_deref(), Some("review pending"));
    }

    /// Regression: a hook that doesn't read stdin with a tool_input beyond the pipe
    /// buffer (64KB) used to deadlock forever.
    #[tokio::test]
    async fn large_stdin_does_not_deadlock_when_hook_ignores_it() {
        let config = config_with("echo '{}'");
        let huge = "x".repeat(512 * 1024);
        let done = tokio::time::timeout(
            Duration::from_secs(20),
            run_pre_tool_use(
                &config,
                "Bash",
                &serde_json::json!({"command": huge}),
                "default",
            ),
        )
        .await;
        let (behavior, _, _) = done.unwrap_or_else(|_| unreachable!("hook stdin write deadlock"));
        assert_eq!(behavior, PermissionBehavior::Allow);
    }

    /// A timed-out hook doesn't block the turn, and no process is left behind
    /// (kill_on_drop).
    #[tokio::test]
    async fn hook_timeout_is_reported_and_does_not_hang() {
        let started = std::time::Instant::now();
        let result = run_hook_with_timeout(
            "sleep 30",
            &serde_json::json!({"hook_event_name": "Test"}),
            Duration::from_millis(300),
        )
        .await;
        assert!(matches!(result, Err(HookError::Failed(ref m)) if m.contains("timed out")));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must return immediately"
        );
    }

    /// matcher is a whole-string-anchored regex: `Edit|Write`, `mcp__.*` work instead
    /// of silently not matching.
    #[test]
    fn matcher_supports_anchored_regex() {
        assert!(matcher_matches("Edit|Write", "Edit"));
        assert!(matcher_matches("Edit|Write", "Write"));
        assert!(!matcher_matches("Edit|Write", "Read"));
        // Whole-string anchored: no substring matching.
        assert!(!matcher_matches("Edit", "EditNotebook"));
        assert!(matcher_matches("mcp__.*", "mcp__files__read"));
        assert!(!matcher_matches("mcp__.*", "Bash"));
        assert!(matcher_matches("Bash", "Bash"));
        // Empty matcher matches everything.
        assert!(matcher_matches("", "Anything"));
        // Invalid regex falls back to exact match.
        assert!(matcher_matches("Web(Fetch", "Web(Fetch"));
        assert!(!matcher_matches("Web(Fetch", "WebFetch"));
    }

    #[tokio::test]
    async fn regex_matcher_selects_hook_end_to_end() {
        let config: HooksConfig = serde_json::from_value(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Edit|Write",
                "hooks": [{"type": "command", "command": r#"echo '{"decision":"deny","reason":"nope"}'"#}]
            }]
        }))
        .unwrap();
        let (behavior, reason, _) = run_pre_tool_use(
            &config,
            "Write",
            &serde_json::json!({"file_path": "x"}),
            "default",
        )
        .await;
        assert_eq!(behavior, PermissionBehavior::Deny);
        assert_eq!(reason, "nope");
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
