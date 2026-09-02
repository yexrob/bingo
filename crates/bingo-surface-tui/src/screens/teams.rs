//! The screens a team is read through (§3 "Teams"): a room's own transcript,
//! a post where a member reads it, what the three message tools answer, and
//! what a room is owed. They are `screens.rs`'s scenes and keep its snapshot
//! names — `both` is still the one that draws them.

use bingo_sdk::{ContentPart, Event, Item, ItemStatus, Tone, ToolOutput, TreeNode, View};
use serde_json::json;

use super::{both, item};
use crate::roster::Cursor;
use crate::test_support::*;
use crate::tree::Tree;
use crate::ui::{Open, Switcher};

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

/// `OpenRoom`: the room and a node per seat, badged with the ear it asked for
/// where that is not the default one a bare name takes (ADR-0034 §6) — five
/// rows, which is exactly what a result keeps.
fn opened_a_room() -> Item {
    let seats = View::Tree {
        nodes: vec![seat(
            "#design",
            None,
            vec![
                leaf("helper", None),
                leaf("scout", None),
                leaf("watcher", Some("120s")),
                leaf("parent", Some("live")),
            ],
        )],
    };
    completed(
        "itm_1",
        "OpenRoom",
        json!({"name": "design", "members": ["helper", "scout"]}),
        answered_with("#design: helper, scout, watcher:120, parent:0", seats),
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
    assert!(wide.contains("├─ watcher [ 120s ]"), "{wide}");
    assert!(wide.contains("└─ parent [ live ]"), "{wide}");
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
    let state = folded(vec![
        item(1, user("itm_1", "ask the room whether the plan is thin")),
        item(
            2,
            assistant("itm_2", "Asked in #design.", ItemStatus::Completed),
        ),
        frame(
            3,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[("#design", "reviewer", 22), ("#design", "@all", 15)]),
            ),
        ),
    ]);
    let tree = solo(&state);
    let (ui, now) = scene();
    let wide = draw_tree(120, 40, &tree, &ui, now);
    assert!(
        wide.contains("owed"),
        "the card is titled by its kind: {wide}"
    );
    assert!(
        wide.contains("#design  reviewer"),
        "two columns fit the rail whole, where three folded the clock \
         (2026-09-02): {wide}"
    );
    both("owed", &tree, &ui, now);
}

// ---- the one list of sessions (M36) -------------------------------------

/// A root with three sub-agents, a room two of them sit in — one on the live
/// ear it asked for, owing an answer and behind the room's head, one on a
/// patience of its own — and the room's own debt signalled on the root: one
/// row of every kind the list has.
fn a_team() -> Tree {
    let mut frames = busy_child("reviewer");
    frames.extend([
        agent_frame(3, 20, agent_announced(3, "watcher")),
        agent_frame(4, 21, agent_announced(4, "scout")),
        agent_frame(4, 22, started("trn_4")),
        agent_frame(
            4,
            23,
            crate::test_support::completed("trn_4", bingo_sdk::TurnStatus::Completed),
        ),
        log_frame(30, log_announced("#design")),
        log_frame(
            31,
            extended(
                "bingo.rooms",
                "members",
                roster_payload(
                    &["reviewer", "watcher"],
                    &[("reviewer", 0), ("watcher", 600)],
                ),
            ),
        ),
        frame(
            32,
            signalled(
                "bingo.rooms",
                "owed",
                owed_payload(&[("#design", "reviewer", 22)]),
            ),
        ),
        item(33, user("itm_0", "what is in this workspace?")),
    ]);
    frames.extend((1..=4u64).map(|n| {
        posted(
            31 + n,
            &format!("itm_p{n}"),
            "watcher",
            &format!("post {n}"),
        )
    }));
    // Four posts stand in the room; this seat stopped reading at the first.
    frames.push(log_frame(36, room_cursor("reviewer", "itm_p1")));
    folded_tree(frames)
}

/// What `↓` on an empty composer and `ctrl+g` both open: one column under two
/// labels — the sessions that answer a model with what each is doing, where it
/// sits and what it owes, then the rooms with their size and their debts.
#[test]
fn the_roster() {
    let tree = a_team();
    let (mut ui, now) = scene();
    shown(&mut ui, Open::Switcher(Switcher::default()), now);
    let wide = draw_tree(120, 40, &tree, &ui, now);
    assert!(wide.contains("Agents"), "{wide}");
    assert!(wide.contains("Rooms"), "{wide}");
    assert!(
        wide.contains("~ reviewer  running · in #design · 3 unread · live"),
        "a live seat wears the sigil and says so: {wide}"
    );
    assert!(
        wide.contains("⏺ watcher   idle · in #design · listening · 600s"),
        "a patience the roster asked for is said in seconds: {wide}"
    );
    assert!(
        wide.contains("owes an answer · 22m"),
        "and a debtor says what it owes and how long it has stood: {wide}"
    );
    assert!(
        wide.contains("in #design · 3 unread"),
        "a seat behind the room's head says how much of it it has not read: {wide}"
    );
    assert!(
        wide.contains("#design  2 seats · 1 owed"),
        "the room's own row is its size and its debts: {wide}"
    );
    both("roster", &tree, &ui, now);
}

/// §4: the list spends a hue on the cursor, on the sessions at work, and on
/// what wants a person — and on nothing else. Which row the keyboard is on is
/// said in weight, so `NO_COLOR` loses none of it.
#[test]
fn the_roster_spends_colour_only_where_the_design_says() {
    let tree = a_team();
    let (mut ui, now) = scene();
    shown(&mut ui, Open::Switcher(Switcher::default()), now);
    let painted = crate::painted::painted(120, 40, &tree, &ui, now);
    assert_eq!(
        painted.coloured("Agents"),
        Vec::<String>::new(),
        "a label is furniture, and furniture is dim"
    );
    assert_eq!(
        painted.coloured("owes an answer · 22m"),
        vec!["~", "owes an answer · 22m"],
        "the sigil in the dot's place for the session at work — it took the \
         glyph and none of the hue — the debt for what wants a person, and \
         nothing else on the row"
    );
}

/// The walk goes on past the last agent onto the rooms, stepping over the
/// label between them; the list still keeps every row the keyboard could be on
/// in view.
#[test]
fn the_roster_with_the_cursor_on_a_room() {
    let tree = a_team();
    let (mut ui, now) = scene();
    shown(
        &mut ui,
        Open::Switcher(Switcher {
            cursor: Cursor { at: 4 },
            ..Default::default()
        }),
        now,
    );
    let screen = draw_tree(80, 24, &tree, &ui, now);
    assert!(screen.contains("❯ #design"), "{screen}");
    insta::assert_snapshot!("roster_in_the_rooms", screen);
}

/// One list, two doors (§3): `↓` on an empty composer and `ctrl+g` open the
/// same thing, so the two screens are the same screen — not two renderers that
/// happen to agree today.
#[test]
fn the_two_doors_open_byte_identical_lists() {
    let tree = a_team();
    let (mut down, now) = scene();
    let (mut chord, _) = scene();
    crate::input::on_key(&mut down, &tree, key(crossterm::event::KeyCode::Down), now);
    crate::input::on_key(&mut chord, &tree, ctrl('g'), now);
    for (width, height) in [(80u16, 24u16), (120, 40)] {
        assert_eq!(
            draw_tree(width, height, &tree, &down, now),
            draw_tree(width, height, &tree, &chord, now),
            "at {width}x{height}"
        );
    }
    assert!(
        draw_tree(80, 24, &tree, &down, now).contains("❯ ⏺ project"),
        "and it is the list that both of them opened"
    );
}
