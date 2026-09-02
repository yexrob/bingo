//! What an agent with no memory of us is handed to read.
//!
//! The third rung of the restore ladder (ADR-0035 §3): where neither
//! `session/resume` nor `session/load` exists, a fresh ACP session is opened
//! and its first prompt names a file. The file is rendered from the request's
//! own messages — the journal, folded by `ContextView` exactly as every other
//! provider is given it — and never maintained alongside it. It is a
//! projection with a timestamp, not a second copy of the conversation.
//!
//! The rendering is pure. Writing it is one function, so the only thing that
//! touches a disk is the one the pool calls.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, Message, Role, SessionId};

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

/// The conversation as one document. What has nothing to say to an agent is
/// left out: bingo's notices and receipts never reach the fold, and the
/// agent's own private thinking is not part of what it said.
pub fn render(messages: &[Message]) -> String {
    let mut out = String::from(PREAMBLE);
    for message in messages {
        for passage in passages(message) {
            out.push('\n');
            out.push_str(&passage);
            out.push('\n');
        }
    }
    out
}

fn passages(message: &Message) -> Vec<String> {
    let said = speech(message);
    let mut out = section(heading(message.role), &said)
        .into_iter()
        .collect::<Vec<_>>();
    out.extend(message.parts.iter().filter_map(work));
    out
}

fn heading(role: Role) -> &'static str {
    match role {
        Role::User => "## The person said",
        Role::Assistant => "## You answered",
    }
}

/// The plain words of a turn, tool traffic excluded.
fn speech(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            ContentPart::Image { .. } => Some("(an image)".to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What was done rather than said. A reasoning part wearing the ACP mark is a
/// tool call the agent itself ran in an earlier session (see [`crate::events`]);
/// a plain one is its private thinking, which is not replayed.
fn work(part: &ContentPart) -> Option<String> {
    match part {
        ContentPart::Reasoning {
            text,
            provider_metadata,
        } if is_external(provider_metadata) => section("## You ran, on your own machine", text),
        ContentPart::ToolUse { name, input, .. } => section(
            &format!("## You ran `{name}`"),
            &serde_json::to_string(input).unwrap_or_default(),
        ),
        ContentPart::ToolResult { parts, .. } => section("### and it answered", &joined(parts)),
        _ => None,
    }
}

fn joined(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_external(metadata: &bingo_sdk::ProviderMetadata) -> bool {
    metadata
        .get(NAMESPACE)
        .and_then(|acp| acp.get(EXTERNAL))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
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

/// Write the transcript where the agent can read it with its own tools, and
/// answer with the path to name in the prompt. One file per bingo session,
/// rewritten each time the ladder falls this far: what an agent is told is
/// what the conversation says now.
pub fn write(dir: &Path, session: &SessionId, messages: &[Message]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{session}.md"));
    std::fs::write(&path, render(messages))?;
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
    use bingo_sdk::ProviderMetadata;
    use serde_json::json;

    fn said(text: &str) -> Message {
        Message::text(Role::User, text)
    }

    fn answered(text: &str) -> Message {
        Message::text(Role::Assistant, text)
    }

    fn marked(text: &str) -> Message {
        let mut acp = serde_json::Map::new();
        acp.insert(EXTERNAL.into(), json!(true));
        Message::assistant(vec![ContentPart::Reasoning {
            text: text.into(),
            provider_metadata: ProviderMetadata::from([(NAMESPACE.to_string(), acp)]),
        }])
    }

    fn thought(text: &str) -> Message {
        Message::assistant(vec![ContentPart::Reasoning {
            text: text.into(),
            provider_metadata: ProviderMetadata::new(),
        }])
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
        let body = render(&[thought("weighing two spellings"), marked("read src/lib.rs")]);
        assert!(body.contains("read src/lib.rs"), "{body}");
        assert!(body.contains("on your own machine"), "{body}");
        assert!(!body.contains("weighing two spellings"), "{body}");
    }

    #[test]
    fn a_tool_bingo_ran_carries_what_it_asked_and_what_came_back() {
        let body = render(&[
            Message::assistant(vec![ContentPart::ToolUse {
                id: "c1".into(),
                name: "Read".into(),
                input: json!({ "file_path": "src/lib.rs" }),
            }]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "c1".into(),
                parts: vec![ContentPart::text("pub mod wire;")],
                is_error: false,
            }]),
        ]);
        assert!(body.contains("## You ran `Read`"), "{body}");
        assert!(body.contains("src/lib.rs"), "{body}");
        assert!(body.contains("pub mod wire;"), "{body}");
    }

    /// A turn that is only tool traffic has no speech to head; the heading is
    /// dropped rather than written over nothing.
    #[test]
    fn nothing_is_written_under_an_empty_heading() {
        let body = render(&[]);
        assert!(body.starts_with("# Earlier turns"));
        assert!(!body.contains("##"), "no empty headings: {body}");
        let only_a_call = render(&[Message::assistant(vec![ContentPart::ToolUse {
            id: "c1".into(),
            name: "Read".into(),
            input: json!({}),
        }])]);
        assert!(!only_a_call.contains("## You answered"), "{only_a_call}");
        assert!(only_a_call.contains("## You ran `Read`"), "{only_a_call}");
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

        // Rewritten from the conversation as it stands, never appended to.
        let again = write(&dir, &session, &[said("hello"), answered("hi")]).expect("again");
        let body = std::fs::read_to_string(&again).expect("it reads back");
        assert_eq!(body.matches("hello").count(), 1, "{body}");
        assert!(body.contains("hi"), "{body}");
    }
}
