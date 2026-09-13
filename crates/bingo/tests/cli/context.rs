//! The context budget (M4): the warning, the overflow retry, the summary.

use super::*;

#[test]
fn the_context_warning_is_said_once_near_the_line() {
    let home = tempfile::tempdir().unwrap();
    let settings =
        script(r#"{"models": {"fake/fake-1": {"contextWindow": 30000, "maxOutput": 1000}}}"#);
    // effective 29 000, warn at 6 100 tokens: a 30 000-char prompt is ~7 500.
    let long = "lorem ipsum ".repeat(2_500);
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Glob","input":{"pattern":"*.md"}}}]},
            {"steps":[{"text":"Done."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .envs(home_env(home.path()))
        .args(["--print", "--settings"])
        .arg(settings.path())
        .args(["--cwd"])
        .arg(home.path())
        .arg(&long));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert_eq!(
        err.matches("CONTEXT_WARNING").count(),
        1,
        "once per turn, across two rounds: {err}"
    );
}

#[test]
fn an_overflow_is_retried_once_and_the_window_is_learned_on_disk() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"error":{"kind":"contextOverflow","message":"prompt is too long: 9000 tokens > 8000 maximum"}}]},
            {"steps":[{"text":"Recovered."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .envs(home_env(home.path()))
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(home.path())
        .arg("go"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames = frames_of(&out);
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::TurnRetrying { .. })),
        "the overflow is announced as a retry"
    );
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
    let learned = std::fs::read_to_string(home.path().join(".bingo/data/learned-windows.json"))
        .expect("the lesson is on disk");
    assert!(learned.contains("\"fake/fake-1\": 8000"), "{learned}");
}

#[test]
fn an_overflow_after_many_rounds_is_summarised_and_the_turn_goes_on() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("a.md"), "# a\n").unwrap();
    // Fourteen tool rounds, an overflow, the summary the strategy asks for,
    // then the answer of the retry.
    let glob = r#"{"steps":[{"toolCall":{"name":"Glob","input":{"pattern":"*.md"}}}]}"#;
    let rounds = std::iter::repeat_n(glob, 14).collect::<Vec<_>>().join(",");
    let script = script(&format!(
        r#"{{"responses":[{rounds},
            {{"steps":[{{"error":{{"kind":"contextOverflow","message":"too long: 9000 tokens > 8000 maximum"}}}}]}},
            {{"steps":[{{"text":"Recovered."}}]}}
        ],"side":[{{"steps":[{{"text":"Summary: globbing markdown files."}}]}}]}}"#
    ));
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .envs(home_env(home.path()))
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(home.path())
        .arg("list the docs"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames = frames_of(&out);
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::Compacted { .. })),
        "the cut is on the wire"
    );
    let summary = frames.iter().find_map(|f| match &f.event {
        Event::ItemCompleted { item } => match &item.body {
            bingo_sdk::ItemBody::Compaction {
                summary,
                replaced,
                before,
                after,
                ..
            } => Some((summary.clone(), *replaced, *before, *after)),
            _ => None,
        },
        _ => None,
    });
    let (summary, replaced, before, after) = summary.expect("a Compaction item");
    assert!(summary.contains("globbing markdown files"), "{summary}");
    assert!(frames.iter().any(|frame| matches!(&frame.event,
        Event::ItemCompleted { item }
            if matches!(&item.body, bingo_sdk::ItemBody::Assistant { text } if text == "Recovered.")
    )), "the conversation must consume its own retry response");
    assert!(
        replaced >= 2 && after < before,
        "{replaced} replaced, {before} -> {after}"
    );
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
}

#[test]
fn a_memory_the_model_writes_by_hand_is_what_memory_lists() {
    let home = tempfile::tempdir().unwrap();
    // Nothing is remembered, and `/memory` says where a memory would go.
    let listing = script(r#"{"responses":[]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", listing.path())
        .envs(home_env(home.path()))
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("/memory"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let said = stdout(&out);
    let (_, project) = said.trim().rsplit_once(" and ").expect("two directories");
    let project = std::path::PathBuf::from(project);
    assert!(
        project.starts_with(home.path().join(".bingo/data/memory")),
        "{said}"
    );
    assert!(!project.exists(), "saying where is not creating it");

    // The model writes one with the tools it has: the file, then its line.
    let file = project.join("the-build-is-cargo-test.md");
    let fact = "---\nname: the-build-is-cargo-test\ndescription: how this project is tested\n\
                type: project\n---\n\nRun `cargo test` from the root.\n";
    let index = project.join("MEMORY.md");
    let line =
        "- [The build is cargo test](the-build-is-cargo-test.md) — how this project is tested\n";
    let writing = script(
        &serde_json::json!({"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":file,"content":fact}}}]},
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":index,"content":line}}}]},
            {"steps":[{"text":"Remembered."}]}
        ]})
        .to_string(),
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", writing.path())
        .envs(home_env(home.path()))
        .args(["--print", "--permission-mode", "bypassPermissions", "--cwd"])
        .arg(home.path())
        .arg("remember how the tests run"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Remembered.\n");
    assert_eq!(std::fs::read_to_string(&index).unwrap(), line);

    // `/memory` shows the person what the model wrote — and nothing else
    // wrote anything: no turn is asked what it learned.
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", listing.path())
        .envs(home_env(home.path()))
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("/memory"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let table = stdout(&out);
    assert!(table.contains("the-build-is-cargo-test"), "{table}");
    assert!(table.contains("how this project is tested"), "{table}");
    assert!(table.contains("project"), "{table}");
    let written: Vec<String> = std::fs::read_dir(&project)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(written.len(), 2, "{written:?}");
}
