//! Fixtures every test in the crate draws on: a session state built frame by
//! frame, the interactions the dialog answers, and a `TestBackend` that turns
//! one `draw` into a string a snapshot can pin.

use std::time::Instant;

use bingo_sdk::{
    Answer, AnswerSpec, ContentPart, Event, Frame, Interaction, InteractionId, InteractionKind,
    Item, ItemBody, ItemId, ItemStatus, Level, LoginFlow, Origin, ParentLink, Preview, Question,
    QuestionOption, Seq, SessionId, SessionState, SessionSummary, ToolOutput, TurnId, Usage, View,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use jiff::Timestamp;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};

pub use crate::test_lanes::*;

use crate::clock::Now;
use crate::tree::Tree;
use crate::ui::Ui;
use crate::view;

/// The doubles the loop is driven against live next door; every test reaches
/// them through this one import.
pub use crate::doubles::*;

pub fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a fixed instant")
}

pub fn summary() -> SessionSummary {
    SessionSummary {
        tools: None,
        system_extra: None,
        driver: Default::default(),
        id: SessionId::from_raw("ses_1"),
        key: None,
        title: None,
        cwd: "/tmp/project".into(),
        parent: None,
        model: Some("fake-1".into()),
        provider: Some("fake".into()),
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
        messages: None,
    }
}

pub fn state() -> SessionState {
    SessionState::new(summary())
}

pub fn frame(seq: u64, event: Event) -> Frame {
    Frame {
        seq: Seq(seq),
        ts: ts(),
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event,
    }
}

/// The sub-session the root's tool call spawned, as its own frames name it.
pub fn child_id() -> SessionId {
    SessionId::from_raw("ses_2")
}

pub fn child_summary(title: &str) -> SessionSummary {
    SessionSummary {
        id: child_id(),
        title: Some(title.into()),
        parent: Some(ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: Some(ItemId::from_raw("itm_1")),
        }),
        ..summary()
    }
}

/// The frame at the head of a child's stream: who it is and whose it is.
pub fn announced(title: &str) -> Event {
    Event::SessionUpdated {
        summary: child_summary(title),
    }
}

pub fn child_frame(seq: u64, event: Event) -> Frame {
    Frame {
        session: child_id(),
        ..frame(seq, event)
    }
}

/// Another sub-agent of the root, so a tree can hold more than the one
/// [`child_id`] names. `n` is both its id and the seq its frames start at.
pub fn agent_id(n: u64) -> SessionId {
    SessionId::from_raw(format!("ses_{n}"))
}

pub fn agent_summary(n: u64, title: &str) -> SessionSummary {
    SessionSummary {
        id: agent_id(n),
        title: Some(title.into()),
        ..child_summary(title)
    }
}

/// The frame at the head of that agent's stream.
pub fn agent_announced(n: u64, title: &str) -> Event {
    Event::SessionUpdated {
        summary: agent_summary(n, title),
    }
}

pub fn agent_frame(n: u64, seq: u64, event: Event) -> Frame {
    Frame {
        session: agent_id(n),
        ..frame(seq, event)
    }
}

/// A room's whole membership, spelled the way `bingo-rooms` publishes it: the
/// names a reader parses, the tree a surface draws, and beside them the seats
/// whose ear is not the default — a bare name asks for the default and is
/// never listed (ADR-0011 §2, ADR-0013 §2, ADR-0034 §6). A patience of `0` is
/// the live ear `name:0` asks for.
pub fn roster_payload(members: &[&str], listeners: &[(&str, u64)]) -> Value {
    let mut payload = json!({
        "members": members,
        "kind": "tree",
        "nodes": members.iter().map(|name| json!({"label": name, "tone": "neutral"}))
            .collect::<Vec<_>>(),
    });
    if !listeners.is_empty() {
        payload["listeners"] = json!(
            listeners
                .iter()
                .map(|(name, patience)| json!({"name": name, "patience_s": patience}))
                .collect::<Vec<_>>()
        );
    }
    payload
}

