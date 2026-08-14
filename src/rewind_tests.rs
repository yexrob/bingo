//! Rewind store and truncation tests (D91). Every test owns a pid-tagged
//! directory of its own, so two sessions never share a store.

use super::*;
use crate::api::types::{ContentBlock, Message, Role};
use crate::transcript::Transcript;

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bingo-rewind-{tag}-{}", std::process::id()));
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_checkpoint(&path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A session of its own, under a home no other test writes to.
fn transcript_at(dir: &Path, name: &str) -> Transcript {
    let home = dir.join(name);
    let _ = std::fs::create_dir_all(&home);
    crate::transcript::create(&home, dir).unwrap()
}

fn tool_use(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: "Edit".to_string(),
            input: serde_json::json!({}),
        }],
    }
}

fn tool_result(id: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.to_string(),
            content: serde_json::json!("ok"),
            is_error: false,
        }],
    }
}

/// A turn: marker, the user message, an assistant tool_use and its result.
fn record_turn(transcript: &Transcript, text: &str, id: &str) {
    transcript.append_turn(1_700_000_000).unwrap();
    transcript.append(&Message::user_text(text)).unwrap();
    transcript.append(&tool_use(id)).unwrap();
    transcript.append(&tool_result(id)).unwrap();
}

#[test]
fn a_pre_image_is_taken_once_per_checkpoint_and_path() {
    let dir = temp("once");
    let file = dir.join("f.txt");
    std::fs::write(&file, "first").unwrap();
    let store = dir.join("store");

    snapshot(&store, 7, &file).unwrap();
    std::fs::write(&file, "second").unwrap();
    snapshot(&store, 7, &file).unwrap();
    std::fs::write(&file, "third").unwrap();
    snapshot(&store, 7, &file).unwrap();

    let restored = restore(&store, 7).unwrap();
    assert_eq!(restored.len(), 1, "one file, however many edits");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "first",
        "the turn's first pre-image is the one the checkpoint means"
    );
}

#[test]
fn a_file_the_tool_created_is_removed_by_name() {
    let dir = temp("created");
    let store = dir.join("store");
    let file = dir.join("new.txt");

    snapshot(&store, 3, &file).unwrap();
    std::fs::write(&file, "written by the tool").unwrap();
    let neighbour = dir.join("untouched.txt");
    std::fs::write(&neighbour, "mine").unwrap();

    let restored = restore(&store, 3).unwrap();
    assert_eq!(
        restored,
        vec![Restored {
            path: file.clone(),
            removed: true
        }]
    );
    assert!(!file.exists(), "the created file is gone");
    assert_eq!(
        std::fs::read_to_string(&neighbour).unwrap(),
        "mine",
        "a file outside the snapshots is never touched"
    );
}

#[test]
fn overlapping_edits_unwind_to_the_oldest_pre_image() {
    let dir = temp("overlap");
    let store = dir.join("store");
    let shared = dir.join("shared.txt");
    let later = dir.join("later.txt");
    std::fs::write(&shared, "v1").unwrap();

    snapshot(&store, 10, &shared).unwrap();
    std::fs::write(&shared, "v2").unwrap();
    snapshot(&store, 20, &shared).unwrap();
    snapshot(&store, 20, &later).unwrap();
    std::fs::write(&shared, "v3").unwrap();
    std::fs::write(&later, "made").unwrap();

    let restored = restore(&store, 10).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "v1",
        "replaying newest first leaves the oldest pre-image on disk"
    );
    assert!(!later.exists());
    assert_eq!(restored.len(), 2);
    assert_eq!(changed_files(&store, 10).files, 2);
    assert_eq!(changed_files(&store, 20).files, 2);
    assert_eq!(changed_files(&store, 21), Coverage::default());
}

#[test]
fn restoring_stops_at_the_chosen_checkpoint() {
    let dir = temp("from");
    let store = dir.join("store");
    let early = dir.join("early.txt");
    let late = dir.join("late.txt");
    std::fs::write(&early, "early v1").unwrap();
    std::fs::write(&late, "late v1").unwrap();

    snapshot(&store, 5, &early).unwrap();
    std::fs::write(&early, "early v2").unwrap();
    snapshot(&store, 9, &late).unwrap();
    std::fs::write(&late, "late v2").unwrap();

    restore(&store, 9).unwrap();
    assert_eq!(std::fs::read_to_string(&late).unwrap(), "late v1");
    assert_eq!(
        std::fs::read_to_string(&early).unwrap(),
        "early v2",
        "a checkpoint before the chosen one is not unwound"
    );
}

