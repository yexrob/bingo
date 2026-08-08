use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::AsyncBufReadExt;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Default check interval for periodic commands (when no explicit -n is given).
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;
/// Default Bash output cap in characters (aligned with Read's 20k cap; overlong
/// tool output costs context and tokens either way). Configurable via settings
/// `maxBashOutputChars` (0 = no cap).
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 20_000;
/// Upper bound for waiting on readers to drain after the command exits (so we don't
/// wait forever if grandchild processes still hold the pipe).
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Truncate Bash output for the model: overlong bodies keep the head and append a
/// hint (Read's truncation semantics); the command echo and exit line stay outside
/// the cap. `limit == 0` disables the cap.
fn cap_output(body: &str, limit: usize) -> String {
    let total = body.chars().count();
    if limit == 0 || total <= limit {
        return body.to_string();
    }
    let head: String = body.chars().take(limit).collect();
    format!(
        "{head}\n[Content truncated: {total} characters total, showing first {limit}; \
         rerun with `> file` redirection and Read the file for the full output]"
    )
}

/// Rejection reason for interactive commands requiring a TTY (shared by the `!` command and the Bash tool).
/// The bingo child process's stdin/stdout are pipes: full-screen TUIs (top/htop/vim) garble their output,
/// ssh/fzf/sudo etc. grab the terminal by connecting to /dev/tty directly (tearing the screen in raw mode),
/// and a bare shell/REPL exits immediately without input (pointless). On a match, returns the rejection
/// reason and usable alternatives. Interactive terminal apps cannot be driven by the agent's bash tool.
pub fn interactive_command_reason(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    // Unwrap common wrapper commands (sudo/env/nohup/command/exec/doas).
    while matches!(
        tokens.get(i).copied(),
        Some("sudo" | "doas" | "env" | "nohup" | "command" | "exec")
    ) {
        let wrapper = tokens.get(i).copied()?;
        i += 1;
        if i >= tokens.len() {
            return match wrapper {
                // A bare sudo/doas grabs /dev/tty to prompt for a password.
                "sudo" | "doas" => {
                    Some("sudo/doas 需要交互口令（TTY），已拒绝".to_string())
                }
                _ => None,
            };
        }
        if matches!(wrapper, "sudo" | "doas") {
            // sudo flags: -i/-s mean interactive login; value-taking flags are skipped
            // together with their value. Flags may exhaust the tokens (`sudo -v`): keep
            // getting throughout; if no command remains at the end, treat as bare sudo.
            let mut non_prompting = false;
            while let Some(flag) = tokens.get(i).copied().filter(|t| t.starts_with('-')) {
                i += 1;
                if matches!(flag, "-i" | "-s") {
                    return Some(format!(
                        "sudo 交互登录 shell（{flag}）需要 TTY，已拒绝"
                    ));
                }
                // These flags never prompt for a password (-n fails immediately, -k/-K only clear timestamps).
                if matches!(flag, "-n" | "--non-interactive" | "-k" | "-K" | "-V" | "--version") {
                    non_prompting = true;
                }
                if matches!(
                    flag,
                    "-u" | "-g" | "-C" | "-p" | "-D" | "-R" | "-T" | "-r" | "-t" | "-U"
                        | "-S" | "-P" | "-h"
                ) && i < tokens.len()
                {
                    i += 1;
                }
            }
            if i >= tokens.len() {
                return (!non_prompting)
                    .then(|| "sudo/doas 需要交互口令（TTY），已拒绝".to_string());
            }
        } else if wrapper == "env" {
            // env's VAR=value assignments are not commands.
            while tokens.get(i).is_some_and(|t| t.contains('=')) {
                i += 1;
            }
            if i >= tokens.len() {
                return None;
            }
        }
    }
    // An empty command (or one with only wrappers) has no base command to judge: let the shell handle it.
    let base = tokens.get(i).copied()?;
    let name = std::path::Path::new(base)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(base);
    let rest = &tokens[i + 1..];

    // Full-screen system monitors: allow -b/--batch (one-shot snapshot, non-interactive).
    const MONITORS: &[&str] = &[
        "top", "htop", "btop", "bpytop", "bashtop", "btm", "nmon", "glances", "s-tui",
        "gtop", "vtop", "ktop", "ctop", "ytop",
    ];
    if MONITORS.contains(&name) {
        let batch = rest
            .iter()
            .any(|a| matches!(*a, "-b" | "-batch" | "--batch"));
        if !batch {
            return Some(format!(
                "{name} 是全屏交互监控程序（需要 TTY），已拒绝。一次性快照可用 `{name} -b -n 1`"
            ));
        }
    }
    const EDITORS: &[&str] = &[
        "vim", "vi", "nvim", "nano", "emacs", "micro", "pico", "mg", "ed", "ex", "kak",
        "kakoune", "helix", "hx", "ne", "zile", "joe",
    ];
    const FILE_MANAGERS: &[&str] = &[
        "ranger", "lf", "yazi", "joshuto", "mc", "midnight-commander",
    ];
    const TUI_TOOLS: &[&str] = &[
        "lazygit", "tig", "lazydocker", "k9s", "kdash", "screen", "fzf",
    ];
    if EDITORS.contains(&name) {
        return Some(format!("{name} 是交互式编辑器（需要 TTY），已拒绝"));
    }
    if FILE_MANAGERS.contains(&name) {
        return Some(format!("{name} 是交互式文件管理器（需要 TTY），已拒绝"));
    }
    if TUI_TOOLS.contains(&name) {
        return Some(format!("{name} 是交互式 TUI 程序（需要 TTY），已拒绝"));
    }
    // gdb: without -batch it is an interactive debugger.
    if name == "gdb"
        && !rest.iter().any(|a| matches!(*a, "-batch" | "--batch"))
    {
        return Some("gdb 调试器需要 TTY，已拒绝。批处理可用 `gdb -batch -ex ...`".to_string());
    }
    // Bare shell/REPL: exits immediately without input (pointless); allow when given
    // arguments (bash -c / python x.py). DB clients have separate rules: connection args
    // without an execution flag/script/SQL positional argument = REPL.
    const REPLS: &[&str] = &[
        "bash", "sh", "zsh", "fish", "ksh", "dash", "elvish", "xonsh", "python", "python2",
        "python3", "ipython", "pypy", "node", "deno", "bun", "ruby", "irb", "perl", "php",
        "lua", "luajit", "bc", "dc", "sbcl", "ghci", "powershell", "pwsh", "cmd", "cmd.exe",
        "powershell.exe",
    ];
    if REPLS.contains(&name) && rest.is_empty() {
        return Some(format!(
            "{name} 是交互式 shell/REPL（需要 TTY），已拒绝。带参数执行（如 `{name} -c '...'`）可以"
        ));
    }
    // Interactive DB clients: without an execution flag (-c/-e/-f/--eval…), stdin redirection,
    // or SQL/script positional arguments → they enter an interactive prompt.
    const DB_REPLS: &[&str] = &["sqlite3", "psql", "mysql", "mongosh", "redis-cli"];
    if DB_REPLS.contains(&name) {
        let flags = |exec: &[&str]| rest.iter().any(|a| exec.contains(a));
        let has_stdin = rest.contains(&"<");
        let positional: Vec<&&str> = rest.iter().filter(|a| !a.starts_with('-')).collect();
        let interactive = match name {
            "sqlite3" | "psql" => {
                !flags(&["-c", "-f", "-l", "--command", "--file", "--list", "--version", "--help"])
                    && !has_stdin
                    && positional.len() <= 1
            }
            "mysql" => {
                !flags(&["-e", "--execute", "-f", "--force", "--version", "--help"])
                    && !has_stdin
            }
            "mongosh" => {
                !flags(&["--eval", "--version", "--help"])
                    && !has_stdin
                    && !positional.iter().any(|a| a.ends_with(".js"))
            }
            // redis-cli: no positional arguments means an interactive prompt.
            _ => {
                !flags(&["--version", "--help"]) && positional.is_empty() && !has_stdin
            }
        };
        if interactive {
            return Some(format!(
                "{name} 是交互式客户端（需要 TTY），已拒绝。传执行旗标或脚本（如 `{name} -c '...'`）可以"
            ));
        }
    }
    // ssh: -t forces a TTY; or a host without a remote command = interactive session (password prompt/remote shell).
    if name == "ssh" {
        let tty = rest.iter().any(|a| matches!(*a, "-t" | "-tt"));
        let mut has_host = false;
        let mut has_cmd = false;
        let mut no_cmd_ok = false;
        let mut j = 0;
        while j < rest.len() {
            let a = rest[j];
            if matches!(
                a,
                "-p" | "-l" | "-i" | "-o" | "-F" | "-J" | "-L" | "-R" | "-D" | "-W" | "-m"
                    | "-c" | "-e" | "-b" | "-K" | "-I" | "-O" | "-Q" | "-S" | "-w" | "-E"
                    | "-G" | "-g"
            ) {
                j += 2;
                continue;
            }
            if matches!(a, "-N" | "-f") {
                no_cmd_ok = true;
            } else if !a.starts_with('-') {
                if has_host {
                    has_cmd = true;
                } else {
                    has_host = true;
                }
            }
            j += 1;
        }
        if tty || (has_host && !has_cmd && !no_cmd_ok) {
            return Some(
                "ssh 交互会话（占用 /dev/tty 问口令或进入远程 shell），已拒绝。传远程命令（`ssh host 'cmd'`）可以"
                    .to_string(),
            );
        }
    }
    // Container exec/run -it / attach: interactive shell.
    if matches!(name, "docker" | "nerdctl" | "podman" | "kubectl" | "docker-compose") {
        let sub = rest.first().copied().unwrap_or("");
        if sub == "attach" {
            return Some(format!(
                "{name} attach 是交互会话（需要 TTY），已拒绝"
            ));
        }
        if matches!(sub, "exec" | "run") {
            let interactive = rest.iter().any(|a| matches!(*a, "-it" | "-ti" | "--interactive"))
                || (rest.contains(&"-i") && rest.contains(&"-t"));
            if interactive {
                return Some(format!(
                    "{name} {sub} -it 是交互会话（需要 TTY），已拒绝"
                ));
            }
        }
    }
    // tmux attach / foreground new (no -d): needs a TTY; scripted uses like send-keys/capture-pane are allowed.
    if name == "tmux" {
        let sub = rest.first().copied().unwrap_or("");
        if matches!(sub, "attach" | "a" | "attach-session") {
            return Some("tmux attach 需要 TTY，已拒绝。`tmux new -d` 脱离会话可以".to_string());
        }
        if rest.is_empty() || (matches!(sub, "new" | "new-session") && !rest.contains(&"-d")) {
            return Some("tmux 前台会话需要 TTY，已拒绝。`tmux new -d` 脱离会话可以".to_string());
        }
    }
    None
}