/// A seat's reading mark, spelled the way `bingo-rooms` journals one in the
/// *room's* own journal (ADR-0034 §2): the id of the last post it has read,
/// under a kind of the seat's own name, lowercased the way a room compares
/// them. It belongs on a [`log_frame`], never on the member's session.
pub fn room_cursor(member: &str, post: &str) -> Event {
    extended(
        "bingo.rooms",
        &format!("cursor:{}", member.to_lowercase()),
        json!({"post": post}),
    )
}

/// What a room's parent is signalled while any answer is owed (ADR-0022 §4):
/// the two columns a card draws and the debts it is drawn from, oldest first.
/// A debt is given as the minutes it has stood at [`ts`], which is the clock
/// every scene here is drawn against.
pub fn owed_payload(debts: &[(&str, &str, i64)]) -> Value {
    let mut payload = as_payload(View::Table {
        headers: ["room", "owed"].map(str::to_string).to_vec(),
        rows: debts
            .iter()
            .map(|(room, who, _)| vec![room.to_string(), who.to_string()])
            .collect(),
    });
    payload["debts"] = json!(
        debts
            .iter()
            .map(|(room, who, minutes)| json!({
                "room": room,
                "who": who,
                "at": (ts() - jiff::SignedDuration::from_mins(*minutes)).to_string(),
            }))
            .collect::<Vec<_>>()
    );
    payload
}

/// The same card as the process before the debts carried their own stamps
/// published it: three columns, the clock time the question was asked at in
/// the third, and no debts beside them. A shape already in people's journals,
/// so the surface still reads it.
pub fn owed_table_payload(rows: &[(&str, &str, &str)]) -> Value {
    as_payload(View::Table {
        headers: ["room", "owed", "asked"].map(str::to_string).to_vec(),
        rows: rows
            .iter()
            .map(|(room, who, asked)| vec![room.to_string(), who.to_string(), asked.to_string()])
            .collect(),
    })
}

/// A session under the root that this process has not opened: what a
/// `sessions` read hands the switcher for its stored rows.
pub fn stored_summary(id: &str, title: &str) -> SessionSummary {
    SessionSummary {
        id: SessionId::from_raw(id),
        title: Some(title.into()),
        parent: Some(ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: None,
        }),
        ..summary()
    }
}

/// The head frame a reopened session's stream opens with: the summary the
/// listing named it by, now on the tree's own stream (ADR-0010 §3).
pub fn woken(seq: u64, summary: SessionSummary) -> Frame {
    Frame {
        session: summary.id.clone(),
        ..frame(seq, Event::SessionUpdated { summary })
    }
}

/// A room under the same root: a session nothing answers, whose journal is
/// the point (ADR-0011 §1). Its id sorts before the sub-agent's, so a switcher
/// can show it between two model rows.
pub fn log_id() -> SessionId {
    SessionId::from_raw("ses_10")
}

pub fn log_summary(title: &str) -> SessionSummary {
    SessionSummary {
        id: log_id(),
        title: Some(title.into()),
        driver: bingo_sdk::Driver::Log,
        model: None,
        provider: None,
        parent: Some(ParentLink {
            session: SessionId::from_raw("ses_1"),
            item: None,
        }),
        ..summary()
    }
}

/// The frame at the head of a room's stream.
pub fn log_announced(title: &str) -> Event {
    Event::SessionUpdated {
        summary: log_summary(title),
    }
}

pub fn log_frame(seq: u64, event: Event) -> Frame {
    Frame {
        session: log_id(),
        ..frame(seq, event)
    }
}

/// A permission the child raised; the root's handle answers it.
pub fn child_permission() -> Interaction {
    Interaction {
        id: InteractionId::from_raw("int_2"),
        session: child_id(),
        ..permission(Some("Edit(src/)"), None)
    }
}

/// Fold frames into a fresh state, the way every client does.
pub fn folded(frames: Vec<Frame>) -> SessionState {
    let mut state = state();
    for frame in &frames {
        state.apply(frame);
    }
    state
}

