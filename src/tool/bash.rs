use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use async_trait::async_trait;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_OUTPUT_MAX_CHARS: usize = 48_000;
pub const MAX_OUTPUT_MAX_CHARS: usize = 48_000;
/// Default check interval for periodic commands (when no explicit -n is given).
pub const DEFAULT_WATCH_INTERVAL_SECS: u64 = 5;
/// Upper bound for waiting on readers to drain after the command exits (so we don't
/// wait forever if grandchild processes still hold the pipe).
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

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
                "sudo" | "doas" => Some(
                    "sudo/doas needs an interactive password prompt (TTY); rejected".to_string(),
                ),
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
                        "sudo interactive login shell ({flag}) needs a TTY; rejected"
                    ));
                }
                // These flags never prompt for a password (-n fails immediately, -k/-K only clear timestamps).
                if matches!(
                    flag,
                    "-n" | "--non-interactive" | "-k" | "-K" | "-V" | "--version"
                ) {
                    non_prompting = true;
                }
                if matches!(
                    flag,
                    "-u" | "-g"
                        | "-C"
                        | "-p"
                        | "-D"
                        | "-R"
                        | "-T"
                        | "-r"
                        | "-t"
                        | "-U"
                        | "-S"
                        | "-P"
                        | "-h"
                ) && i < tokens.len()
                {
                    i += 1;
                }
            }
            if i >= tokens.len() {
                return (!non_prompting).then(|| {
                    "sudo/doas needs an interactive password prompt (TTY); rejected".to_string()
                });
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
        "top", "htop", "btop", "bpytop", "bashtop", "btm", "nmon", "glances", "s-tui", "gtop",
        "vtop", "ktop", "ctop", "ytop",
    ];
    if MONITORS.contains(&name) {
        let batch = rest
            .iter()
            .any(|a| matches!(*a, "-b" | "-batch" | "--batch"));
        if !batch {
            return Some(format!(
                "{name} is a full-screen interactive monitor (needs a TTY); rejected. A one-shot snapshot works via `{name} -b -n 1`"
            ));
        }
    }
    const EDITORS: &[&str] = &[
        "vim", "vi", "nvim", "nano", "emacs", "micro", "pico", "mg", "ed", "ex", "kak", "kakoune",
        "helix", "hx", "ne", "zile", "joe",
    ];
    const FILE_MANAGERS: &[&str] = &[
        "ranger",
        "lf",
        "yazi",
        "joshuto",
        "mc",
        "midnight-commander",
    ];
    const TUI_TOOLS: &[&str] = &[
        "lazygit",
        "tig",
        "lazydocker",
        "k9s",
        "kdash",
        "screen",
        "fzf",
    ];
    if EDITORS.contains(&name) {
        return Some(format!(
            "{name} is an interactive editor (needs a TTY); rejected"
        ));
    }
    if FILE_MANAGERS.contains(&name) {
        return Some(format!(
            "{name} is an interactive file manager (needs a TTY); rejected"
        ));
    }
    if TUI_TOOLS.contains(&name) {
        return Some(format!(
            "{name} is an interactive TUI program (needs a TTY); rejected"
        ));
    }
    // gdb: without -batch it is an interactive debugger.
    if name == "gdb" && !rest.iter().any(|a| matches!(*a, "-batch" | "--batch")) {
        return Some(
            "the gdb debugger needs a TTY; rejected. Batch mode works via `gdb -batch -ex ...`"
                .to_string(),
        );
    }
    // Bare shell/REPL: exits immediately without input (pointless); allow when given
    // arguments (bash -c / python x.py). DB clients have separate rules: connection args
    // without an execution flag/script/SQL positional argument = REPL.
    const REPLS: &[&str] = &[
        "bash",
        "sh",
        "zsh",
        "fish",
        "ksh",
        "dash",
        "elvish",
        "xonsh",
        "python",
        "python2",
        "python3",
        "ipython",
        "pypy",
        "node",
        "deno",
        "bun",
        "ruby",
        "irb",
        "perl",
        "php",
        "lua",
        "luajit",
        "bc",
        "dc",
        "sbcl",
        "ghci",
        "powershell",
        "pwsh",
        "cmd",
        "cmd.exe",
        "powershell.exe",
    ];
    if REPLS.contains(&name) && rest.is_empty() {
        return Some(format!(
            "{name} is an interactive shell/REPL (needs a TTY); rejected. Running it with arguments (e.g. `{name} -c '...'`) is fine"
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
                !flags(&[
                    "-c",
                    "-f",
                    "-l",
                    "--command",
                    "--file",
                    "--list",
                    "--version",
                    "--help",
                ]) && !has_stdin
                    && positional.len() <= 1
            }
            "mysql" => {
                !flags(&["-e", "--execute", "-f", "--force", "--version", "--help"]) && !has_stdin
            }
            "mongosh" => {
                !flags(&["--eval", "--version", "--help"])
                    && !has_stdin
                    && !positional.iter().any(|a| a.ends_with(".js"))
            }
            // redis-cli: no positional arguments means an interactive prompt.
            _ => !flags(&["--version", "--help"]) && positional.is_empty() && !has_stdin,
        };
        if interactive {
            return Some(format!(
                "{name} is an interactive client (needs a TTY); rejected. Pass an execution flag or a script (e.g. `{name} -c '...'`) instead"
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
                "-p" | "-l"
                    | "-i"
                    | "-o"
                    | "-F"
                    | "-J"
                    | "-L"
                    | "-R"
                    | "-D"
                    | "-W"
                    | "-m"
                    | "-c"
                    | "-e"
                    | "-b"
                    | "-K"
                    | "-I"
                    | "-O"
                    | "-Q"
                    | "-S"
                    | "-w"
                    | "-E"
                    | "-G"
                    | "-g"
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
                "ssh interactive session (takes /dev/tty for a password prompt or a remote shell); rejected. Pass a remote command (`ssh host 'cmd'`) instead"
                    .to_string(),
            );
        }
    }
    // Container exec/run -it / attach: interactive shell.
    if matches!(
        name,
        "docker" | "nerdctl" | "podman" | "kubectl" | "docker-compose"
    ) {
        let sub = rest.first().copied().unwrap_or("");
        if sub == "attach" {
            return Some(format!(
                "{name} attach is an interactive session (needs a TTY); rejected"
            ));
        }
        if matches!(sub, "exec" | "run") {
            let interactive = rest
                .iter()
                .any(|a| matches!(*a, "-it" | "-ti" | "--interactive"))
                || (rest.contains(&"-i") && rest.contains(&"-t"));
            if interactive {
                return Some(format!(
                    "{name} {sub} -it is an interactive session (needs a TTY); rejected"
                ));
            }
        }
    }
    // tmux attach / foreground new (no -d): needs a TTY; scripted uses like send-keys/capture-pane are allowed.
    if name == "tmux" {
        let sub = rest.first().copied().unwrap_or("");
        if matches!(sub, "attach" | "a" | "attach-session") {
            return Some("tmux attach needs a TTY; rejected. `tmux new -d` starts a detached session instead".to_string());
        }
        if rest.is_empty() || (matches!(sub, "new" | "new-session") && !rest.contains(&"-d")) {
            return Some("a foreground tmux session needs a TTY; rejected. `tmux new -d` starts a detached session instead".to_string());
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
    #[schemars(
        description = "Background notify condition: notify when any of these strings appears in the output (default: detect error lines)"
    )]
    notify_on: Option<Vec<String>>,
    /// Background monitor regex notification condition: a matching output line triggers a notification.
    #[serde(default)]
    #[schemars(
        description = "Background notify regex condition: notify when an output line matches"
    )]
    notify_regex: Option<String>,
    /// Background mode: returns async_launched immediately and notifies when done. Use for
    /// non-dependent/long-running commands (when the result is not needed right away);
    /// defaults to waiting for output synchronously.
    #[serde(default)]
    #[schemars(
        description = "Run in background (default false): returns a task id immediately and notifies on completion; use for commands whose result is not needed immediately"
    )]
    background: Option<bool>,
}

