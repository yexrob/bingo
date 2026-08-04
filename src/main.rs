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
mod query;
mod settings;
mod system;
mod tool;
mod tools;
mod transcript;
mod tui;

#[derive(Debug, Parser)]
#[command(name = "bingo", version, about = "Rust agent CLI")]
struct Cli {
    /// headless 模式：直接把回复打到 stdout
    #[arg(short, long)]
    print: bool,

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

    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
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

    let client = Client::from_env()?;
    let mut system = build_system(
        &load_memory(&home, &project_dir),
        load_project_memory(&home, &project_dir),
    );
    if client.is_deepseek() {
        for block in &mut system {
            block.cache = false;
        }
    }

    let (transcript, initial_messages): (Option<Transcript>, Vec<Message>) = if cli.continue_ {
        match latest_transcript(&home)? {
            Some(t) => {
                eprintln!("[bingo] continuing transcript: {}", t.path().display());
                (Some(t.clone()), t.load_messages()?)
            }
            None => (create_transcript(&home, &project_dir).ok(), Vec::new()),
        }
    } else {
        (create_transcript(&home, &project_dir).ok(), Vec::new())
    };

    let session = Arc::new(Session {
        client,
        model: cli.model,
        permission_mode,
        settings,
        system,
        transcript,
        depth: 0,
        home: home.clone(),
        quiet: !cli.print,
    });

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
        let messages = run_query(&session, initial_messages, &prompt, &mut ui).await?;
        extract_memory(&session, &messages, &home, &project_dir).await;
    } else {
        drop(initial_messages); // 交互模式下 --continue 历史由后续轮次复用
        tui::run_tui_session(session)?;
    }
    Ok(())
}
