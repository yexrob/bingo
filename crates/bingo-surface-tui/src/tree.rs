//! The sessions one attachment carries: the root this surface opened with
//! `OpenOptions::with_children()` and every live descendant whose frames the
//! same stream delivers, each stamped with its own `session` (ADR-0010 §3).
//!
//! Every state here is the reducer's — one `SessionState` per session, folded
//! by `frame.session`. What is the surface's own is which of them is on
//! screen. Names, tallies, the `↳` rows and the switcher are derived from
//! these states at render time, so nothing about a child is stored twice.
//!
//! The tree holds live descendants only. The switcher lists more than that:
//! [`roster`] merges these states with a listing the host answered with, so
//! what an earlier process spawned is visible and can be chosen — a row, not
//! a state, and the live one always wins.

use std::collections::BTreeMap;

use bingo_sdk::{
    Applied, Driver, Event, Frame, Interaction, ItemBody, ItemId, SessionId, SessionState,
    SessionSummary,
};

use crate::theme;

/// The child a tool call spawned, by the item that called it. The child's own
/// state is the whole of the row, so nothing about it is stored twice.
pub type Agents<'a> = BTreeMap<ItemId, &'a SessionState>;

/// What a session is doing, as a person reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    /// Idle, having finished at least one turn.
    Done,
    /// Idle, and it has not run yet.
    Idle,
    /// Not open in this process: in the store, and reopened by choosing it.
    Stored,
}

impl Status {
    /// What the session is doing, or nothing at all: a `Log` session has no
    /// model behind it, so there is no work to report (ADR-0011 §1).
    pub fn of(state: &SessionState) -> Option<Self> {
        if state.summary.driver == Driver::Log {
            return None;
        }
        if state.busy() {
            Some(Status::Running)
        } else if state.last_turn.is_some() {
            Some(Status::Done)
        } else {
            Some(Status::Idle)
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Idle => "idle",
            Status::Stored => "stored",
        }
    }
}

/// One row of the switcher, derived from a session in the tree or from what
/// the store answered with.
#[derive(Debug)]
pub struct Row<'a> {
    pub session: &'a SessionId,
    pub name: String,
    /// Absent for a session nothing answers.
    pub status: Option<Status>,
    pub attention: bool,
}

pub struct Tree {
    /// The session the surface attached to; its stream carries the tree.
    root: SessionState,
    /// Every live descendant, in id order.
    children: BTreeMap<SessionId, SessionState>,
    /// Which session `draw` paints; `None` is the root.
    view: Option<SessionId>,
}

impl Tree {
    pub fn new(root: SessionState) -> Self {
        Self {
            root,
            children: BTreeMap::new(),
            view: None,
        }
    }

    pub fn root(&self) -> &SessionState {
        &self.root
    }

    pub fn root_id(&self) -> &SessionId {
        &self.root.summary.id
    }

    /// The state `draw` paints.
    pub fn viewed(&self) -> &SessionState {
        self.viewing().unwrap_or(&self.root)
    }

    /// The child on screen, or `None` while the root is.
    pub fn viewing(&self) -> Option<&SessionState> {
        self.view.as_ref().and_then(|id| self.children.get(id))
    }

    /// The session the keyboard writes to: the one a person stepped into,
    /// even while its head frames are still on their way. A line typed at a
    /// session that is still opening is refused, never sent to the root.
    pub fn view(&self) -> &SessionId {
        self.view.as_ref().unwrap_or_else(|| self.root_id())
    }

    pub fn is_root(&self, session: &SessionId) -> bool {
        session == self.root_id()
    }

    /// Root first, then the children in id order.
    pub fn sessions(&self) -> impl Iterator<Item = &SessionState> {
        std::iter::once(&self.root).chain(self.children.values())
    }

    /// The state of one session this attachment carries. A row the store
    /// answered with names a session that is not here, and gets `None`.
    pub fn state(&self, session: &SessionId) -> Option<&SessionState> {
        self.sessions().find(|state| &state.summary.id == session)
    }

