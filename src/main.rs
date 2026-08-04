use std::io::Read;

use clap::Parser;

use crate::api::client::Client;
use crate::api::types::DEFAULT_MODEL;
use crate::permission::PermissionMode;
use crate::query::run_query;

mod api;
mod permission;
mod query;
mod tool;
mod tools;

#[derive(Debug, Parser)]
#[command(name = "bingo", version, about = "Rust agent CLI")]
struct Cli {
    /// headless 模式：直接把回复打到 stdout
    #[arg(short, long)]
    print: bool,

    /// 使用的模型
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,

    /// 权限模式
    #[arg(long, default_value = "default")]
    permission_mode: String,

    /// prompt；缺省时从 stdin 读取
    prompt: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

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

    let permission_mode: PermissionMode = cli.permission_mode.parse()?;

    let client = Client::from_env()?;
    run_query(&client, &cli.model, permission_mode, &prompt).await?;
    Ok(())
}
