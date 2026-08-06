use std::io::Read;
use std::path::PathBuf;
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

mod api;
mod budget;
mod compact;
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
mod tool;
mod tools;
mod transcript;
mod tui;
mod watch;

#[derive(Debug, Parser)]
#[command(name = "bingo", version, about = "Rust agent CLI")]
struct Cli {
    /// headless 模式：直接把回复打到 stdout
    #[arg(short, long)]
    print: bool,

    /// 全屏模式（iocraft canvas，输入吸底、app 内滚动）；默认 inline：
    /// 像普通终端一样输出，历史在终端 scrollback
    #[arg(long)]
    fullscreen: bool,

    /// 使用的模型
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,

    /// 权限模式（默认从 settings 读取）
    #[arg(long)]
    permission_mode: Option<String>,

    /// 恢复最近的会话继续对话
    #[arg(long)]
    continue_: bool,

    /// prompt；缺省时从 stdin 读取（交互模式忽略）
    prompt: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let system = build_system(
        &load_memory(&home, &project_dir),
        load_project_memory(&home, &project_dir),
        settings.cache_control.unwrap_or(false),
    );

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
    let mut runtime = crate::query::Runtime::new(
        cli.model,
        transcript,
        settings.permissions.clone(),
    );
    runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        settings.mcp_servers.clone(),
        settings.disabled_mcp_servers.iter().cloned().collect(),
    )));
    let _ = runtime.thinking_tx.send(settings.thinking_level.clone());
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
    });

    let mode_str = session.permission_mode_str();
    crate::hooks::run_session_start(&session.settings.hooks, mode_str).await;

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

    crate::hooks::run_session_end(&session.settings.hooks, mode_str).await;
    result
}
