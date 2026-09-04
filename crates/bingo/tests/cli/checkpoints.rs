//! Checkpoints across processes (M67, ADR-0045): two turns edit one file,
//! `/rewind` back through both of them puts it back to what it was before
//! either, and the conversation goes with it.

use super::*;

/// One `--print` run whose home is also its working directory.
fn print_in(
    home: &std::path::Path,
    script: &tempfile::NamedTempFile,
    extra: &[&str],
    prompt: &str,
) -> Output {
    run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home)
        .args(["--print", "--cwd"])
        .arg(home)
        .args(extra)
        .arg(prompt))
}

/// A turn that writes `note.md`, then says so.
fn writes(content: &str) -> tempfile::NamedTempFile {
    script(&format!(
        r#"{{"responses":[
            {{"steps":[{{"toolCall":{{"name":"Write","input":{{"file_path":"note.md","content":"{content}"}}}}}}]}},
            {{"steps":[{{"text":"Wrote it."}}]}}
        ]}}"#
    ))
}

/// A script no turn of this test reaches: a command opens no turn, but a
/// session still resolves a provider.
fn idle() -> tempfile::NamedTempFile {
    script(r#"{"responses":[{"steps":[{"text":"unused"}]}]}"#)
}

/// The turn ids of the `/rewind` table, newest first.
fn listed(out: &Output) -> Vec<String> {
    stdout(out)
        .lines()
        .skip(1)
        .filter_map(|row| row.split(" · ").next().map(str::to_string))
        .collect()
}

#[test]
fn rewinding_two_turns_puts_the_file_back_to_before_the_first_of_them() {
    let home = tempfile::tempdir().unwrap();
    let note = home.path().join("note.md");
    std::fs::write(&note, "original").unwrap();
    let allow = ["--allowed-tools", "Write"];

    let first = print_in(home.path(), &writes("one"), &allow, "write one");
    assert!(first.status.success(), "{}", stderr(&first));
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "one");

    let carry = ["--continue", "--allowed-tools", "Write"];
    let second = print_in(home.path(), &writes("two"), &carry, "write two");
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "two");

    let table = print_in(home.path(), &idle(), &["--continue"], "/rewind");
    assert!(table.status.success(), "{}", stderr(&table));
    let turns = listed(&table);
    assert_eq!(
        turns.len(),
        2,
        "two turns to go back to: {}",
        stdout(&table)
    );
    let oldest = turns.last().unwrap().clone();

    let back = print_in(
        home.path(),
        &idle(),
        &["--continue"],
        &format!("/rewind {oldest}"),
    );
    assert!(back.status.success(), "{}", stderr(&back));
    let said = stdout(&back);
    assert!(said.starts_with("rewound to write one,"), "{said}");
    assert!(said.contains("put back note.md"), "{said}");
    assert_eq!(
        std::fs::read_to_string(&note).unwrap(),
        "original",
        "the oldest snapshot across both turns is what the file goes back to"
    );

    let after = print_in(home.path(), &idle(), &["--continue"], "/rewind");
    assert_eq!(
        stdout(&after).trim_end(),
        "turn · asked · files",
        "and both turns are out of the conversation"
    );
}

/// A file the turns created is not put back but removed, and the session's
/// checkpoints go when the session does.
#[test]
fn a_file_a_turn_created_is_removed_and_the_snapshots_outlive_nothing() {
    let home = tempfile::tempdir().unwrap();
    let note = home.path().join("note.md");
    let allow = ["--allowed-tools", "Write"];

    let made = print_in(home.path(), &writes("new"), &allow, "make one");
    assert!(made.status.success(), "{}", stderr(&made));
    assert!(note.is_file(), "the turn created it");

    let checkpoints = home.path().join(".bingo/data/checkpoints");
    let session = std::fs::read_dir(&checkpoints)
        .expect("a checkpoint directory")
        .next()
        .expect("one session")
        .unwrap()
        .file_name();

    let table = print_in(home.path(), &idle(), &["--continue"], "/rewind");
    let oldest = listed(&table).last().unwrap().clone();
    let back = print_in(
        home.path(),
        &idle(),
        &["--continue"],
        &format!("/rewind {oldest}"),
    );
    assert!(back.status.success(), "{}", stderr(&back));
    assert!(
        stdout(&back).contains("removed note.md"),
        "{}",
        stdout(&back)
    );
    assert!(!note.exists(), "a file the turn created is gone again");

    // The session goes, and the next run collects what it kept.
    std::fs::remove_dir_all(home.path().join(".bingo/data/sessions")).unwrap();
    std::fs::write(home.path().join("other.md"), "x").unwrap();
    let fresh = print_in(home.path(), &idle(), &[], "hello");
    assert!(fresh.status.success(), "{}", stderr(&fresh));
    assert!(
        !checkpoints.join(&session).exists(),
        "the checkpoints of a session that is gone go with it"
    );
}
