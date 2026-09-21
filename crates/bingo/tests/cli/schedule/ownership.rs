//! A runner owns an OS lease, not the existence of a file or a diagnostic PID.

use super::*;

fn listing(home: &Path) -> String {
    let script = script(r#"{"responses":[]}"#);
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .envs(home_env(home))
            .args(["--print", "--cwd"])
            .arg(home)
            .arg("/schedule"),
        PATIENCE,
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    stdout(&out)
}

fn overdue(home: &Path, id: &str) {
    write_entry(
        home,
        id,
        "every 1h",
        "identify the runner",
        Timestamp::now() - SignedDuration::from_hours(2),
        None,
    );
}

fn fired_by(home: &Path, id: &str, response: &str) -> String {
    // An external file write has no in-process notification. Allow the normal
    // sixty-second rescan without changing production timing for this test.
    let started = Instant::now();
    loop {
        if let Some(journal) =
            transcript(home, &format!("schedule/{id}")).filter(|j| j.contains(response))
        {
            return journal;
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the expected runner never delivered {id}: {response}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_second_process_stands_by_while_only_the_owner_fires() {
    let home = tempfile::tempdir().unwrap();
    overdue(home.path(), "dddd4444");
    let owner_script = script(r#"{"responses":[{"steps":[{"text":"owner delivered"}]}]}"#);
    let standby_script = script(r#"{"responses":[{"steps":[{"text":"standby delivered"}]}]}"#);
    let mut owner = Running::start(home.path(), &owner_script);
    let mut standby = Running::start(home.path(), &standby_script);
    let answer = listing(home.path());
    assert!(
        answer.contains("schedules: standby — another runner holds"),
        "another process is running the schedules, not a dormant store: {answer}"
    );
    assert!(
        !answer.contains("remove it"),
        "never unlink a modern lease: {answer}"
    );
    assert!(!answer.contains("dormant"), "{answer}");
    assert!(
        runner_is_locked(home.path()).is_some(),
        "the lease is OS-held"
    );

    let journal = fired_by(home.path(), "dddd4444", "owner delivered");
    assert!(!journal.contains("standby delivered"), "{journal}");
    assert!(entry_of(home.path(), "dddd4444")["lastFired"].is_string());
    // Both initialized processes remain alive while one owns the lease.
    assert!(owner.child.try_wait().unwrap().is_none(), "owner is alive");
    assert!(
        standby.child.try_wait().unwrap().is_none(),
        "standby is alive"
    );
    standby.stop();
    assert!(
        runner_is_locked(home.path()).is_some(),
        "standby cannot release the owner's lease"
    );
    owner.stop();
    let journal = transcript(home.path(), "schedule/dddd4444").unwrap();
    assert!(!journal.contains("standby delivered"), "{journal}");
}

fn takeover(stop_owner: fn(Running)) {
    let home = tempfile::tempdir().unwrap();
    let owner_script = script(r#"{"responses":[]}"#);
    let standby_script =
        script(r#"{"responses":[{"steps":[{"text":"already-started standby delivered"}]}]}"#);
    let owner = Running::start(home.path(), &owner_script);
    let standby = Running::start(home.path(), &standby_script);

    // RPC initialize has answered for both: takeover must not require a restart.
    stop_owner(owner);
    overdue(home.path(), "aaaa7777");
    fired_by(home.path(), "aaaa7777", "already-started standby delivered");
    assert!(
        runner_is_locked(home.path()).is_some(),
        "the successor owns the lease"
    );
    standby.stop();
}

#[test]
fn an_already_started_standby_takes_over_after_normal_shutdown() {
    takeover(Running::stop);
}

#[test]
fn an_already_started_standby_takes_over_after_child_kill() {
    takeover(Running::kill);
}

#[test]
fn an_unlocked_modern_file_survives_shutdown_and_does_not_block_restart() {
    let home = tempfile::tempdir().unwrap();
    let idle = script(r#"{"responses":[]}"#);
    let owner = Running::start(home.path(), &idle);
    let path = schedules(home.path()).join("runner.lock");
    owner.stop();
    let marker = std::fs::read(&path).expect("shutdown keeps the lease file");
    assert_eq!(marker, b"bingo-schedule-runner-v1\n");
    assert!(
        runner_is_locked(home.path()).is_none(),
        "shutdown releases the OS lock"
    );

    let active = script(r#"{"responses":[{"steps":[{"text":"restart delivered"}]}]}"#);
    overdue(home.path(), "bbbb7777");
    let successor = Running::start(home.path(), &active);
    fired_by(home.path(), "bbbb7777", "restart delivered");
    successor.stop();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        marker,
        "takeover does not rewrite the marker"
    );
}

fn doctor_fix(home: &Path, script: &tempfile::NamedTempFile) -> String {
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .envs(home_env(home))
            .arg("--cwd")
            .arg(home)
            .args(["gateway", "doctor", "--fix"]),
        PATIENCE,
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    stdout(&out)
}

#[test]
fn doctor_fix_preserves_held_and_released_modern_lease_files() {
    let home = tempfile::tempdir().unwrap();
    let idle = script(r#"{"responses":[]}"#);
    let owner = Running::start(home.path(), &idle);
    let path = schedules(home.path()).join("runner.lock");

    let held_report = doctor_fix(home.path(), &idle);
    assert!(held_report.contains("runner.lock"), "{held_report}");
    assert!(path.exists(), "doctor must not unlink the owner's lease");
    assert!(
        runner_is_locked(home.path()).is_some(),
        "doctor leaves the owner's exclusion intact"
    );
    owner.stop();

    let marker = std::fs::read(&path).expect("the modern lease file survives shutdown");
    let free_report = doctor_fix(home.path(), &idle);
    assert!(free_report.contains("runner.lock"), "{free_report}");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        marker,
        "doctor keeps an unlocked lease too"
    );
    assert!(
        runner_is_locked(home.path()).is_none(),
        "doctor does not retain a lease"
    );

    let successor = Running::start(home.path(), &idle);
    assert!(
        runner_is_locked(home.path()).is_some(),
        "the preserved lease can exclude again"
    );
    successor.stop();
}

fn legacy_is_preserved(contents: &str) {
    let home = tempfile::tempdir().unwrap();
    let dir = schedules(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("runner.lock");
    std::fs::write(&path, contents).unwrap();
    let answer = listing(home.path());
    assert!(
        answer.contains("legacy"),
        "legacy ownership is explicit: {answer}"
    );
    assert!(!answer.contains("held by this process"), "{answer}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
}

#[test]
fn a_live_legacy_pid_is_not_replaced_or_treated_as_a_modern_lease() {
    let other_home = tempfile::tempdir().unwrap();
    let idle = script(r#"{"responses":[]}"#);
    let live = Running::start(other_home.path(), &idle);
    legacy_is_preserved(&live.child.id().to_string());
    live.stop();
}

#[test]
fn an_empty_legacy_file_is_preserved_during_its_possible_pid_write_window() {
    legacy_is_preserved("");
}
