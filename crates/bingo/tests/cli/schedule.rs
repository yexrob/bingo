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
