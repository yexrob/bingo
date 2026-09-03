//! The screens a call an ACP agent ran on its own side is read through
//! (ADR-0035 §4): finished, it is a tool row like every other; still running,
//! it is the heading text it has always been; and a thought beside it is
//! untouched, which is what says the mark is the whole of what tells them
//! apart.

use super::*;

/// One of the agent's calls as `bingo-provider-acp` journals it: the heading
/// lines in the text, the whole call in the `acp` metadata. The text is what
/// the row replaces, so it is written here as the provider's deltas leave it.
fn call(id: &str, text: &str, acp: serde_json::Value) -> bingo_sdk::Item {
    crate::test_support::agent_call(id, text, acp)
}

/// A turn the agent spent on its own tools: a thought, a file read, an edit it
/// reports as a diff, and a command that failed.
fn a_turn_of_its_own() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, user("itm_1", "rename the wire module")),
        item(2, thought("itm_2", "the import list moves too")),
        item(
            3,
            call(
                "itm_3",
                "read Read src/lib.rs (1 - 50)done\npub mod wire;",
                json!({
                    "external": true,
                    "toolCallId": "toolu_01Read",
                    "title": "Read src/lib.rs (1 - 50)",
                    "kind": "read",
                    "status": "completed",
                    "content": [
                        { "type": "content",
                          "content": { "type": "text", "text": "pub mod wire;" } }
                    ],
                    "locations": [{ "path": "/tmp/project/src/lib.rs", "line": 1 }],
                    "rawInput": { "file_path": "/tmp/project/src/lib.rs", "offset": 1 },
                    "rawOutput": { "lines": 1 }
                }),
            ),
        ),
        item(
            4,
            call(
                "itm_4",
                "edit Edit src/lib.rsdone\n--- /tmp/project/src/lib.rs\n-pub mod wire;\n+pub mod envelope;",
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
                    "locations": [{ "path": "/tmp/project/src/lib.rs" }],
                    "rawInput": { "file_path": "/tmp/project/src/lib.rs" }
                }),
            ),
        ),
        item(
            5,
            call(
                "itm_5",
                "run cargo test -p wirefailed\nerror: no target named `wire`",
                json!({
                    "external": true,
                    "toolCallId": "toolu_03Bash",
                    "title": "cargo test -p wire",
                    "kind": "execute",
                    "status": "failed",
                    "content": [
                        { "type": "content",
                          "content": { "type": "text", "text": "error: no target named `wire`" } }
                    ],
                    "rawInput": { "command": "cargo test -p wire", "cwd": "/tmp/project" }
                }),
            ),
        ),
        item(
            6,
            assistant(
                "itm_6",
                "Renamed `wire` to `envelope`; the test target still points at the old name.",
                ItemStatus::Completed,
            ),
        ),
    ])
}

/// A thought of the agent's own, which carries no `acp` mark and is drawn as
/// what it is.
fn thought(id: &str, text: &str) -> bingo_sdk::Item {
    let mut thought = crate::test_support::item(
        id,
        ItemStatus::Completed,
        ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: Default::default(),
        },
    );
    thought.completed_at = Some(ts() + jiff::SignedDuration::from_secs(1));
    thought
}

/// The slice ADR-0035 §4 promised: the agent's calls read as calls — the same
/// bullet, signature and folded result every other row wears, with the edit's
/// diff through the house's own renderer — and the thought above them still
/// reads as a thought.
#[test]
fn an_agents_own_calls_read_as_rows() {
    let (ui, now) = scene();
    both("acp_calls", &solo(&a_turn_of_its_own()), &ui, now);
}

/// The call as it is being made: the mark is written when the block closes, so
/// there is none yet and the heading reads as the thought it looks like. This
/// is the screen the row does not change.
#[test]
fn a_call_still_running_reads_as_the_text_it_is() {
    let state = folded(vec![
        frame(1, started("trn_1")),
        item(2, user("itm_1", "run the tests")),
        frame(
            3,
            Event::ItemStarted {
                item: crate::test_support::item(
                    "itm_2",
                    ItemStatus::Running,
                    ItemBody::Reasoning {
                        text: "run cargo test --workspace".into(),
                        provider_metadata: Default::default(),
                    },
                ),
            },
        ),
    ]);
    let (ui, now) = mid_turn();
    both("acp_call_running", &solo(&state), &ui, now);
}
