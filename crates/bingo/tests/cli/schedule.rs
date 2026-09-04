//! Schedules through the binary (ADR-0019, plan M16 brick 12): the file a
//! creation writes, a real turn on `schedule/<id>`, one fire for an entry
//! that is overdue however long it was overdue, a second process that runs
//! with them dormant, and `/schedule` under `--print`.
//!
//! Every one of these runs against a temporary HOME: the store lives under
//! `$HOME/.bingo/data/schedules`, and a test that leaked would write into
//! the developer's own.

use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};

use super::*;

/// Long enough for a process to boot, take the claim and fire; a schedule
/// that has not fired by then is a failure, not a slow machine.
const PATIENCE: Duration = Duration::from_secs(30);

fn schedules(home: &Path) -> PathBuf {
    home.join(".bingo/data/schedules")
}

fn sessions(home: &Path) -> PathBuf {
    home.join(".bingo/data/sessions")
}

/// One entry, written the way a person editing the store by hand would.
fn write_entry(
    home: &Path,
    id: &str,
    spec: &str,
    text: &str,
    created: Timestamp,
    mode: Option<&str>,
) {
    let dir = schedules(home);
    std::fs::create_dir_all(&dir).unwrap();
    let mut entry = serde_json::json!({
        "spec": spec,
        "text": text,
        "cwd": home,
        "enabled": true,
        "created": created.to_string(),
    });
    if let Some(mode) = mode {
        entry["permissionMode"] = serde_json::json!(mode);
    }
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&entry).unwrap(),
    )
    .unwrap();
}

