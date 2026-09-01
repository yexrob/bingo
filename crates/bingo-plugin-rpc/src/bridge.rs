//! One plugin: its process, the handshake it answered with, and what happens
//! when it dies.
//!
//! A process is allowed to die (ADR-0015 §5). There is no supervisor and no
//! health check: a source read finds the pipe closed, answers with nothing —
//! which is never wrong (ADR-0009 §1) — leaves one notice, and starts the next
//! attempt on its own task. Consecutive failures back off, so a plugin whose
//! command no longer exists costs one spawn per turn at first and then almost
//! nothing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bingo_sdk::{Command as SdkCommand, CommandSpec, Tool, ToolSpec};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::command::PluginCommand;
use crate::completions::Completions;
use crate::connection::Connection;
use crate::manifest::Entry;
use crate::notice::{Notice, Notices};
use crate::tool::PluginTool;
use crate::wire::{HostEnv, InitializeParams, InitializeResult, PROTOCOL, name};

/// How long a process has to spawn and answer `initialize`. A plugin is a
/// local process the person installed; one that cannot say what it is in this
/// long is broken, and waiting longer only delays the session.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The wait before a second consecutive attempt, doubling to [`BACKOFF_MAX`].
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The wait before the attempt after `failures` failed ones. Zero for the
/// first: a process that has just died is respawned at the next read, and only
/// a run of failures is worth slowing down.
pub fn backoff(failures: u32) -> Duration {
    match failures {
        0 => Duration::ZERO,
        n => BACKOFF_BASE
            .saturating_mul(2u32.saturating_pow(n.min(16) - 1))
            .min(BACKOFF_MAX),
    }
}

/// What one process answered `initialize` with, and the pipe it answered on.
struct Live {
    connection: Arc<Connection>,
    tools: Vec<ToolSpec>,
    commands: Vec<CommandSpec>,
}

#[derive(Default)]
struct State {
    live: Option<Arc<Live>>,
    /// Consecutive failed attempts; the backoff is a function of this alone.
    failures: u32,
    /// No attempt before this instant.
    next_attempt: Option<Instant>,
    /// An attempt is in flight, so a second reader starts no second process.
    starting: bool,
}

