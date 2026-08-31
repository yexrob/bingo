//! The binary: parse the command line, compose the plugins, build the host,
//! run one surface, exit with its code. Nothing here knows how a turn works.

mod login;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use bingo_agents::AgentsPlugin;
use bingo_channels::ChannelsPlugin;
use bingo_context::ContextPlugin;
use bingo_core::settings;
use bingo_core::{Host, HostConfig};
use bingo_demo_ui::DemoUiPlugin;
use bingo_experience::ExperiencePlugin;
use bingo_hooks_shell::ShellHooksPlugin;
use bingo_mcp::McpPlugin;
use bingo_permissions::PermissionsPlugin;
use bingo_plugin_rpc::PluginRpcPlugin;
use bingo_provider_anthropic::AnthropicPlugin;
use bingo_provider_fake::{FakePlugin, FakeProvider, Script};
use bingo_provider_openai::OpenAiPlugin;
use bingo_rooms::RoomsPlugin;
use bingo_sdk::{
    Env, ErrorCode, KernelError, LoginMethod, Plugin, SessionId, SessionSelector, SessionSpec,
    SurfaceOptions,
};
use bingo_skills::SkillsPlugin;
use bingo_store_jsonl::JsonlStorePlugin;
use bingo_surface_print::{PrintPlugin, error_report, notice_report};
use bingo_surface_rpc::RpcPlugin;
use bingo_surface_tui::TuiPlugin;
use bingo_tasks::TasksPlugin;
use bingo_tool_bash::BashPlugin;
use bingo_tool_fs::FsPlugin;
use bingo_tool_web::WebPlugin;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

#[derive(Parser, Debug)]
#[command(name = "bingo", version, about = "A local coding-agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// The prompt. Read from stdin when absent.
    prompt: Option<String>,

    /// Run one turn headlessly and exit.
    #[arg(long)]
    print: bool,

    /// What `--print` writes to stdout.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    /// What `--print` reads from stdin: one prompt, or Claude Code's host
    /// protocol, a turn per line (ADR-0007).
    #[arg(long, value_enum, default_value_t = InputFormat::Text)]
    input_format: InputFormat,

    /// Who answers permission prompts headlessly. `stdio` is the host on the
    /// other end of `--input-format stream-json`.
    #[arg(long, value_name = "TOOL")]
    permission_prompt_tool: Option<PromptTool>,

    /// The model provider; the settings' `provider`, else the first registered.
    #[arg(long, global = true)]
    provider: Option<String>,

    /// The model id; the settings' `model`, else the provider's default.
    #[arg(long, global = true)]
    model: Option<String>,

    /// An extra settings file, above the user, project and local layers.
    #[arg(long, value_name = "PATH", global = true)]
    settings: Option<PathBuf>,

    /// A JSON file whose `mcpServers` are added for this run (a host's bundle).
    #[arg(long, value_name = "PATH", global = true)]
    mcp_config: Option<PathBuf>,

    /// default | acceptEdits | plan | bypassPermissions | dontAsk
    #[arg(long, value_name = "MODE", global = true)]
    permission_mode: Option<String>,

    /// Skip every permission prompt (the same as `--permission-mode bypassPermissions`).
    #[arg(long, global = true)]
    dangerously_skip_permissions: bool,

    /// Permission rules to allow for this run, e.g. `Bash(git status:*)`.
    #[arg(long, value_name = "RULE", value_delimiter = ',', global = true)]
    allowed_tools: Vec<String>,

    /// The session's working directory; the process cwd when absent.
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,

    /// An opaque key naming the session, for hosts that route by it.
    #[arg(long)]
    session_id: Option<String>,

    /// Reopen the most recent session in this directory.
    #[arg(long, conflicts_with_all = ["resume", "session_id"])]
    r#continue: bool,

    /// Reopen the session with this id.
    #[arg(long, value_name = "ID", conflicts_with = "session_id")]
    resume: Option<String>,

    /// Stop the turn after this many model rounds.
    #[arg(long, value_name = "N", global = true)]
    max_turns: Option<u32>,

    /// Leave the terminal as it was: the full-screen surface prints the last
    /// screenful of the conversation on the way out unless this is given.
    #[arg(long)]
    no_print_on_exit: bool,

    /// Register the demo plugin: `/board`, `DemoProgress`, and the three
    /// lanes of ADR-0013 to look at. Off unless this or the `demoUi` setting
    /// says otherwise.
    #[arg(long, global = true)]
    demo_ui: bool,

    /// Listen on an IM channel beside this run: `loopback[=host:port]`
    /// (ADR-0016). Repeatable; the `channels` setting says the same thing.
    #[arg(long, value_name = "ADAPTER", global = true)]
    channels: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve sessions over JSON-RPC to one client (ADR-0007).
    Serve {
        /// One client on stdin and stdout, one JSON-RPC message per line.
        #[arg(long)]
        stdio: bool,
    },
    /// Sign in to a provider; the browser opens unless told otherwise (ADR-0012).
    Login {
        /// The provider id, e.g. `codex`.
        provider: String,
        /// Show a code to enter in a browser anywhere, instead of opening one here.
        #[arg(long, conflicts_with = "paste")]
        device: bool,
        /// Read a credential minted elsewhere from stdin.
        #[arg(long)]
        paste: bool,
    },
    /// Forget a provider's stored credential.
    Logout {
        /// The provider id, e.g. `codex`.
        provider: String,
    },
    /// Listen on the configured IM channels and nothing else (ADR-0016).
    Channels,
}