pub struct BashTool {
    output_max_chars: usize,
}

impl BashTool {
    pub fn new() -> Self {
        Self::with_output_max_chars(DEFAULT_OUTPUT_MAX_CHARS)
    }

    pub fn with_output_max_chars(output_max_chars: usize) -> Self {
        Self {
            output_max_chars: output_max_chars.min(MAX_OUTPUT_MAX_CHARS),
        }
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
        // #42: name the real executor and its dialect — the tool name `Bash`
        // primes POSIX syntax even when the resolved shell is PowerShell.
        use crate::platform::ShellDialect;
        let shell = crate::platform::shell();
        let syntax = match crate::platform::shell_dialect() {
            ShellDialect::Posix => String::new(),
            ShellDialect::PowerShell => {
                " Commands are interpreted by PowerShell — use PowerShell syntax, not POSIX."
                    .to_string()
            }
            ShellDialect::Cmd => {
                " Commands are interpreted by cmd.exe — use cmd syntax, not POSIX.".to_string()
            }
            ShellDialect::Unknown => {
                format!(" Commands are interpreted by {shell} — match its syntax.")
            }
        };
        format!(
            "Execute a command in the local shell ({shell}), returning stdout/stderr and the exit code.{syntax} Prefer background:true for long-running tasks (e.g. cargo build, npm install, big test suites) — even when you need the result later: it returns async_launched immediately and tells the user the task runs in the background; continue when the completion notification arrives. Periodic commands (watch/while/until/for/tail -f) become background tasks automatically and can be given notify_on/notify_regex conditions — a hit in the output notifies (no need to wait for the command to finish). Interactive commands that need a TTY (top/htop/vim/bare ssh etc. — full-screen or session programs) are rejected."
        )
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
            return launch_background(&params, ctx, Some(interval), self.output_max_chars).await;
        }
        // Explicit backgrounding: for non-dependent/long-running commands (e.g. cargo build,
        // npm install), the main agent does not wait when the result is not needed immediately.
        if params.background.unwrap_or(false) {
            return launch_background(&params, ctx, None, self.output_max_chars).await;
        }

