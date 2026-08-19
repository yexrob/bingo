//! Starting and resuming the one session an app-server connection has.
//!
//! One [`AppCore`] *is* one session, so `session/start` and `session/resume` are
//! not questions for the actor — they replace it. That makes the assembly here
//! the transport's: settings, the transcript this session writes, the catalogs it
//! reads, the room sidecar it replays, and the actor that owns all of them.
//!
//! The engine arrives here too (B7). A session's [`crate::query::Session`] — its
//! client, its system prompt, its transcript, its task store — is assembled the
//! same way `main.rs` assembles the console's, handed to
//! [`crate::engine::runner::SessionEngine`], and attached to the actor. Until
//! that attachment a core answers everything it can answer out of its own state
//! and refuses a run by name; after it, `conversation/submit` starts real turns.

use std::path::{Path, PathBuf};

use crate::app::snapshot::{PermissionMode, SessionLocator, ThemeChoice, ThinkingLevel};
use crate::app::{AppCore, SessionSetup};
use crate::app_server::AppServerError;
use crate::app_server::protocol::error::ProtocolErrorKind;
use crate::transcript::Transcript;

/// Why the transport would not start or resume a session, in the protocol's own
/// vocabulary. The detail is one sanitized English line; clients branch on the
/// kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub kind: ProtocolErrorKind,
    pub detail: String,
}

impl Refusal {
    fn new(kind: ProtocolErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: crate::error::sanitize_msg(&detail.into()),
        }
    }
}

/// Where this process reads its configuration and writes its state.
///
/// Resolved once, when the transport starts, rather than per request: a client
/// that moves the working directory of one session must not move the place the
/// next one looks for its settings.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub home: PathBuf,
    pub user_dir: PathBuf,
    pub cwd: PathBuf,
}

impl Bootstrap {
    /// The same three directories every other bingo entry point resolves.
    pub fn resolve() -> Result<Self, AppServerError> {
        let home = crate::storage::resolve_home().map_err(|error| AppServerError::Bootstrap {
            detail: error.to_string(),
        })?;
        let cwd = std::env::current_dir().map_err(|error| AppServerError::Bootstrap {
            detail: error.to_string(),
        })?;
        let user_dir = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Self {
            home,
            user_dir,
            cwd,
        })
    }
}

/// What the client asked for, with everything it left out still to be decided.
#[derive(Debug, Clone, Default)]
pub struct Wanted {
    pub cwd: Option<PathBuf>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<ThinkingLevel>,
    pub permission_mode: Option<PermissionMode>,
}

/// A started session: the actor, and the name it goes by on disk.
pub struct Started {
    pub core: AppCore,
    pub transcript: Option<Transcript>,
}

/// `session/start`: a new transcript in the chosen directory.
pub fn start(boot: &Bootstrap, wanted: &Wanted) -> Result<Started, Refusal> {
    let cwd = working_directory(boot, wanted)?;
    let transcript = crate::transcript::create(&boot.home, &cwd).map_err(|error| {
        Refusal::new(
            ProtocolErrorKind::BadArgument,
            format!("cannot create a transcript: {error}"),
        )
    })?;
    // Opened now rather than at the first thing written to it: the session a
    // client just started is one `session/list` can name and `session/resume`
    // can find, and holding it is what stops a second process writing it.
    transcript.activate().map_err(|error| {
        Refusal::new(
            ProtocolErrorKind::BadArgument,
            format!("cannot open the transcript: {error}"),
        )
    })?;
    assemble(boot, wanted, &cwd, Some(transcript), false)
}

/// `session/resume`: an existing transcript, named explicitly.
///
/// The locator is the durable name a session has across epochs — resource
/// identifiers die with the process that minted them, so a client that wants a
/// particular session names its transcript, not an old `sessionId`.
pub fn resume(
    boot: &Bootstrap,
    locator: &SessionLocator,
    wanted: &Wanted,
) -> Result<Started, Refusal> {
    let cwd = working_directory(boot, wanted)?;
    let transcript = locate(&boot.home, locator)?;
    transcript.activate().map_err(|error| {
        Refusal::new(
            ProtocolErrorKind::BadArgument,
            format!("cannot open that transcript: {error}"),
        )
    })?;
    assemble(boot, wanted, &cwd, Some(transcript), true)
}

