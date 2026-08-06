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
/// 命令结束后等 reader 收尾的上限（孙进程仍持有管道时不无限等）。
const READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// 需要 TTY 的交互式命令拒绝原因（`!` 命令与 Bash 工具共用）。
/// bingo 子进程 stdin/stdout 均为管道：全屏 TUI（top/htop/vim）输出乱码，
/// ssh/fzf/sudo 等会直连 /dev/tty 抢占终端（raw mode 下画面被撕毁），
/// 裸 shell/REPL 无输入即退出（无意义）。命中返回拒绝原因与可用替代。
/// 交互式终端应用无法被 agent 的 bash 工具驱动。
pub fn interactive_command_reason(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    // 解包常见包装命令（sudo/env/nohup/command/exec/doas）。
    while matches!(
        tokens.get(i).copied(),
        Some("sudo" | "doas" | "env" | "nohup" | "command" | "exec")
    ) {
        let wrapper = tokens.get(i).copied()?;
        i += 1;
        if i >= tokens.len() {
            return match wrapper {
                // 裸 sudo/doas 会占 /dev/tty 问口令。
                "sudo" | "doas" => {
                    Some("sudo/doas 需要交互口令（TTY），已拒绝".to_string())
                }
                _ => None,
            };
        }
        if matches!(wrapper, "sudo" | "doas") {
            // sudo 旗标：-i/-s 交互登录；取值旗标连值一起跳过。
            // 旗标可能耗尽 tokens（`sudo -v`）：全程 get，末尾无命令时按裸 sudo 处理。
            let mut non_prompting = false;
            while let Some(flag) = tokens.get(i).copied().filter(|t| t.starts_with('-')) {
                i += 1;
                if matches!(flag, "-i" | "-s") {
                    return Some(format!(
                        "sudo 交互登录 shell（{flag}）需要 TTY，已拒绝"
                    ));
                }
                // 这些旗标不会弹口令提示（-n 直接失败，-k/-K 只清时间戳）。
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
            // env 的 VAR=value 赋值不是命令。
            while tokens.get(i).is_some_and(|t| t.contains('=')) {
                i += 1;
            }
            if i >= tokens.len() {
                return None;
            }
        }
    }
    // 空命令（或只有包装词）无基命令可判：放行给 shell。
    let base = tokens.get(i).copied()?;
    let name = std::path::Path::new(base)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(base);
    let rest = &tokens[i + 1..];

    // 全屏系统监控：允许 -b/--batch（一次性快照，非交互）。
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
    // gdb：无 -batch 即交互调试器。
    if name == "gdb"
        && !rest.iter().any(|a| matches!(*a, "-batch" | "--batch"))
    {
        return Some("gdb 调试器需要 TTY，已拒绝。批处理可用 `gdb -batch -ex ...`".to_string());
    }
    // 裸 shell/REPL：无输入立即退出（无意义）；带参数（bash -c / python x.py）放行。
    // DB 客户端另有规则：只有连接参数而无执行旗标/脚本/SQL 位置参数 = REPL。
    const REPLS: &[&str] = &[
        "bash", "sh", "zsh", "fish", "ksh", "dash", "elvish", "xonsh", "python", "python2",
        "python3", "ipython", "pypy", "node", "deno", "bun", "ruby", "irb", "perl", "php",
        "lua", "luajit", "bc", "dc", "sbcl", "ghci",
    ];
    if REPLS.contains(&name) && rest.is_empty() {
        return Some(format!(
            "{name} 是交互式 shell/REPL（需要 TTY），已拒绝。带参数执行（如 `{name} -c '...'`）可以"
        ));
    }
    // DB 交互客户端：无执行旗标（-c/-e/-f/--eval…）、无 stdin 重定向、
    // 无 SQL/脚本位置参数 → 会进入交互提示符。
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
            // redis-cli：无位置参数即交互提示符。
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
    // ssh：-t 强制分配 TTY；或只有主机没有远程命令 = 交互会话（口令/远程 shell）。
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
    // 容器 exec/run -it / attach：交互 shell。
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
    // tmux attach / 前台 new（无 -d）：需要 TTY；send-keys/capture-pane 等脚本用法放行。
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
    /// 后台监控的通知条件：任一字样出现在输出中即触发通知（如 ["ERROR", "panic"]）。
    /// 不设置时默认检测常见错误行。
    #[serde(default)]
    #[schemars(description = "后台监控通知条件：任一字样命中输出即通知（不设则默认检测错误行）")]
    notify_on: Option<Vec<String>>,
    /// 后台监控的正则通知条件：输出行匹配即触发通知。
    #[serde(default)]
    #[schemars(description = "后台监控正则通知条件：输出行匹配即通知")]
    notify_regex: Option<String>,
    /// 后台化：立即返回 async_launched，完成时通知。对非依赖/长时命令
    /// 使用（结果不立即需要时）；默认同步等待输出。
    #[serde(default)]
    #[schemars(description = "后台执行（默认 false）：立即返回任务 id，完成时通知；对结果不立即需要的命令使用")]
    background: Option<bool>,
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
        "在本地 shell 中执行命令，返回 stdout/stderr 与退出码。长时间运行的任务（如 cargo build、npm install、大测试）优先 background:true 异步执行——即使后续需要结果：立即返回 async_launched 并告知用户任务在后台运行，完成通知到达后再继续。周期命令（watch/while/until/for/tail -f）自动转为后台任务，可配 notify_on/notify_regex 条件，输出命中即通知（无需等待命令结束）。需要 TTY 的交互式命令（top/htop/vim/ssh 裸连接等全屏或会话程序）会被拒绝。"
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
        // 交互式/TTY 命令（top/htop/vim/ssh 等）：先拒绝，避免乱码与终端抢占。
        if let Some(reason) = interactive_command_reason(&params.command) {
            return Err(ToolError::failed(reason));
        }
        let timeout = Duration::from_secs(params.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));

        // 周期命令（watch/while/until/for/tail -f）自动后台化：
        // 立即返回 async_launched，后台执行 + 轮次检查 + 完成通知。
        if let Some(interval) = periodic_bash_interval(&params.command) {
            return launch_background(&params, ctx, Some(interval)).await;
        }
        // 显式后台化：非依赖/长时命令（如 cargo build、npm install），
        // 结果不立即需要时主 agent 不等。
        if params.background.unwrap_or(false) {
            return launch_background(&params, ctx, None).await;
        }

        let mut command = shell_command(&params.command, &ctx.cwd);
        // 回合被中断（Esc）时 future drop 会连带杀掉子进程，不留孤儿。
        command.kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|e| ToolError::failed(format!("failed to run command: {e}")))?;
        // 进程组 leader = 子 shell 自身：超时时整组清理，孙进程不孤儿化。
        let pgid = child.id();

        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                return Err(ToolError::failed(format!("failed to run command: {e}")));
            }
            Err(_) => {
                if let Some(pgid) = pgid {
                    kill_process_group(pgid).await;
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

/// 子 shell 命令：独立进程组（超时/取消时整组清理）、stdin 断开
/// （子进程不得抢 TUI 输入）、stdout/stderr 走管道。
fn shell_command(command: &str, cwd: &std::path::Path) -> tokio::process::Command {
    use std::os::unix::process::CommandExt;
    let mut std_command = std::process::Command::new("/bin/zsh");
    std_command
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // 新进程组：pgid == 子 shell pid，孙进程一并落在组内。
        .process_group(0);
    tokio::process::Command::from(std_command)
}

/// 杀掉整个进程组（孙进程一并清理）。
/// AGENTS.md 禁 unsafe，killpg(2) 走 `/bin/sh` 内建 kill 的负 pid 语义达成。
async fn kill_process_group(pgid: u32) {
    let script = format!("kill -TERM -{pgid} 2>/dev/null; kill -KILL -{pgid} 2>/dev/null");
    let _ = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

/// 周期命令后台执行：注册 watchable（interval 轮询）+ spawn 流式执行。
/// interval=None（显式 background）也要轮询：否则 notify_on/notify_regex/
/// 默认 Errors 条件无人驱动，静默失效。
async fn launch_background(
    params: &BashInput,
    ctx: &ToolContext,
    interval: Option<Duration>,
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
    // 周期命令的轮次语义（Idle = 一轮）只对 watch/loop 类命令成立；
    // 显式 background 只借轮询驱动条件匹配，状态保持 Running。
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
        // 后台任务独立生命周期：周期命令不受单次 timeout 限制。
        match run_streaming(&command, &cwd, None, cell, watch.clone(), id).await {
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
    timeout: Option<Duration>,
    cell: Arc<BashCell>,
    watch: std::sync::Arc<crate::watch::WatchRegistry>,
    id: crate::watch::WatchId,
) -> Result<(String, i32), String> {
    let mut child = shell_command(command, cwd)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn: {e}"))?;
    let pgid = child.id();
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
                let _ = child.kill().await;
                if let Some(pgid) = pgid {
                    kill_process_group(pgid).await;
                }
                return Err(format!("command timed out after {}s", t.as_secs()));
            }
        },
        None => match child.wait().await {
            Ok(status) => status,
            Err(e) => return Err(format!("failed to wait: {e}")),
        },
    };
    // 孙进程可能仍持有 stdout：join 加超时兜底，否则 reader 永挂、
    // watch 条目永远停在 Running。
    for mut reader in readers {
        if tokio::time::timeout(READER_DRAIN_TIMEOUT, &mut reader)
            .await
            .is_err()
        {
            if let Some(pgid) = pgid {
                kill_process_group(pgid).await;
            }
            let _ = tokio::time::timeout(READER_DRAIN_TIMEOUT, &mut reader).await;
            reader.abort();
        }
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
    /// 周期命令（watch/loop/tail -f）才有轮次语义；显式 background 只借轮询驱动条件。
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

    #[tokio::test]
    async fn explicit_background_non_periodic_command_notifies() {
        use crate::watch::WatchState;

        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
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

    /// 交互式/TTY 命令拒绝：全屏 TUI、编辑器、裸 shell/REPL、无命令 ssh、
    /// 容器 -it、tmux 前台一律拒绝；批量快照/带参数/脚本用法放行。
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

    /// 回归：旗标耗尽 tokens 时曾越界索引 panic（`sudo -v` / `sudo -k` / 空命令）。
    #[test]
    fn flag_only_wrapper_commands_do_not_panic() {
        // 会弹口令的 sudo 变体：拒绝但不 panic。
        for cmd in ["sudo -v", "sudo -u root", "sudo -p prompt", "doas -"] {
            assert!(
                interactive_command_reason(cmd).is_some(),
                "应当拒绝且不 panic: {cmd}"
            );
        }
        // 不弹口令 / 无基命令：放行且不 panic。
        for cmd in ["", "   ", "sudo -k", "sudo -n", "sudo -K", "env", "nohup", "exec"] {
            assert_eq!(
                interactive_command_reason(cmd),
                None,
                "不应拒绝且不 panic: {cmd:?}"
            );
        }
    }

    /// 超时：整个进程组被清理，孙进程不孤儿化。
    #[tokio::test]
    async fn timeout_kills_whole_process_group() {
        let marker = std::env::temp_dir()
            .join(format!("bingo-pgroup-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let ctx = ToolContext {
            cwd: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        };
        // 孙进程写下自己的 pid 后长睡；父 shell 也长睡触发超时。
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
        // 组清理是异步信号：给内核一点时间。
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

    /// 回归：background:true（非周期）也要驱动条件匹配，notify_on 不再静默失效。
    #[tokio::test]
    async fn explicit_background_conditions_fire() {
        let watch = crate::watch::WatchRegistry::new();
        let ctx = ToolContext {
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

    /// 交互式命令在 Bash 工具层被拒绝（模型路径同样生效）。
    #[tokio::test]
    async fn bash_tool_refuses_interactive_commands() {
        let ctx = ToolContext {
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
