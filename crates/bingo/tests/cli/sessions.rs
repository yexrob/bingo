//! Sessions on disk (M3): resume, the lock, the turn budget.

use super::*;

#[test]
fn max_turns_stops_a_tool_loop_with_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
    let read = r#"{"steps":[{"toolCall":{"name":"Read","input":{"file_path":"a.txt"}}}]}"#;
    let script = script(&format!(r#"{{"responses":[{read},{read},{read}]}}"#));
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--max-turns", "2", "--cwd"])
        .arg(dir.path())
        .arg("loop"));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("TURN_BUDGET_EXHAUSTED"), "{err}");
}

#[test]
fn resuming_an_unknown_session_is_not_found_before_any_turn() {
    let out = run(bingo()
        .env("HOME", tempfile::tempdir().unwrap().path())
        .args(["--print", "--resume", "ses_nope", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.starts_with("[error] code=SESSION_NOT_FOUND"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn continue_and_resume_reopen_the_journal_the_last_run_wrote() {
    let home = tempfile::tempdir().unwrap();
    let first_script = script(r#"{"responses":[{"steps":[{"text":"First."}]}]}"#);
    let first = scripted_run(home.path(), &first_script, &[], "one");
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let first_frames = frames_of(&first);
    let session = first_frames[0].session.clone();
    let last_seq = first_frames.last().unwrap().seq;
    assert!(
        home.path()
            .join(".bingo/data/sessions")
            .join(session.to_string())
            .join("journal.jsonl")
            .is_file(),
        "the journal is on disk"
    );

    let again = script(r#"{"responses":[{"steps":[{"text":"Second."}]}]}"#);
    let second = scripted_run(home.path(), &again, &["--continue"], "two");
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    let second_frames = frames_of(&second);
    assert_eq!(second_frames[0].session, session, "the same session");
    assert!(
        second_frames.iter().all(|f| f.seq > last_seq),
        "the seq goes on from the first run"
    );

    let third = scripted_run(
        home.path(),
        &script(r#"{"responses":[{"steps":[{"text":"Third."}]}]}"#),
        &["--resume", &session.to_string()],
        "three",
    );
    assert_eq!(third.status.code(), Some(0), "stderr: {}", stderr(&third));
    assert_eq!(frames_of(&third)[0].session, session);

    let fresh = scripted_run(home.path(), &first_script, &[], "four");
    assert_ne!(
        frames_of(&fresh)[0].session,
        session,
        "without a flag a run is a new session"
    );
}

/// What a `/resume` row is read from (M32): the summary beside the journal
/// names the session by its first ask and counts what was said in it, and
/// keeps both true across a `--continue`.
#[test]
fn a_resumed_session_carries_its_first_ask_and_what_was_said_in_it() {
    let home = tempfile::tempdir().unwrap();
    let say = |text: &str| {
        script(&format!(
            r#"{{"responses":[{{"steps":[{{"text":"{text}"}}]}}]}}"#
        ))
    };
    let first = say("First.");
    let out = scripted_run(
        home.path(),
        &first,
        &[],
        "Fix the parser. It crashes on unicode.",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let dir = home
        .path()
        .join(".bingo/data/sessions")
        .join(frames_of(&out)[0].session.to_string());

    let summary = || -> serde_json::Value {
        let bytes = std::fs::read(dir.join("summary.json")).expect("a summary beside the journal");
        serde_json::from_slice(&bytes).expect("json")
    };
    assert_eq!(
        summary()["title"],
        "Fix the parser",
        "the first sentence of the first ask, and not the paragraph"
    );
    assert_eq!(summary()["messages"], 2, "the ask and the answer");

    let again = say("Second.");
    let second = scripted_run(home.path(), &again, &["--continue"], "and the lexer");
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(summary()["title"], "Fix the parser", "the mint fires once");
    assert_eq!(summary()["messages"], 4);

    // A summary as it was written before the count existed: it says nothing
    // rather than a `0`, and the next write earns it the journal's own number
    // rather than starting again from one.
    let mut old = summary();
    old.as_object_mut().expect("an object").remove("messages");
    std::fs::write(dir.join("summary.json"), old.to_string()).expect("write the old summary");
    assert_eq!(summary().get("messages"), None);

    let third = say("Third.");
    let out = scripted_run(home.path(), &third, &["--continue"], "and the printer");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(summary()["messages"], 6, "counted from the whole journal");

    // The journal pays nothing for that freshness: one head per segment, and
    // one for the name the first ask minted.
    let heads = std::fs::read_to_string(dir.join("journal.jsonl"))
        .expect("the journal")
        .lines()
        .filter(|line| line.contains(r#""type":"sessionUpdated""#))
        .count();
    assert_eq!(heads, 4, "three segment heads and the mint, no more");
}

#[test]
fn a_session_another_process_holds_cannot_be_continued() {
    let home = tempfile::tempdir().unwrap();
    let slow = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Bash","input":{"command":"sleep 4"}}}]},
            {"steps":[{"text":"Slept."}]}
        ]}"#,
    );
    let mut holder = bingo()
        .env("BINGO_FAKE_SCRIPT", slow.path())
        .env("HOME", home.path())
        .args(["--print", "--dangerously-skip-permissions", "--cwd"])
        .arg(home.path())
        .arg("wait")
        .spawn()
        .unwrap();
    let sessions = home.path().join(".bingo/data/sessions");
    let locked = |sessions: &std::path::Path| {
        std::fs::read_dir(sessions)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| entry.path().join(".lock").is_file())
    };
    let started = std::time::Instant::now();
    while !locked(&sessions) {
        assert!(
            started.elapsed().as_secs() < 10,
            "the holder never took its session"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    let second = scripted_run(
        home.path(),
        &script(r#"{"responses":[{"steps":[{"text":"nope"}]}]}"#),
        &["--continue"],
        "two",
    );
    assert_eq!(second.status.code(), Some(1));
    let err = stderr(&second);
    assert!(err.starts_with("[error] code=SESSION_LOCKED"), "{err}");
    let status = holder.wait().unwrap();
    assert!(status.success());
}
