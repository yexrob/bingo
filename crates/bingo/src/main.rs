//! The binary: parse the command line, compose the plugins, build the host,
//! run one surface, exit with its code. Nothing here knows how a turn works.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use bingo_core::settings;
use bingo_core::{Host, HostConfig};
use bingo_permissions::PermissionsPlugin;
use bingo_provider_anthropic::AnthropicPlugin;
use bingo_provider_fake::{FakePlugin, FakeProvider, Script};
use bingo_sdk::{
    Env, ErrorCode, KernelError, Plugin, SessionSelector, SessionSpec, SurfaceOptions,
};
use bingo_surface_print::{PrintPlugin, error_report, notice_report};
use bingo_tool_bash::BashPlugin;
use bingo_tool_fs::FsPlugin;
use clap::{Parser, ValueEnum};
use serde_json::{Map, Value, json};

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

    /// The model provider; the settings' `provider`, else the first registered.
    #[arg(long)]
    provider: Option<String>,

    /// The model id; the settings' `model`, else the provider's default.
    #[arg(long)]
    model: Option<String>,

    /// An extra settings file, above the user, project and local layers.
    #[arg(long, value_name = "PATH")]
    settings: Option<PathBuf>,

    /// default | acceptEdits | plan | bypassPermissions | dontAsk
    #[arg(long, value_name = "MODE")]
    permission_mode: Option<String>,

    /// Skip every permission prompt (the same as `--permission-mode bypassPermissions`).
    #[arg(long)]
    dangerously_skip_permissions: bool,

    /// Permission rules to allow for this run, e.g. `Bash(git status:*)`.
    #[arg(long, value_name = "RULE", value_delimiter = ',')]
    allowed_tools: Vec<String>,

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
            let human = std::io::IsTerminal::is_terminal(&std::io::stderr());
            eprintln!("{}", error_report(e.code, &e.message, human));
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
    let cwd = working_dir(cli.cwd.as_deref())?;
    let config = host_config(&cli, &cwd)?;
    let host = Host::build(plugins()?, config)
        .await
        .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;
    for (code, text) in host.notices() {
        let human = std::io::IsTerminal::is_terminal(&std::io::stderr());
        eprintln!("{}", notice_report(&code, &text, human));
    }
    let surface = host
        .surface("print")
        .ok_or_else(|| KernelError::new(ErrorCode::Internal, "no print surface"))?;
    let exit = surface.run(host.handle(), surface_options(cli, cwd)).await;
    host.shutdown().await;
    exit.map(|e| e.code)
}

fn working_dir(flag: Option<&std::path::Path>) -> Result<PathBuf, KernelError> {
    match flag {
        Some(cwd) => Ok(cwd.to_path_buf()),
        None => std::env::current_dir()
            .map_err(|e| KernelError::new(ErrorCode::Internal, format!("current directory: {e}"))),
    }
}

fn environment(cwd: &std::path::Path) -> Env {
    Env::rooted(std::env::home_dir().unwrap_or_else(|| cwd.to_path_buf()))
}

/// Every plugin this build ships, in registration order.
fn plugins() -> Result<Vec<Box<dyn Plugin>>, KernelError> {
    let script = Script::from_env()
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?
        .unwrap_or_else(Script::demo);
    Ok(vec![
        Box::new(FakePlugin::new(Arc::new(FakeProvider::new(script)))),
        Box::new(AnthropicPlugin),
        Box::new(PermissionsPlugin),
        Box::new(FsPlugin),
        Box::new(BashPlugin),
        Box::new(PrintPlugin),
    ])
}

fn host_config(cli: &Cli, cwd: &std::path::Path) -> Result<HostConfig, KernelError> {
    let mut config = HostConfig::new(environment(cwd));
    config.layers = settings::load(&config.env, cwd, cli.settings.as_deref())
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?;
    config
        .layers
        .push(settings::Layer::new("cli", cli_layer(cli)));
    Ok(config)
}

fn surface_options(cli: Cli, cwd: PathBuf) -> SurfaceOptions {
    SurfaceOptions {
        cwd: cwd.clone(),
        selector: SessionSelector::Create {
            spec: SessionSpec {
                cwd,
                key: cli.session_id.map(|k| format!("host/{k}")),
                ..SessionSpec::default()
            },
        },
        prompt: cli.prompt,
        args: json!({ "outputFormat": cli.output_format.as_str() }),
    }
}

/// The command line as the highest settings layer: only what was given.
fn cli_layer(cli: &Cli) -> Map<String, Value> {
    let mut layer = Map::new();
    if let Some(provider) = &cli.provider {
        layer.insert("provider".into(), json!(provider));
    }
    if let Some(model) = &cli.model {
        layer.insert("model".into(), json!(model));
    }
    let mut permissions = Map::new();
    let mode = if cli.dangerously_skip_permissions {
        Some("bypassPermissions".to_string())
    } else {
        cli.permission_mode.clone()
    };
    if let Some(mode) = mode {
        permissions.insert("defaultMode".into(), json!(mode));
    }
    if !cli.allowed_tools.is_empty() {
        permissions.insert("allow".into(), json!(cli.allowed_tools));
    }
    if !permissions.is_empty() {
        layer.insert("permissions".into(), Value::Object(permissions));
    }
    layer
}
