//! `gateway doctor`: read everything, change nothing, and say what a person
//! must do (ADR-0020 §5).
//!
//! Every check is a row with a verdict and a remedy, because a diagnosis
//! nobody can act on is not a diagnosis. Two rules hold throughout:
//!
//! - **No secret is ever printed.** A credential row names the variable and
//!   the file, and says whether something is there. What it is stays where it
//!   is.
//! - **`--fix` removes exactly the files this run just reported dead**, and
//!   nothing else. A lock whose process is alive is never touched, whatever it
//!   is locking.

use std::path::{Path, PathBuf};

use bingo_core::settings;
use bingo_sdk::{Env, ErrorCode, KernelError};
use serde_json::{Map, Value};

use super::paths::Paths;
use super::pidfile;
use super::probe::Probe;
use super::service::Mode;
use super::state::State;
use super::unit::Supervisor;

/// How far down the data dir a lock may be. Plugins keep theirs one directory
/// down (`schedules/`, `channels/`); the extra rung is slack, not licence.
const DEPTH: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Warn,
    Bad,
}

impl Verdict {
    fn tag(self) -> &'static str {
        match self {
            Verdict::Ok => "[ok]  ",
            Verdict::Warn => "[warn]",
            Verdict::Bad => "[bad] ",
        }
    }
}

/// One thing that was looked at.
#[derive(Clone, Debug)]
pub struct Row {
    pub check: String,
    pub verdict: Verdict,
    pub say: String,
    /// The file `--fix` would remove, when this row is one it can fix. Only a
    /// `Bad` row ever carries one.
    pub fixable: Option<PathBuf>,
}

impl Row {
    fn new(check: impl Into<String>, verdict: Verdict, say: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            verdict,
            say: say.into(),
            fixable: None,
        }
    }

    fn fixing(mut self, path: PathBuf) -> Self {
        self.fixable = Some(path);
        self
    }

    fn line(&self) -> String {
        format!("{} {} — {}", self.verdict.tag(), self.check, self.say)
    }
}

/// What the doctor needs to know to look: where the files are and what
/// settings this invocation would have loaded.
pub struct Patient<'a> {
    pub paths: &'a Paths,
    pub env: &'a Env,
    pub cwd: &'a Path,
    pub settings: Option<&'a Path>,
}

pub fn doctor(patient: &Patient<'_>, probe: &dyn Probe, fix: bool) -> Result<String, KernelError> {
    let rows = examine(patient, probe);
    let mut report: Vec<String> = rows.iter().map(Row::line).collect();
    if fix {
        report.push(String::new());
        report.push(fixed(&rows));
    } else if rows.iter().any(|row| row.fixable.is_some()) {
        report.push(String::new());
        report.push(
            "`bingo gateway doctor --fix` removes the dead files above, and only those.".into(),
        );
    }
    Ok(report.join("\n"))
}

/// What `install`, `start` and `restart` check before they act (user-directed
/// 2026-09-01): the settings parse, a channel is configured, and every
/// configured channel can sign. The rows are the doctor's own — one
/// implementation, two readers — so a verb refuses with exactly the lines a
/// doctor would print, and a gateway is never handed to a supervisor that
/// would only crash-loop it on a configuration the verb could have read.
pub fn preflight(patient: &Patient<'_>) -> Result<(), KernelError> {
    let refuse = |say: String| Err(KernelError::new(ErrorCode::InvalidInput, say));
    let (settings_row, layers) = settings_check(patient);
    if settings_row.verdict == Verdict::Bad {
        return refuse(settings_row.line());
    }
    let merged = merged_channels(&layers);
    if bingo_channels::secret::configured(&merged).is_empty() {
        return refuse(
            "no channel is configured: `bingo channels add feishu` asks for the \
             app id and the secret and writes both where the next run reads \
             them (or name one under `channels` in the settings by hand). A \
             gateway with no channel would only crash-loop under its supervisor."
                .into(),
        );
    }
    let bad: Vec<String> = credential_checks(patient, &layers)
        .into_iter()
        .filter(|row| row.verdict == Verdict::Bad)
        .map(|row| row.line())
        .collect();
    match bad.is_empty() {
        true => Ok(()),
        false => refuse(bad.join("\n")),
    }
}

