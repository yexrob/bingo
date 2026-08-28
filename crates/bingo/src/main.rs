//! The binary: parse the command line, compose the plugins, build the host,
//! run one surface, exit with its code. Nothing here knows how a turn works.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use bingo_core::{Host, HostConfig};
use bingo_provider_fake::{FakePlugin, FakeProvider, Script};
use bingo_sdk::{
    Env, ErrorCode, KernelError, Plugin, SessionSelector, SessionSpec, SurfaceOptions,
};
use bingo_surface_print::PrintPlugin;
use bingo_tool_fs::FsPlugin;
use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "bingo", version, about = "A local coding-agent harness")]
struct Cli {
    /// The prompt. Read from stdin when absent.
    prompt: Option<String>,

    /// Run one turn headlessly and exit.
    #[arg(long)]
    print: bool,

    /// What `--print` writes to stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    /// The model provider to talk to.
    #[arg(long, default_value = "fake")]
    provider: String,

    /// The model id; the provider's default when absent.
    #[arg(long)]
    model: Option<String>,

    /// The session's working directory; the process cwd when absent.
    #[arg(long)]
    cwd: Option<PathBuf>,

    /// An opaque key naming the session, for hosts that route by it.
    #[arg(long)]
    session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

impl OutputFormat {
    fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("[error] code={} msg={}", e.code.as_str(), e.message);
            ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<i32, KernelError> {
    if !cli.print {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            "the interactive interface is not built yet; run with --print",
        ));
    }
    let cwd = match cli.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(|e| {
            KernelError::new(ErrorCode::Internal, format!("current directory: {e}"))
        })?,
    };
    let home = std::env::home_dir().unwrap_or_else(|| cwd.clone());
    let env = Env {
        config_dir: home.join(".bingo"),
        data_dir: home.join(".bingo").join("data"),
        home,
    };

    let script = Script::from_env()
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?
        .unwrap_or_else(Script::demo);
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(FakePlugin::new(Arc::new(FakeProvider::new(script)))),
        Box::new(FsPlugin),
        Box::new(PrintPlugin),
    ];
    let mut config = HostConfig::new(env);
    config.provider = Some(cli.provider);
    config.model = cli.model;
    config.system_prompt = format!(
        "You are bingo, a coding agent. The working directory is {}.",
        cwd.display()
    );
    let host = Host::build(plugins, config)
        .await
        .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;

    let surface = host
        .surface("print")
        .ok_or_else(|| KernelError::new(ErrorCode::Internal, "no print surface"))?;
    let opts = SurfaceOptions {
        cwd: cwd.clone(),
        selector: SessionSelector::Create {
            spec: SessionSpec {
                cwd,
                key: cli.session_id.map(|k| format!("host/{k}")),
                ..SessionSpec::default()
            },
        },
        prompt: cli.prompt,
        args: serde_json::json!({ "outputFormat": cli.output_format.as_str() }),
    };
    let exit = surface.run(host.handle(), opts).await;
    host.shutdown().await;
    exit.map(|e| e.code)
}