    /// Fold a frame into the session it names. A child announces itself with
    /// the `SessionUpdated` at the head of its stream; anything else from a
    /// session this tree has never heard of has nothing to fold into.
    pub fn apply(&mut self, frame: &Frame) -> Applied {
        if let Some(state) = self.state_mut(&frame.session) {
            return state.apply(frame);
        }
        let Event::SessionUpdated { summary } = &frame.event else {
            return Applied::Stale;
        };
        self.children
            .entry(frame.session.clone())
            .or_insert_with(|| SessionState::new(summary.clone()))
            .apply(frame)
    }

    fn state_mut(&mut self, session: &SessionId) -> Option<&mut SessionState> {
        if self.is_root(session) {
            return Some(&mut self.root);
        }
        self.children.get_mut(session)
    }

    /// A child that closed leaves the tree, and the view with it.
    pub fn close(&mut self, session: &SessionId) {
        self.children.remove(session);
        if self.view.as_ref() == Some(session) {
            self.view = None;
        }
    }

    /// Paint this session from now on. One the tree has not heard of is
    /// remembered all the same: the root stays on the screen until that
    /// session's head frames arrive, and then it is what is painted. That is
    /// how a stored child, reopened by being chosen, turns live in place.
    pub fn show(&mut self, session: &SessionId) {
        self.view = Some(session.clone());
    }

    /// The person is looking at this one.
    pub fn mark_read(&mut self) {
        match self.view.clone().and_then(|id| self.children.get_mut(&id)) {
            Some(child) => child.mark_read(),
            None => self.root.mark_read(),
        }
    }

    /// Something, anywhere in the tree, needs a person.
    pub fn attention(&self) -> bool {
        self.sessions().any(SessionState::attention)
    }

    /// The interaction the dialog answers: the root's first, then the
    /// children's in id order, so one prompt is on screen at a time. The
    /// handle routes the answer back to whichever of them asked.
    pub fn open_interaction(&self) -> Option<(&SessionState, &Interaction)> {
        self.sessions()
            .find_map(|state| state.interactions.first().map(|open| (state, open)))
    }

    /// The children the viewed session spawned, by the tool call that did it;
    /// a child no call spawned hangs under no row.
    pub fn agents(&self) -> Agents<'_> {
        self.children
            .values()
            .filter_map(|child| {
                let parent = child.summary.parent.as_ref()?;
                let item = parent.item.clone()?;
                (&parent.session == self.view()).then_some((item, child))
            })
            .collect()
    }

    /// The session a tool call of the viewed transcript spawned: what `⏎` or
    /// a click on its row steps into.
    pub fn spawned_by(&self, item: &ItemId) -> Option<&SessionId> {
        self.agents().get(item).map(|child| &child.summary.id)
    }

    /// The switcher's rows, in the order it lists them.
    pub fn rows(&self) -> Vec<Row<'_>> {
        self.sessions()
            .map(|state| Row {
                session: &state.summary.id,
                name: name(state),
                status: Status::of(state),
                attention: asking(state),
            })
            .collect()
    }
}

/// The switcher's rows: what this attachment carries, then the root's stored
/// descendants that are not among them. Live wins — a session that is both
/// live and stored is one row, and it is the live one.
pub fn roster<'a>(tree: &'a Tree, stored: &'a [SessionSummary]) -> Vec<Row<'a>> {
    let mut rows = tree.rows();
    let live: Vec<SessionId> = rows.iter().map(|row| row.session.clone()).collect();
    let mut asleep: Vec<Row<'a>> = descendants(tree.root_id(), stored)
        .into_iter()
        .filter(|summary| !live.contains(&summary.id))
        .map(stored_row)
        .collect();
    asleep.sort_by(|a, b| a.session.cmp(b.session));
    rows.append(&mut asleep);
    rows
}

