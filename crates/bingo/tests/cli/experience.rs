//! The experience library through the real binary (ADR-0014): the card that
//! proposes a file, the round trip of commit → query → outcome → revise on
//! disk, the recall line in the transcript, and `/experience`.

use std::path::{Path, PathBuf};

use bingo_sdk::{Event, ItemBody, Preview};
use serde_json::{Value, json};

use super::*;

/// A playbook made of everything that breaks a naive serializer.
fn adversarial() -> Value {
    json!({
        "trigger": ["cargo test, then clippy", "构建失败"],
        "summary": "when the build breaks\nrun the fixer",
        "steps": ["--- reset the tree", "he said \"run \\ it\"", "清理 target/ 目录"],
        "verify": "the suite is green: 0 failed",
        "notes": "Body.\n\n---\n\n中文.",
    })
}

fn call(name: &str, input: Value) -> Value {
    json!({"steps": [{"toolCall": {"name": name, "input": input}}]})
}

fn scripted(responses: Vec<Value>) -> String {
    json!({ "responses": responses }).to_string()
}

/// This project's store: the one directory under `experience`.
fn store(home: &Path) -> Option<PathBuf> {
    std::fs::read_dir(home.join(".bingo/experience"))
        .ok()?
        .flatten()
        .map(|project| project.path())
        .next()
}

/// Every entry file in the store, as `(id, text)`, in name order.
fn entries(home: &Path) -> Vec<(String, String)> {
    let Some(dir) = store(home) else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = std::fs::read_dir(dir)
        .expect("a store")
        .flatten()
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "md"))
        .map(|file| {
            (
                file.path()
                    .file_stem()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read_to_string(file.path()).expect("the entry"),
            )
        })
        .collect();
    found.sort();
    found
}

/// The last completed call of `name`: what the model read, and whether it was
/// handed an error.
fn tool_call(out: &Output, name: &str) -> (String, bool) {
    let output = frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                ItemBody::ToolCall {
                    name: called,
                    output,
                    ..
                } if called == name => output,
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("the {name} call completed"));
    let text = output
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect();
    (text, output.is_error)
}

/// The text of the last completed call of `name`, which must have worked.
fn tool_result(out: &Output, name: &str) -> String {
    let (text, is_error) = tool_call(out, name);
    assert!(!is_error, "{name}: {text}");
    text
}