/// Recognize the check interval of periodic commands: `watch -n N cmd` → N seconds; `watch cmd` /
/// while/until/for loops / `tail -f` → default interval. Anything else returns None.
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
    #[schemars(description = "Shell command to execute")]
    command: String,
    #[serde(default)]
    #[schemars(description = "Timeout in seconds, default 120")]
    timeout: Option<u64>,
    /// Background monitor notification conditions: any of these strings appearing in the output
    /// triggers a notification (e.g. ["ERROR", "panic"]). When unset, common error lines are
    /// detected by default.
    #[serde(default)]
    #[schemars(description = "Background notify condition: notify when any of these strings appears in the output (default: detect error lines)")]
    notify_on: Option<Vec<String>>,
    /// Background monitor regex notification condition: a matching output line triggers a notification.
    #[serde(default)]
    #[schemars(description = "Background notify regex condition: notify when an output line matches")]
    notify_regex: Option<String>,
    /// Background mode: returns async_launched immediately and notifies when done. Use for
    /// non-dependent/long-running commands (when the result is not needed right away);
    /// defaults to waiting for output synchronously.
    #[serde(default)]
    #[schemars(description = "Run in background (default false): returns a task id immediately and notifies on completion; use for commands whose result is not needed immediately")]
    background: Option<bool>,
}