/// The listed sessions whose parent chain reaches `root`. The listing is the
/// only map there is, so the walk goes no further than it is long — which is
/// also what stops a chain that points at itself.
fn descendants<'a>(root: &SessionId, listed: &'a [SessionSummary]) -> Vec<&'a SessionSummary> {
    listed
        .iter()
        .filter(|summary| reaches(root, summary, listed))
        .collect()
}

fn reaches(root: &SessionId, summary: &SessionSummary, listed: &[SessionSummary]) -> bool {
    let of = |summary: &SessionSummary| summary.parent.as_ref().map(|link| link.session.clone());
    let mut parent = of(summary);
    for _ in 0..listed.len() {
        match parent {
            Some(id) if &id == root => return true,
            Some(id) => parent = listed.iter().find(|s| s.id == id).and_then(of),
            None => return false,
        }
    }
    false
}

/// A session this process has not opened: what it is called, and that it is
/// not here. What it was doing is the journal's business, not a row's — and a
/// room answers nothing whether it is here or not, so it reports no status
/// either way and the list puts it where every other room goes.
fn stored_row(summary: &SessionSummary) -> Row<'_> {
    Row {
        session: &summary.id,
        name: name_of(summary),
        status: (summary.driver != Driver::Log).then_some(Status::Stored),
        attention: false,
    }
}

/// What a person calls a session: the title a plugin gave it — a sub-agent's
/// name — else the directory it works in.
pub fn name(state: &SessionState) -> String {
    name_of(&state.summary)
}

fn name_of(summary: &SessionSummary) -> String {
    summary
        .title
        .clone()
        .unwrap_or_else(|| directory(&summary.cwd))
}