impl Command {
    /// The sign-in or sign-out a subcommand asks for, if it is one.
    fn credential(&self) -> Option<Credential<'_>> {
        match self {
            Command::Login {
                provider,
                device,
                paste,
            } => Some(Credential::Login {
                provider,
                method: match (device, paste) {
                    (true, _) => Some(LoginMethod::Device),
                    (_, true) => Some(LoginMethod::Paste),
                    _ => None,
                },
            }),
            Command::Logout { provider } => Some(Credential::Logout { provider }),
            Command::Serve { .. } | Command::Channels => None,
        }
    }
}

enum Credential<'a> {
    Login {
        provider: &'a str,
        method: Option<LoginMethod>,
    },
    Logout {
        provider: &'a str,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
    /// Claude Code's envelope, for hosts that already speak it (ADR-0007).
    StreamJson,
}

impl OutputFormat {
    fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Text => "text",
            OutputFormat::Json => "json",
            OutputFormat::StreamJson => "stream-json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InputFormat {
    /// One prompt: the argument, or the whole of stdin.
    Text,
    /// Claude Code's host protocol, one JSON object per line (ADR-0007).
    StreamJson,
}

impl InputFormat {
    fn as_str(self) -> &'static str {
        match self {
            InputFormat::Text => "text",
            InputFormat::StreamJson => "stream-json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PromptTool {
    /// The host on the other end of the stream-json protocol.
    Stdio,
}

impl PromptTool {
    fn as_str(self) -> &'static str {
        match self {
            PromptTool::Stdio => "stdio",
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

/// What this run is for. Decided before the surface options are built,
/// because building them consumes the command line.
enum Work {
    Rpc {
        stdio: bool,
    },
    /// The chat surface as the whole run (ADR-0016 §1): nothing owns the
    /// terminal, so the concurrent surface is the work.
    Channels,
    Session,
}

async fn run(cli: Cli) -> Result<i32, KernelError> {
    let work = match &cli.command {
        Some(Command::Serve { stdio }) => Work::Rpc { stdio: *stdio },
        Some(Command::Channels) => Work::Channels,
        _ => Work::Session,
    };
    check_input(&cli)?;
    let interactive = interactive(&cli);
    let cwd = working_dir(cli.cwd.as_deref())?;
    let config = host_config(&cli, &cwd)?;
    let demo = demo_ui(&cli, &config.layers);
    let listening = channels_wanted(&cli, &config.layers);
    let host = Host::build(plugins(demo)?, config)
        .await
        .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;
    for (code, text) in host.notices() {
        let human = std::io::IsTerminal::is_terminal(&std::io::stderr());
        eprintln!("{}", notice_report(&code, &text, human));
    }
    if let Some(credential) = cli.command.as_ref().and_then(Command::credential) {
        let receipt = match credential {
            Credential::Login { provider, method } => login::login(&host, provider, method).await,
            Credential::Logout { provider } => login::logout(&host, provider).await,
        };
        host.shutdown().await;
        println!("{}", receipt?);
        return Ok(0);
    }
    let env = Arc::new(environment(&cwd));
    let (id, options) = match work {
        Work::Rpc { stdio } => ("rpc", serve_options(stdio, cwd.clone(), env.clone())?),
        Work::Channels => (
            bingo_channels::SURFACE_ID,
            channel_options(cwd.clone(), env.clone()),
        ),
        Work::Session if interactive => ("tui", surface_options(cli, cwd.clone(), env.clone())),
        Work::Session => ("print", surface_options(cli, cwd.clone(), env.clone())),
    };
    let beside = match listening && id != bingo_channels::SURFACE_ID {
        true => start_channels(&host, channel_options(cwd, env)),
        false => None,
    };
    let surface = host
        .surface(id)
        .ok_or_else(|| KernelError::new(ErrorCode::Internal, format!("no {id} surface")))?;
    let exit = surface.run(host.handle(), options).await;
    if let Some(channels) = beside {
        channels.abort();
    }
    host.shutdown().await;
    exit.map(|e| e.code)
}

/// Whether a chat is being listened on: the flag, else any settings layer
/// that names an adapter. The plugin owns the spelling of the key.
fn channels_wanted(cli: &Cli, layers: &[settings::Layer]) -> bool {
    !cli.channels.is_empty()
        || layers
            .iter()
            .any(|layer| bingo_channels::wanted(&Value::Object(layer.value.clone())))
}

/// The chat surface beside the one that owns the terminal. It is
/// `SurfaceKind::Concurrent` (ADR-0016 §1), so it lives as long as the run.
fn start_channels(host: &Host, options: SurfaceOptions) -> Option<tokio::task::JoinHandle<()>> {
    let surface = host.surface(bingo_channels::SURFACE_ID)?;
    let handle = host.handle();
    Some(tokio::spawn(async move {
        if let Err(error) = surface.run(handle, options).await {
            let human = std::io::IsTerminal::is_terminal(&std::io::stderr());
            eprintln!("{}", error_report(error.code, &error.message, human));
        }
    }))
}

/// The chat surface mints its own session keys; the selector is a placeholder
/// it never reads, as the server's is.
fn channel_options(cwd: PathBuf, env: Arc<Env>) -> SurfaceOptions {
    SurfaceOptions {
        selector: SessionSelector::Latest { cwd: cwd.clone() },
        cwd,
        prompt: None,
        args: Value::Null,
        env,
    }
}

/// The flag combinations that would leave a flag with nothing to act on.
fn check_input(cli: &Cli) -> Result<(), KernelError> {
    if cli.input_format == InputFormat::StreamJson && !cli.print {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            "--input-format stream-json is a headless protocol: it needs --print",
        ));
    }
    if cli.permission_prompt_tool.is_some() && cli.input_format != InputFormat::StreamJson {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            "--permission-prompt-tool needs --input-format stream-json: \
             there is no other way for an answer to arrive",
        ));
    }
    Ok(())
}