pub fn item(id: &str, status: ItemStatus, body: ItemBody) -> Item {
    Item {
        id: ItemId::from_raw(id),
        turn: Some(TurnId::from_raw("trn_1")),
        round: 0,
        status,
        started_at: ts(),
        completed_at: status.is_terminal().then(ts),
        intent: None,
        body,
        meta: Default::default(),
    }
}

pub fn user(id: &str, text: &str) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin::surface("tui"),
        },
    )
}

/// What a subsystem put into a session: a user item stamped with the surface
/// that sent it, the way `bash`, `agent`, `room`, `schedule` and `command` do
/// — and the way a surface nobody has called quiet does too.
pub fn delivered(id: &str, surface: &str, principal: Option<&str>, text: &str) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin {
                surface: surface.into(),
                principal: principal.map(str::to_owned),
                conversation: None,
            },
        },
    )
}

/// What a member posted into a room: a user item that names who wrote it and
/// where, as the room plugin's fan-out stamps it.
pub fn post(id: &str, principal: &str, text: &str) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin {
                surface: "room".into(),
                principal: Some(principal.into()),
                conversation: Some("#design".into()),
            },
        },
    )
}

pub fn assistant(id: &str, text: &str, status: ItemStatus) -> Item {
    item(id, status, ItemBody::Assistant { text: text.into() })
}

pub fn tool(
    id: &str,
    name: &str,
    input: Value,
    output: Option<ToolOutput>,
    status: ItemStatus,
) -> Item {
    item(
        id,
        status,
        ItemBody::ToolCall {
            call_id: "call_1".into(),
            name: name.into(),
            input,
            output,
            progress: None,
            duration_ms: Some(12),
        },
    )
}

pub fn running_tool(id: &str, name: &str, progress: &str) -> Item {
    item(
        id,
        ItemStatus::Running,
        ItemBody::ToolCall {
            call_id: "call_1".into(),
            name: name.into(),
            input: json!({ "command": "cargo test" }),
            output: None,
            progress: Some(progress.into()),
            duration_ms: None,
        },
    )
}

/// A live command with a call id and a command of its own, so a step can hold
/// more than one of them and each row still reads as itself.
pub fn running_command(id: &str, command: &str) -> Item {
    item(
        id,
        ItemStatus::Running,
        ItemBody::ToolCall {
            call_id: format!("call_{id}"),
            name: "Bash".into(),
            input: json!({ "command": command }),
            output: None,
            progress: None,
            duration_ms: None,
        },
    )
}

/// A call an ACP agent ran on its own side, as `bingo-provider-acp` journals
/// one (ADR-0035 §4): a reasoning item whose text is the heading a person
/// reads and whose `acp` provider metadata is the whole call. `acp` is that
/// object, written out at each call site the way the wire spells it — the
/// shape is the contract, and `Call::metadata` in that crate is where it is
/// written and its fixtures are where it is pinned.
pub fn agent_call(id: &str, text: &str, acp: Value) -> Item {
    let Value::Object(map) = acp else {
        panic!("an acp mark is an object");
    };
    let mut call = item(
        id,
        ItemStatus::Completed,
        ItemBody::Reasoning {
            text: text.into(),
            provider_metadata: [("acp".to_string(), map)].into_iter().collect(),
        },
    );
    call.completed_at = Some(ts() + jiff::SignedDuration::from_secs(1));
    call
}

pub fn diff_output() -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::text("wrote src/lib.rs")],
        is_error: false,
        display: Some(View::Diff {
            unified: "@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n ok\n".into(),
        }),
    }
}

pub fn started(turn: &str) -> Event {
    Event::TurnStarted {
        turn: TurnId::from_raw(turn),
        inputs: vec![],
        origin: bingo_sdk::TurnOrigin::Submit,
    }
}

pub fn completed(turn: &str, status: bingo_sdk::TurnStatus) -> Event {
    Event::TurnCompleted {
        turn: TurnId::from_raw(turn),
        status,
        usage: Usage::default(),
    }
}