/// The transcript a locator names, or the reason there is none.
///
/// Matching is exact. The fuzzy `--session` fragment left with the first
/// protocol (D140): a locator is a name a client was handed by `session/list`,
/// and guessing which session it meant is not the transport's job.
fn locate(home: &Path, locator: &SessionLocator) -> Result<Transcript, Refusal> {
    if let SessionLocator::Stem { stem } = locator
        && (stem.is_empty() || stem.contains(['/', '\\']) || stem.contains(".."))
    {
        return Err(Refusal::new(
            ProtocolErrorKind::BadArgument,
            "a session stem is a file name, not a path",
        ));
    }
    let sessions = crate::transcript::list(home).map_err(|error| {
        Refusal::new(
            ProtocolErrorKind::BadArgument,
            format!("cannot read the transcript directory: {error}"),
        )
    })?;
    let found = match locator {
        SessionLocator::Latest => sessions.into_iter().next(),
        SessionLocator::Stem { stem } => sessions.into_iter().find(|found| &found.name() == stem),
        SessionLocator::Path { path } => sessions.into_iter().find(|found| found.path() == path),
    };
    found.ok_or_else(|| {
        Refusal::new(
            ProtocolErrorKind::SessionNotFound,
            "no session matches that locator",
        )
    })
}

/// Where the session runs. A directory that does not exist is refused here
/// rather than found out later by the first tool that tries to use it.
fn working_directory(boot: &Bootstrap, wanted: &Wanted) -> Result<PathBuf, Refusal> {
    let Some(cwd) = wanted.cwd.clone() else {
        return Ok(boot.cwd.clone());
    };
    if !cwd.is_dir() {
        return Err(Refusal::new(
            ProtocolErrorKind::BadArgument,
            "the working directory does not exist",
        ));
    }
    Ok(cwd)
}

/// Everything both paths share: settings, catalogs, the actor, and the sidecars
/// a session keeps beside its transcript.
fn assemble(
    boot: &Bootstrap,
    wanted: &Wanted,
    cwd: &Path,
    transcript: Option<Transcript>,
    resumed: bool,
) -> Result<Started, Refusal> {
    let settings = crate::settings::load_settings(&boot.user_dir, cwd).map_err(|error| {
        Refusal::new(
            ProtocolErrorKind::BadArgument,
            format!("the settings could not be read: {error}"),
        )
    })?;
    let catalog =
        crate::app::catalog::CatalogSource::load(&boot.home, &boot.user_dir, cwd, settings.clone());
    let provider = match &wanted.provider {
        Some(name) => {
            if !catalog.providers().iter().any(|p| &p.name == name) {
                return Err(Refusal::new(
                    ProtocolErrorKind::BadArgument,
                    "that provider is not configured",
                ));
            }
            name.clone()
        }
        None => settings
            .provider
            .clone()
            .unwrap_or_else(|| crate::app::catalog::DEFAULT_PROVIDER.to_string()),
    };
    let model = wanted
        .model
        .clone()
        .or_else(|| settings.model.clone())
        .unwrap_or_else(|| crate::api::types::DEFAULT_MODEL.to_string());
    let thinking = wanted.thinking.unwrap_or_else(|| {
        ThinkingLevel::ALL
            .into_iter()
            .find(|level| Some(level.as_str()) == settings.thinking_level.as_deref())
            .unwrap_or(ThinkingLevel::Off)
    });
    let permission_mode = match wanted.permission_mode {
        Some(mode) => mode,
        None => permission_mode_of(settings.permission_mode.as_deref())?,
    };
    let theme = ThemeChoice::ALL
        .into_iter()
        .find(|theme| Some(theme.as_str()) == settings.theme.as_deref())
        .unwrap_or(ThemeChoice::Auto);
    crate::platform::init_shell(settings.shell.as_deref());
    let core = AppCore::start(SessionSetup {
        title: transcript
            .as_ref()
            .map(Transcript::name)
            .unwrap_or_default(),
        cwd: cwd.to_path_buf(),
        locator: match transcript.as_ref() {
            Some(transcript) => SessionLocator::Path {
                path: transcript.path().to_path_buf(),
            },
            None => SessionLocator::Latest,
        },
        provider,
        model,
        thinking,
        permission_mode,
        theme,
        shell: crate::platform::shell().to_string(),
        shell_dialect: match crate::platform::shell_dialect() {
            crate::platform::ShellDialect::Posix => crate::app::snapshot::ShellDialect::Posix,
            crate::platform::ShellDialect::PowerShell => {
                crate::app::snapshot::ShellDialect::Powershell
            }
            crate::platform::ShellDialect::Cmd => crate::app::snapshot::ShellDialect::Cmd,
            crate::platform::ShellDialect::Unknown => crate::app::snapshot::ShellDialect::Unknown,
        },
        resumed,
        channel_limits: crate::channels::ChannelLimits::from_settings(&settings),
        catalog,
        ..Default::default()
    });
    attach_sidecars(boot, &core, transcript.as_ref());
    attach_engine(
        boot,
        &core,
        cwd,
        &settings,
        transcript.as_ref(),
        permission_mode,
    );
    Ok(Started { core, transcript })
}

