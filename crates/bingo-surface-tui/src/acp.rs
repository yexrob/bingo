//! An ACP agent's own tool call, read back out of the journal.
//!
//! ADR-0035 §4: an agent that runs its own tools hands bingo a finished call
//! rather than an instruction, so the provider journals it as a reasoning item
//! whose text is the heading a person reads and whose provider metadata is the
//! whole call — under the `acp` namespace, marked `external`. This is the
//! surface half that ADR left open: the metadata read back into the few facts
//! a tool row is drawn from ([`crate::transcript`]).
//!
//! Nothing here imports the provider — no surface imports a plugin (ADR-0001)
//! — so the namespace, the flag and the field names are read as the journal
//! spells them. That spelling is the contract: it is written in
//! `crates/bingo-provider-acp/src/events.rs` (`Call::metadata`) and pinned by
//! that crate's fixtures, so an item an older build wrote reads exactly the
//! same way. A reasoning item without the mark is a thought and stays one.

use bingo_sdk::{ContentPart, Item, ItemBody, ItemStatus, ToolOutput, View};
use serde_json::{Map, Value};

/// The namespace everything ACP-private hangs under, and the flag inside it
/// that says the agent ran the call itself — copied from
/// `bingo_provider_acp::events`, which is where the two are defined.
const NAMESPACE: &str = "acp";
const EXTERNAL: &str = "external";

/// One of the agent's own calls, in the words a tool row is drawn from.
pub struct Call {
    /// What the row is called. The tool's own name never crosses the wire, so
    /// it is the kind, in the casing every other row's name wears.
    pub name: &'static str,
    /// What the row says the call was about.
    pub about: String,
    pub status: ItemStatus,
    /// What came back, when the agent sent anything to read.
    pub output: Option<ToolOutput>,
}

/// Whether this item is one of the agent's own calls.
///
/// The mark rides the end of the block — the provider writes it in
/// `close_call` — so a call still arriving carries none and reads as the
/// thought it looks like until it is over. That is deliberate: a finished call
/// is what a row can state, and there is no live tool-row state here to invent
/// for one that is still running.
pub fn is_call(item: &Item) -> bool {
    marked(item).is_some()
}

/// The whole call, for the row that draws it.
pub fn call(item: &Item) -> Option<Call> {
    let acp = marked(item)?;
    Some(Call {
        name: name(at(acp, "kind")),
        about: about(acp),
        status: status(at(acp, "status")),
        output: output(acp),
    })
}

fn marked(item: &Item) -> Option<&Map<String, Value>> {
    let ItemBody::Reasoning {
        provider_metadata, ..
    } = &item.body
    else {
        return None;
    };
    let acp = provider_metadata.get(NAMESPACE)?;
    (acp.get(EXTERNAL).and_then(Value::as_bool) == Some(true)).then_some(acp)
}

fn at<'a>(acp: &'a Map<String, Value>, key: &str) -> &'a str {
    acp.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// ACP's kinds as a row names them. A kind this build has never heard of is
/// `Tool`: an unknown call says it was one and claims nothing else.
fn name(kind: &str) -> &'static str {
    match kind {
        "read" => "Read",
        "edit" => "Edit",
        "delete" => "Delete",
        "move" => "Move",
        "search" => "Search",
        "execute" => "Run",
        "think" => "Think",
        "fetch" => "Fetch",
        "switch_mode" => "Mode",
        _ => "Tool",
    }
}

/// What the row says the call was about: the input in the words every other
/// row is read in — the one field a person recognises, shortened against the
/// session's directory — and the adapter's own title where the call carried no
/// input to read it from.
fn about(acp: &Map<String, Value>) -> String {
    let summarised = acp
        .get("rawInput")
        .map(crate::transcript::summarize)
        .unwrap_or_default();
    match summarised.is_empty() {
        true => at(acp, "title").to_string(),
        false => summarised,
    }
}

/// The call's own status, as the bullet reads it. Nothing maps to `Running`:
/// the mark is written when the block closes, so a call in progress there is
/// one the turn ended without a verdict — a row that cannot move, and a
/// running bullet would pulse on it for ever.
fn status(status: &str) -> ItemStatus {
    match status {
        "completed" => ItemStatus::Completed,
        "failed" => ItemStatus::Failed,
        _ => ItemStatus::Interrupted,
    }
}