pub fn interaction(kind: InteractionKind, answers: Vec<AnswerSpec>) -> Interaction {
    Interaction {
        id: InteractionId::from_raw("int_1"),
        session: SessionId::from_raw("ses_1"),
        turn: Some(TurnId::from_raw("trn_1")),
        item: Some(ItemId::from_raw("itm_2")),
        opened_at: ts(),
        guard_until: None,
        expires_at: None,
        kind,
        answers,
    }
}

pub fn permission(scope: Option<&str>, preview: Option<Preview>) -> Interaction {
    let mut answers = vec![AnswerSpec::AllowOnce, AnswerSpec::Deny];
    if scope.is_some() {
        answers.insert(1, AnswerSpec::AllowSession);
    }
    interaction(
        InteractionKind::Permission {
            tool: "Edit".into(),
            summary: "Edit src/lib.rs".into(),
            preview,
            session_scope: scope.map(str::to_owned),
        },
        answers,
    )
}

pub fn long_diff() -> Preview {
    Preview::Diff {
        unified: (0..20).map(|i| format!("+line {i}\n")).collect::<String>(),
    }
}

pub fn question(multi: bool, free_text: bool) -> Interaction {
    let mut answers = vec![AnswerSpec::Choice, AnswerSpec::Cancel];
    if free_text {
        answers.push(AnswerSpec::Text);
    }
    interaction(
        InteractionKind::Question(Question {
            question: "Which provider?".into(),
            header: Some("Auth".into()),
            options: vec![
                QuestionOption {
                    id: "a".into(),
                    label: "Anthropic".into(),
                    description: Some("claude models".into()),
                    role: None,
                    preview: None,
                },
                QuestionOption {
                    id: "o".into(),
                    label: "OpenAI".into(),
                    description: None,
                    role: None,
                    preview: None,
                },
            ],
            free_text,
            multi,
        }),
        answers,
    )
}

pub fn confirm() -> Interaction {
    interaction(
        InteractionKind::Confirm {
            title: "Delete the branch".into(),
            detail: "feature/x has unmerged commits".into(),
        },
        vec![AnswerSpec::Confirm, AnswerSpec::Cancel],
    )
}

/// A provider's sign-in, asked by a holding command rather than a turn
/// (ADR-0012 §5): a paste flow takes words, the others only a way out.
pub fn login(flow: LoginFlow) -> Interaction {
    let answers = match flow {
        LoginFlow::Paste => vec![AnswerSpec::Text, AnswerSpec::Cancel],
        _ => vec![AnswerSpec::Cancel],
    };
    Interaction {
        turn: None,
        item: None,
        ..interaction(
            InteractionKind::Login {
                provider: "codex".into(),
                flow,
            },
            answers,
        )
    }
}

pub fn opened(interaction: Interaction) -> Event {
    Event::InteractionOpened { interaction }
}

pub fn resolved() -> Event {
    Event::InteractionResolved {
        id: InteractionId::from_raw("int_1"),
        answer: Answer::AllowOnce,
        by: bingo_sdk::ResolvedBy::Kernel,
    }
}

/// A plugin publishing the whole of one kind of its state (ADR-0011 §2).
pub fn extended(plugin: &str, kind: &str, payload: Value) -> Event {
    Event::Extension {
        plugin: plugin.into(),
        kind: kind.into(),
        payload,
    }
}

/// The kernel's projection of one plugin's per-session setting (ADR-0009 §5).
pub fn plugin_view(plugin: &str, value: Value) -> Event {
    Event::ConfigChanged {
        config: bingo_sdk::ConfigView {
            plugins: std::collections::BTreeMap::from([(plugin.to_string(), value)]),
            ..Default::default()
        },
    }
}

/// What the permission policy publishes for a session: the mode, the list
/// it may be cycled through, the rules it accepted.
pub fn permission_view(mode: &str) -> Event {
    plugin_view(
        "bingo.permissions",
        json!({
            "mode": mode,
            "modes": ["default", "acceptEdits", "plan", "bypassPermissions", "dontAsk"],
            "rules": [],
        }),
    )
}

/// A session whose policy has published this mode and nothing else.
pub fn with_permission_mode(mode: &str) -> SessionState {
    folded(vec![frame(1, permission_view(mode))])
}