        let output = run_command(&params.command, &ctx.cwd, timeout, self.output_max_chars).await?;
        let mut text = format!("$ {}\n{}", params.command, output.text);
        text.push_str(&format!("\n[Exited with code {}]", output.exit_code));

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: output.exit_code != 0,
            diff: None,
        })
    }
}

fn truncation_note(total: usize, max_chars: usize) -> String {
    format!(
        "\n[Content truncated: {total} characters total, showing first {max_chars}. Use Read on a redirected output file for the complete content.]"
    )
}

struct CommandOutput {
    text: String,
    exit_code: i32,
}

async fn run_command(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
    output_max_chars: usize,
) -> Result<CommandOutput, ToolError> {
    let mut child = shell_command(command, cwd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ToolError::failed(format!("failed to run command: {e}")))?;
    let child_pid = child.id();
    let output = Arc::new(Mutex::new(BoundedOutput::new(output_max_chars)));
    let readers = spawn_output_readers(&mut child, output.clone(), None);
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(ToolError::failed(format!("failed to run command: {e}"))),
        Err(_) => {
            if let Some(child_pid) = child_pid {
                crate::platform::kill_process_tree(child_pid).await;
            }
            let _ = child.kill().await;
            return Err(ToolError::failed(format!(
                "command timed out after {}s",
                timeout.as_secs()
            )));
        }
    };
    drain_readers(readers, child_pid).await;
    Ok(CommandOutput {
        text: output.lock().map(|b| b.finish()).unwrap_or_default(),
        exit_code: status.code().unwrap_or(-1),
    })
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
    output_max_chars: usize,
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
    let id = ctx.watch.register_with_conditions(
        Box::new(BashWatch {
            cell: cell.clone(),
            label: label.clone(),
            interval: interval.unwrap_or_else(|| Duration::from_secs(DEFAULT_WATCH_INTERVAL_SECS)),
            periodic,
        }),
        conditions,
        ctx.instance.clone(),
    );
    let watch = ctx.watch.clone();
    let command = params.command.clone();
    let cwd = ctx.cwd.clone();
    tokio::spawn(async move {
        // Background tasks have their own lifecycle: periodic commands are not limited by a single timeout.
        match run_streaming(
            &command,
            &cwd,
            None,
            output_max_chars,
            cell,
            watch.clone(),
            id,
        )
        .await
        {
            Ok((text, code)) => {
                watch.set_state(
                    id,
                    crate::watch::WatchState::Done,
                    Some(format!("exit code {code}")),
                    Some(serde_json::json!(text)),
                );
            }
            Err(e) => {
                watch.set_state(id, crate::watch::WatchState::Failed, Some(e), None);
            }
        }
    });
    Ok(ToolResult {
        content: serde_json::Value::String(
            serde_json::json!({
                "status": "async_launched",
                "task_id": id.0,
                "label": label,
                "note": "periodic command is now running in the background; state changes and completion notifications will arrive",
            })
            .to_string(),
        ),
        is_error: false,
        diff: None,
    })
}