#[test]
fn the_store_evicts_oldest_first_and_stays_per_session() {
    let dir = temp("evict");
    let a = dir.join("session-a");
    let b = dir.join("session-b");
    let file = dir.join("f.txt");
    std::fs::write(&file, "x").unwrap();

    for checkpoint in 0..(MAX_CHECKPOINTS + 3) {
        snapshot(&a, checkpoint, &file).unwrap();
    }
    snapshot(&b, 0, &file).unwrap();

    let recorder = Recorder::default();
    recorder.open(a.clone(), MAX_CHECKPOINTS + 3);
    let kept = checkpoints(&a);
    assert_eq!(kept.len(), MAX_CHECKPOINTS, "the cap is enforced");
    assert_eq!(
        kept.first().map(|(id, _)| *id),
        Some(3),
        "the oldest checkpoints went first"
    );
    assert_eq!(
        checkpoints(&b).len(),
        1,
        "another session's store is untouched"
    );
}

#[test]
fn a_recorder_with_no_checkpoint_snapshots_nothing() {
    let dir = temp("closed");
    let file = dir.join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let recorder = Recorder::default();
    recorder.snapshot(&file);
    assert!(
        !dir.join("store").exists(),
        "no open checkpoint, no store, no error"
    );

    let store = dir.join("store2");
    recorder.open(store.clone(), 1);
    recorder.snapshot(&file);
    assert_eq!(changed_files(&store, 1).files, 1);
    recorder.close();
    let other = dir.join("g.txt");
    std::fs::write(&other, "y").unwrap();
    recorder.snapshot(&other);
    assert_eq!(
        changed_files(&store, 1).files,
        1,
        "closed means recording stopped"
    );
}

#[test]
fn a_file_past_the_size_cap_is_refused_rather_than_stored() {
    let dir = temp("large");
    let store = dir.join("store");
    let file = dir.join("big.bin");
    std::fs::write(&file, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
    let error = snapshot(&store, 1, &file).unwrap_err();
    assert!(matches!(error, RewindError::TooLarge(..)), "{error}");
    assert_eq!(
        changed_files(&store, 1),
        Coverage::default(),
        "nothing was recorded"
    );

    // The recorder swallows it and writes a miss instead, so the edit goes
    // ahead and the selector can still say the file will not come back.
    let recorder = Recorder::default();
    recorder.open(store.clone(), 1);
    recorder.snapshot(&file);
    assert_eq!(
        changed_files(&store, 1),
        Coverage {
            files: 0,
            missed: 1
        }
    );
    std::fs::write(&file, "changed").unwrap();
    assert!(
        restore(&store, 1).unwrap().is_empty(),
        "nothing to put back"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "changed",
        "a file with no pre-image is left exactly as it is"
    );
}

#[test]
fn only_turn_opening_messages_are_offered_newest_first() {
    let dir = temp("checkpoints");
    let store = dir.join("store");
    let transcript = transcript_at(&dir, "list");
    record_turn(&transcript, "first question", "t1");
    record_turn(&transcript, "second question", "t2");
    // A steered message rides on a tool_result, and the harness's own
    // injections are recorded with no marker at all: neither is a checkpoint.
    transcript
        .append(&Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "t3".to_string(),
                    content: serde_json::json!("ok"),
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "steered mid-turn".to_string(),
                },
            ],
        })
        .unwrap();
    transcript
        .append(&Message::user_text(
            "<task-notifications>\nping\n</task-notifications>",
        ))
        .unwrap();

    let entries = transcript.load_projection().unwrap();
    let found = checkpoints_of(&entries, &store, 50);
    assert_eq!(
        found.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        vec!["second question", "first question"],
        "turn openers only, newest first"
    );
    assert_eq!(found[0].at, 1_700_000_000, "the marker carries the clock");
    assert_eq!(found[1].line, 1, "the checkpoint is the message's line");
}

