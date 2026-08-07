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
mod share;
mod share_html;
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
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Headless mode: print the reply straight to stdout
    #[arg(short, long)]
    print: bool,

    /// Fullscreen mode (alternate-screen canvas, input pinned at the bottom, in-app
    /// scrolling); default is inline: output like a normal terminal, history in the
    /// terminal scrollback
    #[arg(long)]
    fullscreen: bool,

    /// Model to use (defaults to settings `model`, then the built-in default)
    #[arg(long)]
    model: Option<String>,

    /// Do not auto-start the project team (overrides settings `team.autoStart`; D31)
    #[arg(long)]
    no_team: bool,

    /// Permission mode (defaults to the settings)
    #[arg(long)]
    permission_mode: Option<String>,

    /// Resume the most recent session
    #[arg(long)]
    continue_: bool,

    /// Prompt; reads from stdin when omitted (ignored in interactive mode).
    /// Mutually exclusive with subcommands (args_conflicts_with_subcommands) —
    /// `bingo --print "x"` works without a subcommand; `bingo share` parses independently.
    #[command(subcommand)]
    command: Option<Command>,

    prompt: Vec<String>,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Export the current session as a self-contained HTML share page (conversation/Team/DM/channels, offline)
    Share {
        /// Session key: transcript stem (`{slug}-{ts}`) or a matching fragment; defaults to the latest session (/resume semantics)
        session: Option<String>,
        /// Output file path (default `<session>.html` in the current directory)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Open the generated page in the system default browser
        #[arg(long)]
        open: bool,
    },
}

/// Top-level exit (C exit mapping): all errors propagated to the top with `?` are
/// formatted through [`report_error`] — non-TTY (headless/pipe/CI) uses the stable
/// contract `[error] code=... msg=...` (AC-30/31/32); TTY keeps it as-is.
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        report_error(&*e);
        std::process::exit(1);
    }
}

/// The actual main flow (formerly the `main` body). Errors propagate upward and exit through [`main`].
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
    // 子命令快路径：share 只需 home（transcript/shares 目录），不碰 settings/API。
    if let Some(Command::Share { session, output, open }) = cli.command {
        run_share(&home, session.as_deref(), output, open)?;
        return Ok(());
    }
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
    // Inject this project's experience index at session start (only when hits > 0; ≤10 lines,
    // one per line; full entries via Query on demand).
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
    // Task lists are isolated per session: key = transcript file stem (--continue restores the
    // same session's todos; new sessions get a fresh list). Falls back to the project-wide
    // shared list if the transcript fails to create.
    let task_list_key = transcript
        .as_ref()
        .and_then(|t| t.path().file_stem())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| crate::tasks::project_task_key(&project_dir));
    // Model precedence: --model > settings (merged user < project < local) > built-in default.
    let model = cli
        .model
        .or_else(|| settings.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let mut runtime = crate::query::Runtime::new(
        model,
        transcript.clone(),
        settings.permissions.clone(),
    );
    runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        settings.mcp_servers.clone(),
        settings.disabled_mcp_servers.iter().cloned().collect(),
    )));
    let _ = runtime.thinking_tx.send(settings.thinking_level.clone());
    // provider 恢复：settings.provider（/provider 与 /model 菜单持久化）
    // 存在且有效则切换；无效名回落 default + warning（不阻断启动）。
    if let Some(name) = settings.provider.as_deref() {
        match client.set_provider(name) {
            Ok(()) => {
                let _ = runtime.provider_tx.send(name.to_string());
            }
            Err(e) => eprintln!(
                "[bingo] warning: provider \"{name}\" 已失效，回落 default: {e}"
            ),
        }
    }
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

    // share 持久化：随会话增量记录子代理/频道快照（`bingo share` 的数据源）。
    // 与任务列表同 key = transcript 文件 stem；创建/读取失败只告警（增强不是契约）。
    if let Some(stem) = transcript
        .as_ref()
        .map(|t| t.name())
        .filter(|s| !s.is_empty())
    {
        let share_path = crate::share::shares_dir(&home).join(format!("{stem}.json"));
        match crate::share::ShareStore::load_or_create(&share_path) {
            Ok(store) => {
                session.agents.attach_share(store.clone());
                session.channels.attach_share(store.clone());
            }
            Err(e) => eprintln!(
                "[bingo] warning: share store unavailable ({e}); bingo share 将只有对话视图"
            ),
        }
    }

    let mode_str = session.permission_mode_str();
    crate::hooks::run_session_start(&session.settings.hooks, mode_str).await;

    // D31 startup default: project-bound team with autoStart (default true) → spawn it.
    // Double opt-out: settings `team.autoStart:false` + `--no-team`.
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
            let outcome = run_query(&session, initial_messages, &prompt, &[], &mut ui, None).await?;
            extract_memory(&session, &outcome.messages, &home, &project_dir).await;
        } else {
            drop(initial_messages); // in interactive mode, --continue history is reused by later turns
            tui::run_tui_session(session.clone(), expand_rx, cli.fullscreen).await?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    // D31 session-end persistence: latest history of team members (for cross-session
    // restore; failures are silent).
    if !cli.no_team && session.settings.team.auto_start.unwrap_or(true) {
        persist_team_memory(&session, &home, &project_dir);
    }
    crate::hooks::run_session_end(&session.settings.hooks, mode_str).await;
    result
}