/// Every check, in the order a person reads them: what is configured, what is
/// running, what is holding something, and what it would sign with.
fn examine(patient: &Patient<'_>, probe: &dyn Probe) -> Vec<Row> {
    let (settings_row, layers) = settings_check(patient);
    let mut rows = vec![settings_row];
    rows.push(mode_check(patient));
    rows.push(gateway_check(patient, probe));
    rows.extend(lock_checks(patient, probe));
    rows.push(log_check(patient));
    rows.extend(credential_checks(patient, &layers));
    rows.extend(lingering_check(patient));
    rows
}

fn settings_check(patient: &Patient<'_>) -> (Row, Vec<settings::Layer>) {
    match settings::load(patient.env, patient.cwd, patient.settings) {
        Ok(layers) => {
            let sources: Vec<&str> = layers.iter().map(|l| l.source.as_str()).collect();
            let say = match sources.is_empty() {
                true => "no settings file exists yet; every default is in force".to_string(),
                false => format!("{} parse: {}", sources.len(), sources.join(", ")),
            };
            (Row::new("settings", Verdict::Ok, say), layers)
        }
        Err(e) => (
            Row::new(
                "settings",
                Verdict::Bad,
                format!("{e}. Nothing will start until this file is valid."),
            ),
            Vec::new(),
        ),
    }
}

fn mode_check(patient: &Patient<'_>) -> Row {
    let home = &patient.env.home;
    let mode = Mode::here(home);
    let say = match mode {
        Mode::Installed(supervisor) => format!(
            "{} keeps it alive ({}). It has no exported environment, so a \
             channel secret must be in the store, not a shell.",
            supervisor.name(),
            supervisor.path(home).display()
        ),
        Mode::Hand => "started by hand; nothing restarts it after a reboot. \
                       `bingo gateway install` changes that."
            .to_string(),
    };
    Row::new("mode", Verdict::Ok, say)
}

/// The pidfile, and the binary the process it names is running.
fn gateway_check(patient: &Patient<'_>, probe: &dyn Probe) -> Row {
    let path = patient.paths.pidfile();
    match State::read(patient.paths, probe) {
        Err(e) => Row::new("gateway.pid", Verdict::Bad, e.message),
        Ok(State::Stopped) => Row::new(
            "gateway.pid",
            Verdict::Ok,
            format!("no gateway is running here (no {})", path.display()),
        ),
        Ok(State::Stale(record)) => Row::new(
            "gateway.pid",
            Verdict::Bad,
            format!(
                "pid {} is gone but its record is still here ({}). It did not \
                 stop cleanly, and the next `start` will refuse until this goes.",
                record.pid,
                path.display()
            ),
        )
        .fixing(path),
        Ok(State::Running(record)) => running(&record),
    }
}

/// A running gateway: the one row that also answers "why is my change not
/// live", which is nearly always that the process predates the binary.
fn running(record: &pidfile::Record) -> Row {
    if record.version == pidfile::version() {
        return Row::new(
            "gateway.pid",
            Verdict::Ok,
            format!(
                "pid {} is running bingo {}, started {}",
                record.pid, record.version, record.started
            ),
        );
    }
    Row::new(
        "gateway.pid",
        Verdict::Warn,
        format!(
            "pid {} is running bingo {}, but the binary here is {}. \
             `bingo gateway restart` picks it up.",
            record.pid,
            record.version,
            pidfile::version()
        ),
    )
}

/// Every claim under the data dir, checked against the process that took it.
///
/// A lock is found by its shape — a `*.lock` file whose whole content is the
/// pid that took it — and not by name. That is why this needs no plugin's
/// private constants, and why a plugin that adds a claim tomorrow is already
/// covered.
fn lock_checks(patient: &Patient<'_>, probe: &dyn Probe) -> Vec<Row> {
    let mut rows = Vec::new();
    for path in locks(patient.paths.data_dir()) {
        let name = relative(&path, patient.paths.data_dir());
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(pid) = text.trim().parse::<u32>() else {
            rows.push(Row::new(
                name,
                Verdict::Warn,
                format!(
                    "{} holds no pid; nothing can say whether it is live",
                    path.display()
                ),
            ));
            continue;
        };
        rows.push(match probe.alive(pid) {
            true => Row::new(
                name,
                Verdict::Ok,
                format!("held by pid {pid}, which is running"),
            ),
            false => Row::new(
                name,
                Verdict::Bad,
                format!(
                    "held by pid {pid}, which is gone. Whatever it was guarding \
                     is dormant until this file goes ({})",
                    path.display()
                ),
            )
            .fixing(path),
        });
    }
    rows
}

