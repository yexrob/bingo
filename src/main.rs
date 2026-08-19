use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;

use crate::api::client::Client;
use crate::api::types::{DEFAULT_MODEL, Message};
use crate::memory::load_project_memory;
use crate::permission::PermissionMode;
use crate::query::Session;
use crate::settings::load_settings;
use crate::system::{build_system, load_memory};
use crate::transcript::{Transcript, create as create_transcript, latest as latest_transcript};

mod agents;
mod api;
mod app;
mod app_server;
mod auth;
mod bm25;
mod budget;
mod channels;
mod compact;
mod context_usage;
mod engine;
mod error;
mod experience;
mod hooks;
mod live;
mod mcp;
mod memory;
mod model_cache;
mod model_families;
mod permission;
mod platform;
mod preapproved;
mod print;
mod query;
mod query_session;
mod query_turn;
mod rewind;
mod settings;
mod share;
mod share_html;
mod skills;
mod storage;
mod system;
mod tasks;
mod team;
mod team_cmd;
mod token_rate;
mod tool;
mod tools;
mod transcript;
mod tui;
mod ui;
mod update;
mod watch;

#[derive(Debug, Parser)]
#[command(name = "bingo", version, about = "Rust agent CLI")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// Headless mode: print the reply straight to stdout
    #[arg(short, long)]
    print: bool,

    /// Fullscreen mode (default): alternate-screen canvas, input pinned at the
    /// bottom, and in-app scrolling. Retained as an explicit compatibility flag.
    #[arg(long, conflicts_with = "inline")]
    fullscreen: bool,

    /// Inline mode: finalized output stays in the terminal scrollback
    #[arg(long, conflicts_with = "fullscreen")]
    inline: bool,

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

impl Cli {
    fn fullscreen_mode(&self) -> bool {
        self.fullscreen || !self.inline
    }
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Export a session to local HTML by default; use --public to publish it
    Share {
        /// Session key: transcript stem (`{slug}-{ts}`) or a matching fragment; defaults to the latest session (/resume semantics)
        session: Option<String>,
        /// Publish the generated page to a publicly accessible share URL
        #[arg(long)]
        public: bool,
        /// Output file path (default `<session>.html` in the current directory; local export and upload fallback)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Open the generated page in the system default browser (link in upload mode, file in local mode)
        #[arg(long)]
        open: bool,
    },
    /// Update bingo to the latest GitHub release (SHA-256 verified, atomic replace)
    Update {
        /// Check for a newer version without downloading (dry-run)
        #[arg(long)]
        check: bool,
    },
    /// The GUI app-server (JSON-RPC 2.0 over NDJSON on stdio)
    AppServer {
        #[command(subcommand)]
        command: Option<AppServerCommand>,
    },
}

#[derive(Debug, clap::Subcommand)]
enum AppServerCommand {
    /// Write the deterministic JSON Schema bundle for the app-server protocol
    GenerateSchema {
        /// Output directory (the committed bundle lives in `schema/app-server`)
        #[arg(long)]
        out: PathBuf,
    },
}

/// Top-level exit (C exit mapping): all errors propagated to the top with `?` are
/// formatted through [`report_error`] — non-TTY (headless/pipe/CI) uses the stable
/// contract `[error] code=... msg=...` (AC-30/31/32); TTY keeps it as-is.
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        report_error(&*e);
        std::process::exit(1);
    }
}

