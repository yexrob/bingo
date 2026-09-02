//! The screens a team is read through (§3 "Teams"): a room's own transcript,
//! a post where a member reads it, what the three message tools answer, and
//! what a room is owed. They are `screens.rs`'s scenes and keep its snapshot
//! names — `both` is still the one that draws them.

use bingo_sdk::{ContentPart, Event, Item, ItemStatus, Tone, ToolOutput, TreeNode, View};
use serde_json::json;

use super::{both, item};
use crate::test_support::*;

#[test]
fn a_room_transcript() {
    let tree = room_tree(vec![
        posted(2, "itm_1", "reviewer", "the plan is thin on tests"),
        posted(3, "itm_2", "scout", "M9's exit criteria cover them"),
        log_frame(
            4,
            Event::ItemCompleted {
                item: user("itm_3", "then let us ship it"),
            },
        ),
    ]);
    let (ui, now) = scene();
    both("room_transcript", &tree, &ui, now);
}

/// The same posts where a member reads them. A member's own transcript
/// carries every conversation it is in, so a post says which one it came
/// from; a direct message came from nowhere but its sender.
#[test]
fn a_room_post_in_a_member_s_own_transcript() {
    let state = folded(vec![
        item(1, post("itm_1", "reviewer", "the plan is thin on tests")),
        item(
            2,
            delivered("itm_2", "agent", Some("scout"), "Two nits, else fine."),
        ),
    ]);
    let tree = solo(&state);
    let (ui, now) = scene();
    let screen = draw_tree(80, 24, &tree, &ui, now);
    assert!(
        screen.contains("⏺ reviewer in #design: the plan"),
        "{screen}"
    );
    assert!(
        screen.contains("⏺ scout: Two nits"),
        "a direct message names no room: {screen}"
    );
    both("room_post", &tree, &ui, now);
}

/// A tool that drew for a person as well as answering the model.
fn answered_with(text: &str, view: View) -> Option<ToolOutput> {
    Some(ToolOutput {
        parts: vec![ContentPart::text(text)],
        is_error: false,
        display: Some(view),
    })
}

fn seat(label: &str, badge: Option<&str>, children: Vec<TreeNode>) -> TreeNode {
    TreeNode {
        label: label.into(),
        badge: badge.map(str::to_string),
        tone: Tone::Neutral,
        children,
    }
}

fn leaf(label: &str, badge: Option<&str>) -> TreeNode {
    seat(label, badge, Vec::new())
}

fn completed(id: &str, name: &str, input: serde_json::Value, out: Option<ToolOutput>) -> Item {
    tool(id, name, input, out, ItemStatus::Completed)
}

/// `OpenRoom`: the room, its live seats, and the ones that listen under a node
/// of their own — five rows, which is exactly what a result keeps.
fn opened_a_room() -> Item {
    let seats = View::Tree {
        nodes: vec![seat(
            "#design",
            None,
            vec![
                leaf("helper", None),
                leaf("scout", None),
                seat("listening", None, vec![leaf("watcher", Some("120s"))]),
            ],
        )],
    };
    completed(
        "itm_1",
        "OpenRoom",
        json!({"name": "design", "members": ["helper", "scout"]}),
        answered_with("#design: helper, scout, ~watcher(120s)", seats),
    )
}

/// `SendMessage`: where it went, whose name it arrives under, when it is read.
fn posted_to_a_room() -> Item {
    let receipt = View::KeyValue {
        rows: vec![
            ("to".into(), "#design".into()),
            ("from".into(), "parent".into()),
            ("read".into(), "by every member, as it lands".into()),
        ],
    };
    completed(
        "itm_2",
        "SendMessage",
        json!({"to": "#design", "text": "look again"}),
        answered_with("Posted to #design.", receipt),
    )
}

/// `ListAgents`: a node per agent, badged with what it is doing.
fn listed_the_agents() -> Item {
    let roster = View::Tree {
        nodes: vec![leaf("helper", Some("busy")), leaf("scout", Some("idle"))],
    };
    completed(
        "itm_3",
        "ListAgents",
        json!({}),
        answered_with("helper  ses_2  busy", roster),
    )
}

/// What the three tools a team is run through now answer (ADR-0013 §2). The
/// values are spelled here because a surface may not depend on a plugin —
/// what this asserts is the drawing, and `bingo-rooms` and `bingo-agents`
/// assert the values.
#[test]
fn what_the_message_tools_answer() {
    let state = folded(vec![
        item(1, opened_a_room()),
        item(2, posted_to_a_room()),
        item(3, listed_the_agents()),
    ]);
    let tree = solo(&state);
    let (ui, now) = scene();
    let wide = draw_tree(120, 40, &tree, &ui, now);
    assert!(wide.contains("└─ watcher [ 120s ]"), "{wide}");
    assert!(
        wide.contains("read  by every member, as it lands"),
        "{wide}"
    );
    assert!(wide.contains("├─ helper [ busy ]"), "{wide}");
    both("message_tools", &tree, &ui, now);
}

/// What a room is owed, on the session it hangs under (ADR-0022 §4): a live
/// card in the rail past 120 columns, and the same card under the running
/// rows below it.
#[test]
fn what_a_room_is_owed() {
    let owed = View::Table {
        headers: ["room", "owed", "asked"].map(str::to_string).to_vec(),
        rows: vec![
            vec!["#design".into(), "reviewer".into(), "14:02".into()],
            vec!["#design".into(), "@all".into(), "14:09".into()],
        ],
    };
    let state = folded(vec![
        item(1, user("itm_1", "ask the room whether the plan is thin")),
        item(
            2,
            assistant("itm_2", "Asked in #design.", ItemStatus::Completed),
        ),
        frame(3, signalled("bingo.rooms", "owed", as_payload(owed))),
    ]);
    let tree = solo(&state);
    let (ui, now) = scene();
    let wide = draw_tree(120, 40, &tree, &ui, now);
    assert!(
        wide.contains("owed"),
        "the card is titled by its kind: {wide}"
    );
    assert!(
        wide.contains("#design  reviewer  14…"),
        "three columns of `owed` are one column wider than the rail, and the \
         clock time is what folds: {wide}"
    );
    both("owed", &tree, &ui, now);
}