/// Give the core the thing that runs a turn.
///
/// The assembly is `main.rs`'s, because it has to be: a run reads its model, its
/// system prompt, its tools and its transcript from a [`crate::query::Session`],
/// and a GUI's turn must read them from the same place the console's does or the
/// two frontends are running different products.
///
/// A client that cannot be given an engine still gets a session. The client is
/// the one part that can fail here — an unreadable credential, a provider whose
/// endpoint will not resolve — and refusing the whole session over it would take
/// away the catalogs and the configuration reads that are exactly how a client
/// would diagnose it.
fn attach_engine(
    boot: &Bootstrap,
    core: &AppCore,
    cwd: &Path,
    settings: &crate::settings::Settings,
    transcript: Option<&Transcript>,
    permission_mode: PermissionMode,
) {
    let client = match crate::api::client::Client::from_settings_at(settings, &boot.home) {
        Ok(client) => client,
        Err(error) => {
            eprintln!(
                "[bingo] warning: no engine for this session ({error}); it can be read and \
                 configured but not run"
            );
            return;
        }
    };
    let model = settings
        .model
        .clone()
        .unwrap_or_else(|| crate::api::types::DEFAULT_MODEL.to_string());
    let mut runtime = crate::query::Runtime::new(
        model.clone(),
        transcript.cloned(),
        settings.permissions.clone(),
    );
    runtime.mcp = std::sync::Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        settings.mcp_servers.clone(),
        settings.disabled_mcp_servers.iter().cloned().collect(),
    )));
    let _ = runtime.thinking_tx.send(settings.thinking_level.clone());
    if let Some(name) = settings.provider.as_deref()
        && client.set_provider(name).is_ok()
    {
        let _ = runtime.provider_tx.send(name.to_string());
    }
    let mut system = crate::system::build_system(
        &crate::system::load_memory(&boot.home, cwd),
        crate::load_project_memory(&boot.home, cwd),
        settings.cache_control.unwrap_or(false),
        cwd,
    );
    system.push(crate::system::model_capability_block(
        &model,
        &runtime.provider.borrow().clone(),
        &client.models(),
    ));
    let key = transcript
        .map(Transcript::name)
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| crate::tasks::project_task_key(cwd));
    let session = std::sync::Arc::new(crate::query::Session {
        client,
        runtime,
        permission_mode: match permission_mode {
            PermissionMode::Default => crate::permission::PermissionMode::Default,
            PermissionMode::AcceptEdits => crate::permission::PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions => {
                crate::permission::PermissionMode::BypassPermissions
            }
            PermissionMode::DontAsk => crate::permission::PermissionMode::DontAsk,
            PermissionMode::Plan => crate::permission::PermissionMode::Plan,
        },
        settings: settings.clone(),
        system,
        depth: 0,
        cwd: std::sync::Arc::new(std::sync::Mutex::new(cwd.to_path_buf())),
        home: boot.home.clone(),
        user_config_dir: boot.user_dir.clone(),
        // stdout carries protocol frames and nothing else, and `quiet` is what
        // the engine's own `println!` progress is gated on — so this is not a
        // preference here, it is the framing contract.
        quiet: true,
        compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: core.watch(),
        tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&boot.home, &key)),
        expand_tasks: tokio::sync::watch::channel(false).0,
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
    // A subagent asks the same way main does: the prompt is the core's, and the
    // answer comes back through `interaction/respond` (D144).
    session
        .agents
        .attach_ask(crate::app::interaction::permission_ask(
            core.interactions(),
            crate::app::conversation::ConvKey::Main,
        ));
    let Some(engine) = crate::engine::runner::SessionEngine::new(session) else {
        return;
    };
    core.attach_engine(engine);
}

