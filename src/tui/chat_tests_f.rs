//! Chat state-machine tests, part six: D94's delivery rerouting and the
//! `notify_user` relay.
//!
//! Two halves, matching the batch. The first pins what the hub *stopped*
//! printing: an agent's spawn and completion used to hang a `◉ name · task` row
//! off whatever assistant message happened to be last, and now they do not —
//! while the signal they carried still lands in the lifecycle log, on the bar,
//! and in the agent's own DM. The second is the road that replaced it.
//!
//! The relay's own arithmetic (windows, coalescing, the notifier ceiling) is
//! tested in `crate::notify_user`; these tests are about what the hub does with
//! a notice once the arithmetic has spoken.

use super::tests_a::*;
use super::*;

use crate::agents::AgentKind;
use crate::api::types::{ContentBlock, Message};
use crate::notify_user::{Notice, NotifyLevel};
use crate::tui::buffer::BufferId;
use crate::tui::notify::{Notifier, NotifyChannel, TerminalEnv};
use crate::watch::{WatchKind, WatchState};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn assistant(text: &str) -> Message {
    Message {
        role: crate::api::types::Role::Assistant,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

/// Register a hire on the chat's own session, the way a `Task` spawn would.
fn seed_agent(chat: &Chat, name: &str) {
    chat.session.agents.insert(
        name,
        AgentKind::Hire,
        None,
        "test instance".to_string(),
        chat.session.clone(),
    );
}

fn lifecycle(label: &str, status: WatchState, detail: Option<&str>) -> UiEvent {
    UiEvent::WatchEvent {
        label: label.to_string(),
        kind: WatchKind::Agent,
        status,
        detail: detail.map(str::to_string),
        duration_ms: 0,
        payload: None,
        signal: None,
    }
}

/// The hub as the user sees it: every rendered row, flattened.
fn hub_rows(chat: &mut Chat) -> Vec<String> {
    chat.build_rows(80);
    chat.doc
        .rows
        .iter()
        .map(|r| r.line.plain_text().trim_end().to_string())
        .collect()
}

/// A chat wired to the bell channel, its startup title already drained.
fn chat_with_bell() -> Chat {
    let mut chat = test_chat();
    chat.set_notifier(Notifier::new(NotifyChannel::Bell, &TerminalEnv::default()));
    let _ = chat.notify.take();
    chat
}

fn emitted(chat: &mut Chat) -> String {
    String::from_utf8_lossy(&chat.notify.take()).to_string()
}

// ---------------------------------------------------------------------------
// A. the hub stops being the bus
// ---------------------------------------------------------------------------

/// The inventory this batch was written against: a spawn and a completion
/// arriving while the hub is idle used to add a `◉ scout · fix the parser` row
/// with a `⎿ done` under it, stapled to the last assistant message — a reply
/// that had nothing to do with either event.
#[test]
fn a_spawn_and_a_completion_add_no_rows_to_an_idle_hub() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "have someone look"));
    chat.messages
        .push(msg(Role::Assistant, "I have asked scout"));
    assert!(
        chat.stream_msg.is_none(),
        "the turn is over: what arrives now is the bus, not the conversation"
    );
    let before = hub_rows(&mut chat);

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("done"),
    ));

    assert_eq!(
        hub_rows(&mut chat),
        before,
        "the hub flow is byte-identical: no spawn row, no completion row"
    );
    assert!(
        chat.messages.iter().all(|m| m.activities.is_empty()),
        "and nothing was hung off an older reply"
    );
}

/// The signal is rerouted, not dropped. D95 renders this log as the team
/// directory; until then it is where a hub-idle lifecycle event is written down.
#[test]
fn the_lifecycle_log_keeps_what_the_hub_no_longer_prints() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("fixed the parser"),
    ));

    let log = chat.buffers.team_log();
    assert_eq!(log.len(), 2, "both events were kept");
    assert_eq!(log[0].state, Some(WatchState::Running));
    assert_eq!(log[1].state, Some(WatchState::Done));
    assert_eq!(log[1].detail.as_deref(), Some("fixed the parser"));
}

/// The other half of the reroute: a completed agent's report is in its DM, and
/// the DM says so with an unread badge. Nothing new was built for this — the
/// badge follows the instance's history, and the lifecycle event's own registry
/// sweep is what re-reads it — so the test exists to pin that the chain holds
/// end to end now that the hub prints nothing.
#[test]
fn a_completion_bumps_the_dm_instead_of_the_hub() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    seed_agent(&chat, "scout");
    chat.refresh_conversations();

    let dm = BufferId::Dm("scout".to_string());
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::unread),
        Some(0),
        "a conversation seen for the first time starts read"
    );
    let before = hub_rows(&mut chat);

    // The agent finishes: its reply lands in its history, then the lifecycle
    // event arrives — the order the domain actually produces.
    chat.session
        .agents
        .finish("scout", vec![assistant("the parser is fixed")], 0);
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("done"),
    ));

    assert_eq!(
        hub_rows(&mut chat),
        before,
        "the hub is untouched by someone else's completion"
    );
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::unread),
        Some(1),
        "the report is in the DM, and the DM is what carries the badge"
    );
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::mention),
        Some(true),
        "a DM is addressed to the user by construction"
    );
}

/// The exception, and the reason the rule is about *when* rather than *what*:
/// `Agent` is a hidden tool, so this watch row is the only row the Task call the
/// user just watched the model make will ever have. Inside the turn it is hub
/// content and it stays.
#[test]
fn the_running_turn_keeps_the_row_for_its_own_task_call() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));

    let rows = hub_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("◉ scout · fix the parser")),
        "the turn's own tool row is not bus noise: {rows:?}"
    );
}

