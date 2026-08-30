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
        .env("HOME", home.path())
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
        .env("HOME", home.path())
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
            {{"steps":[{{"text":"Summary: globbing markdown files."}}]}},
            {{"steps":[{{"text":"Recovered."}}]}}
        ]}}"#
    ));
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home.path())
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
fn a_working_turn_leaves_facts_in_the_project_memory() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("notes.txt"), "alpha\n").unwrap();
    // A tool round and the answer; what the extractor is told at turn end is
    // a side answer, never one of the conversation's.
    let first = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Read","input":{"file_path":"notes.txt"}}}]},
            {"steps":[{"text":"One line."}]}
        ],"side":[
            {"steps":[{"text":"notes.txt holds the alpha list\nthe project has no build step"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", first.path())
        .env("HOME", home.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("what is in notes.txt?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "One line.\n");
    let memory_dir = home.path().join(".bingo/data/memory");
    let files: Vec<_> = std::fs::read_dir(&memory_dir)
        .expect("a memory directory")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let memory = std::fs::read_to_string(&files[0]).unwrap();
    assert!(
        memory.contains("notes.txt holds the alpha list"),
        "{memory}"
    );
    assert!(memory.contains("the project has no build step"), "{memory}");

    // The next run reads it back into the prompt and learns nothing new
    // from a turn without a tool call.
    let again = script(r#"{"responses":[{"steps":[{"text":"Still one line."}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", again.path())
        .env("HOME", home.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("and now?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), memory);
}