#[test]
fn the_selector_shows_at_most_the_requested_number() {
    let dir = temp("cap");
    let transcript = transcript_at(&dir, "cap");
    for index in 0..8 {
        record_turn(
            &transcript,
            &format!("question {index}"),
            &format!("t{index}"),
        );
    }
    let entries = transcript.load_projection().unwrap();
    let found = checkpoints_of(&entries, &dir.join("store"), 3);
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].label, "question 7", "the newest survives the cap");
}

#[test]
fn a_long_first_line_is_elided_and_the_rest_kept_for_the_composer() {
    let dir = temp("label");
    let transcript = transcript_at(&dir, "label");
    let long = "x".repeat(200);
    record_turn(&transcript, &format!("{long}\nsecond line"), "t1");
    let entries = transcript.load_projection().unwrap();
    let found = checkpoints_of(&entries, &dir.join("store"), 50);
    assert_eq!(found[0].label.chars().count(), LABEL_CHARS + 1, "elided");
    assert!(found[0].label.ends_with('…'));
    assert!(
        found[0].text.ends_with("second line"),
        "the composer gets the whole message, not the label"
    );
}

#[test]
fn truncation_ends_the_history_at_the_checkpoint_with_pairs_intact() {
    let dir = temp("truncate");
    let transcript = transcript_at(&dir, "truncate");
    record_turn(&transcript, "one", "t1");
    record_turn(&transcript, "two", "t2");
    record_turn(&transcript, "three", "t3");

    let entries = transcript.load_projection().unwrap();
    let found = checkpoints_of(&entries, &dir.join("store"), 50);
    let two = found.iter().find(|c| c.label == "two").unwrap();
    transcript.truncate_at_line(two.line).unwrap();

    let messages = transcript.load_messages().unwrap();
    assert_eq!(messages.len(), 4, "turn one in full, then two's opener");
    assert_eq!(
        messages.last().unwrap().content,
        vec![ContentBlock::Text {
            text: "two".to_string()
        }],
        "history ends exactly at the chosen user message"
    );
    let uses: Vec<&str> = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(uses, results, "every tool_use kept its tool_result");
}

#[test]
fn a_truncated_session_reloads_as_it_was_left() {
    let dir = temp("roundtrip");
    let transcript = transcript_at(&dir, "roundtrip");
    record_turn(&transcript, "one", "t1");
    record_turn(&transcript, "two", "t2");
    let entries = transcript.load_projection().unwrap();
    let target = checkpoints_of(&entries, &dir.join("store"), 50)
        .into_iter()
        .find(|c| c.label == "two")
        .unwrap();
    transcript.truncate_at_line(target.line).unwrap();
    let after_cut = transcript.load_messages().unwrap();

    // A new turn lands on the shortened file. If the rename had orphaned the
    // append handle, these lines would go to the unlinked inode and the file
    // would come back without them.
    record_turn(&transcript, "three", "t9");
    let reloaded = transcript.load_messages().unwrap();
    assert_eq!(&reloaded[..after_cut.len()], &after_cut[..]);
    assert_eq!(reloaded.len(), after_cut.len() + 3, "the new turn is there");
}

#[test]
fn a_cut_after_compaction_keeps_the_summary_and_the_kept_tail() {
    let dir = temp("compact-after");
    let transcript = transcript_at(&dir, "compact-after");
    record_turn(&transcript, "one", "t1");
    record_turn(&transcript, "two", "t2");
    // Two message lines stay verbatim; everything above becomes the summary.
    transcript.append_compact("earlier work", 2).unwrap();
    record_turn(&transcript, "three", "t3");
    record_turn(&transcript, "four", "t4");

    let entries = transcript.load_projection().unwrap();
    let found = checkpoints_of(&entries, &dir.join("store"), 50);
    assert_eq!(
        found.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        vec!["four", "three"],
        "a message the summary swallowed is not offered"
    );
    let target = found.iter().find(|c| c.label == "four").unwrap();
    transcript.truncate_at_line(target.line).unwrap();

    let messages = transcript.load_messages().unwrap();
    assert!(
        messages[0]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("earlier work"))),
        "the summary still leads the history"
    );
    assert_eq!(
        messages.last().unwrap().content,
        vec![ContentBlock::Text {
            text: "four".to_string()
        }]
    );
}

