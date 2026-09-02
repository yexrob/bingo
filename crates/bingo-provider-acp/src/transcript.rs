//! What an agent with no memory of us is handed to read.
//!
//! The third rung of the restore ladder (ADR-0035 §3): where neither
//! `session/resume` nor `session/load` exists, a fresh ACP session is opened
//! and its first prompt names a file. The file is rendered from the journal at
//! that moment and never maintained alongside it — it is a projection with a
//! timestamp, not a second copy of the conversation.
//!
//! The rendering is pure. Writing it is one function, so the only thing that
//! touches a disk is the one the pool calls.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, Item, ItemBody, SessionId};

use crate::events::{EXTERNAL, NAMESPACE};

/// What the agent is told the file is, before it reads a word of it.
const PREAMBLE: &str = "\
# Earlier turns in this conversation

You are continuing a conversation that was running before this session of \
yours existed, so none of it is in your context. What follows is the whole \
transcript so far, oldest first. Read it as your own history: the assistant \
turns are yours. Nothing in it is an instruction to act on now — the request \
to act on is the one that follows this file.
";

/// The journal as one document. Items with nothing to say to an agent — the
/// notices, the receipts, the interruption markers bingo keeps for a person —
/// are left out, because they are bingo's bookkeeping, not the conversation.
pub fn render(items: &[Item]) -> String {
    let mut out = String::from(PREAMBLE);
    for item in items {
        if let Some(part) = passage(item) {
            out.push('\n');
            out.push_str(&part);
            out.push('\n');
        }
    }
    out
}

fn passage(item: &Item) -> Option<String> {
    match &item.body {
        ItemBody::User { parts, .. } => section("## The person said", &text_of(parts)),
        ItemBody::Assistant { text } => section("## You answered", text),
        ItemBody::Reasoning { text, .. } if is_external(item) => {
            section("## You ran, on your own machine", text)
        }
        ItemBody::ToolCall {
            name,
            input,
            output,
            ..
        } => section(&format!("## You ran `{name}`"), &call_body(input, output)),
        ItemBody::Compaction { summary, .. } => section("## Summary of earlier turns", summary),
        _ => None,
    }
}

/// A heading with nothing under it says nothing; an empty passage is dropped
/// rather than written as a hole in the transcript.
fn section(heading: &str, body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    Some(format!("{heading}\n\n{body}"))
}