/// Everything about one plugin that does not change, and the one thing that does.
pub struct Bridge {
    name: String,
    root: PathBuf,
    /// Already resolved: `${PLUGIN_ROOT}` is a manifest's spelling, not a
    /// process's, so nothing past this point knows the placeholder exists.
    entry: Entry,
    config: Value,
    env: HostEnv,
    data_dir: PathBuf,
    notices: Arc<Notices>,
    /// One per plugin, not one per command object: a command object is built
    /// afresh on every source read.
    completions: Arc<Completions>,
    state: Mutex<State>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("name", &self.name)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Bridge {
    pub fn new(
        name: impl Into<String>,
        root: PathBuf,
        entry: Entry,
        config: Value,
        env: HostEnv,
        data_dir: PathBuf,
        notices: Arc<Notices>,
    ) -> Self {
        Self {
            name: name.into(),
            root,
            entry,
            config,
            env,
            data_dir,
            notices,
            completions: Arc::new(Completions::default()),
            state: Mutex::new(State::default()),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Spawn and shake hands now, waiting for the outcome. `Plugin::start`
    /// does this so the first turn of a session has the plugin's tools;
    /// a respawn does it on a task of its own.
    pub async fn connect(&self) {
        if !self.claim().await {
            return;
        }
        let outcome = self.handshake().await;
        self.file(outcome).await;
    }

    /// The tools of a living process, and nothing when there is none.
    pub async fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let Some(live) = self.ready().await else {
            return Vec::new();
        };
        live.tools
            .iter()
            .map(|spec| {
                Arc::new(PluginTool::new(
                    &self.name,
                    spec.clone(),
                    Arc::clone(&live.connection),
                    Arc::clone(&self.notices),
                )) as Arc<dyn Tool>
            })
            .collect()
    }

    /// The commands of a living process, and nothing when there is none.
    pub async fn commands(self: &Arc<Self>) -> Vec<Arc<dyn SdkCommand>> {
        let Some(live) = self.ready().await else {
            return Vec::new();
        };
        live.commands
            .iter()
            .map(|spec| {
                Arc::new(PluginCommand::new(
                    &self.name,
                    spec.clone(),
                    Arc::clone(&live.connection),
                    Arc::clone(&self.completions),
                )) as Arc<dyn SdkCommand>
            })
            .collect()
    }

    /// End the process, and leave nothing that would respawn it.
    pub async fn stop(&self) {
        let mut state = self.state.lock().await;
        state.next_attempt = None;
        if let Some(live) = state.live.take() {
            live.connection.stop().await;
        }
    }

    /// The live process, or nothing plus a respawn on its way. This is the
    /// only place a death is noticed, so it is the only place one is reported.
    async fn ready(self: &Arc<Self>) -> Option<Arc<Live>> {
        let mut state = self.state.lock().await;
        if let Some(live) = &state.live
            && live.connection.is_alive()
        {
            return Some(Arc::clone(live));
        }
        let departed = state.live.take();
        if departed.is_some() {
            // A process that has just died is due an attempt at once, and the
            // reset shares the critical section with the take so that a
            // failure filed meanwhile cannot have its backoff undone.
            state.failures = 0;
            state.next_attempt = None;
        }
        drop(state);
        if let Some(live) = departed {
            self.died(&live);
        }
        self.respawn();
        None
    }

    /// Say the process ended, once however many readers saw it.
    fn died(&self, live: &Live) {
        if live.connection.claim_death() {
            self.notices.push(Notice::warn(
                "PLUGIN_DIED",
                format!("the {} plugin process ended; restarting it", self.name),
            ));
        }
    }

    /// Start an attempt on its own task, unless one is running or the backoff
    /// has not run out. A source read must never wait on a spawn.
    fn respawn(self: &Arc<Self>) {
        let bridge = Arc::clone(self);
        tokio::spawn(async move { bridge.connect().await });
    }

    /// Whether this caller owns the next attempt. Holds the lock only long
    /// enough to say so: the spawn that follows holds nothing.
    async fn claim(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.starting || state.live.is_some() {
            return false;
        }
        if state.next_attempt.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        state.starting = true;
        true
    }

    async fn file(&self, outcome: Result<Live, String>) {
        let mut state = self.state.lock().await;
        state.starting = false;
        match outcome {
            Ok(live) => {
                state.live = Some(Arc::new(live));
                state.failures = 0;
                state.next_attempt = None;
            }
            Err(why) => {
                state.failures += 1;
                state.next_attempt = Some(Instant::now() + backoff(state.failures));
                self.notices.push(Notice::warn(
                    "PLUGIN_UNAVAILABLE",
                    format!("the {} plugin is not running: {why}", self.name),
                ));
            }
        }
    }

    /// Spawn, ask what it is, and believe only that it answered.
    async fn handshake(&self) -> Result<Live, String> {
        let connection = Arc::new(Connection::spawn(
            &self.name,
            &self.entry,
            &self.root,
            &self.data_dir,
        )?);
        let answered = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.initialize(&connection)).await;
        match answered {
            Ok(Ok(result)) => Ok(Live {
                connection,
                tools: result.tools,
                commands: result.commands,
            }),
            Ok(Err(why)) => {
                connection.stop().await;
                Err(why)
            }
            Err(_) => {
                connection.stop().await;
                Err(format!(
                    "initialize timed out after {}s",
                    HANDSHAKE_TIMEOUT.as_secs()
                ))
            }
        }
    }

    async fn initialize(&self, connection: &Connection) -> Result<InitializeResult, String> {
        let params = InitializeParams {
            protocol: PROTOCOL,
            plugin_root: self.root.clone(),
            config: self.config.clone(),
            env: self.env.clone(),
        };
        let value = serde_json::to_value(params).map_err(|e| e.to_string())?;
        let answer = connection
            .request(name::INITIALIZE, value)
            .await
            .map_err(|e| e.message)?;
        let result: InitializeResult =
            serde_json::from_value(answer).map_err(|e| format!("initialize: {e}"))?;
        check_protocol(&result)?;
        Ok(result)
    }
}

/// A major this host does not speak is refused rather than guessed at
/// (ADR-0015 §Consequences): a wire whose meaning is unknown is worse than no
/// wire at all.
fn check_protocol(result: &InitializeResult) -> Result<(), String> {
    if result.protocol == PROTOCOL {
        return Ok(());
    }
    Err(format!(
        "it speaks plugin protocol {}, this host speaks {PROTOCOL}",
        result.protocol
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(protocol: u32) -> InitializeResult {
        InitializeResult {
            protocol,
            name: "wordcount".into(),
            version: "0.1.0".into(),
            tools: Vec::new(),
            commands: Vec::new(),
            contributors: Vec::new(),
            compactors: Vec::new(),
        }
    }

    #[test]
    fn the_first_attempt_after_a_death_waits_for_nothing() {
        assert_eq!(backoff(0), Duration::ZERO);
    }

    #[test]
    fn a_run_of_failures_backs_off_and_then_stops_growing() {
        assert_eq!(backoff(1), BACKOFF_BASE);
        assert_eq!(backoff(2), BACKOFF_BASE * 2);
        assert_eq!(backoff(3), BACKOFF_BASE * 4);
        assert_eq!(backoff(20), BACKOFF_MAX);
        assert_eq!(backoff(u32::MAX), BACKOFF_MAX, "and never overflows");
    }

    #[test]
    fn this_host_speaks_one_major_and_refuses_the_rest() {
        assert!(check_protocol(&result(PROTOCOL)).is_ok());
        let later = PROTOCOL + 1;
        let why = check_protocol(&result(later)).expect_err("a later major is refused");
        assert!(why.contains(&format!("protocol {later}")), "{why}");
        assert!(why.contains(&format!("speaks {PROTOCOL}")), "{why}");
        let earlier = PROTOCOL - 1;
        let why = check_protocol(&result(earlier)).expect_err("and so is an earlier one");
        assert!(why.contains(&format!("protocol {earlier}")), "{why}");
    }
}
