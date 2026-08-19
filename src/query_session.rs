use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;

use crate::api::client::Client;
use crate::api::contract::SystemBlock;
use crate::permission::PermissionMode;
use crate::settings::Settings;
use crate::transcript::Transcript;

/// Session runtime mutable via slash commands (/model /clear /resume /permissions):
/// watch channels are read by the query loop each turn.
#[derive(Clone)]
pub struct Runtime {
    pub model_tx: watch::Sender<String>,
    pub model: watch::Receiver<String>,
    pub transcript_tx: watch::Sender<Option<Transcript>>,
    pub transcript: watch::Receiver<Option<Transcript>>,
    /// Runtime permission rules table (modified via /permissions; initially from settings).
    pub permissions: Arc<std::sync::Mutex<crate::settings::PermissionRules>>,
    /// Current provider (/provider switch; "default" = top-level apiKey/apiBaseUrl/env).
    pub provider_tx: watch::Sender<String>,
    pub provider: watch::Receiver<String>,
    /// Current thinking level (/think switch; None = no thinking parameter sent).
    pub thinking_tx: watch::Sender<Option<String>>,
    pub thinking: watch::Receiver<Option<String>>,
    /// MCP connection manager (lazy connection cache; initialized from settings at main
    /// construction; tests default to an empty manager — no MCP tools, behavior unchanged).
    pub mcp: Arc<tokio::sync::Mutex<crate::mcp::McpManager>>,
    /// Rewind checkpoint recorder (D91): which turn the file snapshots being
    /// taken right now belong to. Starts detached, so a host that never opens a
    /// checkpoint records nothing and costs nothing.
    pub rewind: Arc<crate::rewind::Recorder>,
}

impl Runtime {
    pub fn new(
        model: String,
        transcript: Option<Transcript>,
        permissions: crate::settings::PermissionRules,
    ) -> Self {
        let (model_tx, model) = watch::channel(model);
        let (transcript_tx, transcript) = watch::channel(transcript);
        let (provider_tx, provider) = watch::channel("default".to_string());
        let (thinking_tx, thinking) = watch::channel(None);
        Self {
            model_tx,
            model,
            transcript_tx,
            transcript,
            permissions: Arc::new(std::sync::Mutex::new(permissions)),
            provider_tx,
            provider,
            thinking_tx,
            thinking,
            rewind: Arc::new(crate::rewind::Recorder::default()),
            mcp: Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
                HashMap::new(),
                Default::default(),
            ))),
        }
    }
}

/// Full context of a query (shared by TUI and headless).
#[derive(Clone)]
pub struct Session {
    pub client: Client,
    /// Runtime state mutable via slash commands (model/transcript/permission rules).
    pub runtime: Runtime,
    pub permission_mode: PermissionMode,
    pub settings: Settings,
    pub system: Vec<SystemBlock>,
    /// Sub-agent nesting depth (Agent tool recursion).
    pub depth: usize,
    /// Session working directory, shared by main and all derived sub-sessions.
    pub cwd: Arc<std::sync::Mutex<PathBuf>>,
    /// User home (memdir memory location).
    pub home: PathBuf,
    /// User config dir (`$XDG_CONFIG_HOME` or `~/.config`), resolved once at
    /// startup: scoped settings writes and /config source display read it
    /// (re-reading the env in library code would break test hermeticity).
    pub user_config_dir: PathBuf,
    /// Interactive TUI session: suppress stderr progress prints (to avoid polluting the screen).
    pub quiet: bool,
    /// Consecutive auto-compact failure count (circuit breaker: skip after MAX_COMPACT_FAILURES).
    pub compact_failures: Arc<std::sync::atomic::AtomicU64>,
    /// The session actor itself, which every handle below is a face of.
    ///
    /// A front end needs it to *attach* — to buy the ordered frame channel a
    /// client reads its projection from — and attaching is a write, so this is
    /// reachable from anywhere a session is, with or without a runtime
    /// (B7b ruling ②).
    pub core: crate::app::AppCore,
    /// Background work — commands, agent runs, room operations — as the session
    /// actor holds it (B2b). A handle, not a table: the state is the actor's,
    /// and every change here is a message on its one queue.
    pub watch: crate::watch::WatchHandle,
    /// Task store (shared by the Task tool family + TUI task panel + reminder injection).
    pub tasks: Arc<crate::tasks::TaskStore>,
    /// Task panel expand signal (subscribed by the TUI loop).
    pub expand_tasks: watch::Sender<bool>,
    /// The subagent instances (continuation/lifecycle). A handle on the session
    /// actor's state; a sub-session carries a clone, so main and every instance
    /// reach one table through one queue.
    pub agents: crate::agents::AgentHandle,
    /// The rooms, and the main agent's inbox. The same shape as `agents`.
    pub channels: crate::channels::ChannelHandle,
    /// The turns in flight, and the items they produce. A run opens one here and
    /// closes it exactly once (B3); the handle is what makes the terminal state
    /// the actor's fact rather than a message an abort could swallow.
    pub turns: crate::app::turn::TurnHandle,
    /// The input queue, and the barrier/pull-back race it arbitrates (B3).
    pub queue: crate::app::queue::QueueHandle,
    /// The one submission path: what a composer line is and where it goes are
    /// the core's to decide, not a front end's (B3).
    pub submit: crate::app::submit::SubmitHandle,
    /// The prompts a run is stopped on: permission and question alike, with the
    /// oneshot that answers them held by the actor rather than by a surface (B3).
    pub interactions: crate::app::interaction::InteractionHandle,
    /// The mail waiting for main, and whether it is time to read it. The window,
    /// the deadline and the idle gates are the core's one answer, so both
    /// frontends wake main the same way (乙案, B4).
    pub mail: crate::app::mail::MailHandle,
    /// Accepted work that is not a turn — a team coming up, a login, a share —
    /// each with progress and exactly one terminal state (B4).
    pub operations: crate::app::operation::OperationHandle,
    /// This session's instance name (sub-agents = Some(registry name); main session None,
    /// channel member name main).
    pub instance: Option<String>,
    /// Images the user mounted on the input box, addressed by the `#[image N]` markers left in
    /// the message text. Sub-sessions share the table, so main forwards an image to a
    /// subagent by repeating its marker.
    pub attachments: Arc<crate::api::image::Attachments>,
}

impl Session {
    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        *self.cwd.lock().unwrap_or_else(|e| e.into_inner()) = cwd;
    }

    pub fn permission_mode_str(&self) -> &'static str {
        match self.permission_mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }
}