/// `bingo share`: resolve the session with /resume semantics → read transcript + share doc →
/// generate self-contained HTML → print the output path → (optionally) open the browser.
fn run_share(
    home: &Path,
    key: Option<&str>,
    output: Option<PathBuf>,
    open: bool,
) -> Result<(), crate::share::ShareError> {
    let transcript = crate::share::resolve_transcript(home, key)?;
    let stem = transcript.name();
    let messages = transcript.load_messages()?;

    // Fall back to an empty doc when the share file is missing/corrupt (conversation-only view; the old-session main path).
    let share_path = crate::share::shares_dir(home).join(format!("{stem}.json"));
    let doc = match crate::share::ShareStore::load_or_create(&share_path) {
        Ok(store) => store.snapshot(),
        Err(e) => {
            eprintln!("[bingo] warning: 无法读取 share 文档（{e}）；仅生成对话视图");
            crate::share::ShareDoc::new(stem.clone())
        }
    };

    let html = crate::share_html::render(&doc, &messages);
    let out = output.unwrap_or_else(|| PathBuf::from(format!("{stem}.html")));
    let overwritten = out.exists();
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out.with_extension("html.tmp");
    std::fs::write(&tmp, &html)?;
    std::fs::rename(&tmp, &out)?;
    println!(
        "[share] wrote {}{}",
        out.display(),
        if overwritten { " (overwritten)" } else { "" }
    );
    eprintln!("[share] 注意：此文件包含完整对话与工具输出（可能含敏感信息），分享前请自行审阅。");
    if open {
        open_in_browser(&out).map_err(crate::share::ShareError::Io)?;
    }
    Ok(())
}

/// Open a file with the system default browser (macOS open / Linux xdg-open / Windows cmd start).
fn open_in_browser(path: &Path) -> Result<(), std::io::Error> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else if cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("cmd");
        c.arg("/c").arg("start").arg("").arg(path);
        c
    };
    cmd.spawn()?;
    Ok(())
}

/// Top-level error exit (C exit mapping): `Box<dyn Error>` walks the cause chain for a stable
/// code ([`crate::error::error_code_boxed`]); msg is escaped/truncated.
/// Non-TTY prints `[error] code=<SCREAMING_SNAKE> msg=<single line ≤200>` (AC-30/31/32);
/// TTY prints it as-is (interactive errors are shown in the UI).
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

/// Persist the latest message history of all team members (only members with content;
/// failures are silent — memory is an enhancement, not a contract).
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