/// The two files a session keeps beside its transcript: the share document, and
/// the room log a resume replays (Amendment #6).
///
/// Replay happens before the writer is attached, so a resumed session comes back
/// to the rooms it left and only then starts appending. Neither file failing is
/// worth refusing a session over — both are enhancements to a session that is
/// otherwise complete — so a failure is a stderr diagnostic and nothing else.
fn attach_sidecars(boot: &Bootstrap, core: &AppCore, transcript: Option<&Transcript>) {
    let Some(stem) = transcript.map(Transcript::name).filter(|s| !s.is_empty()) else {
        return;
    };
    let share_path = crate::share::shares_dir(&boot.home).join(format!("{stem}.json"));
    match crate::share::ShareStore::load_or_create(&share_path) {
        Ok(store) => {
            core.agents().attach_share(store.clone());
            core.channels().attach_share(store);
        }
        Err(error) => eprintln!(
            "[bingo] warning: share store unavailable ({error}); bingo share will have the conversation view only"
        ),
    }
    let rooms = crate::app::roomlog::path(&boot.home, &stem);
    core.channels()
        .restore_rooms(crate::app::roomlog::replay(&rooms));
    core.channels().attach_sidecar(rooms);
}

/// The permission mode a settings file asked for, refused by name rather than
/// silently downgraded.
fn permission_mode_of(configured: Option<&str>) -> Result<PermissionMode, Refusal> {
    let Some(name) = configured else {
        return Ok(PermissionMode::Default);
    };
    PermissionMode::ALL
        .into_iter()
        .find(|mode| mode.as_str() == name)
        .ok_or_else(|| {
            Refusal::new(
                ProtocolErrorKind::BadArgument,
                "the configured permission mode is not one this build knows",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home of its own, so a test never reads or writes the developer's.
    struct Root(PathBuf);

    impl Root {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            let path = std::env::temp_dir().join(format!(
                "bingo-app-server-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
            Self(path)
        }

        fn boot(&self) -> Bootstrap {
            Bootstrap {
                home: self.0.clone(),
                user_dir: self.0.join("config"),
                cwd: self.0.clone(),
            }
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn starting_writes_a_transcript_the_locator_can_name_again() {
        let root = Root::new("start");
        let boot = root.boot();
        let started = start(&boot, &Wanted::default()).unwrap_or_else(|error| panic!("{error:?}"));
        let stem = started
            .transcript
            .as_ref()
            .map(Transcript::name)
            .unwrap_or_default();
        assert!(!stem.is_empty());
        // The file only exists once something is written to it; the name is the
        // durable half, and it is what resume takes.
        started
            .transcript
            .as_ref()
            .map(|transcript| transcript.activate())
            .transpose()
            .unwrap_or_else(|error| panic!("{error}"));
        let resumed = resume(
            &boot,
            &SessionLocator::Stem { stem: stem.clone() },
            &Wanted::default(),
        );
        assert!(resumed.is_ok(), "{resumed:?}", resumed = resumed.err());
    }

    #[test]
    fn a_locator_that_names_nothing_is_refused_by_name() {
        let root = Root::new("missing");
        let boot = root.boot();
        for locator in [
            SessionLocator::Latest,
            SessionLocator::Stem {
                stem: "not-a-session".to_string(),
            },
            SessionLocator::Path {
                path: root.0.join("nowhere.jsonl"),
            },
        ] {
            let error = resume(&boot, &locator, &Wanted::default())
                .err()
                .unwrap_or_else(|| panic!("{locator:?} must not resolve"));
            assert_eq!(error.kind, ProtocolErrorKind::SessionNotFound);
        }
    }

    /// A stem is a file name. One that walks out of the transcript directory is
    /// refused rather than followed.
    #[test]
    fn a_stem_cannot_walk_out_of_the_transcript_directory() {
        let root = Root::new("escape");
        let error = resume(
            &root.boot(),
            &SessionLocator::Stem {
                stem: "../../etc/passwd".to_string(),
            },
            &Wanted::default(),
        )
        .err()
        .unwrap_or_else(|| panic!("a traversing stem must be refused"));
        assert_eq!(error.kind, ProtocolErrorKind::BadArgument);
    }

    #[test]
    fn a_working_directory_that_does_not_exist_is_refused() {
        let root = Root::new("cwd");
        let error = start(
            &root.boot(),
            &Wanted {
                cwd: Some(root.0.join("nowhere")),
                ..Wanted::default()
            },
        )
        .err()
        .unwrap_or_else(|| panic!("a missing directory must be refused"));
        assert_eq!(error.kind, ProtocolErrorKind::BadArgument);
    }

    #[test]
    fn a_provider_that_is_not_configured_is_refused() {
        let root = Root::new("provider");
        let error = start(
            &root.boot(),
            &Wanted {
                provider: Some("not-a-provider".to_string()),
                ..Wanted::default()
            },
        )
        .err()
        .unwrap_or_else(|| panic!("an unknown provider must be refused"));
        assert_eq!(error.kind, ProtocolErrorKind::BadArgument);
    }
}
