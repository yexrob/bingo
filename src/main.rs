use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;

use crate::api::client::Client;
use crate::api::types::{Message, DEFAULT_MODEL};
use crate::permission::PermissionMode;
use crate::query::{headless_hooks, run_query, Session};
use crate::memory::{extract_memory, load_project_memory};
use crate::settings::load_settings;
use crate::system::{build_system, load_memory};
use crate::transcript::{create as create_transcript, latest as latest_transcript, Transcript};

mod agents;
mod api;
mod budget;
mod channels;
mod compact;
mod error;
mod experience;
mod hooks;
mod mcp;
mod memory;
mod permission;
mod preapproved;
mod query;
mod settings;
mod skills;
mod system;
mod tasks;
mod team;
mod team_cmd;
mod tool;
mod tools;
mod transcript;
mod tui;
mod ui;
mod watch;

#[derive(Debug, Parser)]
#[command(name = "bingo", version, about = "Rust agent CLI")]
struct Cli {
    /// headless 模式：直接把回复打到 stdout
    #[arg(short, long)]
    print: bool,

    /// 全屏模式（备用屏 canvas，输入吸底、app 内滚动）；默认 inline：
    /// 像普通终端一样输出，历史在终端 scrollback
    #[arg(long)]
    fullscreen: bool,

    /// 使用的模型（缺省依次回落 settings `model`、内置默认）
    #[arg(long)]
    model: Option<String>,

    /// 不自动拉起项目 team（覆盖 settings `team.autoStart`；D31）
    #[arg(long)]
    no_team: bool,

    /// 权限模式（默认从 settings 读取）
    #[arg(long)]
    permission_mode: Option<String>,

    /// 恢复最近的会话继续对话
    #[arg(long)]
    continue_: bool,

    /// prompt；缺省时从 stdin 读取（交互模式忽略）
    prompt: Vec<String>,
}

/// 顶层出口（C 出口映射）：所有 `?` 传播到顶层的错误统一经
/// [`report_error`] 格式化——非 TTY（headless/管道/CI）走稳定契约
/// `[error] code=... msg=...`（AC-30/31/32），TTY 下保持原样。
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        report_error(&*e);
        std::process::exit(1);
    }
}

