//! The gateway through the binary (ADR-0020, plan M17 bricks 5 and 6): a real
//! detached process started, asked about, and stopped; the locks it gives back
//! when it goes; the log it writes; the service file it installs; and the
//! secret it can be given without a shell.
//!
//! Every one of these runs against a temporary HOME — the pidfile, the locks,
//! the log, `auth.json` and the launchd/systemd file all live under it, and a
//! test that leaked would write into the developer's own.
//!
//! Nothing here touches a real supervisor: `install` and the verbs that
//! delegate to one are run with a fake `launchctl`/`systemctl` first on PATH,
//! recording the argv it was called with.
//!
//! A started gateway is detached by design, so it does not die with the test
//! that started it. [`Gateway`] kills whatever it left behind on the way out,
//! whether the test passed, failed or panicked.

use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

use super::*;

/// Long enough for a process to boot, take its claims and write a pidfile.
const PATIENCE: Duration = Duration::from_secs(30);

/// A temporary home with a gateway that can be started in it.
struct Gateway {
    home: tempfile::TempDir,
    settings: PathBuf,
    script: PathBuf,
}

impl Gateway {
    /// A home configured with a loopback channel that has no peer: it parks
    /// until it is cancelled, which is a resident gateway with no socket to
    /// get in the way of what is being tested.
    fn new() -> Self {
        Self::with(r#"{ "channels": { "loopback": {} } }"#)
    }

    fn with(settings: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let settings_path = home.path().join("settings.json");
        std::fs::write(&settings_path, settings).unwrap();
        let script = home.path().join("script.json");
        std::fs::write(&script, r#"{"responses":[]}"#).unwrap();
        Self {
            home,
            settings: settings_path,
            script,
        }
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    fn data(&self) -> PathBuf {
        self.path().join(".bingo/data")
    }

    fn pidfile(&self) -> PathBuf {
        self.data().join("gateway/gateway.pid")
    }

    fn log(&self) -> PathBuf {
        self.data().join("gateway/gateway.log")
    }

    fn runner_lock(&self) -> PathBuf {
        self.data().join("schedules/runner.lock")
    }

    /// The bingo of this home, with the fake provider and the settings that
    /// name its channel.
    fn cmd(&self) -> Command {
        let mut cmd = bingo();
        cmd.env("HOME", self.path())
            .env("BINGO_FAKE_SCRIPT", &self.script)
            .arg("--cwd")
            .arg(self.path())
            .arg("--settings")
            .arg(&self.settings);
        cmd
    }

    /// One gateway verb, run to completion.
    fn verb(&self, args: &[&str]) -> Output {
        run_within(self.cmd().arg("gateway").args(args), PATIENCE)
    }

    /// The pid in the pidfile, whatever state it is in.
    fn pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.pidfile()).ok()?;
        let record: serde_json::Value = serde_json::from_str(&text).ok()?;
        record["pid"].as_u64().map(|pid| pid as u32)
    }

    fn logged(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    /// Start, and fail with the log if it does not come up.
    fn start(&self) -> String {
        let out = self.verb(&["start"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "start failed: {}\nlog:\n{}",
            stderr(&out),
            self.logged()
        );
        stdout(&out)
    }

    /// One overdue schedule entry, written the way a person editing the store
    /// by hand would (the M16 pattern).
    fn schedule(&self, id: &str, text: &str) {
        let dir = self.data().join("schedules");
        std::fs::create_dir_all(&dir).unwrap();
        let entry = serde_json::json!({
            "spec": "every 1h",
            "text": text,
            "cwd": self.path(),
            "enabled": true,
            "created": (Timestamp::now() - SignedDuration::from_hours(2)).to_string(),
        });
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string_pretty(&entry).unwrap(),
        )
        .unwrap();
    }

    /// The journal of the session a schedule fired on, once there is one.
    fn transcript(&self, key: &str) -> Option<String> {
        for session in std::fs::read_dir(self.data().join("sessions"))
            .ok()?
            .flatten()
        {
            let summary = std::fs::read_to_string(session.path().join("summary.json")).ok();
            let named = summary
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .is_some_and(|s| s["key"] == key);
            if named {
                return std::fs::read_to_string(session.path().join("journal.jsonl")).ok();
            }
        }
        None
    }
}

impl Drop for Gateway {
    /// A detached gateway outlives the test by design, so an assertion that
    /// failed before `stop` must not leave one running on the machine.
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
    }
}

/// Poll until something is there, or fail saying what never happened.
fn until<T>(what: &str, mut look: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(found) = look() {
            return found;
        }
        assert!(started.elapsed() < PATIENCE, "{what} never happened");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn start_status_stop_round_trips_on_a_real_detached_process() {
    let gateway = Gateway::new();
    let started = gateway.start();
    assert!(started.contains("The gateway is up: pid "), "{started}");
    assert!(
        started.contains("gateway.pid"),
        "the receipt names the file it wrote: {started}"
    );
    let pid = gateway.pid().expect("a pidfile");

    let status = stdout(&gateway.verb(&["status"]));
    assert!(status.contains(&format!("running: pid {pid}")), "{status}");
    assert!(status.contains("mode: by hand"), "{status}");
    assert!(status.contains("gateway.log"), "{status}");

    let logs = stdout(&gateway.verb(&["logs"]));
    assert!(logs.contains("the gateway is up"), "{logs}");

    let stopped = stdout(&gateway.verb(&["stop"]));
    assert!(stopped.contains("has stopped"), "{stopped}");
    assert!(
        !gateway.pidfile().exists(),
        "a graceful stop gives the pidfile back"
    );

    let after = stdout(&gateway.verb(&["status"]));
    assert!(after.contains("not running: no pidfile"), "{after}");
}

#[test]
fn a_second_start_refuses_and_names_the_pid_that_already_has_it() {
    let gateway = Gateway::new();
    gateway.start();
    let pid = gateway.pid().expect("a pidfile");

    let out = gateway.verb(&["start"]);
    assert_eq!(out.status.code(), Some(1), "a second gateway is refused");
    let said = stderr(&out);
    assert!(said.contains(&format!("pid {pid}")), "{said}");
    assert!(
        said.contains("gateway stop"),
        "and what to do about it: {said}"
    );
    assert_eq!(stdout(&out), "", "a refusal writes nothing to stdout");

    gateway.verb(&["stop"]);
}

/// The verbs that bring a gateway up validate the channels first
/// (user-directed): a configuration that cannot run is refused with the
/// remedy, never handed to a supervisor to crash-loop under KeepAlive.
#[test]
fn a_start_with_no_channel_refuses_before_anything_moves() {
    let gateway = Gateway::with(r#"{}"#);
    let out = gateway.verb(&["start"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    let said = stderr(&out);
    assert!(said.contains("no channel is configured"), "{said}");
    assert!(said.contains("bingo channels add"), "{said}");
    assert!(!gateway.pidfile().exists(), "nothing was spawned");

    let install = gateway.verb(&["install"]);
    assert_eq!(install.status.code(), Some(1), "install is preflighted too");
    assert!(
        !service_file(gateway.path()).exists(),
        "no service file was written for a configuration that cannot run"
    );
}

#[test]
fn a_stale_pidfile_is_reported_dead_and_doctor_fix_clears_it() {
    let gateway = Gateway::new();
    std::fs::create_dir_all(gateway.data().join("gateway")).unwrap();
    // A gateway that was killed: a record whose process is long gone.
    let corpse = serde_json::json!({
        "pid": 999_999,
        "version": "0.1.0",
        "started": Timestamp::now().to_string(),
    });
    std::fs::write(gateway.pidfile(), corpse.to_string()).unwrap();

    let status = stdout(&gateway.verb(&["status"]));
    assert!(status.contains("which is gone"), "{status}");

    let doctor = stdout(&gateway.verb(&["doctor"]));
    assert!(doctor.contains("[bad]  gateway.pid"), "{doctor}");
    assert!(doctor.contains("pid 999999 is gone"), "{doctor}");

    let fixed = stdout(&gateway.verb(&["doctor", "--fix"]));
    assert!(fixed.contains("--fix:"), "{fixed}");
    assert!(fixed.contains("removed"), "{fixed}");
    assert!(!gateway.pidfile().exists(), "the corpse's record went");

    // And a gateway starts again in a data dir that was wedged before.
    gateway.start();
    gateway.verb(&["stop"]);
}

/// ADR-0020 §4: TERM stops the surfaces, runs `Plugin::stop`, and the claims
/// go back. The proof is on disk — both files gone, not merely a process gone.
#[test]
fn stopping_gives_back_the_schedule_runner_claim_and_the_pidfile() {
    let gateway = Gateway::new();
    gateway.schedule("aaaa1111", "nothing that needs a provider");
    gateway.start();

    let held = until("the gateway took the schedule runner", || {
        std::fs::read_to_string(gateway.runner_lock()).ok()
    });
    assert_eq!(
        held.trim().parse::<u32>().ok(),
        gateway.pid(),
        "the claim names the resident process"
    );

    let stopped = stdout(&gateway.verb(&["stop"]));
    assert!(
        stopped.contains("gave back its pidfile and its locks"),
        "{stopped}"
    );
    assert!(
        !gateway.runner_lock().exists(),
        "the schedule runner claim was given back, not orphaned"
    );
    assert!(!gateway.pidfile().exists());
}

/// The point of the whole milestone: a schedule fires with nothing attached to
/// a terminal (M16 could only do it while a `serve --stdio` was held open).
#[test]
fn a_schedule_fires_with_no_terminal_attached() {
    let gateway = Gateway::with(
        r#"{ "channels": { "loopback": {} }, "permissions": { "defaultMode": "bypassPermissions" } }"#,
    );
    std::fs::write(
        &gateway.script,
        r#"{"responses":[{"steps":[{"text":"fired with nobody watching"}]}]}"#,
    )
    .unwrap();
    gateway.schedule("bbbb2222", "say the word");
    gateway.start();

    let journal = until("the overdue schedule fired under the gateway", || {
        gateway
            .transcript("schedule/bbbb2222")
            .filter(|j| j.contains("fired with nobody watching"))
    });
    assert!(
        journal.contains("\"surface\":\"schedule\""),
        "the turn says where it came from: {journal}"
    );
    gateway.verb(&["stop"]);
}

/// ADR-0020 §6: the gateway is the first process in this tree with a tracing
/// sink, so a line written with `tracing` reaches a file instead of nowhere.
#[test]
fn tracing_lines_land_in_the_gateway_log_at_info_and_at_warn() {
    let gateway = Gateway::new();
    std::fs::create_dir_all(gateway.data().join("gateway")).unwrap();
    // A record left by a gateway that was killed: starting over it is what
    // writes a `warn!`, and a supervisor's respawn must not wedge on it.
    let corpse = serde_json::json!({
        "pid": 999_999,
        "version": "0.0.1-ancient",
        "started": Timestamp::now().to_string(),
    });
    std::fs::write(gateway.pidfile(), corpse.to_string()).unwrap();
    gateway.start();

    let log = until("the gateway wrote its log", || {
        let log = gateway.logged();
        log.contains("the gateway is up").then_some(log)
    });
    assert!(log.contains(" INFO "), "the level is on every line: {log}");
    assert!(
        log.contains(" WARN ") && log.contains("did not stop cleanly"),
        "a warn from the run that replaced the corpse is in it: {log}"
    );
    assert!(
        log.contains("bingo::gateway::run"),
        "and the target says which module said it: {log}"
    );
    gateway.verb(&["stop"]);
}

#[test]
fn doctor_names_a_missing_credential_and_never_prints_one() {
    let gateway = Gateway::with(r#"{ "channels": { "feishu": { "appId": "cli_public" } } }"#);
    let doctor = stdout(&gateway.verb(&["doctor"]));
    assert!(
        doctor.contains("[bad]  channels.feishu — no secret"),
        "{doctor}"
    );
    assert!(
        doctor.contains("BINGO_FEISHU_APP_SECRET"),
        "it names the variable: {doctor}"
    );
    assert!(
        doctor.contains("bingo channels secret feishu"),
        "and the other way to give it one: {doctor}"
    );
    assert!(doctor.contains("[ok]   settings —"), "{doctor}");
    assert!(doctor.contains("[ok]   gateway.log — writable"), "{doctor}");
}

/// ADR-0020 §8: a gateway started at boot inherits no shell, so a secret has a
/// disk home. The environment still wins wherever it is set.
#[test]
fn a_pasted_channel_secret_lands_in_auth_json_and_doctor_names_its_source() {
    let gateway = Gateway::with(r#"{ "channels": { "feishu": { "appId": "cli_public" } } }"#);
    let mut cmd = gateway.cmd();
    let out = typed(
        cmd.args(["channels", "secret", "feishu"]),
        &["s-pasted-not-printed"],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("auth.json"), "{said}");
    assert!(said.contains("`channels.feishu`"), "{said}");
    assert!(
        !said.contains("s-pasted-not-printed"),
        "the receipt never echoes the secret: {said}"
    );

    let auth = gateway.data().join("auth.json");
    let written = std::fs::read_to_string(&auth).unwrap();
    assert!(written.contains("channels.feishu"), "{written}");
    assert!(written.contains("s-pasted-not-printed"), "{written}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a credential is nobody else's to read");
    }

    let doctor = stdout(&gateway.verb(&["doctor"]));
    assert!(
        doctor.contains("[ok]   channels.feishu — its secret comes from"),
        "{doctor}"
    );
    assert!(doctor.contains("auth.json"), "{doctor}");
    assert!(
        !doctor.contains("s-pasted-not-printed"),
        "a doctor never prints a secret: {doctor}"
    );

    // The environment wins wherever it is exported, unchanged from before.
    let exported = run_within(
        gateway
            .cmd()
            .env("BINGO_FEISHU_APP_SECRET", "s-from-the-shell")
            .args(["gateway", "doctor"]),
        PATIENCE,
    );
    let said = stdout(&exported);
    assert!(
        said.contains("the environment (BINGO_FEISHU_APP_SECRET)"),
        "{said}"
    );
    assert!(!said.contains("s-from-the-shell"), "{said}");
}

/// `channels add`: the app id and the secret in one sitting (user-directed),
/// each written where it belongs, and a settings neighbour untouched.
#[test]
fn channels_add_asks_for_both_and_writes_each_where_it_belongs() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".bingo");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("settings.json"), r#"{ "provider": "openai" }"#).unwrap();

    let mut cmd = bingo();
    let out = typed(
        cmd.env("HOME", home.path())
            .args(["channels", "add", "feishu"]),
        &["cli_myapp", "s-added-not-printed"],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("settings.json"), "{said}");
    assert!(said.contains("auth.json"), "{said}");
    assert!(said.contains("gateway restart"), "{said}");
    assert!(!said.contains("s-added-not-printed"), "no echo: {said}");

    let settings = std::fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(settings.contains("\"appId\": \"cli_myapp\""), "{settings}");
    assert!(
        settings.contains("\"provider\": \"openai\""),
        "a neighbour survived the round trip: {settings}"
    );
    let auth = std::fs::read_to_string(home.path().join(".bingo/data/auth.json")).unwrap();
    assert!(auth.contains("s-added-not-printed"), "{auth}");
    assert!(auth.contains("channels.feishu"), "{auth}");
}

// ---- the supervisor, faked ------------------------------------------------

/// A `launchctl` and a `systemctl` that record their argv and succeed. First
/// on PATH, so no test ever reaches the real one.
struct Shim {
    dir: tempfile::TempDir,
}

impl Shim {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for name in ["launchctl", "systemctl"] {
            let path = dir.path().join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"{name} $@\" >> \"$(dirname \"$0\")/argv\"\nexit 0\n"),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        Self { dir }
    }

    /// PATH with the fakes in front of everything else.
    fn path(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("{}:{inherited}", self.dir.path().display())
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("argv")).unwrap_or_default()
    }
}

/// The service file this platform's supervisor would read.
fn service_file(home: &Path) -> PathBuf {
    match std::env::consts::OS {
        "macos" => home.join("Library/LaunchAgents/com.bingo.gateway.plist"),
        _ => home.join(".config/systemd/user/bingo-gateway.service"),
    }
}

#[test]
fn install_writes_a_secret_free_service_and_the_verbs_then_delegate() {
    let gateway = Gateway::new();
    let shim = Shim::new();
    let installed = run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "install"]),
        PATIENCE,
    );
    assert_eq!(
        installed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&installed)
    );

    let file = service_file(gateway.path());
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(
        text.contains(env!("CARGO_BIN_EXE_bingo")),
        "it names the binary that installed it: {text}"
    );
    assert!(text.contains("gateway"), "{text}");
    assert!(
        text.contains(&gateway.log().display().to_string()),
        "and where its output goes: {text}"
    );
    for secret in [
        "BINGO_FEISHU_APP_SECRET",
        "ANTHROPIC_API_KEY",
        "EnvironmentVariables",
        "Environment=",
    ] {
        assert!(
            !text.contains(secret),
            "{secret} reached {}: {text}",
            file.display()
        );
    }

    // The supervisor was told to take it, in its own words.
    let argv = shim.argv();
    match std::env::consts::OS {
        "macos" => assert!(argv.contains("launchctl bootstrap gui/"), "{argv}"),
        _ => assert!(
            argv.contains("systemctl --user daemon-reload")
                && argv.contains("systemctl --user enable --now bingo-gateway.service"),
            "{argv}"
        ),
    }

    // While installed, `stop` is the supervisor's job, not a signal of ours.
    let status = stdout(&run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "status"]),
        PATIENCE,
    ));
    assert!(status.contains("mode: installed"), "{status}");
    run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "stop"]),
        PATIENCE,
    );
    let argv = shim.argv();
    match std::env::consts::OS {
        "macos" => assert!(argv.contains("launchctl bootout gui/"), "{argv}"),
        _ => assert!(
            argv.contains("systemctl --user stop bingo-gateway.service"),
            "{argv}"
        ),
    }

    // And uninstall gives it back and takes the file away with it.
    let gone = run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "uninstall"]),
        PATIENCE,
    );
    assert_eq!(gone.status.code(), Some(0), "stderr: {}", stderr(&gone));
    assert!(!file.exists(), "the service file went with the service");
    let status = stdout(&run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "status"]),
        PATIENCE,
    ));
    assert!(status.contains("mode: by hand"), "{status}");
}

/// The mode switch is the file, and it is never left on when the supervisor
/// refused: otherwise `start` would delegate to a service nothing is running.
#[test]
fn a_supervisor_that_refuses_leaves_no_service_file_behind() {
    let gateway = Gateway::new();
    let shim = Shim::new();
    for name in ["launchctl", "systemctl"] {
        std::fs::write(
            shim.dir.path().join(name),
            "#!/bin/sh\necho 'no' >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                shim.dir.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
    }
    let out = run_within(
        gateway
            .cmd()
            .env("PATH", shim.path())
            .args(["gateway", "install"]),
        PATIENCE,
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "an install that failed is a failure"
    );
    assert!(
        stderr(&out).contains("was removed again"),
        "{}",
        stderr(&out)
    );
    assert!(
        !service_file(gateway.path()).exists(),
        "a file the supervisor never took would make the mode a lie"
    );
}