/// Streaming execution: read output line by line (updating line counts), return full text + exit code when done.
async fn run_streaming(
    command: &str,
    cwd: &std::path::Path,
    timeout: Option<Duration>,
    output_max_chars: usize,
    cell: Arc<BashCell>,
    watch: std::sync::Arc<crate::watch::WatchRegistry>,
    id: crate::watch::WatchId,
) -> Result<(String, i32), String> {
    let mut child = shell_command(command, cwd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;
    let child_pid = child.id();
    let buf = Arc::new(Mutex::new(BoundedOutput::new(output_max_chars)));
    let readers = spawn_output_readers(
        &mut child,
        buf.clone(),
        Some((cell.clone(), watch.clone(), id)),
    );
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
    drain_readers(readers, child_pid).await;
    let code = status.code().unwrap_or(-1);
    let text = buf.lock().map(|b| b.finish()).unwrap_or_default();
    Ok((text, code))
}

type OutputReader = tokio::task::JoinHandle<()>;

fn spawn_output_readers(
    child: &mut tokio::process::Child,
    output: Arc<Mutex<BoundedOutput>>,
    watch: Option<(
        Arc<BashCell>,
        Arc<crate::watch::WatchRegistry>,
        crate::watch::WatchId,
    )>,
) -> Vec<OutputReader> {
    let streams: Vec<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    .collect();
    streams
        .into_iter()
        .map(|mut stream| {
            let output = output.clone();
            let watch = watch.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;

                let mut bytes = [0u8; 8 * 1024];
                let mut utf8_tail = Vec::new();
                loop {
                    let read = match stream.read(&mut bytes).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    utf8_tail.extend_from_slice(&bytes[..read]);
                    loop {
                        match std::str::from_utf8(&utf8_tail) {
                            Ok(_) => {
                                if !utf8_tail.is_empty() {
                                    feed_bash_output(&utf8_tail, &output, watch.as_ref());
                                    utf8_tail.clear();
                                }
                                break;
                            }
                            Err(error) if error.error_len().is_none() => {
                                let valid = error.valid_up_to();
                                if valid > 0 {
                                    feed_bash_output(&utf8_tail[..valid], &output, watch.as_ref());
                                    utf8_tail.drain(..valid);
                                }
                                break;
                            }
                            Err(error) => {
                                let valid = error.valid_up_to();
                                if valid > 0 {
                                    feed_bash_output(&utf8_tail[..valid], &output, watch.as_ref());
                                }
                                if let Ok(mut output) = output.lock() {
                                    output.mark_invalid_utf8();
                                    output.push("�");
                                }
                                utf8_tail.drain(..valid + error.error_len().unwrap_or(1));
                            }
                        }
                    }
                }
                if !utf8_tail.is_empty()
                    && let Ok(mut output) = output.lock()
                {
                    output.mark_invalid_utf8();
                    output.push("�");
                }
            })
        })
        .collect()
}

fn feed_bash_output(
    bytes: &[u8],
    output: &Arc<Mutex<BoundedOutput>>,
    watch: Option<&(
        Arc<BashCell>,
        Arc<crate::watch::WatchRegistry>,
        crate::watch::WatchId,
    )>,
) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    if let Some((cell, registry, id)) = watch {
        cell.record_text(text);
        registry.feed_content(*id, text);
    }
    if let Ok(mut output) = output.lock() {
        output.push(text);
    }
}

async fn drain_readers(readers: Vec<OutputReader>, child_pid: Option<u32>) {
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
}

struct BoundedOutput {
    text: String,
    max_chars: usize,
    total_chars: usize,
    saw_invalid_utf8: bool,
}

impl BoundedOutput {
    fn new(max_chars: usize) -> Self {
        Self {
            text: String::new(),
            max_chars,
            total_chars: 0,
            saw_invalid_utf8: false,
        }
    }

    fn push(&mut self, text: &str) {
        let remaining = self.max_chars.saturating_sub(self.text.chars().count());
        let mut chars = text.chars();
        let mut kept = 0usize;
        self.text
            .extend(chars.by_ref().take(remaining).inspect(|_| {
                kept += 1;
            }));
        self.total_chars = self
            .total_chars
            .saturating_add(kept)
            .saturating_add(chars.count());
    }

    fn mark_invalid_utf8(&mut self) {
        self.saw_invalid_utf8 = true;
    }

    fn finish(&self) -> String {
        let mut text = self.text.clone();
        if self.total_chars > self.max_chars {
            text.push_str(&truncation_note(self.total_chars, self.max_chars));
        }
        if self.saw_invalid_utf8 {
            text.push_str("\n[Invalid UTF-8 replaced while decoding command output]");
        }
        text
    }
}