fn entry_of(home: &Path, id: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(schedules(home).join(format!("{id}.json")))
        .unwrap_or_else(|e| panic!("{id}.json: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// The one entry a run wrote, whatever id it was given.
fn only_entry(home: &Path) -> serde_json::Value {
    let mut files: Vec<PathBuf> = std::fs::read_dir(schedules(home))
        .unwrap()
        .flatten()
        .map(|f| f.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    assert_eq!(files.len(), 1, "one schedule, not {files:?}");
    let path = files.remove(0);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The journal of the session a schedule fires on, once there is one.
fn transcript(home: &Path, key: &str) -> Option<String> {
    for session in std::fs::read_dir(sessions(home)).ok()?.flatten() {
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

/// A bingo that stays up while the test watches the disk: `serve --stdio`
/// with its stdin held open, since a `--print` run leaves as soon as its own
/// turn is done and a schedule needs a process to be alive.
struct Running {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
}

impl Running {
    /// A process that would allow anything: what a fire is allowed to do is
    /// not what most of these tests are asking about.
    fn start(home: &Path, script: &tempfile::NamedTempFile) -> Self {
        Self::spawn(home, script, &["--dangerously-skip-permissions"])
    }

    /// A process with the gate as it ships and nobody at the keyboard, so
    /// only the entry's own permission mode can let a scheduled turn write.
    fn unattended(home: &Path, script: &tempfile::NamedTempFile) -> Self {
        Self::spawn(home, script, &[])
    }

    fn spawn(home: &Path, script: &tempfile::NamedTempFile, extra: &[&str]) -> Self {
        let mut child = bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home)
            .args(["serve", "--stdio"])
            .args(extra)
            .arg("--cwd")
            .arg(home)
            .stdin(Stdio::piped())
            .spawn()
            .expect("the binary runs");
        let stdin = child.stdin.take();
        Self { child, stdin }
    }

    /// Close stdin and wait: the surface ends, the host shuts down, and the
    /// plugins give their claims back. A killed process would leave the
    /// store looking held.
    fn stop(mut self) {
        drop(self.stdin.take());
        let started = Instant::now();
        while started.elapsed() < PATIENCE {
            if self.child.try_wait().expect("wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("bingo serve did not exit when its stdin closed");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_create_tool_writes_one_entry_a_person_can_read() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"ScheduleCreate","input":{
                "spec":"daily at 09:00",
                "text":"summarise what changed overnight"
            }}}]},
            {"steps":[{"text":"Set."}]}
        ]}"#,
    );
    let out = scripted_run(
        home.path(),
        &script,
        &["--dangerously-skip-permissions"],
        "remind me every morning",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let entry = only_entry(home.path());
    assert_eq!(entry["spec"], "daily at 09:00");
    assert_eq!(entry["text"], "summarise what changed overnight");
    assert_eq!(entry["cwd"], home.path().to_string_lossy().as_ref());
    assert_eq!(entry["enabled"], true);
    assert!(entry.get("lastFired").is_none(), "it has not fired yet");
}

#[test]
fn the_schedule_command_folds_to_stdout_under_print() {
    let home = tempfile::tempdir().unwrap();
    write_entry(
        home.path(),
        "aaaa1111",
        "every 2h",
        "check the nightly build",
        Timestamp::now(),
        None,
    );
    let script = script(r#"{"responses":[]}"#);
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/schedule"),
        PATIENCE,
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let answer = stdout(&out);
    assert!(answer.contains("id · spec · next fire"), "{answer}");
    assert!(
        answer.contains("aaaa1111 · every 2h · "),
        "the row is the entry: {answer}"
    );
    assert!(answer.contains("check the nightly build"), "{answer}");
    assert!(
        answer.contains("schedules: held by this process"),
        "the holder line is not a maybe: {answer}"
    );
}

#[test]
fn a_short_every_fires_a_real_turn_on_the_schedule_s_own_session() {
    let home = tempfile::tempdir().unwrap();
    write_entry(
        home.path(),
        "bbbb2222",
        "every 2s",
        "say the word",
        Timestamp::now(),
        None,
    );
    let script = script(r#"{"responses":[{"steps":[{"text":"the word, on time"}]}]}"#);
    let running = Running::start(home.path(), &script);

    let journal = until("the schedule opened its session", || {
        transcript(home.path(), "schedule/bbbb2222").filter(|j| j.contains("the word, on time"))
    });
    assert!(
        journal.contains("say the word"),
        "the prompt is in the transcript: {journal}"
    );
    assert!(
        journal.contains("\"surface\":\"schedule\""),
        "the turn says where it came from: {journal}"
    );

    let fired = until("the fire was written down", || {
        let entry = entry_of(home.path(), "bbbb2222");
        entry.get("lastFired").cloned()
    });
    assert!(fired.is_string(), "{fired}");
    running.stop();
}

#[test]
fn an_overdue_schedule_fires_once_however_long_it_was_overdue() {
    let home = tempfile::tempdir().unwrap();
    write_entry(
        home.path(),
        "cccc3333",
        "every 1h",
        "the overdue one",
        Timestamp::now() - SignedDuration::from_hours(5),
        None,
    );
    // Five hours late is five missed occurrences. A second response would
    // only ever be reached by a second fire.
    let script = script(
        r#"{"responses":[
            {"steps":[{"text":"fire one"}]},
            {"steps":[{"text":"fire two"}]}
        ]}"#,
    );
    let first = Running::start(home.path(), &script);
    let journal = until("the overdue schedule fired", || {
        transcript(home.path(), "schedule/cccc3333").filter(|j| j.contains("fire one"))
    });
    assert!(
        !journal.contains("fire two"),
        "one fire, not one per missed hour: {journal}"
    );
    let after = entry_of(home.path(), "cccc3333");
    assert!(after["lastFired"].is_string(), "{after}");
    first.stop();

    // The clock moved, so the next process owes nothing until the hour is up.
    let restart = super::script(r#"{"responses":[{"steps":[{"text":"the second run"}]}]}"#);
    let second = Running::start(home.path(), &restart);
    std::thread::sleep(Duration::from_secs(2));
    let journal = transcript(home.path(), "schedule/cccc3333").expect("the session is still there");
    assert!(
        !journal.contains("the second run"),
        "a restart does not re-fire what has already fired: {journal}"
    );
    second.stop();
}

#[test]
fn a_once_at_fires_and_then_disables_itself() {
    let home = tempfile::tempdir().unwrap();
    let due = Timestamp::now() - SignedDuration::from_mins(1);
    write_entry(
        home.path(),
        "eeee5555",
        &format!("once at {due}"),
        "the only time",
        Timestamp::now() - SignedDuration::from_mins(5),
        None,
    );
    let script = script(r#"{"responses":[{"steps":[{"text":"once and no more"}]}]}"#);
    let running = Running::start(home.path(), &script);

    until("the once at fired", || {
        transcript(home.path(), "schedule/eeee5555").filter(|j| j.contains("once and no more"))
    });
    let spent = until("the once at spent itself", || {
        let entry = entry_of(home.path(), "eeee5555");
        (entry["enabled"] == false).then_some(entry)
    });
    assert!(spent["lastFired"].is_string(), "{spent}");
    running.stop();
}

/// ADR-0019 §4: the scheduled session runs under the entry's
/// `permission_mode`. Nobody is there to answer a card, so a `Write` that
/// had to ask would never land; under `acceptEdits` it lands without one.
#[test]
fn a_schedule_fires_under_the_permission_mode_its_entry_names() {
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join("written-by-a-schedule.txt");
    write_entry(
        home.path(),
        "ffff6666",
        "every 1h",
        "write the file",
        Timestamp::now() - SignedDuration::from_hours(2),
        Some("acceptEdits"),
    );
    let script = script(&format!(
        r#"{{"responses":[
            {{"steps":[{{"toolCall":{{"name":"Write","input":{{
                "file_path":{},
                "content":"a schedule wrote this\n"
            }}}}}}]}},
            {{"steps":[{{"text":"Written."}}]}}
        ]}}"#,
        serde_json::to_string(&target).unwrap()
    ));
    let running = Running::unattended(home.path(), &script);

    let written = until("the scheduled turn wrote the file", || {
        std::fs::read_to_string(&target).ok()
    });
    assert_eq!(written, "a schedule wrote this\n");
    running.stop();
}

#[test]
fn a_second_process_runs_with_the_schedules_dormant_and_says_who_has_them() {
    let home = tempfile::tempdir().unwrap();
    write_entry(
        home.path(),
        "dddd4444",
        "every 2h",
        "not this process's job",
        Timestamp::now(),
        None,
    );
    let script = script(r#"{"responses":[]}"#);
    let holder = Running::start(home.path(), &script);
    let lock = until("the first process took the store", || {
        std::fs::read_to_string(schedules(home.path()).join("runner.lock")).ok()
    });
    assert_eq!(
        lock.trim().parse::<u32>().ok(),
        Some(holder.child.id()),
        "the claim names the process that took it"
    );

    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/schedule"),
        PATIENCE,
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let answer = stdout(&out);
    assert!(
        answer.contains(&format!(
            "schedules: dormant — held by pid {}",
            holder.child.id()
        )),
        "the second process says who has them: {answer}"
    );
    assert!(
        answer.contains("remove it if no bingo is running"),
        "and what to do about it: {answer}"
    );
    assert!(
        answer.contains("dddd4444"),
        "a dormant process still reads the store: {answer}"
    );
    holder.stop();
}

// ---- the wake the model sets (ADR-0019 §8) ------------------------------

/// The one session a run left behind, by the id its directory is named for.
fn only_session(home: &Path) -> String {
    let mut ids: Vec<String> = std::fs::read_dir(sessions(home))
        .unwrap()
        .flatten()
        .map(|dir| dir.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("ses_"))
        .collect();
    assert_eq!(ids.len(), 1, "one session, not {ids:?}");
    ids.remove(0)
}

/// A wake standing on `session`, coming at `due`. The store is
/// hand-editable, which is what lets a test say "the moment came" without
/// waiting for it, and "it has not yet" without waiting either.
fn write_wake(home: &Path, id: &str, session: &str, note: &str, due: Timestamp) {
    let dir = schedules(home);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = serde_json::json!({
        "spec": format!("once at {due}"),
        "text": note,
        "cwd": home,
        "session": session,
        "enabled": true,
        "created": (Timestamp::now() - SignedDuration::from_hours(1)).to_string(),
    });
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(&entry).unwrap(),
    )
    .unwrap();
}

/// The tool is trusted and read-only, so this run allows nothing by hand: a
/// wake that had to be approved could not pace a loop.
#[test]
fn the_wake_tool_writes_one_entry_bound_to_the_session_that_set_it() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Wake","input":{
                "after":"5m",
                "note":"check whether the build went green"
            }}}]},
            {"steps":[{"text":"I will look again in five minutes."}]}
        ]}"#,
    );
    let out = scripted_run(home.path(), &script, &[], "watch the build");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let entry = only_entry(home.path());
    assert_eq!(entry["session"], only_session(home.path()));
    assert_eq!(entry["text"], "check whether the build went green");
    assert!(
        entry["spec"].as_str().unwrap().starts_with("once at "),
        "a wake is the model's own once: {entry}"
    );
    assert!(
        entry.get("permissionMode").is_none(),
        "it wakes a session already in a mode: {entry}"
    );
}

