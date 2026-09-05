//! The jobs: shell commands that are no longer the turn's to wait for.
//!
//! One table per process, keyed by a short minted id that every verb accepts
//! by prefix. A job is a process group, the log it writes and how it ended.
//!
//! **A job lives exactly as long as this process.** There is no daemon and no
//! persisted queue (ADR-0018): when bingo exits, every job's group goes with
//! it and only the log file is left behind. Out-living the process is a
//! schedule's business, not a job's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bingo_sdk::{CancellationToken, HostHandle, SessionId, View};
use serde_json::Value;
use tokio::sync::watch;

/// The plugin the live signal is published under, and the kind it fills.
pub const PLUGIN: &str = "bingo.tools.bash";
pub const KIND: &str = "jobs";

/// The custom kind a call that went to the background answers with
/// (ADR-0038): a surface that has learned it draws the job's state from the
/// set, and one that has not draws the fold.
pub const SHOWN: &str = "job";

/// Characters of a command the rail and a notification show. Enough to tell
/// two builds apart, short enough for a card.
const HEAD: usize = 48;

/// What every job id starts with, as every id in this tree is prefixed: it
/// says what the thing is, and it is a prefix a person can type when only one
/// job is running.
pub const PREFIX: &str = "job_";

/// Random characters after the prefix. Long enough that two runs' logs do not
/// collide in the same directory, short enough to type.
const ID_LEN: usize = 8;

/// A fresh id. The tail of a ULID is its random half; the head is the clock,
/// and two jobs started in the same second should not share a prefix.
pub fn mint() -> String {
    let raw = ulid::Ulid::generate().to_string().to_lowercase();
    let slug: String = raw
        .chars()
        .skip(raw.chars().count().saturating_sub(ID_LEN))
        .collect();
    format!("{PREFIX}{slug}")
}

/// The id a log file's name carries. The log is named after the job, so the
/// id is read back out of it rather than kept twice.
pub fn id_of(log: &Path) -> String {
    log.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where a job has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Exited {
        code: i32,
    },
    /// `KillShell` ended it, or the grace after it did.
    Killed,
}

impl State {
    pub fn ended(self) -> bool {
        !matches!(self, State::Running)
    }

    /// How a job's state reads in a result or a notification.
    pub fn said(self) -> String {
        match self {
            State::Running => "running".into(),
            State::Exited { code } => format!("exited with code {code}"),
            State::Killed => "killed".into(),
        }
    }
}

/// One background command.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub command: String,
    pub log: PathBuf,
    /// The session that started it, and the one its completion wakes.
    pub session: SessionId,
    /// The clock time it started, as the rail shows it. A card holds the fact
    /// rather than an age, which would be wrong a second after it was drawn.
    pub since: String,
    started: Instant,
    state: watch::Sender<State>,
    /// Cancelled by `KillShell`; the job's own task does the killing.
    kill: CancellationToken,
}

impl Job {
    pub fn new(id: String, command: String, log: PathBuf, session: SessionId) -> Self {
        Self {
            id,
            command,
            log,
            session,
            since: jiff::Zoned::now().strftime("%H:%M:%S").to_string(),
            started: Instant::now(),
            state: watch::Sender::new(State::Running),
            kill: CancellationToken::new(),
        }
    }

    pub fn state(&self) -> State {
        *self.state.borrow()
    }

    /// Called by the job's own task, and by nobody else.
    pub fn finished(&self, state: State) {
        self.state.send_replace(state);
    }

    /// Ask the job to die. The task that owns the process does the signalling.
    pub fn ask_to_die(&self) {
        self.kill.cancel();
    }

    pub fn killed(&self) -> CancellationToken {
        self.kill.clone()
    }

    /// Wait until the job is over. A job that is already over returns at once.
    pub async fn wait(&self) -> State {
        let mut watching = self.state.subscribe();
        loop {
            let state = *watching.borrow_and_update();
            if state.ended() {
                return state;
            }
            if watching.changed().await.is_err() {
                return self.state();
            }
        }
    }

    /// How long it has been going, as a person would say it.
    pub fn age(&self) -> String {
        let seconds = self.started.elapsed().as_secs();
        match seconds {
            0..=59 => format!("{seconds}s"),
            60..=3599 => format!("{}m", seconds / 60),
            _ => format!("{}h", seconds / 3600),
        }
    }

    /// The job as a message names it.
    pub fn named(&self) -> String {
        format!("{} (`{}`)", self.id, head(&self.command))
    }
}

/// What a person sees under the call that started a job, beside the text the
/// model reads (ADR-0013 §2, the block lane): the id and the command as data
/// for a surface that knows the kind, and one line for one that does not.
pub fn shown(job: &Job) -> View {
    View::Custom {
        kind: SHOWN.into(),
        data: serde_json::json!({ "id": job.id, "command": head(&job.command) }),
        fold: format!("Started in the background as {}", job.id),
    }
}