/// What the contributors appended to the transcript, by label.
fn contributed(out: &Output, id: &str) -> Vec<String> {
    frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                ItemBody::User { parts, origin }
                    if origin.surface == format!("contributor:{id}") =>
                {
                    Some(
                        parts
                            .iter()
                            .filter_map(bingo_sdk::ContentPart::as_text)
                            .collect(),
                    )
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The gate asks before an experience is written, and what it shows is the
/// file itself: the card is the propose step, so there is no propose tool.
#[test]
fn a_commit_is_proposed_as_the_file_it_would_write() {
    let home = tempfile::tempdir().unwrap();
    let script = script(&scripted(vec![
        call("ExperienceCommit", adversarial()),
        json!({"steps": [{"text": "Could not."}]}),
    ]));
    let out = scripted_run(home.path(), &script, &[], "write down what we learned");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let preview = frames_of(&out)
        .into_iter()
        .find_map(|f| match f.event {
            Event::InteractionOpened { interaction } => match interaction.kind {
                bingo_sdk::InteractionKind::Permission { tool, preview, .. }
                    if tool == "ExperienceCommit" =>
                {
                    Some(preview)
                }
                _ => None,
            },
            _ => None,
        })
        .expect("the gate asked before writing an experience");
    let Some(Preview::Diff { unified }) = preview else {
        panic!("the card shows the file, not {preview:?}");
    };
    assert!(unified.contains("<new>.md"), "{unified}");
    assert!(
        unified.contains("+summary: \"when the build breaks\\nrun the fixer\""),
        "{unified}"
    );
    assert!(unified.contains("+  - \"构建失败\""), "{unified}");
    assert!(
        entries(home.path()).is_empty(),
        "the denied call wrote a file"
    );
}

/// One entry, written, searched, given an outcome and revised under the same
/// id — across two runs, so what the second reads is what the first left on
/// disk.
#[test]
fn commit_query_outcome_and_revise_round_trip_on_disk() {
    let home = tempfile::tempdir().unwrap();
    let allowed = ["--allowed-tools", "ExperienceCommit,ExperienceOutcome"];

    let first = script(&scripted(vec![
        call("ExperienceCommit", adversarial()),
        json!({"steps": [{"text": "Written."}]}),
    ]));
    let out = scripted_run(home.path(), &first, &allowed, "write down what we learned");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        contributed(&out, "experience:recall").is_empty(),
        "an empty store recalled something"
    );

    let written = entries(home.path());
    assert_eq!(written.len(), 1, "{written:?}");
    let (id, text) = &written[0];
    assert_eq!(id.chars().count(), 8, "the id is a short slug: {id}");
    // Every scalar is escaped, so the file a person opens is still YAML.
    assert!(
        text.contains("summary: \"when the build breaks\\nrun the fixer\""),
        "{text}"
    );
    assert!(text.contains("  - \"--- reset the tree\""), "{text}");
    assert!(
        text.contains("  - \"he said \\\"run \\\\ it\\\"\""),
        "{text}"
    );
    assert!(text.contains("  - \"cargo test, then clippy\""), "{text}");
    assert!(text.contains("  - \"清理 target/ 目录\""), "{text}");
    assert!(text.ends_with("---\nBody.\n\n---\n\n中文.\n"), "{text}");
    assert!(
        !text.contains(id),
        "the id is the file name and nothing else"
    );
    let created = text
        .lines()
        .find(|line| line.starts_with("created:"))
        .expect("a created line")
        .to_string();

    let mut revised = adversarial();
    revised["id"] = json!(&id[..4]);
    revised["summary"] = json!("when the build breaks, run the fixer");
    let second = script(&scripted(vec![
        call("ExperienceQuery", json!({"query": "the build breaks"})),
        call(
            "ExperienceOutcome",
            json!({"id": &id[..4], "outcome": "helpful", "evidence": "cargo build went green"}),
        ),
        call("ExperienceCommit", revised),
        json!({"steps": [{"text": "Revised."}]}),
    ]));
    let out = scripted_run(home.path(), &second, &allowed, "the build breaks again");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    // The search hands back what was written, byte for byte.
    let found = tool_result(&out, "ExperienceQuery");
    assert!(
        found.contains("when the build breaks\n     run the fixer"),
        "{found}"
    );
    assert!(found.contains("1. --- reset the tree"), "{found}");
    assert!(found.contains("he said \"run \\ it\""), "{found}");
    assert!(found.contains("清理 target/ 目录"), "{found}");
    assert!(found.contains("中文."), "{found}");

    // The recall of the second run's question is in the transcript.
    let recalled = contributed(&out, "experience:recall");
    assert_eq!(recalled.len(), 1, "{recalled:?}");
    assert!(recalled[0].contains(id), "{}", recalled[0]);

    let after = entries(home.path());
    assert_eq!(after.len(), 1, "the revision forked the entry: {after:?}");
    let (again, text) = &after[0];
    assert_eq!(again, id, "the id changed under a revision");
    assert!(
        text.contains("summary: \"when the build breaks, run the fixer\""),
        "{text}"
    );
    assert!(
        text.contains(&created),
        "the day it was written changed:\n{text}"
    );
    assert!(text.contains("outcome: \"helpful\""), "{text}");
    assert!(
        text.contains("evidence: \"cargo build went green\""),
        "{text}"
    );
    assert!(
        !text.contains("helpful:"),
        "a count was written down:\n{text}"
    );
}

/// Evidence is what keeps an outcome from being a self-confirmation, so a
/// call without it never reaches the library.
#[test]
fn an_outcome_without_evidence_is_an_input_error() {
    let home = tempfile::tempdir().unwrap();
    let first = script(&scripted(vec![
        call("ExperienceCommit", adversarial()),
        json!({"steps": [{"text": "Written."}]}),
    ]));
    let out = scripted_run(
        home.path(),
        &first,
        &["--allowed-tools", "ExperienceCommit"],
        "write it down",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let (id, before) = entries(home.path()).remove(0);

    let second = script(&scripted(vec![
        call("ExperienceOutcome", json!({"id": id, "outcome": "helpful"})),
        json!({"steps": [{"text": "Refused."}]}),
    ]));
    let out = scripted_run(
        home.path(),
        &second,
        &["--allowed-tools", "ExperienceOutcome"],
        "say it worked",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let (said, is_error) = tool_call(&out, "ExperienceOutcome");
    assert!(is_error, "a record with no evidence was taken: {said}");
    assert!(said.contains("evidence"), "{said}");
    assert_eq!(entries(home.path())[0].1, before, "the entry was touched");
}

/// `/experience` is instant and answers a table; `--print` shows its fold.
#[test]
fn the_command_folds_the_library_into_a_table() {
    let home = tempfile::tempdir().unwrap();
    let empty = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script(&scripted(Vec::new())).path())
            .env("HOME", home.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/experience"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(empty.status.code(), Some(0), "stderr: {}", stderr(&empty));
    assert!(
        stdout(&empty).contains("no experience for this project yet"),
        "{}",
        stdout(&empty)
    );

    let written = script(&scripted(vec![
        call("ExperienceCommit", adversarial()),
        json!({"steps": [{"text": "Written."}]}),
    ]));
    let out = scripted_run(
        home.path(),
        &written,
        &["--allowed-tools", "ExperienceCommit"],
        "write it down",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let (id, _) = entries(home.path()).remove(0);

    let listed = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script(&scripted(Vec::new())).path())
            .env("HOME", home.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/experience"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let table = stdout(&listed);
    assert!(table.contains("id · status · summary"), "{table}");
    assert!(table.contains(&id), "{table}");
    assert!(table.contains("when the build breaks …"), "{table}");
    assert!(table.contains("+0 / -0"), "{table}");
}