/// The actual main flow (formerly the `main` body). Errors propagate upward and exit through [`main`].
async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let fullscreen = cli.fullscreen_mode();

    let home = crate::storage::resolve_home()?;
    if let Err(error) = crate::storage::cleanup(&home, None) {
        eprintln!("[bingo] warning: session storage cleanup failed: {error}; run /gc to retry");
    }
    // Subcommand fast path: share only needs home (transcript/shares dirs), update only needs home (cache),
    // neither touches settings/API.
    if let Some(Command::Share {
        session,
        public,
        output,
        open,
    }) = cli.command
    {
        run_share(&home, session.as_deref(), output, public, open).await?;
        return Ok(());
    }
    if let Some(Command::Update { check }) = cli.command {
        crate::update::run_update(&home, check).await?;
        return Ok(());
    }
    // The app-server owns its own startup: it resolves its directories, and the
    // session it serves is the one a client asks for rather than one this
    // function builds on the way past.
    if let Some(Command::AppServer { command }) = cli.command {
        match command {
            Some(AppServerCommand::GenerateSchema { out }) => {
                let written = crate::app_server::schema::generate(&out)?;
                eprintln!(
                    "[bingo] wrote {} schema files to {}",
                    written.len(),
                    out.display()
                );
                return Ok(());
            }
            None => {
                crate::app_server::stdio::serve().await?;
                return Ok(());
            }
        }
    }
    let project_dir = std::env::current_dir()?;
    let user_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));

    let settings = load_settings(&user_dir, &project_dir)?;
    crate::platform::init_shell(settings.shell.as_deref());

    // Config lint → startup notes: unknown top-level keys (typos parse fine
    // and silently do nothing) and enum values that silently fall back.
    let mut config_notes: Vec<String> = Vec::new();
    // Family-catalog file (D73): seed on first run, refresh its builtin
    // section after an upgrade; any problem becomes a note, never a refusal.
    config_notes.extend(crate::model_families::maintain(&user_dir));
    for path in crate::settings::layer_paths(&user_dir, &project_dir) {
        for key in crate::settings::layer_keys(&path) {
            if !crate::settings::KNOWN_KEYS.contains(&key.as_str()) {
                config_notes.push(format!(
                    "⚠ {} contains unknown config key \"{key}\" (a typo? it will have no effect)",
                    path.display()
                ));
            }
        }
    }
    if let Some(level) = settings.thinking_level.as_deref()
        && level != "off"
        && !crate::api::contract::THINKING_LEVELS.contains(&level)
    {
        config_notes.push(format!(
            "⚠ thinkingLevel \"{level}\" is invalid (use off|low|medium|high|xhigh|max); fell back to off"
        ));
    }
    if let Some(theme) = settings.theme.as_deref()
        && !matches!(theme, "auto" | "dark" | "light")
    {
        config_notes.push(format!(
            "⚠ theme \"{theme}\" is invalid (use auto|dark|light); fell back to auto"
        ));
    }
    for note in &config_notes {
        eprintln!("[bingo] warning: {note}");
    }

    let permission_mode: PermissionMode = cli
        .permission_mode
        .or(settings.permission_mode.clone())
        .unwrap_or_else(|| "default".to_string())
        .parse()?;

    let client = Client::from_settings_at(&settings, &home)?;
    let mut system = build_system(
        &load_memory(&home, &project_dir),
        load_project_memory(&home, &project_dir),
        settings.cache_control.unwrap_or(false),
        &project_dir,
    );
    // The crew note, room etiquette and experience index — shared with the
    // app-server frontend (D156), because a prompt block only the console
    // pushes is a fourth answer the parity ledger would never see.
    crate::system::push_main_extras(&mut system, &home, &project_dir, &settings);

    // Startup notes for the TUI: everything printed to stderr before the
    // alternate screen opens is wiped by it — the fullscreen (default) host
    // never showed these. They still go to stderr for headless/log capture.
    let mut startup_notes: Vec<String> = config_notes;
    // A started session opens its transcript, rather than waiting for the first
    // thing written to it (D155). `session/start` has always done this over the
    // wire, and the two frontends must not disagree about whether a session that
    // exists is one `--continue`, `/resume` and `session/list` can find, or one
    // another process could claim the name of. Failing to open it is the same
    // condition as failing to create it: history will not persist, and the
    // warning below says so.
    let started = |held: Transcript| -> Result<Transcript, crate::transcript::TranscriptError> {
        held.activate()?;
        Ok(held)
    };
    let (transcript, initial_messages): (Option<Transcript>, Vec<Message>) = if cli.continue_ {
        match latest_transcript(&home)? {
            Some(t) => {
                t.activate()?;
                eprintln!("[bingo] continuing transcript: {}", t.path().display());
                (Some(t.clone()), t.load_messages()?)
            }
            None => match create_transcript(&home, &project_dir).and_then(started) {
                Ok(t) => (Some(t), Vec::new()),
                Err(e) => {
                    eprintln!(
                        "[bingo] warning: cannot create transcript (history will not persist): {e}"
                    );
                    startup_notes.push(format!(
                        "⚠ cannot create transcript (history will not persist): {e}"
                    ));
                    (None, Vec::new())
                }
            },
        }
    } else {
        match create_transcript(&home, &project_dir).and_then(started) {
            Ok(t) => (Some(t), Vec::new()),
            Err(e) => {
                // Said whichever frontend is starting: a session whose history
                // will not persist is the same fact in all three, and the
                // startup note below is simply read by only one of them.
                eprintln!(
                    "[bingo] warning: cannot create transcript (history will not persist): {e}"
                );
                startup_notes.push(format!(
                    "⚠ cannot create transcript (history will not persist): {e}"
                ));
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
        model.clone(),
        transcript.clone(),
        settings.permissions.clone(),
    );
    runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        settings.mcp_servers.clone(),
        settings.disabled_mcp_servers.iter().cloned().collect(),
    )));
    let _ = runtime.thinking_tx.send(settings.thinking_level.clone());
    // Provider restore: settings.provider (persisted by the /provider and /model menus)
    // switches when present and valid; an invalid name falls back to default + a warning (startup not blocked).
    if let Some(name) = settings.provider.as_deref() {
        match client.set_provider(name) {
            Ok(()) => {
                let _ = runtime.provider_tx.send(name.to_string());
            }
            Err(e) => {
                eprintln!(
                    "[bingo] warning: provider \"{name}\" is no longer valid, falling back to default: {e}"
                );
                startup_notes.push(format!(
                    "⚠ the provider \"{name}\" in settings is no longer valid; fell back to default: {e}"
                ));
            }
        }
    }
    // Tell the model what it is and what it can do (vision above all): a text-only
    // model must not be handed image-first work. Built after the provider restore so
    // the capabilities resolve against the provider actually in use.
    system.push(crate::system::model_capability_block(
        &model,
        &runtime.provider.borrow().clone(),
        &client.models(),
    ));
    // What the core reports about this session. The frontends still drive it
    // themselves (B7 attaches them), but `config/read` and `catalog/read` answer
    // out of this from the moment the actor starts.
    let core = crate::app::AppCore::start(crate::app::SessionSetup {
        title: transcript
            .as_ref()
            .map(crate::transcript::Transcript::name)
            .unwrap_or_default(),
        cwd: project_dir.clone(),
        locator: match transcript.as_ref() {
            Some(t) => crate::app::snapshot::SessionLocator::Path {
                path: t.path().to_path_buf(),
            },
            None => crate::app::snapshot::SessionLocator::Latest,
        },
        provider: runtime.provider.borrow().clone(),
        model: model.clone(),
        thinking: crate::app::snapshot::ThinkingLevel::ALL
            .into_iter()
            .find(|level| Some(level.as_str()) == settings.thinking_level.as_deref())
            .unwrap_or(crate::app::snapshot::ThinkingLevel::Off),
        permission_mode: match permission_mode {
            PermissionMode::Default => crate::app::snapshot::PermissionMode::Default,
            PermissionMode::AcceptEdits => crate::app::snapshot::PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions => {
                crate::app::snapshot::PermissionMode::BypassPermissions
            }
            PermissionMode::DontAsk => crate::app::snapshot::PermissionMode::DontAsk,
            PermissionMode::Plan => crate::app::snapshot::PermissionMode::Plan,
        },
        theme: crate::app::snapshot::ThemeChoice::ALL
            .into_iter()
            .find(|theme| Some(theme.as_str()) == settings.theme.as_deref())
            .unwrap_or(crate::app::snapshot::ThemeChoice::Auto),
        shell: crate::platform::shell().to_string(),
        shell_dialect: match crate::platform::shell_dialect() {
            crate::platform::ShellDialect::Posix => crate::app::snapshot::ShellDialect::Posix,
            crate::platform::ShellDialect::PowerShell => {
                crate::app::snapshot::ShellDialect::Powershell
            }
            crate::platform::ShellDialect::Cmd => crate::app::snapshot::ShellDialect::Cmd,
            crate::platform::ShellDialect::Unknown => crate::app::snapshot::ShellDialect::Unknown,
        },
        resumed: cli.continue_,
        channel_limits: crate::channels::ChannelLimits::from_settings(&settings),
        catalog: crate::app::catalog::CatalogSource::load(
            &home,
            &user_dir,
            &project_dir,
            settings.clone(),
        ),
        ..Default::default()
    });
    let session = Arc::new(Session {
        client,
        runtime,
        permission_mode,
        settings,
        system,
        depth: 0,
        cwd: Arc::new(std::sync::Mutex::new(project_dir.clone())),
        home: home.clone(),
        user_config_dir: user_dir.clone(),
        quiet: !cli.print,
        compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        core: core.clone(),
        watch: core.watch(),
        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&home, &task_list_key)),
        expand_tasks: expand_tx,
        agents: core.agents(),
        channels: core.channels(),
        turns: core.turns(),
        queue: core.queue(),
        submit: core.submit(),
        interactions: core.interactions(),
        mail: core.mail(),
        operations: core.operations(),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    });

    // The thing that runs a turn (D154). The console stopped driving its own
    // model calls here: it submits, the core opens the turn, and this runs it —
    // the same engine `bingo app-server` attaches, so the two frontends are one
    // product rather than two that agree by hand.
    if let Some(engine) = crate::engine::runner::SessionEngine::new(session.clone()) {
        core.attach_engine(engine);
    }

    // Share persistence: incrementally records subagent/channel snapshots per session (the data source for `bingo share`).
    // Same key as the task list = transcript file stem; create/read failures only warn (an enhancement, not a contract).
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
                "[bingo] warning: share store unavailable ({e}); bingo share will have the conversation view only"
            ),
        }
        // The room sidecar (Amendment #6): replay first, so a resumed session
        // comes back to the rooms it left, and only then start appending.
        let rooms = crate::app::roomlog::path(&home, &stem);
        session
            .channels
            .restore_rooms(crate::app::roomlog::replay(&rooms));
        session.channels.attach_sidecar(rooms);
    }

    let mode_str = session.permission_mode_str();
    crate::hooks::run_session_start(&session.settings.hooks, mode_str, &session.cwd()).await;

    // D31 startup default: project-bound team with autoStart (default true) → spawn it.
    // The whole tree, not just the root (D54): a chart declared in one file is one
    // formation, and a half-started org is worse than none.
    // Double opt-out: settings `team.autoStart:false` + `--no-team`.
    if !cli.no_team && session.settings.team.auto_start.unwrap_or(true) {
        match crate::team::load_team_tree(&project_dir) {
            Ok(Some(tree)) => {
                let name = tree.root().def.name.clone();
                let teams = tree.nodes().len();
                match crate::team::spawn_tree(&session, &tree, &home) {
                    Ok(summary) => {
                        let ready = summary.ready();
                        let total = ready + summary.failed.len();
                        let scope = if teams > 1 {
                            format!(" across {teams} teams")
                        } else {
                            String::new()
                        };
                        if summary.failed.is_empty() {
                            eprintln!(
                                "[team] {name} ready · {ready}/{total} on standby{scope} (/team status · /team stop)"
                            );
                        } else {
                            eprintln!(
                                "[team] {name} partially spawned · {ready}/{total}{scope} ({} failed; see /team status)",
                                summary.failed.len()
                            );
                        }
                    }
                    Err(e) => eprintln!(
                        "[team] {name} validation failed: {e} (fix and /team start to spawn)"
                    ),
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("[team] {} read failed: {e}", crate::team::TEAM_FILE),
        }
    }

    let result = async {
        if cli.print {
            let prompt = if !cli.prompt.is_empty() {
                cli.prompt.join(" ")
            } else {
                // A TTY stdin means nothing is piped in: reading would block
                // forever and look like a hang. Fail fast with usage instead.
                use std::io::IsTerminal;
                if std::io::stdin().is_terminal() {
                    return Err("no prompt provided (pass a prompt argument or pipe stdin)".into());
                }
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input)?;
                input.trim().to_string()
            };
            if prompt.is_empty() {
                // Propagate instead of eprintln+exit: the non-TTY error contract
                // (`[error] code=… msg=…`) only holds on the report_error path.
                return Err("no prompt provided (stdin was empty)".into());
            }
            // Subagents borrow the same stdin prompt (serialized in the Agent tool), so a
            // background instance can still ask rather than having the call auto-denied.
            session.agents.attach_ask(crate::query::stdin_ask());
            // The history is the transcript's, and the run reads it there: a
            // turn the core opened is run by the engine assembled above, which
            // is the same engine `bingo app-server` attaches. The memory pass is
            // that engine's too, after the turn closes.
            drop(initial_messages);
            crate::print::run(&core, &prompt).await?;
        } else {
            drop(initial_messages); // in interactive mode, --continue history is reused by later turns
            tui::run_tui_session(session.clone(), expand_rx, fullscreen, startup_notes).await?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    // D31 session-end persistence: latest history of team members (for cross-session
    // restore; failures are silent).
    if !cli.no_team && session.settings.team.auto_start.unwrap_or(true) {
        persist_team_memory(&session, &home, &session.cwd());
    }
    crate::hooks::run_session_end(&session.settings.hooks, mode_str, &session.cwd()).await;
    // The session actor is what everything else was reachable through, so it goes
    // last: it interrupts whatever is still running, fails every pending prompt
    // closed, and lets go of the instances that were holding it open (B3).
    core.close().await;
    result
}

/// `bingo share`: resolve a session, generate self-contained HTML, and write it
/// locally by default. `--public` explicitly opts into publishing; failed uploads
/// fall back to the same local artifact.
async fn run_share(
    home: &Path,
    key: Option<&str>,
    output: Option<PathBuf>,
    public: bool,
    open: bool,
) -> Result<(), crate::share::ShareError> {
    let transcript = crate::share::resolve_transcript(home, key)?;
    let stem = transcript.name();
    // Human-facing export: the full canonical conversation, not the compacted
    // projection the model sees.
    let messages = transcript.load_canonical()?;

    // Fall back to an empty doc when the share file is missing/corrupt (conversation-only view; the old-session main path).
    let share_path = crate::share::shares_dir(home).join(format!("{stem}.json"));
    let doc = match crate::share::ShareStore::load_or_create(&share_path) {
        Ok(store) => store.snapshot(),
        Err(e) => {
            eprintln!(
                "[bingo] warning: cannot read the share document ({e}); generating the conversation view only"
            );
            crate::share::ShareDoc::new(stem.clone())
        }
    };
    // Legacy-session fallback: when the share document has no agents/channels (the process started before share landed),
    // derive Team/DM/channel data from the main transcript.
    let doc = if doc.agents.is_empty() && doc.channels.is_empty() {
        crate::share::derive_share_doc(&stem, &messages)
    } else {
        doc
    };

    let html = crate::share_html::render(&doc, &messages);

    // Local export is the safe default; `--open` opens this file without publishing it.
    if !public {
        let out = output.unwrap_or_else(|| PathBuf::from(format!("{stem}.html")));
        let overwritten = out.exists();
        crate::share::write_html_atomic(&out, &html)?;
        println!(
            "[share] wrote {}{}",
            out.display(),
            if overwritten { " (overwritten)" } else { "" }
        );
        eprintln!(
            "[share] note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
        );
        if open {
            crate::share::open_in_browser(&out.display().to_string())?;
        }
        return Ok(());
    }

    // Safety preflight must be visible before the first upload byte leaves the machine.
    eprintln!(
        "[share] warning: about to publish publicly; anyone can access the full conversation and tool outputs, which may contain sensitive information."
    );

    // Public upload uses settings.share.baseUrl (the official service by default).
    let project_dir = std::env::current_dir()?;
    let user_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    let settings = crate::settings::load_settings(&user_dir, &project_dir).unwrap_or_default();
    let base = settings
        .share
        .base_url
        .unwrap_or_else(|| crate::share::DEFAULT_SHARE_BASE.to_string());
    let id = crate::share::share_id(&stem);
    match crate::share::upload_share(&base, &id, &html).await {
        Ok(url) => {
            println!("[share] published {url}");
            if open {
                crate::share::open_in_browser(&url)?;
            }
        }
        Err(e) => {
            // Upload failure falls back to a local file + a notice.
            eprintln!("[bingo] warning: upload failed ({e}); falling back to a local file.");
            let out = output.unwrap_or_else(|| PathBuf::from(format!("{stem}.html")));
            let overwritten = out.exists();
            crate::share::write_html_atomic(&out, &html)?;
            println!(
                "[share] wrote {}{}",
                out.display(),
                if overwritten { " (overwritten)" } else { "" }
            );
            eprintln!(
                "[share] note: this file contains the full conversation and tool outputs (possibly sensitive); review it before sharing."
            );
            if open {
                crate::share::open_in_browser(&out.display().to_string())?;
            }
        }
    }
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
    // A failed turn already carries a code: the core computed it from the error
    // the run hit, and that error is gone by the time this runs. The registry
    // maps a type to a code and could only answer GENERIC here.
    let code = match err.downcast_ref::<crate::print::PrintError>() {
        Some(failure) => failure.code(),
        None => crate::error::error_code_boxed(err),
    };
    let msg = crate::error::sanitize_msg(&err.to_string());
    eprintln!("[error] code={code} msg={msg}");
}

/// Persist the latest message history of every member in the tree (only members with
/// content; failures are silent — memory is an enhancement, not a contract). Each
/// member's history is filed under its own team's directory and branch, so a
/// department in another repo keeps its memory with that repo.
fn persist_team_memory(session: &Arc<Session>, home: &Path, project_dir: &std::path::Path) {
    let Ok(Some(tree)) = crate::team::load_team_tree(project_dir) else {
        return;
    };
    for node in tree.nodes() {
        let branch = crate::team::current_branch(&node.dir);
        for m in &node.def.members {
            if let Some((history, _, _)) = session.agents.view_of(&m.name)
                && !history.is_empty()
            {
                crate::team::save_member_history(
                    home,
                    &node.dir,
                    &branch,
                    &node.def.name,
                    &m.name,
                    &history,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn interactive_mode_defaults_to_fullscreen() {
        let cli = Cli::try_parse_from(["bingo"]).unwrap_or_else(|error| panic!("{error}"));

        assert!(cli.fullscreen_mode());
    }

    #[test]
    fn inline_flag_selects_terminal_scrollback_mode() {
        let cli =
            Cli::try_parse_from(["bingo", "--inline"]).unwrap_or_else(|error| panic!("{error}"));

        assert!(!cli.fullscreen_mode());
    }

    #[test]
    fn fullscreen_flag_remains_a_compatible_explicit_selection() {
        let cli = Cli::try_parse_from(["bingo", "--fullscreen"])
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(cli.fullscreen_mode());
    }

    /// The v1 JSON protocol's flags are gone with it (D140): the app-server
    /// replaces them, and a flag left parsing is a flag a client will find.
    #[test]
    fn the_deleted_json_protocol_flags_are_no_longer_arguments() {
        for args in [
            vec!["bingo", "--json-events"],
            vec!["bingo", "--probe"],
            vec!["bingo", "--inspect"],
            vec!["bingo", "--session", "project-123"],
        ] {
            let error = Cli::try_parse_from(args).expect_err("the flag must not parse");
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn inline_and_fullscreen_flags_conflict() {
        let error = Cli::try_parse_from(["bingo", "--inline", "--fullscreen"])
            .expect_err("display modes must be mutually exclusive");

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