/// What the call came back with, as a result. A diff is drawn through the
/// house's own renderer, so an agent's edit is coloured exactly like every
/// other patch on the screen; everything else is its text.
///
/// `rawOutput` is not read. It is the same answer in the tool's own machine
/// words, and one fact is drawn once.
fn output(acp: &Map<String, Value>) -> Option<ToolOutput> {
    let blocks = acp.get("content").and_then(Value::as_array)?;
    let unified = patches(blocks);
    let said = said(blocks);
    if unified.is_empty() && said.is_empty() {
        return None;
    }
    Some(ToolOutput {
        parts: match said.is_empty() {
            true => Vec::new(),
            false => vec![ContentPart::text(said)],
        },
        is_error: at(acp, "status") == "failed",
        display: (!unified.is_empty()).then_some(View::Diff { unified }),
    })
}

/// Every diff block as one patch, in the columns the house diff renderer reads
/// — the same three the provider's text half writes as prose.
fn patches(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|block| kind_of(block) == "diff")
        .map(patch)
        .collect::<Vec<_>>()
        .join("\n")
}

fn patch(block: &Value) -> String {
    [
        format!("--- {}", string_at(block, "path")),
        prefixed(string_at(block, "oldText"), '-'),
        prefixed(string_at(block, "newText"), '+'),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

fn prefixed(body: &str, mark: char) -> String {
    body.lines()
        .map(|line| format!("{mark}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The blocks a person reads as text: what a content block says, and the id of
/// a terminal this client never joined (ADR-0035 §6).
fn said(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(spoken)
        .collect::<Vec<_>>()
        .join("\n")
}

fn spoken(block: &Value) -> Option<String> {
    match kind_of(block) {
        "content" => Some(shown(block.get("content")?)),
        "terminal" => Some(format!("terminal {}", string_at(block, "terminalId"))),
        _ => None,
    }
}

/// One content block as a line. What is not text still says it was there,
/// because the alternative is a result with a silent hole in it.
fn shown(content: &Value) -> String {
    match kind_of(content) {
        "text" => string_at(content, "text").to_string(),
        "" => String::new(),
        other => format!("({other})"),
    }
}

fn kind_of(block: &Value) -> &str {
    string_at(block, "type")
}

fn string_at<'a>(block: &'a Value, key: &str) -> &'a str {
    block.get(key).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{agent_call, item};
    use serde_json::json;

    /// The metadata `bingo-provider-acp` writes for the fixture call its own
    /// tests pin (`update_tool_call` + `update_tool_call_completed`).
    fn a_read() -> Item {
        agent_call(
            "itm_1",
            "read Read src/lib.rs (1 - 50)done\npub mod wire;",
            json!({
                "external": true,
                "toolCallId": "toolu_01Read",
                "title": "Read src/lib.rs (1 - 50)",
                "kind": "read",
                "status": "completed",
                "content": [
                    { "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }
                ],
                "locations": [{ "path": "/tmp/project/src/lib.rs", "line": 1 }],
                "rawInput": { "file_path": "/tmp/project/src/lib.rs", "offset": 1 },
                "rawOutput": { "lines": 1 }
            }),
        )
    }

    #[test]
    fn a_marked_item_reads_back_as_the_call_it_was() {
        let call = call(&a_read()).expect("the mark is there");
        assert_eq!(call.name, "Read");
        assert_eq!(call.about, "/tmp/project/src/lib.rs");
        assert_eq!(call.status, ItemStatus::Completed);
        let output = call.output.expect("the call came back");
        assert_eq!(output.parts, vec![ContentPart::text("pub mod wire;")]);
        assert!(!output.is_error);
        assert_eq!(output.display, None);
    }

    /// A thought is a thought: nothing without the namespace and the flag is
    /// ever read as a call.
    #[test]
    fn a_plain_thought_is_no_call_and_neither_is_an_unmarked_namespace() {
        let thought = item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Reasoning {
                text: "the manifest first".into(),
                provider_metadata: Default::default(),
            },
        );
        assert!(!is_call(&thought));
        assert!(call(&thought).is_none());

        let unflagged = agent_call("itm_2", "thinking aloud", json!({ "title": "not a call" }));
        assert!(!is_call(&unflagged), "the flag is what says it was a call");

        let assistant = item(
            "itm_3",
            ItemStatus::Completed,
            ItemBody::Assistant {
                text: "done".into(),
            },
        );
        assert!(!is_call(&assistant));
    }

    /// An edit reports itself as a diff, which is the whole reason the call is
    /// carried structurally: it comes back through the one diff renderer.
    #[test]
    fn a_diff_block_becomes_the_houses_own_diff() {
        let edit = agent_call(
            "itm_1",
            "edit src/lib.rs",
            json!({
                "external": true,
                "toolCallId": "toolu_02Edit",
                "title": "Edit src/lib.rs",
                "kind": "edit",
                "status": "completed",
                "content": [{
                    "type": "diff",
                    "path": "/tmp/project/src/lib.rs",
                    "oldText": "pub mod wire;",
                    "newText": "pub mod envelope;"
                }],
                "locations": [{ "path": "/tmp/project/src/lib.rs" }]
            }),
        );
        let call = call(&edit).expect("a call");
        assert_eq!(call.name, "Edit");
        assert_eq!(
            call.about, "Edit src/lib.rs",
            "no input to read, so the adapter's own title stands"
        );
        assert_eq!(
            call.output.and_then(|output| output.display),
            Some(View::Diff {
                unified: "--- /tmp/project/src/lib.rs\n-pub mod wire;\n+pub mod envelope;".into()
            }),
        );
    }

    /// A file being created has nothing to remove, and a terminal is named
    /// rather than joined (ADR-0035 §6).
    #[test]
    fn a_creation_removes_nothing_and_a_terminal_is_named() {
        let wrote = agent_call(
            "itm_1",
            "edit new.rs",
            json!({
                "external": true, "kind": "edit", "status": "completed",
                "content": [{ "type": "diff", "path": "new.rs", "newText": "fn main() {}" }]
            }),
        );
        assert_eq!(
            call(&wrote).and_then(|c| c.output).and_then(|o| o.display),
            Some(View::Diff {
                unified: "--- new.rs\n+fn main() {}".into()
            }),
        );

        let ran = agent_call(
            "itm_2",
            "run npm test",
            json!({
                "external": true, "kind": "execute", "status": "in_progress",
                "title": "npm test",
                "content": [{ "type": "terminal", "terminalId": "command-123" }],
                "rawInput": { "command": "npm test", "cwd": "/tmp/project" }
            }),
        );
        let call = call(&ran).expect("a call");
        assert_eq!((call.name, call.about.as_str()), ("Run", "npm test"));
        assert_eq!(
            call.status,
            ItemStatus::Interrupted,
            "the turn ended before the agent said how it went"
        );
        assert_eq!(
            call.output.map(|o| o.parts),
            Some(vec![ContentPart::text("terminal command-123")]),
        );
    }

    #[test]
    fn a_failed_call_says_so_and_carries_what_it_said() {
        let failed = agent_call(
            "itm_1",
            "tool toolu_04Bash",
            json!({
                "external": true, "toolCallId": "toolu_04Bash", "status": "failed",
                "title": "toolu_04Bash",
                "content": [
                    { "type": "content", "content": { "type": "text", "text": "no such file" } }
                ]
            }),
        );
        let call = call(&failed).expect("a call");
        assert_eq!((call.name, call.about.as_str()), ("Tool", "toolu_04Bash"));
        assert_eq!(call.status, ItemStatus::Failed);
        let output = call.output.expect("what it said");
        assert!(output.is_error);
        assert_eq!(output.parts, vec![ContentPart::text("no such file")]);
    }

    /// A call with nothing to show has no result at all, so its row promises
    /// none: no `⎿`, no fold, no sheet.
    #[test]
    fn a_call_that_said_nothing_has_no_result() {
        let quiet = agent_call(
            "itm_1",
            "read Read src/lib.rs",
            json!({ "external": true, "kind": "read", "status": "completed", "content": [] }),
        );
        assert!(call(&quiet).expect("a call").output.is_none());
    }

    /// What is not text still says it was there.
    #[test]
    fn content_that_is_not_text_is_named_rather_than_dropped() {
        let looked = agent_call(
            "itm_1",
            "fetch a picture",
            json!({
                "external": true, "kind": "fetch", "status": "completed",
                "title": "a picture",
                "content": [{ "type": "content", "content": { "type": "image", "data": "…" } }]
            }),
        );
        assert_eq!(
            call(&looked).and_then(|c| c.output).map(|o| o.parts),
            Some(vec![ContentPart::text("(image)")]),
        );
    }

    /// An adapter newer than this build names a kind nobody here has heard of.
    /// It is still a call, and it still says so.
    #[test]
    fn a_kind_this_build_does_not_know_is_a_tool() {
        assert_eq!(name("subagent"), "Tool");
        assert_eq!(name(""), "Tool");
    }
}