pub struct BashTool {
    max_output_chars: usize,
}

impl BashTool {
    pub fn new() -> Self {
        Self { max_output_chars: DEFAULT_MAX_OUTPUT_CHARS }
    }

    /// Configured cap (settings `maxBashOutputChars`); None = default, Some(0) = no cap.
    pub fn with_max_output_chars(limit: Option<usize>) -> Self {
        Self { max_output_chars: limit.unwrap_or(DEFAULT_MAX_OUTPUT_CHARS) }
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
        "Execute a command in the local shell, returning stdout/stderr and the exit code. Prefer background:true for long-running tasks (e.g. cargo build, npm install, big test suites) — even when you need the result later: it returns async_launched immediately and tells the user the task runs in the background; continue when the completion notification arrives. Periodic commands (watch/while/until/for/tail -f) become background tasks automatically and can be given notify_on/notify_regex conditions — a hit in the output notifies (no need to wait for the command to finish). Interactive commands that need a TTY (top/htop/vim/bare ssh etc. — full-screen or session programs) are rejected. Output is truncated at a size cap (20k chars by default, settings maxBashOutputChars); for large outputs rerun with `> file` redirection and Read the file."
            .to_string()
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
        // Interactive/TTY commands (top/htop/vim/ssh etc.): reject up front to avoid garbled
        // output and terminal hijacking.
        if let Some(reason) = interactive_command_reason(&params.command) {
            return Err(ToolError::failed(reason));
        }
        let timeout = Duration::from_secs(params.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));