/// A background command is the hub's own tool and keeps the walk-back it always
/// had: D94 reroutes agents, and only agents.
#[test]
fn a_command_watch_still_reaches_the_last_reply() {
    let mut chat = test_chat();
    chat.messages
        .push(msg(Role::Assistant, "watching the build"));
    assert!(chat.stream_msg.is_none());

    chat.apply_event(UiEvent::WatchEvent {
        label: "cargo watch".into(),
        kind: WatchKind::Command,
        status: WatchState::Running,
        detail: Some("round 1".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
    });

    let rows = hub_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("⏺ cargo watch")),
        "a command watch is the hub's own tool: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// B. the road that replaced it
// ---------------------------------------------------------------------------

/// An `info` relay: one dim line in the hub, stamped, and no interruption.
#[test]
fn an_info_relay_lands_in_the_hub_with_a_stamp_and_no_bell() {
    let mut chat = chat_with_bell();
    chat.apply_event(UiEvent::NotifyUser(Notice::Relay {
        agent: "scout".into(),
        text: "the migration finished".into(),
        level: NotifyLevel::Info,
        notifier: false,
    }));

    let rows = hub_rows(&mut chat);
    let line = rows
        .iter()
        .find(|r| r.contains("🔔 @scout → you: the migration finished"))
        .unwrap_or_else(|| panic!("the relay line is in the hub: {rows:?}"));
    assert!(
        !line.contains('❯'),
        "a relay is a state line: no bubble putting the agent's words in the user's mouth"
    );
    assert!(
        crate::tui::chat::is_state_line(&chat.messages[0].text),
        "and it is classified as one"
    );
    // D93's stamp convention: beside the message, flush right.
    let stamp = crate::tui::buffer::stamp(chat.messages[0].at);
    assert!(
        line.trim_end().ends_with(&stamp),
        "a relay keeps its stamp — when it arrived is part of what it says: {line:?}"
    );
    assert_eq!(
        emitted(&mut chat),
        "",
        "info waits for the user to look; it does not go and get them"
    );
}

/// `urgent` is the level that reaches a user who is in another window.
#[test]
fn an_urgent_relay_rings_the_attention_channel() {
    let mut chat = chat_with_bell();
    chat.apply_event(UiEvent::NotifyUser(Notice::Relay {
        agent: "scout".into(),
        text: "I need the deploy key".into(),
        level: NotifyLevel::Urgent,
        notifier: true,
    }));

    assert!(
        emitted(&mut chat).contains('\x07'),
        "the bell is the channel every terminal has"
    );
    let rows = hub_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("🔔 @scout → you: I need the deploy key")),
        "and the line is on screen too — the notification carries no detail of its own: {rows:?}"
    );
}

/// The relay's ceiling reaches the notifier as well as the flow: a second urgent
/// notice inside the window still prints, and still does not ring. The decision
/// is the relay's; this pins that the hub honours it rather than re-deciding.
#[test]
fn a_relay_that_lost_the_notifier_ceiling_prints_without_ringing() {
    let mut chat = chat_with_bell();
    chat.apply_event(UiEvent::NotifyUser(Notice::Relay {
        agent: "scout".into(),
        text: "still blocked".into(),
        level: NotifyLevel::Urgent,
        notifier: false,
    }));

    assert_eq!(emitted(&mut chat), "", "the window already rang once");
    let rows = hub_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("🔔 @scout → you: still blocked")),
        "urgent is never coalesced away: {rows:?}"
    );
}

/// What the rolled window owes, rendered.
#[test]
fn a_coalesced_relay_names_the_count_and_points_at_the_dm() {
    let mut chat = test_chat();
    chat.apply_event(UiEvent::NotifyUser(Notice::Coalesced {
        agent: "scout".into(),
        count: 4,
    }));

    let rows = hub_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("🔔 @scout: 4 more — see the DM")),
        "the swallowed notices are accounted for, and the DM is named: {rows:?}"
    );
}

/// A relay arrives whether or not the hub is what the user is looking at, and
/// the badge is how the bar says so. The hub had no unread source before D94 —
/// nothing could reach it that the user had not asked for.
#[test]
fn a_relay_bumps_the_hub_unread_when_the_user_is_elsewhere() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    chat.refresh_conversations();
    chat.switch_to(BufferId::Dm("scout".to_string()));
    assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));

    chat.apply_event(UiEvent::NotifyUser(Notice::Relay {
        agent: "scout".into(),
        text: "the migration finished".into(),
        level: NotifyLevel::Info,
        notifier: false,
    }));

    let hub = chat
        .buffers
        .get(&BufferId::Hub)
        .expect("the hub is always listed");
    assert_eq!(
        hub.unread(),
        1,
        "the hub says something came while you were away"
    );
    assert!(hub.mention(), "a relay is addressed to the user");

    // Going home reads it.
    chat.switch_to(BufferId::Hub);
    assert_eq!(
        chat.buffers
            .get(&BufferId::Hub)
            .map(crate::tui::buffer::Buffer::unread),
        Some(0),
        "entering a conversation reads it"
    );
}

/// The same event with the hub already active never raises a badge for the
/// conversation the user is reading.
#[test]
fn a_relay_read_where_it_lands_raises_no_badge() {
    let mut chat = test_chat();
    assert_eq!(*chat.buffers.active(), BufferId::Hub);

    chat.apply_event(UiEvent::NotifyUser(Notice::Relay {
        agent: "scout".into(),
        text: "the migration finished".into(),
        level: NotifyLevel::Info,
        notifier: false,
    }));

    assert_eq!(
        chat.buffers
            .get(&BufferId::Hub)
            .map(crate::tui::buffer::Buffer::unread),
        Some(0),
        "you are looking at it"
    );
}