/// Every `*.lock` under `dir`, to a fixed depth, in a fixed order.
fn locks(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(dir, DEPTH, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth - 1, found);
        } else if path.extension().is_some_and(|e| e == "lock") {
            found.push(path);
        }
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// A log that cannot be written is a gateway whose failures go nowhere, which
/// is the state this whole milestone exists to end.
fn log_check(patient: &Patient<'_>) -> Row {
    let path = patient.paths.log();
    if let Err(e) = patient.paths.ensure() {
        return Row::new("gateway.log", Verdict::Bad, e);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(_) => Row::new(
            "gateway.log",
            Verdict::Ok,
            format!("writable ({})", path.display()),
        ),
        Err(e) => Row::new(
            "gateway.log",
            Verdict::Bad,
            format!(
                "{}: {e}. A gateway that cannot log fails silently.",
                path.display()
            ),
        ),
    }
}

/// One row per configured channel: which credential it needs, and which of the
/// two sources is actually going to answer.
///
/// The names come from the channels plugin, so this file never learns how a
/// channel is spelled — and no value is read into the report.
fn credential_checks(patient: &Patient<'_>, layers: &[settings::Layer]) -> Vec<Row> {
    let merged = merged_channels(layers);
    bingo_channels::secret::configured(&merged)
        .into_iter()
        .map(|wanted| credential_row(patient, wanted))
        .collect()
}

fn credential_row(patient: &Patient<'_>, wanted: bingo_channels::secret::Requirement) -> Row {
    let check = format!("channels.{}", wanted.id);
    let Some(variable) = wanted.variable else {
        return Row::new(check, Verdict::Ok, "configured; it signs with nothing");
    };
    match bingo_channels::secret::find(patient.env, wanted.id, variable) {
        Some(found) => Row::new(
            check,
            Verdict::Ok,
            format!("its secret comes from {}", found.source),
        ),
        None => Row::new(
            check,
            Verdict::Bad,
            format!(
                "no secret. Export {variable}, or run `bingo channels secret {}` \
                 to put one in the store — which is the only source a gateway \
                 started at boot can read.",
                wanted.id
            ),
        ),
    }
}

/// The `channels` key as every layer together names it. `Merge::ByName` is
/// what the kernel does with this key, so a channel named in any layer counts.
fn merged_channels(layers: &[settings::Layer]) -> Value {
    let mut channels = Map::new();
    for layer in layers {
        if let Some(Value::Object(named)) = layer.value.get(bingo_channels::SETTING) {
            for (name, value) in named {
                channels.insert(name.clone(), value.clone());
            }
        }
    }
    Value::Object(Map::from_iter([(
        bingo_channels::SETTING.to_string(),
        Value::Object(channels),
    )]))
}

/// A Linux user service stops at logout unless lingering is on. The command is
/// said, never run: enabling it is a change to the machine, not a diagnosis.
fn lingering_check(patient: &Patient<'_>) -> Option<Row> {
    if !matches!(
        Mode::here(&patient.env.home),
        Mode::Installed(Supervisor::Systemd)
    ) {
        return None;
    }
    Some(Row::new(
        "lingering",
        Verdict::Warn,
        "a systemd user unit stops at logout unless lingering is on. If this \
         machine is a server you log out of, run `loginctl enable-linger $USER`.",
    ))
}