/// 实际主流程（原 `main` 主体）。错误一律向上传播，由 [`main`] 统一出口。
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => {
            if !cli.print {
                eprintln!("[bingo] warning: HOME is not set; using current dir for state");
            }
            PathBuf::new()
        }
    };
    let project_dir = std::env::current_dir()?;
    let user_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));

    let settings = load_settings(&user_dir, &project_dir)?;

    let permission_mode: PermissionMode = cli
        .permission_mode
        .or(settings.permission_mode.clone())
        .unwrap_or_else(|| "default".to_string())
        .parse()?;

    let client = Client::from_settings(&settings)?;
    let mut system = build_system(
        &load_memory(&home, &project_dir),
        load_project_memory(&home, &project_dir),
        settings.cache_control.unwrap_or(false),
    );
    // 会话开始注入本项目经验索引（仅命中>0 时；≤10 行一行一条，全文按需 Query）。
    let experience_index = crate::tool::experience::session_index(&home, &project_dir);
    if !experience_index.is_empty() {
        system.push(crate::api::types::SystemBlock {
            text: format!("Project experience (reusable patterns from past sessions):\n{experience_index}\n(Query full details with ExperienceQuery; propose new ones with ExperiencePropose)"),
            cache: settings.cache_control.unwrap_or(false),
        });
    }

    let (transcript, initial_messages): (Option<Transcript>, Vec<Message>) = if cli.continue_ {
        match latest_transcript(&home)? {
            Some(t) => {
                eprintln!("[bingo] continuing transcript: {}", t.path().display());
                (Some(t.clone()), t.load_messages()?)
            }
            None => match create_transcript(&home, &project_dir) {
                Ok(t) => (Some(t), Vec::new()),
                Err(e) => {
                    if !cli.print {
                        eprintln!("[bingo] warning: cannot create transcript (history will not persist): {e}");
                    }
                    (None, Vec::new())
                }
            },
        }
    } else {
        match create_transcript(&home, &project_dir) {
            Ok(t) => (Some(t), Vec::new()),
            Err(e) => {
                if !cli.print {
                    eprintln!("[bingo] warning: cannot create transcript (history will not persist): {e}");
                }
                (None, Vec::new())
            }
        }
    };

    let (expand_tx, expand_rx) = tokio::sync::watch::channel(false);
    // 任务列表按会话隔离：key = transcript 文件 stem（--continue 恢复同一会话
    // 的 todo；新会话另开列表）。transcript 创建失败时回落项目级共享列表。
    let task_list_key = transcript
        .as_ref()
        .and_then(|t| t.path().file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::tasks::project_task_key(&project_dir));
    // 模型优先级：--model > settings（user < project < local 合并结果）> 内置默认。
    let model = cli
        .model
        .or_else(|| settings.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let mut runtime = crate::query::Runtime::new(
        model,
        transcript,
        settings.permissions.clone(),
    );
    runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        settings.mcp_servers.clone(),
        settings.disabled_mcp_servers.iter().cloned().collect(),
    )));
    let _ = runtime.thinking_tx.send(settings.thinking_level.clone());
    let channel_limits = crate::channels::ChannelLimits::from_settings(&settings);
    let session = Arc::new(Session {
        client,
        runtime,
        permission_mode,
        settings,
        system,
        depth: 0,
        home: home.clone(),
        quiet: !cli.print,
        compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: crate::watch::WatchRegistry::new(),
        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&home, &task_list_key)),
        last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        expand_tasks: expand_tx,
        agents: crate::agents::AgentRegistry::new(),
        channels: crate::channels::ChannelRegistry::new(channel_limits),
        instance: None,
    });

    let mode_str = session.permission_mode_str();
    crate::hooks::run_session_start(&session.settings.hooks, mode_str).await;

    // D31 启动默认加载：项目绑定 team 且 autoStart（缺省 true）→ 拉起。
    // 双 opt-out：settings `team.autoStart:false` + `--no-team`。
    if !cli.no_team && session.settings.team.auto_start.unwrap_or(true) {
        let branch = crate::team::current_branch(&project_dir);
        let defs = crate::agents::load_agent_defs(&home, &project_dir);
        match crate::team::load_team_file(&project_dir) {
            Ok(Some(team)) => match crate::team::spawn_team(
                &session, &team, &defs, &home, &project_dir, &branch,
            ) {
                Ok(summary) => {
                    let total =
                        summary.spawned.len() + summary.reused.len() + summary.failed.len();
                    let ready = total - summary.failed.len();
                    if summary.failed.is_empty() {
                        eprintln!(
                            "[team] {} 就绪 · {ready}/{total} 待命（/team status · /team stop）",
                            team.name
                        );
                    } else {
                        eprintln!(
                            "[team] {} 部分拉起 · {ready}/{total}（失败 {}，/team status 查看）",
                            team.name,
                            summary.failed.len()
                        );
                    }
                }
                Err(e) => eprintln!(
                    "[team] {} 校验失败：{e}（修复后 /team start 拉起）",
                    team.name
                ),
            },
            Ok(None) => {}
            Err(e) => eprintln!("[team] {} 读取失败：{e}", crate::team::TEAM_FILE),
        }
    }

    let result = async {
        if cli.print {
            let prompt = if !cli.prompt.is_empty() {
                cli.prompt.join(" ")
            } else {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                input.trim().to_string()
            };
            if prompt.is_empty() {
                eprintln!("no prompt provided");
                std::process::exit(1);
            }
            let mut ui = headless_hooks();
            let outcome = run_query(&session, initial_messages, &prompt, &mut ui, None).await?;
            extract_memory(&session, &outcome.messages, &home, &project_dir).await;
        } else {
            drop(initial_messages); // 交互模式下 --continue 历史由后续轮次复用
            tui::run_tui_session(session.clone(), expand_rx, cli.fullscreen).await?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    // D31 会话结束落盘：team 成员最新历史（跨会话恢复用；失败静默）。
    if !cli.no_team && session.settings.team.auto_start.unwrap_or(true) {
        persist_team_memory(&session, &home, &project_dir);
    }
    crate::hooks::run_session_end(&session.settings.hooks, mode_str).await;
    result
}

/// 顶层错误出口（C 出口映射）：`Box<dyn Error>` 沿 cause 链取稳定码
/// （[`crate::error::error_code_boxed`]），msg 经转义/截断。
/// 非 TTY 输出 `[error] code=<SCREAMING_SNAKE> msg=<单行 ≤200>`（AC-30/31/32）；
/// TTY 下打印原样（交互环境错误在界面内呈现）。
fn report_error(err: &(dyn std::error::Error + 'static)) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprintln!("Error: {err}");
        return;
    }
    let code = crate::error::error_code_boxed(err);
    let msg = crate::error::sanitize_msg(&err.to_string());
    eprintln!("[error] code={code} msg={msg}");
}

/// 落盘全部 team 成员的最新消息历史（仅保存有内容的成员；失败静默——
/// 记忆是增强不是契约）。
fn persist_team_memory(session: &Arc<Session>, home: &Path, project_dir: &std::path::Path) {
    let Ok(Some(team)) = crate::team::load_team_file(project_dir) else {
        return;
    };
    let branch = crate::team::current_branch(project_dir);
    for m in &team.members {
        if let Some((history, _, _)) = session.agents.view_of(&m.name)
            && !history.is_empty()
        {
            crate::team::save_member_history(
                home, project_dir, &branch, &team.name, &m.name, &history,
            );
        }
    }
}