/// Shared execution state for background Bash: a round = new output lines since the last poll.
struct BashCell {
    started: Instant,
    rounds: AtomicUsize,
    line_delta: AtomicUsize,
    total_lines: AtomicUsize,
    partial_line: std::sync::atomic::AtomicBool,
}

impl BashCell {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            rounds: AtomicUsize::new(0),
            line_delta: AtomicUsize::new(0),
            total_lines: AtomicUsize::new(0),
            partial_line: std::sync::atomic::AtomicBool::new(false),
        }
    }
    fn record_text(&self, text: &str) {
        let complete = text.bytes().filter(|byte| *byte == b'\n').count();
        let had_partial = self.partial_line.load(Ordering::SeqCst);
        let has_partial = !text.ends_with('\n');
        self.partial_line.store(has_partial, Ordering::SeqCst);
        let lines = complete + usize::from(has_partial && !had_partial);
        if lines > 0 {
            self.line_delta.fetch_add(lines, Ordering::SeqCst);
            self.total_lines.fetch_add(lines, Ordering::SeqCst);
        }
    }
    fn poll(&self, periodic: bool) -> crate::watch::WatchPoll {
        let delta = self.line_delta.swap(0, Ordering::SeqCst);
        let total = self.total_lines.load(Ordering::SeqCst);
        if delta > 0 && periodic {
            let rounds = self.rounds.fetch_add(1, Ordering::SeqCst) + 1;
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Idle,
                detail: Some(format!(
                    "round {rounds} · {delta} lines output ({total} total)"
                )),
                payload: None,
                signal: None,
            }
        } else {
            crate::watch::WatchPoll {
                state: crate::watch::WatchState::Running,
                detail: Some(format!(
                    "running for {}s · {total} lines output",
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

    #[test]
    fn configured_output_limit_cannot_exceed_the_model_result_budget() {
        let tool = BashTool::with_output_max_chars(usize::MAX);
        assert_eq!(tool.output_max_chars, MAX_OUTPUT_MAX_CHARS);
    }

    /// #42: the description names the real executor so the tool name `Bash`
    /// does not prime POSIX syntax against a PowerShell/cmd executor.
    #[test]
    fn description_names_the_resolved_shell() {
        let text = BashTool::new().description();
        assert!(text.contains(crate::platform::shell()), "{text}");
        if crate::platform::shell_dialect() == crate::platform::ShellDialect::PowerShell {
            assert!(text.contains("use PowerShell syntax, not POSIX"), "{text}");
        }
    }

    #[test]
    fn bounded_output_counts_characters_without_growing_past_limit() {
        let mut output = BoundedOutput::new(3);
        output.push("❤❤");
        output.push("❤❤❤");
        let text = output.finish();
        assert!(
            text.starts_with("❤❤❤\n[Content truncated: 5 characters total, showing first 3."),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn utf8_split_across_pipe_reads_is_preserved() {
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
            instance: None,
        };
        let command = r#"python3 -c 'import os,time; b="❤".encode(); os.write(1,b[:1]); time.sleep(.05); os.write(1,b[1:])'"#;
        let result = BashTool::new()
            .call(serde_json::json!({"command": command}), &ctx)
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default();
        assert!(text.contains("❤"), "{text}");
        assert!(!text.contains("Invalid UTF-8"), "{text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn newline_free_output_is_streamed_through_the_cap() {
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
            instance: None,
        };
        let result = BashTool::with_output_max_chars(32)
            .call(
                serde_json::json!({"command": r#"python3 -c 'print("x" * 100000, end="")'"#}),
                &ctx,
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default();
        assert!(text.contains("showing first 32"), "{text}");
        assert!(text.chars().count() < 512, "{}", text.chars().count());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn invalid_utf8_output_is_lossy_not_dropped() {
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
            instance: None,
        };
        let result = BashTool::new()
            .call(serde_json::json!({"command": r"printf '\377ok\n'"}), &ctx)
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default();
        assert!(text.contains("�ok"), "{text}");
        assert!(text.contains("[Invalid UTF-8 replaced"), "{text}");
    }

    #[tokio::test]
    async fn truncates_stdout_and_stderr_at_configured_limit() {
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
            instance: None,
        };
        #[cfg(unix)]
        let command = "printf 12345; printf 67890 >&2";
        #[cfg(windows)]
        let command = "[Console]::Out.Write('12345'); [Console]::Error.Write('67890')";
        let result = BashTool::with_output_max_chars(7)
            .call(serde_json::json!({"command": command}), &ctx)
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default();
        let body = text
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or_default();
        let shown = body.lines().next().unwrap_or_default();
        assert_eq!(shown.chars().count(), 7, "{text}");
        assert!(shown.chars().all(|c| c.is_ascii_digit()), "{text}");
        assert!(
            text.contains("[Content truncated: 10 characters total, showing first 7."),
            "{text}"
        );
        assert!(
            text.contains("Use Read on a redirected output file"),
            "{text}"
        );
        assert!(text.ends_with("[Exited with code 0]"), "{text}");
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
            instance: None,
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
        let notes = watch.consume_notifications(None);
        assert!(
            notes.iter().any(|n| n.contains("finished")),
            "output in notification: {notes:?}"
        );
    }

    #[tokio::test]
    async fn non_zero_exit_is_flagged_as_error() {
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
            instance: None,
        };
        let tool = BashTool::new();
        let result = tool
            .call(serde_json::json!({"command": "sh -c 'exit 7'"}), &ctx)
            .await
            .unwrap();
        assert!(result.is_error, "non-zero exit must be an error result");
        let text = result.content.as_str().unwrap();
        assert!(text.contains("[Exited with code 7]"), "output: {text}");
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
            instance: None,
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
        let notes = watch.consume_notifications(None);
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
                "should be rejected: {cmd}"
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
                "should not be rejected: {cmd}"
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
                "should be rejected without panicking: {cmd}"
            );
        }
        // No password prompt / no base command: allowed without panicking.
        for cmd in [
            "", "   ", "sudo -k", "sudo -n", "sudo -K", "env", "nohup", "exec",
        ] {
            assert_eq!(
                interactive_command_reason(cmd),
                None,
                "should not be rejected without panicking: {cmd:?}"
            );
        }
    }

    /// Timeout: the whole process tree is cleaned up; grandchild processes are not orphaned.
    /// Unix variant: shell job syntax + `/bin/sh kill -0` liveness check.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_whole_process_group() {
        let marker = std::env::temp_dir().join(format!("bingo-pgroup-{}.pid", std::process::id()));
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
            instance: None,
        };
        // The grandchild writes its pid then sleeps; the parent shell also sleeps to trigger the timeout.
        let command = format!(
            "( sleep 30 & echo $! > {} ); sleep 30",
            marker.to_string_lossy()
        );
        let err = BashTool::new()
            .call(serde_json::json!({"command": command, "timeout": 1}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        // Group cleanup is an asynchronous signal: give the kernel a moment.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(!pid.is_empty(), "grandchild should have written its pid");
        let alive = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("kill -0 {pid} 2>/dev/null"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&marker);
        assert!(
            !alive,
            "grandchild {pid} should be cleaned up with the process group"
        );
    }

    /// Windows: `taskkill /T` removes the whole process tree. Tested directly against
    /// `kill_process_tree` (not via the BashTool timeout): PowerShell 5.1 cold start on
    /// a loaded CI runner can exceed any reasonable timeout, so the tool timeout path
    /// isn't the thing under test here.
    #[cfg(windows)]
    #[tokio::test]
    async fn kill_process_tree_removes_grandchildren() {
        let marker = std::env::temp_dir().join(format!("bingo-ptree-{}.pid", std::process::id()));
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
        assert!(!pid.is_empty(), "grandchild should have written its pid");
        crate::platform::kill_process_tree(root_pid).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let alive = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid))
            .unwrap_or(false);
        let _ = std::fs::remove_file(&marker);
        let _ = child.kill().await;
        assert!(
            !alive,
            "grandchild {pid} should be cleaned up with the process tree"
        );
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
            instance: None,
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
        assert!(
            result
                .content
                .as_str()
                .unwrap_or_default()
                .contains("async_launched")
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut signalled = false;
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if watch
                .consume_notifications(None)
                .iter()
                .any(|n| n.contains("BOOM_MARKER"))
            {
                signalled = true;
                break;
            }
        }
        assert!(
            signalled,
            "notify_on should fire a signal on background tasks"
        );
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
            instance: None,
        };
        let tool = BashTool::new();
        let err = tool
            .call(serde_json::json!({"command": "htop"}), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("TTY"),
            "rejection reason should mention TTY: {err}"
        );
    }
}