pub fn notice(level: Level, text: &str) -> Event {
    Event::Notice {
        level,
        code: "TEST".into(),
        text: text.into(),
    }
}

/// A transcript with more lines than any test screen has rows.
pub fn long_transcript(items: usize) -> SessionState {
    let mut state = state();
    state.items = (0..items)
        .map(|i| user(&format!("itm_{i}"), &format!("line {i}")))
        .collect();
    state
}

/// A `Ui` and the instant it was born, so a test can move time by hand.
pub fn scene() -> (Ui, Now) {
    let instant = Instant::now();
    (
        Ui::new(Vec::new(), instant),
        Now {
            instant,
            wall: ts(),
            motion: true,
        },
    )
}

/// The same scene with the motion switched off: what `BINGO_MOTION=off`
/// draws.
pub fn still(now: Now) -> Now {
    Now {
        motion: false,
        ..now
    }
}

/// A scene a turn has been running in for a second and a half: past the
/// activity row's delay, and on the turn of every cycle §6 names — so a cue
/// sampled from here starts at the beginning of itself.
pub fn mid_turn() -> (Ui, Now) {
    let (ui, now) = scene();
    (ui, later(now, 1_600))
}

/// A scene whose wall clock is far enough past [`ts`] for a card the kernel
/// opened at `ts` to have finished arriving: the settled screen.
pub fn settled() -> (Ui, Now) {
    let (ui, now) = scene();
    (
        ui,
        Now {
            wall: now.wall + jiff::SignedDuration::from_millis(200),
            ..now
        },
    )
}

/// Open a layer that has finished arriving: what a settled sheet or switcher
/// looks like, rather than the first frame of its slide.
pub fn shown(ui: &mut Ui, open: crate::ui::Open, now: Now) {
    ui.layer
        .show(open, now.instant - std::time::Duration::from_millis(500));
}