        // Periodic commands (watch/while/until/for/tail -f) are backgrounded automatically:
        // immediately return async_launched; background execution + per-round checks + completion notification.
        if let Some(interval) = periodic_bash_interval(&params.command) {
            return launch_background(&params, ctx, Some(interval), self.max_output_chars).await;
        }
        // Explicit backgrounding: for non-dependent/long-running commands (e.g. cargo build,
        // npm install), the main agent does not wait when the result is not needed immediately.
        if params.background.unwrap_or(false) {
            return launch_background(&params, ctx, None, self.max_output_chars).await;
        }

        let mut command = shell_command(&params.command, &ctx.cwd);
        // When the turn is interrupted (Esc), dropping the future kills the child process too,
        // leaving no orphans.
        command.kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| ToolError::failed(format!("failed to run command: {e}")))?;
        // Process-tree root = the child shell itself: on timeout the whole tree is cleaned
        // up; grandchild processes are not orphaned.
        let child_pid = child.id();

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ToolError::failed(format!("failed to run command: {e}")));
            }
            Err(_) => {
                if let Some(child_pid) = child_pid {
                    crate::platform::kill_process_tree(child_pid).await;
                }
                return Err(ToolError::failed(format!(
                    "command timed out after {}s",
                    timeout.as_secs()
                )));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut body = String::new();
        if !stdout.is_empty() {
            body.push_str(&stdout);
        }
        if !stderr.is_empty() {
            body.push_str(&stderr);
        }
        let mut text = format!("$ {}\n", params.command);
        text.push_str(&cap_output(&body, self.max_output_chars));
        text.push_str(&format!("\n[Exited with code {exit_code}]"));

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// Child shell command: its own process tree (whole tree cleaned up on timeout/cancel),
/// stdin disconnected (child must not steal TUI input), stdout/stderr through pipes.
fn shell_command(command: &str, cwd: &std::path::Path) -> tokio::process::Command {
    let mut cmd = crate::platform::shell_command(command, cwd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

/// Run a periodic command in the background: register a watchable (interval polling) +
/// spawn streaming execution. interval=None (explicit background) must also poll:
/// otherwise notify_on/notify_regex/the default Errors condition are never driven
/// and silently fail.
async fn launch_background(
    params: &BashInput,
    ctx: &ToolContext,
    interval: Option<Duration>,
    max_output_chars: usize,
) -> Result<ToolResult, ToolError> {
    let mut conditions = Vec::new();
    if let Some(patterns) = params.notify_on.clone() {
        conditions.push(crate::watch::NotifyCondition::Contains(patterns));
    }
    if let Some(re) = params.notify_regex.clone() {
        conditions.push(crate::watch::NotifyCondition::Regex(re));
    }
    if conditions.is_empty() {
        conditions.push(crate::watch::NotifyCondition::Errors);
    }
    let cell = Arc::new(BashCell::new());
    let label = format!("$ {}", params.command);
    // Round semantics for periodic commands (Idle = one round) only apply to watch/loop-style
    // commands; explicit background only uses polling to drive condition matching, staying Running.
    let periodic = interval.is_some();
    let id = ctx
        .watch
        .register_with_conditions(Box::new(BashWatch {
            cell: cell.clone(),
            label: label.clone(),
            interval: interval
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS)),
            periodic,
        }), conditions);
    let watch = ctx.watch.clone();
    let command = params.command.clone();
    let cwd = ctx.cwd.clone();
    tokio::spawn(async move {
        // Background tasks have their own lifecycle: periodic commands are not limited by a single timeout.
        match run_streaming(&command, &cwd, None, cell, watch.clone(), id, max_output_chars).await {
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

/// Streaming execution: read output line by line (updating line counts), return full text + exit code when done.
async fn run_streaming(
    command: &str,
    cwd: &std::path::Path,
    timeout: Option<Duration>,
    cell: Arc<BashCell>,
    watch: std::sync::Arc<crate::watch::WatchRegistry>,
    id: crate::watch::WatchId,
    max_output_chars: usize,
) -> Result<(String, i32), String> {
    let mut child = shell_command(command, cwd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;
    let child_pid = child.id();
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
        let watch = watch.clone();
        readers.push(tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stream);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        cell.record_line(&line);
                        watch.feed_content(id, &line);
                        if let Ok(mut b) = buf.lock() {
                            b.push_str(&line);
                        }
                    }
                }
            }
        }));
    }
    let status = match timeout {
        Some(t) => match tokio::time::timeout(t, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(format!("failed to wait: {e}")),
            Err(_) => {
                // Tree first: Windows taskkill needs the root alive to walk the tree;
                // child.kill() afterwards is a harmless no-op on both platforms.
                if let Some(child_pid) = child_pid {
                    crate::platform::kill_process_tree(child_pid).await;
                }
                let _ = child.kill().await;
                return Err(format!("command timed out after {}s", t.as_secs()));
            }
        },
        None => match child.wait().await {
            Ok(status) => status,
            Err(e) => return Err(format!("failed to wait: {e}")),
        },
    };
    // Grandchild processes may still hold stdout: guard the join with a timeout, otherwise
    // readers hang forever and the watch entry stays Running.
    for mut reader in readers {
        if tokio::time::timeout(READER_DRAIN_TIMEOUT, &mut reader)
            .await
            .is_err()
        {
            if let Some(child_pid) = child_pid {
                crate::platform::kill_process_tree(child_pid).await;
            }
            let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, &mut reader).await;
            reader.abort();
        }
    }
    let code = status.code().unwrap_or(-1);
    let text = buf.lock().map(|b| b.clone()).unwrap_or_default();
    Ok((cap_output(&text, max_output_chars), code))
}