/// The terminal interface when a person is at both ends of the pipe;
/// `--print` and any redirection keep the headless one.
fn interactive(cli: &Cli) -> bool {
    use std::io::IsTerminal;
    !cli.print && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The server has no prompt and no session of its own; the selector is a
/// placeholder the surface ignores (ADR-0007).
fn serve_options(stdio: bool, cwd: PathBuf, env: Arc<Env>) -> Result<SurfaceOptions, KernelError> {
    if !stdio {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            "serve needs a transport: --stdio is the one there is",
        ));
    }
    Ok(SurfaceOptions {
        selector: SessionSelector::Create {
            spec: SessionSpec {
                cwd: cwd.clone(),
                ..SessionSpec::default()
            },
        },
        cwd,
        prompt: None,
        args: json!({ "transport": "stdio" }),
        env,
    })
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

/// Whether the demo plugin is wanted: the flag, else the highest settings
/// layer that names the key. The plugin list is composed before a host
/// exists, so the bin reads this one key itself; the plugin owns its
/// spelling and its meaning.
fn demo_ui(cli: &Cli, layers: &[settings::Layer]) -> bool {
    if cli.demo_ui {
        return true;
    }
    layers
        .iter()
        .rev()
        .find(|layer| layer.value.contains_key(bingo_demo_ui::SETTING))
        .is_some_and(|layer| bingo_demo_ui::wanted(&Value::Object(layer.value.clone())))
}