#[test]
fn a_cut_into_the_kept_tail_re_emits_the_summary_it_removed() {
    let dir = temp("compact-into");
    let transcript = transcript_at(&dir, "compact-into");
    record_turn(&transcript, "one", "t1");
    record_turn(&transcript, "two", "t2");
    record_turn(&transcript, "three", "t3");
    // Keeps the last two turns (8 message lines) verbatim.
    transcript.append_compact("earlier work", 8).unwrap();

    let entries = transcript.load_projection().unwrap();
    let before = transcript.load_messages().unwrap();
    let found = checkpoints_of(&entries, &dir.join("store"), 50);
    let target = found.iter().find(|c| c.label == "three").unwrap();
    // The chosen message is physically *above* the compact marker.
    transcript.truncate_at_line(target.line).unwrap();

    let messages = transcript.load_messages().unwrap();
    assert_eq!(
        messages[0], before[0],
        "the summary is re-emitted byte for byte"
    );
    assert_eq!(
        messages.last().unwrap().content,
        vec![ContentBlock::Text {
            text: "three".to_string()
        }]
    );
    assert!(
        !messages.iter().skip(1).any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "one"))),
        "a message the summary covers must not come back verbatim"
    );
}

#[test]
fn a_line_inside_a_compacted_span_is_refused() {
    let dir = temp("compact-refuse");
    let transcript = transcript_at(&dir, "compact-refuse");
    record_turn(&transcript, "one", "t1");
    record_turn(&transcript, "two", "t2");
    transcript.append_compact("earlier work", 2).unwrap();
    // Line 1 is "one" — folded into the summary, so it is not a rewind point.
    let error = transcript.truncate_at_line(1).unwrap_err();
    assert!(error.to_string().contains("compacted span"), "{error}");
    assert!(
        transcript.load_messages().unwrap().len() > 1,
        "the refusal changed nothing"
    );
}

#[test]
fn a_line_that_is_not_a_message_is_refused() {
    let dir = temp("refuse");
    let transcript = transcript_at(&dir, "refuse");
    record_turn(&transcript, "one", "t1");
    // Line 0 is the turn marker, not a message.
    assert!(transcript.truncate_at_line(0).is_err());
    assert!(transcript.truncate_at_line(999).is_err());
    assert_eq!(transcript.load_messages().unwrap().len(), 3);
}

#[test]
fn summarizing_from_a_turn_replaces_it_and_everything_after() {
    let dir = temp("summarize");
    let transcript = transcript_at(&dir, "summarize");
    record_turn(&transcript, "one", "t1");
    transcript
        .append(&Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "answered one".to_string(),
            }],
        })
        .unwrap();
    record_turn(&transcript, "two", "t2");
    record_turn(&transcript, "three", "t3");

    let entries = transcript.load_projection().unwrap();
    let target = entries
        .iter()
        .position(|entry| {
            entry.opens_turn.is_some()
                && entry.message.content.contains(&ContentBlock::Text {
                    text: "two".to_string(),
                })
        })
        .unwrap();
    let cut = entries[target - 1].line.unwrap();
    write_summary(
        &transcript,
        cut,
        "the user asked two things and got answers",
    )
    .unwrap();

    let messages = transcript.load_messages().unwrap();
    let last = &messages[messages.len() - 1];
    assert_eq!(last.role, Role::User);
    let text = match &last.content[0] {
        ContentBlock::Text { text } => text.clone(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(text.starts_with("(summary of the turns rewound from here)"));
    assert!(text.contains("the user asked two things"));
    assert!(
        !text.contains("from automatic compaction"),
        "compaction's wording is a contract about a prefix; a tail must not borrow it"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.content.contains(&ContentBlock::Text {
                text: "one".to_string()
            })),
        "the turns before the cut are still there verbatim"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.content.contains(&ContentBlock::Text {
                text: "three".to_string()
            })),
        "the summarized turns are gone"
    );
    // The summary is an ordinary message, so it is not a rewind point itself.
    let after = transcript.load_projection().unwrap();
    assert!(after.last().unwrap().opens_turn.is_none());
}

#[test]
fn the_rewind_summary_is_not_the_compaction_contract() {
    let rewound = summary_message("body");
    let compacted = crate::transcript::summary_message("body");
    assert_ne!(
        rewound, compacted,
        "a reload must be able to tell a rewound tail from a compacted prefix"
    );
}