/// A synthetic mouse event at a cell of the screen.
pub fn mouse(kind: crossterm::event::MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub fn click(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn dragged(column: u16, row: u16) -> MouseEvent {
    mouse(
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column,
        row,
    )
}

pub fn wheel(up: bool, column: u16, row: u16) -> MouseEvent {
    let kind = match up {
        true => crossterm::event::MouseEventKind::ScrollUp,
        false => crossterm::event::MouseEventKind::ScrollDown,
    };
    mouse(kind, column, row)
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub fn typed(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

pub fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

pub fn alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::ALT)
}

/// What a terminal sends for shift+tab: `BackTab`, with the modifier set.
pub fn shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

/// A tree of one session, which is what most of these tests are about.
pub fn solo(state: &SessionState) -> Tree {
    Tree::new(state.clone())
}

/// Fold frames into a fresh tree, routed by `frame.session` the way the loop
/// does; a child joins on the `SessionUpdated` at the head of its stream.
pub fn folded_tree(frames: Vec<Frame>) -> Tree {
    let mut tree = Tree::new(state());
    for frame in &frames {
        tree.apply(frame);
    }
    tree
}

/// Type a whole line, one key at a time, through the real handler.
pub fn write(ui: &mut Ui, state: &SessionState, text: &str, now: Now) {
    let tree = solo(state);
    for c in text.chars() {
        crate::input::on_key(ui, &tree, typed(c), now);
    }
}

/// One frame, rendered into a fixed-size buffer, as text.
pub fn render(state: &SessionState, ui: &Ui, now: Now) -> String {
    draw_sized(80, 24, state, ui, now)
}

pub fn draw_sized(width: u16, height: u16, state: &SessionState, ui: &Ui, now: Now) -> String {
    draw_tree(width, height, &solo(state), ui, now)
}

pub fn render_tree(tree: &Tree, ui: &Ui, now: Now) -> String {
    draw_tree(80, 24, tree, ui, now)
}

pub fn draw_tree(width: u16, height: u16, tree: &Tree, ui: &Ui, now: Now) -> String {
    drawn(width, height, tree, ui, now).to_string()
}

/// The screen row carrying `needle`, for a test that puts the pointer on the
/// text a person sees rather than on a row counted by hand.
pub fn row_carrying(screen: &str, needle: &str) -> u16 {
    let row = screen
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no row carries {needle:?}:\n{screen}"));
    u16::try_from(row).expect("a row of the screen")
}

/// The terminal one draw leaves, for a test that asks where a style landed.
pub fn drawn(width: u16, height: u16, tree: &Tree, ui: &Ui, now: Now) -> TestBackend {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| view::draw(tree, ui, frame, now))
        .expect("a drawn frame");
    terminal.backend().clone()
}

/// The same frame, `ms` further along. Both clocks move together, as they do
/// in a terminal: the wall says how far a card the kernel opened has come, the
/// instant how far a breath has.
pub fn later(now: Now, ms: i64) -> Now {
    Now {
        wall: now.wall + jiff::SignedDuration::from_millis(ms),
        instant: now.instant + std::time::Duration::from_millis(ms.unsigned_abs()),
        ..now
    }
}

/// The frame `frames` steps of motion further along (§6: 33 ms each).
pub fn frames_at(now: Now, frames: u32) -> Now {
    later(
        now,
        i64::from(frames) * crate::clock::FRAME.as_millis() as i64,
    )
}

// ---- M11b: the fixtures the screens are built from --------------------

/// A tool call whose gate a person answered.
pub fn receipt_item(
    id: &str,
    tool: &str,
    decision: bingo_sdk::DecisionKind,
    feedback: Option<&str>,
) -> Item {
    item(
        id,
        ItemStatus::Completed,
        ItemBody::PermissionReceipt {
            interaction: InteractionId::from_raw("int_1"),
            tool: tool.into(),
            decision,
            feedback: feedback.map(str::to_owned),
        },
    )
}

/// The frame that puts a tool on screen while it is still running.
pub fn started_tool(seq: u64, item: Item) -> Frame {
    frame(seq, Event::ItemStarted { item })
}

/// A transcript whose tool call spawned a sub-session, and that child's own
/// frames after it, in the order one stream delivers them.
pub fn spawned_tree(child: Vec<Frame>) -> Tree {
    let mut frames = vec![
        frame(
            1,
            Event::ItemCompleted {
                item: user("itm_0", "have it reviewed"),
            },
        ),
        frame(
            2,
            Event::ItemCompleted {
                item: tool(
                    "itm_1",
                    "SpawnAgent",
                    json!({"prompt": "review the diff"}),
                    Some(ToolOutput::text("reviewer started")),
                    ItemStatus::Completed,
                ),
            },
        ),
    ];
    frames.extend(child);
    folded_tree(frames)
}

/// A room under the root, with the room in view: what a member of it sees.
pub fn room_tree(frames: Vec<Frame>) -> Tree {
    let mut all = vec![log_frame(1, log_announced("#design"))];
    all.extend(frames);
    let mut tree = folded_tree(all);
    tree.show(&log_id());
    tree
}

/// What a member posted into a room, as a frame of its stream.
pub fn posted(seq: u64, id: &str, principal: &str, text: &str) -> Frame {
    log_frame(
        seq,
        Event::ItemCompleted {
            item: post(id, principal, text),
        },
    )
}

/// A sub-session that has run a while: three tool calls and some tokens
/// spent, which is what its row in the parent's transcript reports.
pub fn busy_child(title: &str) -> Vec<Frame> {
    let mut summary = child_summary(title);
    summary.usage = Usage {
        input_tokens: 1_100,
        output_tokens: 140,
        ..Usage::default()
    };
    let mut frames = vec![
        child_frame(1, Event::SessionUpdated { summary }),
        child_frame(2, started("trn_9")),
    ];
    for i in 1..=3 {
        frames.push(child_frame(
            2 + i,
            Event::ItemCompleted {
                item: tool(
                    &format!("itm_{i}"),
                    "Read",
                    json!({ "file_path": "src/lib.rs" }),
                    Some(ToolOutput::text("Read 3 lines")),
                    ItemStatus::Completed,
                ),
            },
        ));
    }
    frames
}