/// Every plugin this build ships, in registration order.
fn plugins(demo_ui: bool) -> Result<Vec<Box<dyn Plugin>>, KernelError> {
    let script = Script::from_env()
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?
        .unwrap_or_else(Script::demo);
    let mut all: Vec<Box<dyn Plugin>> = vec![
        Box::new(FakePlugin::new(Arc::new(FakeProvider::new(script)))),
        Box::new(AnthropicPlugin),
        Box::new(OpenAiPlugin),
        Box::new(PermissionsPlugin),
        Box::new(ShellHooksPlugin),
        Box::new(JsonlStorePlugin::default()),
        Box::new(ContextPlugin),
        Box::new(FsPlugin),
        Box::new(BashPlugin),
        Box::new(WebPlugin),
        Box::new(SkillsPlugin),
        Box::new(McpPlugin::default()),
        Box::new(PluginRpcPlugin::default()),
        Box::new(AgentsPlugin),
        Box::new(RoomsPlugin),
        Box::new(TasksPlugin),
        Box::new(ExperiencePlugin),
        Box::new(PrintPlugin),
        Box::new(RpcPlugin),
        Box::new(TuiPlugin),
        Box::new(ChannelsPlugin),
    ];
    if demo_ui {
        all.push(Box::new(DemoUiPlugin));
    }
    Ok(all)
}

fn host_config(cli: &Cli, cwd: &std::path::Path) -> Result<HostConfig, KernelError> {
    let mut config = HostConfig::new(environment(cwd));
    config.layers = settings::load(&config.env, cwd, cli.settings.as_deref())
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e.to_string()))?;
    if let Some(path) = &cli.mcp_config {
        config.layers.push(mcp_layer(path)?);
    }
    let mut cli_settings = cli_layer(cli);
    cli_settings.extend(
        bingo_channels::from_flags(&cli.channels)
            .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e))?,
    );
    config
        .layers
        .push(settings::Layer::new("cli", cli_settings));
    if let Some(rounds) = cli.max_turns {
        config.budget.max_rounds = rounds;
    }
    Ok(config)
}

fn surface_options(cli: Cli, cwd: PathBuf, env: Arc<Env>) -> SurfaceOptions {
    SurfaceOptions {
        selector: selector(&cli, cwd.clone()),
        cwd,
        prompt: cli.prompt,
        args: json!({
            "outputFormat": cli.output_format.as_str(),
            "inputFormat": cli.input_format.as_str(),
            "permissionPromptTool": cli.permission_prompt_tool.map(PromptTool::as_str),
            "noPrintOnExit": cli.no_print_on_exit,
        }),
        env,
    }
}

/// Which session the run is about: a new one unless told to reopen.
fn selector(cli: &Cli, cwd: PathBuf) -> SessionSelector {
    if cli.r#continue {
        return SessionSelector::Latest { cwd };
    }
    if let Some(id) = &cli.resume {
        return SessionSelector::ById {
            id: SessionId::from_raw(id),
        };
    }
    SessionSelector::Create {
        spec: SessionSpec {
            cwd,
            key: cli.session_id.as_ref().map(|k| format!("host/{k}")),
            ..SessionSpec::default()
        },
    }
}

/// `--mcp-config`: the file's `mcpServers`, and nothing else in it, as a layer
/// above the settings files (Claude Code's flag; OpenClaw's `bundleMcp`).
fn mcp_layer(path: &std::path::Path) -> Result<settings::Layer, KernelError> {
    let invalid = |message: String| KernelError::new(ErrorCode::InvalidInput, message);
    let layer = settings::read_layer(path)
        .map_err(|e| invalid(e.to_string()))?
        .ok_or_else(|| invalid(format!("--mcp-config: {} does not exist", path.display())))?;
    let servers = layer.value.get("mcpServers").cloned().ok_or_else(|| {
        invalid(format!(
            "--mcp-config: {} has no mcpServers",
            path.display()
        ))
    })?;
    let mut only = Map::new();
    only.insert("mcpServers".into(), servers);
    Ok(settings::Layer::new("mcp-config", only))
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