/// The other half: the runner delivers the note to that session, marked as a
/// wake, and the entry is gone once it has.
#[test]
fn a_wake_fires_on_the_session_that_set_it_and_leaves_no_entry() {
    let home = tempfile::tempdir().unwrap();
    let first = script(r#"{"responses":[{"steps":[{"text":"I will look again."}]}]}"#);
    let out = scripted_run(home.path(), &first, &[], "watch the build");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let session = only_session(home.path());
    write_wake(
        home.path(),
        "aaaa1111",
        &session,
        "look at the build again",
        Timestamp::now() - SignedDuration::from_mins(1),
    );

    let woken = script(r#"{"responses":[{"steps":[{"text":"still red, one more look"}]}]}"#);
    let running = Running::start(home.path(), &woken);
    let journal = until("the wake opened a turn on the same session", || {
        std::fs::read_to_string(sessions(home.path()).join(&session).join("journal.jsonl"))
            .ok()
            .filter(|j| j.contains("still red, one more look"))
    });
    assert!(
        journal.contains("look at the build again"),
        "the note is the turn's own input: {journal}"
    );
    assert!(
        journal.contains("\"surface\":\"wake\""),
        "and it says it is a wake, not a person: {journal}"
    );
    until("the spent wake was forgotten", || {
        (!schedules(home.path()).join("aaaa1111.json").exists()).then_some(())
    });
    running.stop();
}

/// What a person types to see the wake and to end it, through the binary: the
/// same session, resumed, so `/wake` is asked of the session the wake stands
/// on rather than a fresh one.
#[test]
fn the_wake_command_shows_what_stands_and_off_ends_it() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"I will look again."}]}]}"#);
    let out = scripted_run(home.path(), &script, &[], "watch the build");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let session = only_session(home.path());
    write_wake(
        home.path(),
        "bbbb2222",
        &session,
        "look at the build again",
        // Still to come, so no runner in these processes fires it first.
        Timestamp::now() + SignedDuration::from_mins(5),
    );

    let asked = |args: &str| {
        let out = run_within(
            bingo()
                .env("BINGO_FAKE_SCRIPT", script.path())
                .env("HOME", home.path())
                .args(["--print", "--resume", &session, "--cwd"])
                .arg(home.path())
                .arg(args),
            PATIENCE,
        );
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        stdout(&out)
    };

    let shown = asked("/wake");
    assert!(shown.contains("look at the build again"), "{shown}");
    let ended = asked("/wake off");
    assert!(ended.contains("the wake set for"), "{ended}");
    assert!(
        !schedules(home.path()).join("bbbb2222.json").exists(),
        "the file is gone: {ended}"
    );
    assert!(
        asked("/wake").contains("no wake is standing"),
        "and it says so"
    );
}

/// The bound a person sets (ADR-0019 §8): the tool is still offered, so a
/// model that reaches for it is told whose decision it was, and nothing is
/// written.
#[test]
fn wakes_a_person_turned_off_are_refused_and_the_run_still_ends_well() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".bingo");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("settings.json"),
        r#"{ "schedule": { "wakes": false } }"#,
    )
    .unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Wake","input":{
                "after":"5m",
                "note":"check the build"
            }}}]},
            {"steps":[{"text":"I cannot wake myself here."}]}
        ]}"#,
    );
    let out = scripted_run(home.path(), &script, &[], "watch the build");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        !schedules(home.path()).exists()
            || std::fs::read_dir(schedules(home.path()))
                .unwrap()
                .flatten()
                .all(|f| f.path().extension().is_none_or(|e| e != "json")),
        "nothing was written"
    );
    let said = stdout(&out);
    assert!(
        said.contains("schedule.wakes"),
        "the model is told why: {said}"
    );
}