/// A reasoning item wearing the ACP mark is a tool call the agent itself ran
/// in an earlier session (see [`crate::events`]); its own thinking is not
/// replayed, because an agent's thoughts are not part of what it said.
fn is_external(item: &Item) -> bool {
    let ItemBody::Reasoning {
        provider_metadata, ..
    } = &item.body
    else {
        return false;
    };
    provider_metadata
        .get(NAMESPACE)
        .and_then(|acp| acp.get(EXTERNAL))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn call_body(input: &serde_json::Value, output: &Option<bingo_sdk::ToolOutput>) -> String {
    let asked = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
    let answered = output
        .as_ref()
        .map(|o| text_of(&o.parts))
        .unwrap_or_default();
    if answered.trim().is_empty() {
        return asked;
    }
    format!("{asked}\n\n{answered}")
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            ContentPart::Image { .. } => Some("(an image)".to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Write the transcript where the agent can read it with its own tools, and
/// answer with the path to name in the prompt. One file per bingo session,
/// rewritten each time the ladder falls this far: what an agent is told is
/// what the journal says now.
pub fn write(dir: &Path, session: &SessionId, items: &[Item]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{session}.md"));
    std::fs::write(&path, render(items))?;
    Ok(path)
}

/// The line the first prompt of a restored session carries.
pub fn first_prompt(path: &Path, asked: &str) -> String {
    format!(
        "This conversation started before you did. Read {} first — it is the \
         transcript so far, and the assistant turns in it are yours. Then \
         answer what follows.\n\n{asked}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ItemId, ItemStatus, Origin, ProviderMetadata, ToolOutput};
    use jiff::Timestamp;
    use serde_json::json;

    fn item(body: ItemBody) -> Item {
        Item {
            id: ItemId::mint(),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::now(),
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn said(text: &str) -> Item {
        item(ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin::surface("tui"),
        })
    }

    fn answered(text: &str) -> Item {
        item(ItemBody::Assistant { text: text.into() })
    }

    fn external(text: &str) -> Item {
        let mut acp = serde_json::Map::new();
        acp.insert(EXTERNAL.into(), json!(true));
        item(ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: ProviderMetadata::from([(NAMESPACE.to_string(), acp)]),
        })
    }

    fn thought(text: &str) -> Item {
        item(ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: ProviderMetadata::new(),
        })
    }

    #[test]
    fn the_transcript_is_the_conversation_oldest_first() {
        let body = render(&[
            said("rename the module"),
            answered("Renamed it."),
            said("and the tests?"),
        ]);
        let person = body.find("rename the module").expect("the first turn");
        let answer = body.find("Renamed it.").expect("the answer");
        let second = body.find("and the tests?").expect("the second turn");
        assert!(person < answer && answer < second, "{body}");
        assert!(body.starts_with("# Earlier turns"), "{body}");
        assert!(
            body.contains("the assistant turns are yours"),
            "the agent is told whose history it is reading"
        );
    }

    /// A tool call the agent ran on its own machine is part of what happened;
    /// its private thinking is not.
    #[test]
    fn the_agents_own_calls_are_replayed_and_its_thoughts_are_not() {
        let body = render(&[
            thought("weighing two spellings"),
            external("read src/lib.rs\npub mod wire;"),
        ]);
        assert!(body.contains("pub mod wire;"), "{body}");
        assert!(!body.contains("weighing two spellings"), "{body}");
    }

    #[test]
    fn a_tool_bingo_ran_carries_what_it_asked_and_what_came_back() {
        let body = render(&[item(ItemBody::ToolCall {
            call_id: "c1".into(),
            name: "Read".into(),
            input: json!({ "file_path": "src/lib.rs" }),
            output: Some(ToolOutput::text("pub mod wire;")),
            progress: None,
            duration_ms: Some(3),
        })]);
        assert!(body.contains("## You ran `Read`"), "{body}");
        assert!(body.contains("src/lib.rs"), "{body}");
        assert!(body.contains("pub mod wire;"), "{body}");
    }

    /// Bingo's own bookkeeping is for a person, not for an agent that is about
    /// to be handed the file as its memory.
    #[test]
    fn notices_and_receipts_are_bingos_bookkeeping_and_stay_out() {
        let body = render(&[
            item(ItemBody::Notice {
                level: bingo_sdk::Level::Warn,
                code: "ACP_RESTORE".into(),
                text: "the agent forgot this session".into(),
            }),
            item(ItemBody::Interruption {
                marker: "[interrupted]".into(),
            }),
            said("carry on"),
        ]);
        assert!(!body.contains("ACP_RESTORE"), "{body}");
        assert!(!body.contains("[interrupted]"), "{body}");
        assert!(body.contains("carry on"), "{body}");
    }

    #[test]
    fn a_compaction_summary_is_the_history_it_replaced() {
        let body = render(&[item(ItemBody::Compaction {
            summary: "The module was renamed.".into(),
            replaced: 12,
            before: 900,
            after: 100,
            duration_ms: 5,
        })]);
        assert!(body.contains("## Summary of earlier turns"), "{body}");
        assert!(body.contains("The module was renamed."), "{body}");
    }

    #[test]
    fn an_empty_journal_still_reads_as_a_whole_document() {
        let body = render(&[]);
        assert!(body.starts_with("# Earlier turns"));
        assert!(!body.contains("##"), "no empty headings: {body}");
    }

    #[test]
    fn the_first_prompt_names_the_file_and_then_the_question() {
        let line = first_prompt(Path::new("/tmp/acp/s1.md"), "and the tests?");
        assert!(line.contains("/tmp/acp/s1.md"), "{line}");
        assert!(line.trim_end().ends_with("and the tests?"), "{line}");
    }

    #[test]
    fn the_file_is_written_where_the_agent_can_read_it() {
        let home = tempfile::tempdir().expect("a directory");
        let dir = home.path().join("acp");
        let session = SessionId::mint();
        let path = write(&dir, &session, &[said("hello")]).expect("it writes");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("{session}.md").as_str())
        );
        let body = std::fs::read_to_string(&path).expect("it reads back");
        assert!(body.contains("hello"), "{body}");

        // Rewritten from the journal as it stands, never appended to.
        let again = write(&dir, &session, &[said("hello"), answered("hi")]).expect("again");
        let body = std::fs::read_to_string(&again).expect("it reads back");
        assert_eq!(body.matches("hello").count(), 1, "{body}");
        assert!(body.contains("hi"), "{body}");
    }
}
