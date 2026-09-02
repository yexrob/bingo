//! What the agent's own tool call reads like.
//!
//! The structure of the call is kept whole in `provider_options` (see
//! [`crate::events`]); this is the human half — the lines a surface that knows
//! nothing about ACP still shows a person. Everything here is bounded: an
//! agent's `cat` of a large file must not become the whole transcript.

use agent_client_protocol_schema::v1::{
    ContentBlock, Diff, ToolCallContent, ToolCallStatus, ToolKind,
};

/// How much of one content block a transcript carries. Past this the reader is
/// told what was cut rather than shown a wall.
const MAX_BLOCK_CHARS: usize = 2000;

pub fn heading(kind: ToolKind, title: &str) -> String {
    format!("{} {title}", verb(kind))
}

/// The kinds ACP names, in the words a person reads. `switch_mode` is the
/// agent changing its own permission mode — worth saying, never acted on.
fn verb(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "run",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "mode",
        _ => "tool",
    }
}

pub fn outcome(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        _ => "ended",
    }
}

/// One content block as text. A diff is shown as what it replaced and what it
/// wrote, line by line, because that is the shape a person checks.
pub fn block(content: &ToolCallContent) -> String {
    match content {
        ToolCallContent::Content(inner) => text_of(&inner.content),
        ToolCallContent::Diff(diff) => diff_body(diff),
        ToolCallContent::Terminal(terminal) => {
            format!("terminal {}", terminal.terminal_id.0)
        }
        // A block kind newer than this build's schema still says it was there.
        _ => "(content this build does not render)".to_string(),
    }
}

fn text_of(content: &ContentBlock) -> String {
    match content {
        ContentBlock::Text(text) => clamp(&text.text),
        ContentBlock::Image(_) => "(image)".to_string(),
        ContentBlock::Audio(_) => "(audio)".to_string(),
        ContentBlock::Resource(_) | ContentBlock::ResourceLink(_) => "(resource)".to_string(),
        _ => "(content this build does not render)".to_string(),
    }
}

fn diff_body(diff: &Diff) -> String {
    let path = diff.path.display();
    let removed = prefixed(diff.old_text.as_deref().unwrap_or_default(), '-');
    let added = prefixed(&diff.new_text, '+');
    [format!("--- {path}"), removed, added]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn prefixed(body: &str, mark: char) -> String {
    if body.is_empty() {
        return String::new();
    }
    clamp(body)
        .lines()
        .map(|line| format!("{mark}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clamp(body: &str) -> String {
    let kept: String = body.chars().take(MAX_BLOCK_CHARS).collect();
    if kept.len() == body.len() {
        return kept;
    }
    let cut = body.chars().count() - MAX_BLOCK_CHARS;
    format!("{kept}\n… {cut} more characters")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use agent_client_protocol_schema::v1::{SessionNotification, SessionUpdate};

    fn content_of(recorded: serde_json::Value) -> Vec<ToolCallContent> {
        let note: SessionNotification =
            serde_json::from_value(recorded).expect("a recorded update parses");
        match note.update {
            SessionUpdate::ToolCall(call) => call.content,
            SessionUpdate::ToolCallUpdate(update) => update.fields.content.unwrap_or_default(),
            _ => panic!("the fixture is a tool call"),
        }
    }

    #[test]
    fn a_heading_says_what_the_agent_is_doing_in_words() {
        assert_eq!(
            heading(ToolKind::Read, "Read src/lib.rs (1 - 50)"),
            "read Read src/lib.rs (1 - 50)"
        );
        assert_eq!(heading(ToolKind::Execute, "npm test"), "run npm test");
    }

    #[test]
    fn a_diff_shows_what_went_and_what_came() {
        let body = block(&content_of(fixtures::update_tool_call_diff())[0]);
        assert!(body.contains("--- /work/repo/src/lib.rs"), "{body}");
        assert!(body.contains("-pub mod wire;"), "{body}");
        assert!(body.contains("+pub mod envelope;"), "{body}");
    }

    /// This client owns no terminal (ADR-0035 §6), so the id is all there is
    /// to say — and saying it is better than dropping the call.
    #[test]
    fn a_terminal_is_named_never_joined() {
        assert_eq!(
            block(&content_of(fixtures::update_tool_call_terminal())[0]),
            "terminal command-123"
        );
    }

    #[test]
    fn a_long_body_is_cut_and_the_cut_is_said() {
        let long = "x".repeat(MAX_BLOCK_CHARS + 40);
        let shown = clamp(&long);
        assert!(shown.starts_with(&"x".repeat(80)));
        assert!(shown.ends_with("… 40 more characters"), "{shown}");
        assert!(shown.chars().count() < long.chars().count() + 40);
    }

    #[test]
    fn a_file_being_created_has_nothing_to_remove() {
        let diff: Diff = serde_json::from_value(serde_json::json!({
            "path": "/work/repo/new.rs",
            "oldText": null,
            "newText": "fn main() {}"
        }))
        .expect("a diff");
        let body = diff_body(&diff);
        assert_eq!(body, "--- /work/repo/new.rs\n+fn main() {}");
    }
}