/// The last segment of a path.
pub fn directory(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

/// A session with a question open wants a person now. One that finished while
/// you were looking elsewhere is unread, which is not the same thing —
/// `SessionState::attention` counts both, and the window title wants that
/// wider sense, while a row that says `Needs you` means this one.
pub fn asking(state: &SessionState) -> bool {
    !state.interactions.is_empty()
}

/// What a child's row says under it (design §4): what it is doing and what it
/// has spent doing it. Nothing for a session nothing answers — a room is not
/// at work, so it reports no work.
pub fn activity(state: &SessionState) -> Option<String> {
    if asking(state) {
        return Some("Needs you".to_string());
    }
    let spent = spent(state);
    match Status::of(state)? {
        Status::Running => Some(format!("Running{} {spent}", theme::ellipsis())),
        Status::Done => Some(format!("Done ({spent} · {}s)", seconds(state))),
        Status::Idle => Some(format!("Starting{}", theme::ellipsis())),
        // A row under a transcript is a session this attachment carries;
        // `Status::of` reads a live state and never answers this.
        Status::Stored => None,
    }
}

/// The first thing a session was asked, as a row says it: the opening line of
/// the first thing put to it — a person's own words at the root, the prompt a
/// spawn carried in a sub-agent. A session nobody has asked anything says
/// nothing rather than a placeholder.
pub fn brief(state: &SessionState) -> Option<String> {
    state.items.iter().find_map(|item| match &item.body {
        ItemBody::User { parts, .. } => opening(parts),
        _ => None,
    })
}

/// The first line with something on it, of everything a message said in text.
fn opening(parts: &[bingo_sdk::ContentPart]) -> Option<String> {
    parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// `3 tools · 1.2k tokens`.
pub fn spent(state: &SessionState) -> String {
    let tools = state
        .items
        .iter()
        .filter(|item| matches!(item.body, ItemBody::ToolCall { .. }))
        .count();
    let usage = &state.summary.usage;
    format!(
        "{tools} tool{} · {} tokens",
        if tools == 1 { "" } else { "s" },
        thousands(usage.input_total() + usage.output_tokens)
    )
}

/// How long it has been at it: the first item it started to the last it
/// finished, which is the only clock a client has for somebody else's work.
pub fn seconds(state: &SessionState) -> i64 {
    let first = state.items.first().map(|item| item.started_at);
    let last = state
        .items
        .iter()
        .filter_map(|item| item.completed_at)
        .max();
    match (first, last) {
        (Some(first), Some(last)) => last.duration_since(first).as_secs().max(0),
        _ => 0,
    }
}

fn thousands(n: u64) -> String {
    match n < 1_000 {
        true => n.to_string(),
        false => format!("{:.1}k", n as f64 / 1000.0),
    }
}

/// A session's bullet takes the colour a tool row in the same state takes, and
/// one that wants a person takes bingo's own colour wherever it is drawn.
pub fn bullet_style(status: Option<Status>, attention: bool) -> ratatui::style::Style {
    if attention {
        return theme::presence();
    }
    match status {
        Some(Status::Running) => theme::presence(),
        Some(Status::Done) => theme::good(),
        Some(Status::Idle) | Some(Status::Stored) | None => theme::dim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn a_child_joins_the_tree_on_the_session_frame_at_the_head_of_its_stream() {
        let mut tree = Tree::new(state());
        assert_eq!(
            tree.apply(&child_frame(1, started("trn_9"))),
            Applied::Stale,
            "a session that never introduced itself has nothing to fold into"
        );
        assert_eq!(tree.rows().len(), 1);

        tree.apply(&child_frame(1, announced("reviewer")));
        tree.apply(&child_frame(2, started("trn_9")));
        let rows = tree.rows();
        assert_eq!(rows[1].name, "reviewer");
        assert_eq!(rows[1].status, Some(Status::Running));
        assert_eq!(rows[0].name, "project", "the root is named by its cwd");
    }

    #[test]
    fn the_view_is_a_key_into_the_tree_and_falls_back_to_the_root() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        tree.show(&child_id());
        assert_eq!(tree.view(), &child_id());
        assert_eq!(tree.viewing().map(name).as_deref(), Some("reviewer"));

        tree.close(&child_id());
        assert!(
            tree.viewing().is_none(),
            "a closed child gives the view back"
        );
        assert_eq!(tree.view(), tree.root_id());
        assert_eq!(tree.rows().len(), 1, "and the tree is the root alone");
    }

    /// A session chosen before its head frames arrive — a stored child being
    /// reopened. The root stays on the screen, but the keyboard already
    /// belongs to the one that was chosen, so a line typed meanwhile is
    /// refused rather than sent to the root.
    #[test]
    fn a_session_the_tree_has_not_heard_of_is_waited_for_not_forgotten() {
        let mut tree = Tree::new(state());
        tree.show(&child_id());
        assert_eq!(&tree.viewed().summary.id, tree.root_id());
        assert!(tree.viewing().is_none(), "there is nothing to paint yet");
        assert_eq!(tree.view(), &child_id(), "and the keyboard is already its");

        tree.apply(&child_frame(1, announced("reviewer")));
        assert_eq!(
            tree.viewing().map(name).as_deref(),
            Some("reviewer"),
            "the row turns live in place"
        );
    }

    #[test]
    fn the_tally_counts_who_is_waiting_and_the_dialog_takes_the_root_first() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        tree.apply(&child_frame(2, opened(child_permission())));
        assert_eq!(tree.rows().iter().filter(|row| row.attention).count(), 1);
        assert!(tree.attention());
        let (owner, _) = tree.open_interaction().expect("the child's prompt");
        assert_eq!(owner.summary.id, child_id());

        tree.apply(&frame(1, opened(permission(None, None))));
        let (owner, _) = tree.open_interaction().expect("the root's prompt");
        assert_eq!(
            owner.summary.id,
            tree.root_id().clone(),
            "the root is first"
        );
    }

    #[test]
    fn a_child_is_found_by_the_tool_call_that_spawned_it() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        let agents = tree.agents();
        let spawned = ItemId::from_raw("itm_1");
        assert_eq!(
            agents.get(&spawned).map(|child| name(child)),
            Some("reviewer".to_string())
        );
        assert_eq!(tree.spawned_by(&spawned), Some(&child_id()));
        assert_eq!(
            agents.get(&spawned).and_then(|child| activity(child)),
            Some("Starting…".to_string())
        );

        tree.show(&child_id());
        assert!(
            tree.agents().is_empty(),
            "the rows belong to the transcript that spawned them"
        );
    }

    #[test]
    fn a_session_nothing_answers_reports_no_status_at_all() {
        let mut tree = Tree::new(state());
        tree.apply(&log_frame(1, log_announced("#design")));
        tree.apply(&log_frame(
            2,
            Event::ItemCompleted {
                item: post("itm_5", "reviewer", "shipped"),
            },
        ));
        let room = tree.rows().pop().expect("the room's row");
        assert_eq!(room.name, "#design");
        assert_eq!(room.status, None);
        assert!(Status::of(tree.sessions().last().expect("the room")).is_none());
    }

    #[test]
    fn the_row_under_a_call_that_opened_a_room_is_its_name_alone() {
        let mut tree = Tree::new(state());
        let mut summary = log_summary("#design");
        summary.parent = Some(bingo_sdk::ParentLink {
            session: tree.root_id().clone(),
            item: Some(ItemId::from_raw("itm_1")),
        });
        tree.apply(&log_frame(1, Event::SessionUpdated { summary }));
        let agents = tree.agents();
        let opened = ItemId::from_raw("itm_1");
        assert_eq!(
            agents.get(&opened).map(|room| name(room)),
            Some("#design".to_string())
        );
        assert_eq!(
            agents.get(&opened).and_then(|room| activity(room)),
            None,
            "a room is not at work, so its row reports none"
        );
    }

    #[test]
    fn a_child_row_says_what_it_is_doing_and_what_it_has_spent() {
        let mut tree = Tree::new(state());
        for frame in busy_child("reviewer") {
            tree.apply(&frame);
        }
        let running = tree.children.values().next().expect("the child");
        assert_eq!(
            activity(running).as_deref(),
            Some("Running… 3 tools · 1.2k tokens")
        );

        tree.apply(&child_frame(
            6,
            completed("trn_9", bingo_sdk::TurnStatus::Completed),
        ));
        let done = tree.children.values().next().expect("the child");
        assert_eq!(
            activity(done).as_deref(),
            Some("Done (3 tools · 1.2k tokens · 0s)")
        );

        tree.apply(&child_frame(7, opened(child_permission())));
        let asking = tree.children.values().next().expect("the child");
        assert_eq!(activity(asking).as_deref(), Some("Needs you"));
    }

    /// What a row says of a session that has not run yet: the thing it was
    /// asked, which is the whole of what there is to know about it.
    #[test]
    fn the_brief_is_the_first_line_of_the_first_thing_a_session_was_asked() {
        let asked = folded(vec![
            frame(
                1,
                Event::ItemCompleted {
                    item: user("itm_1", "\n  what is in this workspace?\nand why\n"),
                },
            ),
            frame(
                2,
                Event::ItemCompleted {
                    item: user("itm_2", "write me a note"),
                },
            ),
        ]);
        assert_eq!(
            brief(&asked).as_deref(),
            Some("what is in this workspace?"),
            "the first ask, and one line of it"
        );
        assert_eq!(brief(&state()), None, "a session nobody has asked anything");
    }

    #[test]
    fn the_rows_are_the_root_first_and_the_rest_in_id_order() {
        let mut tree = Tree::new(state());
        for frame in busy_child("reviewer") {
            tree.apply(&frame);
        }
        tree.apply(&log_frame(9, log_announced("#design")));
        let named: Vec<(String, Option<Status>)> = tree
            .rows()
            .into_iter()
            .map(|row| (row.name, row.status))
            .collect();
        assert_eq!(
            named,
            vec![
                ("project".to_string(), Some(Status::Idle)),
                ("#design".to_string(), None),
                ("reviewer".to_string(), Some(Status::Running)),
            ]
        );
    }

    // ---- the merged roster (M31) ----------------------------------------

    /// The order is the pin: what this attachment carries first, in the order
    /// it already lists it, then the stored rows by id after them.
    #[test]
    fn the_roster_puts_the_tree_first_and_the_stored_rows_after_it_by_id() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        let stored = vec![
            stored_summary("ses_9", "scout"),
            stored_summary("ses_3", "archivist"),
        ];
        let rows = roster(&tree, &stored);
        assert_eq!(
            rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>(),
            ["project", "reviewer", "archivist", "scout"]
        );
        assert_eq!(rows[2].status, Some(Status::Stored));
        assert_eq!(rows[3].session, &SessionId::from_raw("ses_9"));
        assert!(
            !rows[2].attention,
            "a session that is not here asks nothing"
        );
    }

    /// One session, one row. A listing carries the live ones too, and the
    /// live one is the truth: the stored copy of it is dropped, not shown.
    #[test]
    fn a_session_that_is_both_live_and_stored_is_one_row_and_the_live_one() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        tree.apply(&child_frame(2, started("trn_9")));
        let stored = vec![child_summary("reviewer"), summary()];
        let rows = roster(&tree, &stored);
        assert_eq!(rows.len(), 2, "the root and the child, once each");
        assert_eq!(rows[1].status, Some(Status::Running));
    }

    /// Only what hangs under this root, however deep: a listing is every
    /// session the host knows of, and most of them are somebody else's.
    #[test]
    fn the_roster_takes_only_the_sessions_whose_parent_chain_reaches_the_root() {
        let tree = Tree::new(state());
        let grandchild = SessionSummary {
            parent: Some(bingo_sdk::ParentLink {
                session: SessionId::from_raw("ses_3"),
                item: None,
            }),
            ..stored_summary("ses_4", "runner")
        };
        let elsewhere = SessionSummary {
            parent: None,
            ..stored_summary("ses_8", "another root")
        };
        let orphan = SessionSummary {
            parent: Some(bingo_sdk::ParentLink {
                session: SessionId::from_raw("ses_8"),
                item: None,
            }),
            ..stored_summary("ses_9", "somebody else's")
        };
        let stored = vec![
            stored_summary("ses_3", "archivist"),
            grandchild,
            elsewhere,
            orphan,
        ];
        let rows = roster(&tree, &stored);
        assert_eq!(
            rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>(),
            ["project", "archivist", "runner"],
            "the chain reaches the root through the child"
        );
    }

    /// A listing whose parent links point in a circle must not spin.
    #[test]
    fn a_chain_that_never_reaches_the_root_ends_all_the_same() {
        let tree = Tree::new(state());
        let link = |id: &str, parent: &str| SessionSummary {
            parent: Some(bingo_sdk::ParentLink {
                session: SessionId::from_raw(parent),
                item: None,
            }),
            ..stored_summary(id, id)
        };
        let circle = vec![link("ses_5", "ses_6"), link("ses_6", "ses_5")];
        let rows = roster(&tree, &circle);
        assert_eq!(rows.len(), 1, "the root alone: {rows:?}");
    }

    #[test]
    fn a_stored_row_is_dim_and_a_stored_room_still_answers_nothing() {
        let tree = Tree::new(state());
        let stored = vec![
            stored_summary("ses_3", "archivist"),
            SessionSummary {
                driver: Driver::Log,
                ..stored_summary("ses_4", "#design")
            },
        ];
        let rows = roster(&tree, &stored);
        assert_eq!(rows[1].status, Some(Status::Stored));
        assert_eq!(
            rows[2].status, None,
            "a room answers nothing whether it is here or not, so the list \
             puts it where every other room goes"
        );
        assert_eq!(bullet_style(Some(Status::Stored), false), theme::dim());
    }
}