/// Shared execution state for background Bash: a round = new output lines since the last poll.
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
    fn record_line(&self, _line: &str) {
        self.line_delta.fetch_add(1, Ordering::SeqCst);
        self.total_lines.fetch_add(1, Ordering::SeqCst);
    }
    fn poll(&self, periodic: bool) -> crate::watch::WatchPoll {
        let delta = self.line_delta.swap(0, Ordering::SeqCst);
        let total = self.total_lines.load(Ordering::SeqCst);
        if delta > 0 && periodic {
            let rounds = self.rounds.fetch_add(1, Ordering::SeqCst) + 1;
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Idle,
                detail: Some(format!("第 {rounds} 轮 · 输出 {delta} 行（累计 {total} 行）")),
                payload: None,
                signal: None,
            }
        } else {
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Running,
                detail: Some(format!(
                    "已运行 {}s · 输出 {total} 行",
                    self.started.elapsed().as_secs()
                )),
                payload: None,
                signal: None,
            }
        }
    }
}

struct BashWatch {
    cell: Arc<BashCell>,
    label: String,
    interval: Duration,
    /// Only periodic commands (watch/loop/tail -f) have round semantics; explicit
    /// background only uses polling to drive conditions.
    periodic: bool,
}

impl crate::watch::Watchable for BashWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll(self.periodic)
    }
    fn check_interval(&self) -> Option<Duration> {
        Some(self.interval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        let watch = crate::watch::WatchRegistry::new();
        ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: watch.clone(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }
    }

    #[tokio::test]
    async fn explicit_background_non_periodic_command_notifies() {
        use crate::watch::WatchState;

        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: watch.clone(),
            http: reqwest::Client::new(),
                        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let tool = BashTool::new();
        let result = tool
            .call(
                serde_json::json!({"command": "sleep 0.4; echo finished", "background": true}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.contains("async_launched"), "launched: {text}");
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
        assert!(done, "explicit background reaches Done");
        let notes = watch.consume_notifications();
        assert!(
            notes.iter().any(|n| n.contains("finished")),
            "output in notification: {notes:?}"
        );
    }

    #[tokio::test]
    async fn periodic_command_backgrounds_and_notifies() {
        use crate::watch::WatchState;

        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: watch.clone(),
            http: reqwest::Client::new(),
                        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let tool = BashTool::new();
        #[cfg(unix)]
        let command = "for i in 1 2 3; do echo tick; sleep 0.1; done";
        #[cfg(windows)]
        let command = "for ($i=0; $i -lt 3; $i++) { echo tick; Start-Sleep -Milliseconds 100 }";
        let result = tool
            .call(serde_json::json!({"command": command}), &ctx)
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.contains("async_launched"), "launched: {text}");
        // Background task completion → Done event + notification contains output.
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
            periodic_bash_interval("watch --interval 5 ls"),
            Some(std::time::Duration::from_secs(5))
        );
        assert_eq!(periodic_bash_interval("ls"), None);
    }

    #[test]
    fn output_cap_truncates_and_keeps_hint() {
        let body = "x".repeat(300);
        let capped = cap_output(&body, 100);
        assert!(capped.contains("300 characters total"), "{capped}");
        assert!(capped.contains("showing first 100"), "{capped}");
        assert!(capped.starts_with(&"x".repeat(100)));
        // 无上限（0）时原样返回；未超限时原样返回。
        assert_eq!(cap_output(&body, 0), body);
        assert_eq!(cap_output(&body, 300), body);
    }

    #[tokio::test]
    async fn oversized_output_truncated_in_tool_result() {
        #[cfg(unix)]
        let command = "python3 -c \"print('x' * 300)\"";
        #[cfg(windows)]
        let command = "'x' * 300";
        let tool = BashTool::with_max_output_chars(Some(100));
        let result = tool.call(serde_json::json!({ "command": command }), &test_ctx()).await.unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.contains("[Content truncated: 30"), "{text}");
        assert!(text.contains("showing first 100"), "{text}");
        assert!(text.contains("[Exited with code 0]"), "{text}");
        let stdout = text.splitn(2, '\n').nth(1).unwrap_or_default();
        assert_eq!(stdout.chars().take_while(|c| *c == 'x').count(), 100);
    }

    /// Interactive/TTY commands are rejected: full-screen TUIs, editors, bare shell/REPL,
    /// ssh without a command, container -it, and foreground tmux are all refused; batch
    /// snapshots/commands with arguments/scripted uses are allowed.
    #[test]
    fn rejects_interactive_tty_commands() {
        let rejected = [
            "top",
            "htop",
            "btop -d 2",
            "vim README.md",
            "nano file.txt",
            "nvim -p a b",
            "emacs",
            "fzf",
            "lazygit",
            "ranger",
            "screen",
            "python3",
            "node",
            "irb",
            "sqlite3 data.db",
            "psql mydb",
            "bc",
            "ssh example.com",
            "ssh -p 2222 user@example.com",
            "ssh -t example.com 'ls'",
            "sudo -i",
            "sudo -s",
            "sudo htop",
            "sudo",
            "docker exec -it app bash",
            "docker exec -i -t app bash",
            "kubectl exec -it pod -- bash",
            "docker attach app",
            "tmux",
            "tmux new",
            "tmux attach",
            "gdb ./prog",
        ];
        for cmd in rejected {
            assert!(
                interactive_command_reason(cmd).is_some(),
                "应当拒绝: {cmd}"
            );
        }

        let allowed = [
            "ls -la",
            "echo hello",
            "git log --oneline",
            "top -b -n 1",
            "top -b",
            "gdb -batch -ex 'run' ./prog",
            "python3 test.py",
            "python3 -c 'print(1)'",
            "bash -c 'echo hi'",
            "node app.js",
            "ssh example.com 'ls -la'",
            "ssh -N -L 8080:localhost:80 example.com",
            "ssh -f -N example.com",
            "env A=1 echo hi",
            "env A=1 python3 script.py",
            "sudo rm -rf /tmp/x",
            "sudo -u root ls",
            "docker run -d nginx",
            "docker build -t app .",
            "kubectl logs pod",
            "tmux new -d -s app",
            "tmux send-keys -t app 'x' Enter",
            "tmux capture-pane -t app",
            "kubectl exec pod -- ls",
            "sqlite3 data.db \"SELECT 1\"",
            "sqlite3 data.db < dump.sql",
            "sqlite3 --version",
            "psql -d mydb -c 'SELECT 1'",
            "psql mydb < dump.sql",
            "psql --version",
            "psql -l",
            "mysql -e 'SHOW TABLES'",
            "mysql db < dump.sql",
            "mongosh --eval 'db.runCommand({ping:1})'",
            "mongosh mongo.js",
            "redis-cli GET key",
            "redis-cli --version",
        ];
        for cmd in allowed {
            assert_eq!(
                interactive_command_reason(cmd),
                None,
                "不应拒绝: {cmd}"
            );
        }
    }

    /// Regression: flag parsing once panicked on out-of-bounds indexing when flags
    /// exhausted the tokens (`sudo -v` / `sudo -k` / empty command).
    #[test]
    fn flag_only_wrapper_commands_do_not_panic() {
        // sudo variants that prompt for a password: rejected without panicking.
        for cmd in ["sudo -v", "sudo -u root", "sudo -p prompt", "doas -"] {
            assert!(
                interactive_command_reason(cmd).is_some(),
                "应当拒绝且不 panic: {cmd}"
            );
        }
        // No password prompt / no base command: allowed without panicking.
        for cmd in ["", "   ", "sudo -k", "sudo -n", "sudo -K", "env", "nohup", "exec"] {
            assert_eq!(
                interactive_command_reason(cmd),
                None,
                "不应拒绝且不 panic: {cmd:?}"
            );
        }
    }

    /// Timeout: the whole process tree is cleaned up; grandchild processes are not orphaned.
    /// Unix variant: shell job syntax + `/bin/sh kill -0` liveness check.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_whole_process_group() {
        let marker = std::env::temp_dir()
            .join(format!("bingo-pgroup-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        // The grandchild writes its pid then sleeps; the parent shell also sleeps to trigger the timeout.
        let command = format!(
            "( sleep 30 & echo $! > {} ); sleep 30",
            marker.to_string_lossy()
        );
        let err = BashTool::new()
            .call(
                serde_json::json!({"command": command, "timeout": 1}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        // Group cleanup is an asynchronous signal: give the kernel a moment.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(!pid.is_empty(), "孙进程应已写下 pid");
        let alive = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&marker);
        assert!(!alive, "孙进程 {pid} 应随进程组一起被清理");
    }

    /// Windows: `taskkill /T` removes the whole process tree. Tested directly against
    /// `kill_process_tree` (not via the BashTool timeout): PowerShell 5.1 cold start on
    /// a loaded CI runner can exceed any reasonable timeout, so the tool timeout path
    /// isn't the thing under test here.
    #[cfg(windows)]
    #[tokio::test]
    async fn kill_process_tree_removes_grandchildren() {
        let marker = std::env::temp_dir()
            .join(format!("bingo-ptree-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        // PowerShell: spawn a hidden cmd that pings (stays alive), write its pid, then sleep.
        let script = format!(
            "$p = Start-Process cmd -ArgumentList '/c','ping -n 30 127.0.0.1 > nul' -PassThru -WindowStyle Hidden; $p.Id | Out-File -FilePath '{}' -Encoding ascii; Start-Sleep 30",
            marker.to_string_lossy()
        );
        let mut child = tokio::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn powershell");
        let root_pid = child.id().expect("powershell pid");
        // Poll for the grandchild pid: no deadline pressure beyond CI slowness (30s).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut pid = String::new();
        while std::time::Instant::now() < deadline {
            pid = std::fs::read_to_string(&marker)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !pid.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(!pid.is_empty(), "孙进程应已写下 pid");
        crate::platform::kill_process_tree(root_pid).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let alive = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid))
            .unwrap_or(false);
        let _ = std::fs::remove_file(&marker);
        let _ = child.kill().await;
        assert!(!alive, "孙进程 {pid} 应随进程树一起被清理");
    }

    /// Regression: background:true (non-periodic) must also drive condition matching;
    /// notify_on no longer silently fails.
    #[tokio::test]
    async fn explicit_background_conditions_fire() {
        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: watch.clone(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let result = BashTool::new()
            .call(
                serde_json::json!({
                    "command": "echo BOOM_MARKER; sleep 0.2",
                    "background": true,
                    "notify_on": ["BOOM_MARKER"],
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.content.as_str().unwrap_or_default().contains("async_launched"));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut signalled = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if watch
                .consume_notifications()
                .iter()
                .any(|n| n.contains("BOOM_MARKER"))
            {
                signalled = true;
                break;
            }
        }
        assert!(signalled, "notify_on 条件应在后台任务上触发信号");
    }

    /// Interactive commands are rejected at the Bash tool layer (the model path is covered too).
    #[tokio::test]
    async fn bash_tool_refuses_interactive_commands() {
        let ctx = ToolContext {
            home: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        let tool = BashTool::new();
        let err = tool
            .call(serde_json::json!({"command": "htop"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("TTY"),
            "拒绝原因说明 TTY: {err}"
        );
    }
}