/// The first line of a command, clipped: what a card and a notification show.
pub fn head(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    if line.chars().count() <= HEAD {
        return line.to_string();
    }
    let kept: String = line.chars().take(HEAD - 1).collect();
    format!("{kept}…")
}

/// What an id or a prefix named.
#[derive(Debug)]
enum Named {
    One(Arc<Job>),
    Unknown,
    /// The ids it could have meant.
    Ambiguous(Vec<String>),
}

/// Every job this process started, finished ones included: a job's output is
/// read after it ends as often as while it runs.
#[derive(Debug, Default)]
pub struct Jobs {
    table: Mutex<BTreeMap<String, Arc<Job>>>,
}

impl Jobs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file(&self, job: Arc<Job>) {
        self.locked().insert(job.id.clone(), job);
    }

    /// The job a prefix names, or what to tell the caller instead. Every verb
    /// that takes an id comes through here, so a mistyped one is answered the
    /// same way whichever asked.
    pub fn find(&self, prefix: &str) -> Result<Arc<Job>, String> {
        match self.named(prefix) {
            Named::One(job) => Ok(job),
            Named::Ambiguous(ids) => Err(format!(
                "`{}` could be any of {}; say more of the id.",
                prefix.trim(),
                ids.join(", ")
            )),
            Named::Unknown => Err(format!(
                "no job is called `{}`. {}",
                prefix.trim(),
                self.roll()
            )),
        }
    }

    /// The jobs there are to name, for the answer to one that is not there.
    fn roll(&self) -> String {
        let known: Vec<String> = self
            .locked()
            .values()
            .map(|job| format!("{} ({})", job.id, job.state().said()))
            .collect();
        match known.as_slice() {
            [] => "No shell command has been backgrounded in this run.".into(),
            some => format!("The jobs there are: {}.", some.join(", ")),
        }
    }

    /// The job a prefix names. An exact id always wins over the ids it is a
    /// prefix of.
    fn named(&self, prefix: &str) -> Named {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return Named::Unknown;
        }
        let table = self.locked();
        if let Some(exact) = table.get(prefix) {
            return Named::One(exact.clone());
        }
        let matched: Vec<&Arc<Job>> = table
            .values()
            .filter(|job| job.id.starts_with(prefix))
            .collect();
        match matched.as_slice() {
            [] => Named::Unknown,
            [one] => Named::One((*one).clone()),
            many => Named::Ambiguous(many.iter().map(|job| job.id.clone()).collect()),
        }
    }

    /// The jobs one session has running, oldest first.
    pub fn running_in(&self, session: &SessionId) -> Vec<Arc<Job>> {
        self.locked()
            .values()
            .filter(|job| &job.session == session && !job.state().ended())
            .cloned()
            .collect()
    }

    /// What a session's jobs look like on the rail (ADR-0013 §2): a table
    /// while any run, and nothing at all when none do.
    pub fn view(&self, session: &SessionId) -> Value {
        let rows: Vec<Vec<String>> = self
            .running_in(session)
            .iter()
            .map(|job| vec![job.id.clone(), head(&job.command), job.since.clone()])
            .collect();
        if rows.is_empty() {
            return Value::Null;
        }
        let view = View::Table {
            headers: vec!["job".into(), "command".into(), "since".into()],
            rows,
        };
        serde_json::to_value(view).unwrap_or(Value::Null)
    }

    /// Publish the set as it is now. One signal per change of it: a job that
    /// only wrote more output has not changed the set.
    pub async fn publish(&self, host: &HostHandle, session: &SessionId) {
        let payload = self.view(session);
        if let Err(error) = host.signal(session, PLUGIN, KIND, payload).await {
            tracing::debug!(%error, "the running jobs were not published");
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Job>>> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::from_raw("ses_test")
    }

    fn job(command: &str) -> Arc<Job> {
        Arc::new(Job::new(
            mint(),
            command.to_string(),
            PathBuf::from("/tmp/job.log"),
            session(),
        ))
    }

    fn with_id(id: &str) -> Arc<Job> {
        Arc::new(Job::new(
            id.into(),
            "sleep 1".into(),
            PathBuf::from("/tmp/x.log"),
            session(),
        ))
    }

    #[test]
    fn a_job_reads_its_id_back_out_of_the_log_it_writes() {
        assert_eq!(id_of(Path::new("/data/bash/ab12cd34.log")), "ab12cd34");
        assert_eq!(id_of(Path::new("")), "");
    }

    #[test]
    fn a_minted_id_is_short_prefixed_lowercase_and_unique() {
        let ids: Vec<String> = (0..100).map(|_| mint()).collect();
        for id in &ids {
            assert!(id.starts_with(PREFIX), "{id}");
            assert_eq!(id.chars().count(), PREFIX.len() + ID_LEN, "{id}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{id}"
            );
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "minted ids collided");
    }

    #[test]
    fn a_prefix_names_one_job_and_an_exact_id_beats_the_ids_it_prefixes() {
        let jobs = Jobs::new();
        jobs.file(with_id("ab12"));
        jobs.file(with_id("ab12cd34"));
        jobs.file(with_id("ff000000"));
        assert_eq!(jobs.find("ff").expect("one job").id, "ff000000");
        assert_eq!(jobs.find("ab12").expect("the exact id").id, "ab12");
        let ambiguous = jobs.find("ab").expect_err("two jobs start with ab");
        assert!(ambiguous.contains("ab12, ab12cd34"), "{ambiguous}");
    }

    #[test]
    fn an_id_nobody_has_is_answered_with_the_ones_there_are() {
        let jobs = Jobs::new();
        let empty = jobs.find("ab").expect_err("no jobs at all");
        assert!(
            empty.contains("No shell command has been backgrounded"),
            "{empty}"
        );
        assert!(jobs.find("").is_err(), "an empty id names nothing");

        jobs.file(with_id("ab12cd34"));
        let unknown = jobs.find("zz").expect_err("no such job");
        assert!(unknown.contains("no job is called `zz`"), "{unknown}");
        assert!(unknown.contains("ab12cd34 (running)"), "{unknown}");
    }

    #[test]
    fn only_the_running_jobs_of_this_session_are_on_the_rail() {
        let jobs = Jobs::new();
        let running = job("cargo test --workspace");
        let over = job("cargo build");
        over.finished(State::Exited { code: 0 });
        jobs.file(running.clone());
        jobs.file(over);

        let view = jobs.view(&session());
        assert_eq!(view["kind"], "table");
        assert_eq!(view["rows"].as_array().expect("rows").len(), 1);
        assert_eq!(view["rows"][0][0], Value::String(running.id.clone()));
        assert_eq!(view["rows"][0][1], "cargo test --workspace");
        assert_eq!(view["headers"][2], "since");

        assert_eq!(
            jobs.view(&SessionId::from_raw("ses_other")),
            Value::Null,
            "a session with none running publishes nothing"
        );
    }

    #[test]
    fn an_empty_set_is_a_null_payload_which_is_what_removes_the_card() {
        let jobs = Jobs::new();
        assert_eq!(jobs.view(&session()), Value::Null);
        let over = job("true");
        over.finished(State::Killed);
        jobs.file(over);
        assert_eq!(jobs.view(&session()), Value::Null);
    }

    #[test]
    fn a_started_job_is_shown_as_its_own_kind_with_a_fold_for_everyone_else() {
        let job = with_id("job_ab12cd34");
        let View::Custom { kind, data, fold } = shown(&job) else {
            panic!("a custom kind");
        };
        assert_eq!(kind, SHOWN);
        assert_eq!(data["id"], "job_ab12cd34");
        assert_eq!(data["command"], "sleep 1");
        assert_eq!(fold, "Started in the background as job_ab12cd34");
        let wire = serde_json::to_value(shown(&job)).expect("a view");
        assert_eq!(wire["kind"], "custom");
        assert_eq!(wire["customKind"], "job");
    }

    #[test]
    fn a_long_command_is_clipped_to_its_first_line() {
        assert_eq!(head("echo hi"), "echo hi");
        assert_eq!(head("echo one\necho two"), "echo one");
        let long = "x".repeat(200);
        assert_eq!(head(&long).chars().count(), HEAD);
        assert!(head(&long).ends_with('…'));
    }

    #[test]
    fn a_state_says_how_the_job_ended() {
        assert_eq!(State::Running.said(), "running");
        assert!(!State::Running.ended());
        assert_eq!(State::Exited { code: 2 }.said(), "exited with code 2");
        assert!(State::Exited { code: 0 }.ended());
        assert_eq!(State::Killed.said(), "killed");
    }

    #[tokio::test]
    async fn waiting_on_a_job_returns_the_moment_it_ends() {
        let job = job("sleep 1");
        let waiting = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            waiting.finished(State::Exited { code: 3 });
        });
        assert_eq!(job.wait().await, State::Exited { code: 3 });
        assert_eq!(job.wait().await, State::Exited { code: 3 }, "and again");
    }

    #[test]
    fn a_kill_is_asked_for_and_the_job_s_own_task_does_it() {
        let job = job("tail -f x");
        let token = job.killed();
        assert!(!token.is_cancelled());
        job.ask_to_die();
        assert!(token.is_cancelled());
        assert_eq!(job.state(), State::Running, "asking is not the killing");
    }

    #[test]
    fn an_age_reads_the_way_a_person_says_it() {
        let job = job("sleep 1");
        assert_eq!(job.age(), "0s");
        assert!(job.named().starts_with(&job.id));
        assert!(job.named().contains("`sleep 1`"));
    }
}