/// Remove exactly what was reported dead.
fn fixed(rows: &[Row]) -> String {
    let dead: Vec<&Row> = rows
        .iter()
        .filter(|row| row.verdict == Verdict::Bad && row.fixable.is_some())
        .collect();
    if dead.is_empty() {
        return "--fix: nothing to remove; no file here names a process that is gone.".into();
    }
    let mut said = vec!["--fix:".to_string()];
    for row in dead {
        let Some(path) = &row.fixable else { continue };
        said.push(match std::fs::remove_file(path) {
            Ok(()) => format!("  removed {}", path.display()),
            Err(e) => format!("  {} could not be removed: {e}", path.display()),
        });
    }
    said.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::probe::tests::Fake;
    use jiff::Timestamp;

    struct Case {
        home: tempfile::TempDir,
    }

    impl Case {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().expect("a temporary home"),
            }
        }

        fn env(&self) -> Env {
            Env::rooted(self.home.path())
        }

        fn lock(&self, under: &str, name: &str, pid: u32) -> PathBuf {
            let dir = self.env().data_dir.join(under);
            std::fs::create_dir_all(&dir).expect("the directory");
            let path = dir.join(name);
            std::fs::write(&path, pid.to_string()).expect("a lock");
            path
        }

        fn pidfile(&self, pid: u32) -> PathBuf {
            let paths = Paths::new(&self.env());
            paths.ensure().expect("the directory");
            let record = pidfile::Record {
                pid,
                version: pidfile::version().into(),
                started: Timestamp::now(),
            };
            std::fs::write(paths.pidfile(), pidfile::render(&record)).expect("a record");
            paths.pidfile()
        }

        fn report(&self, probe: &dyn Probe, fix: bool) -> String {
            let env = self.env();
            let paths = Paths::new(&env);
            doctor(
                &Patient {
                    paths: &paths,
                    env: &env,
                    cwd: self.home.path(),
                    settings: None,
                },
                probe,
                fix,
            )
            .expect("a report")
        }
    }

    #[test]
    fn every_lock_under_the_data_dir_is_found_whatever_plugin_left_it() {
        let case = Case::new();
        case.lock("schedules", "runner.lock", 4242);
        case.lock("channels", "feishu-cli_a1.lock", 4242);
        let found = locks(&case.env().data_dir);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found.iter().any(|p| p.ends_with("runner.lock")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|p| p.ends_with("feishu-cli_a1.lock")),
            "{found:?}"
        );
    }

    #[test]
    fn a_live_lock_is_ok_and_a_dead_one_is_bad_and_fixable() {
        let case = Case::new();
        case.lock("schedules", "runner.lock", 4242);
        case.lock("channels", "feishu-cli_a1.lock", 9999);
        let said = case.report(&Fake::of(&[(4242, "bingo")]), false);
        assert!(
            said.contains("[ok]   schedules/runner.lock — held by pid 4242"),
            "{said}"
        );
        assert!(
            said.contains("[bad]  channels/feishu-cli_a1.lock — held by pid 9999, which is gone"),
            "{said}"
        );
        assert!(
            said.contains("--fix` removes the dead files above"),
            "{said}"
        );
    }

    #[test]
    fn fix_removes_the_dead_locks_it_just_reported_and_only_those() {
        let case = Case::new();
        let live = case.lock("schedules", "runner.lock", 4242);
        let dead = case.lock("channels", "feishu-cli_a1.lock", 9999);
        let stale = case.pidfile(9999);
        let said = case.report(&Fake::of(&[(4242, "bingo")]), true);
        assert!(said.contains("--fix:"), "{said}");
        assert!(!dead.exists(), "the dead lock went: {said}");
        assert!(!stale.exists(), "the stale pidfile went: {said}");
        assert!(live.exists(), "the live one was never touched: {said}");
    }

    #[test]
    fn fix_with_nothing_wrong_removes_nothing_and_says_so() {
        let case = Case::new();
        case.lock("schedules", "runner.lock", 4242);
        let said = case.report(&Fake::of(&[(4242, "bingo")]), true);
        assert!(said.contains("nothing to remove"), "{said}");
    }

    #[test]
    fn a_configured_channel_with_no_secret_is_named_and_no_secret_is_printed() {
        let case = Case::new();
        std::fs::create_dir_all(case.env().config_dir).expect("the directory");
        std::fs::write(
            case.env().config_dir.join("settings.json"),
            r#"{ "channels": { "feishu": { "appId": "cli_public" } } }"#,
        )
        .expect("settings");
        let said = case.report(&Fake::empty(), false);
        assert!(said.contains("[ok]   settings —"), "{said}");
        assert!(said.contains("settings.json"), "it names them: {said}");
        assert!(
            said.contains("[bad]  channels.feishu — no secret"),
            "{said}"
        );
        assert!(said.contains("BINGO_FEISHU_APP_SECRET"), "{said}");
        assert!(said.contains("bingo channels secret feishu"), "{said}");
    }

    #[test]
    fn a_secret_in_the_store_is_reported_by_its_location_and_never_its_value() {
        let case = Case::new();
        let env = case.env();
        std::fs::create_dir_all(&env.config_dir).expect("the directory");
        std::fs::write(
            env.config_dir.join("settings.json"),
            r#"{ "channels": { "feishu": {} } }"#,
        )
        .expect("settings");
        bingo_channels::secret::store(&env, "feishu", "s-do-not-print-me".into())
            .expect("it is written");
        let said = case.report(&Fake::empty(), false);
        assert!(
            said.contains("[ok]   channels.feishu — its secret comes from"),
            "{said}"
        );
        assert!(said.contains("auth.json"), "{said}");
        assert!(
            !said.contains("s-do-not-print-me"),
            "a doctor never prints a secret: {said}"
        );
    }

    #[test]
    fn settings_that_will_not_parse_are_the_first_thing_reported() {
        let case = Case::new();
        std::fs::create_dir_all(case.env().config_dir).expect("the directory");
        std::fs::write(case.env().config_dir.join("settings.json"), "{ not json")
            .expect("a broken file");
        let said = case.report(&Fake::empty(), false);
        assert!(said.starts_with("[bad]  settings —"), "{said}");
        assert!(said.contains("Nothing will start"), "{said}");
    }

    fn patient_of<'a>(case: &'a Case, env: &'a Env, paths: &'a Paths) -> Patient<'a> {
        Patient {
            paths,
            env,
            cwd: case.home.path(),
            settings: None,
        }
    }

    #[test]
    fn preflight_refuses_a_gateway_with_no_channel_and_says_how_to_add_one() {
        let case = Case::new();
        let env = case.env();
        let paths = Paths::new(&env);
        let refused = preflight(&patient_of(&case, &env, &paths))
            .expect_err("no channel is configured")
            .message;
        assert!(refused.contains("no channel is configured"), "{refused}");
        assert!(refused.contains("bingo channels add"), "{refused}");
        assert!(refused.contains("crash-loop"), "{refused}");
    }

    #[test]
    fn preflight_refuses_a_channel_that_cannot_sign_with_the_doctor_s_own_line() {
        let case = Case::new();
        std::fs::create_dir_all(case.env().config_dir).expect("the directory");
        std::fs::write(
            case.env().config_dir.join("settings.json"),
            r#"{ "channels": { "feishu": { "appId": "cli_public" } } }"#,
        )
        .expect("settings");
        let env = case.env();
        let paths = Paths::new(&env);
        let refused = preflight(&patient_of(&case, &env, &paths))
            .expect_err("no secret anywhere")
            .message;
        assert!(
            refused.contains("[bad]  channels.feishu — no secret"),
            "{refused}"
        );
        assert!(refused.contains("BINGO_FEISHU_APP_SECRET"), "{refused}");
    }

    #[test]
    fn preflight_passes_a_channel_that_signs_with_nothing() {
        let case = Case::new();
        std::fs::create_dir_all(case.env().config_dir).expect("the directory");
        std::fs::write(
            case.env().config_dir.join("settings.json"),
            r#"{ "channels": { "loopback": {} } }"#,
        )
        .expect("settings");
        let env = case.env();
        let paths = Paths::new(&env);
        preflight(&patient_of(&case, &env, &paths)).expect("a loopback needs no secret");
    }

    #[test]
    fn a_running_gateway_older_than_this_binary_is_a_warning_not_a_failure() {
        let case = Case::new();
        let paths = Paths::new(&case.env());
        paths.ensure().expect("the directory");
        let record = pidfile::Record {
            pid: 4242,
            version: "0.0.1-ancient".into(),
            started: Timestamp::now(),
        };
        std::fs::write(paths.pidfile(), pidfile::render(&record)).expect("a record");
        let said = case.report(&Fake::of(&[(4242, "bingo")]), false);
        assert!(said.contains("[warn] gateway.pid"), "{said}");
        assert!(said.contains("gateway restart"), "{said}");
        assert!(
            paths.pidfile().exists(),
            "a warning is not something --fix removes"
        );
    }

    #[test]
    fn the_log_row_says_where_it_is_and_that_it_can_be_written() {
        let case = Case::new();
        let said = case.report(&Fake::empty(), false);
        assert!(said.contains("[ok]   gateway.log — writable"), "{said}");
        assert!(said.contains("[ok]   mode —"), "{said}");
    }
}
