//! The sessions one attachment carries: the root this surface opened with
//! `OpenOptions::with_children()` and every live descendant whose frames the
//! same stream delivers, each stamped with its own `session` (ADR-0010 §3).
//!
//! Every state here is the reducer's — one `SessionState` per session, folded
//! by `frame.session`. What is the surface's own is which of them is on
//! screen. Names, tallies, the `↳` rows and the switcher are derived from
//! these states at render time, so nothing about a child is stored twice.

use std::collections::BTreeMap;

use bingo_sdk::{Applied, Driver, Event, Frame, Interaction, ItemId, SessionId, SessionState};

/// The `↳` label of the child a tool call spawned, by the item that called it.
pub type Agents = BTreeMap<ItemId, String>;

/// What a session is doing, as a person reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    /// Idle, having finished at least one turn.
    Done,
    /// Idle, and it has not run yet.
    Idle,
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
        }
    }
}

/// The ` · running` a name is followed by, and nothing for a session nothing
/// answers: one place decides it, so no view has to ask about the driver.
pub fn suffix(status: Option<Status>) -> String {
    status
        .map(|status| format!(" · {}", status.label()))
        .unwrap_or_default()
}

pub fn status_suffix(state: &SessionState) -> String {
    suffix(Status::of(state))
}

/// One row of the switcher, derived from a session in the tree.
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

    /// The session the keyboard writes to.
    pub fn view(&self) -> &SessionId {
        &self.viewed().summary.id
    }

    pub fn is_root(&self, session: &SessionId) -> bool {
        session == self.root_id()
    }

    /// Root first, then the children in id order.
    pub fn sessions(&self) -> impl Iterator<Item = &SessionState> {
        std::iter::once(&self.root).chain(self.children.values())
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

    /// Paint this session from now on; an unknown one is the root.
    pub fn show(&mut self, session: &SessionId) {
        self.view = self.children.contains_key(session).then(|| session.clone());
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

    /// `2 agents · 1 needs you`, and nothing while the tree is only the root.
    /// A session nothing answers is doing no work, so it is nobody's count.
    pub fn tally(&self) -> Option<String> {
        let agents = self
            .children
            .values()
            .filter(|child| Status::of(child).is_some())
            .count();
        if agents == 0 {
            return None;
        }
        let noun = if agents == 1 { "agent" } else { "agents" };
        let waiting = self.children.values().filter(|c| c.attention()).count();
        let mut out = format!("{agents} {noun}");
        if waiting > 0 {
            out.push_str(&format!(" · {waiting} needs you"));
        }
        Some(out)
    }

    /// The children the viewed session spawned, by the tool call that did it;
    /// a child no call spawned hangs under no row.
    pub fn agents(&self) -> Agents {
        self.children
            .values()
            .filter_map(|child| {
                let parent = child.summary.parent.as_ref()?;
                let item = parent.item.clone()?;
                (&parent.session == self.view()).then(|| (item, label(child)))
            })
            .collect()
    }

    /// The switcher's rows, in the order it lists them.
    pub fn rows(&self) -> Vec<Row<'_>> {
        self.sessions()
            .map(|state| Row {
                session: &state.summary.id,
                name: name(state),
                status: Status::of(state),
                attention: state.attention(),
            })
            .collect()
    }
}

/// What a person calls a session: the title a plugin gave it — a sub-agent's
/// name — else the directory it works in.
pub fn name(state: &SessionState) -> String {
    state
        .summary
        .title
        .clone()
        .unwrap_or_else(|| directory(&state.summary.cwd))
}

/// The last segment of a path.
pub fn directory(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

fn label(state: &SessionState) -> String {
    format!("{}{}", name(state), status_suffix(state))
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
        assert_eq!(tree.tally().as_deref(), Some("1 agent"));
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
        assert!(tree.tally().is_none());
    }

    #[test]
    fn an_unknown_session_is_never_shown() {
        let mut tree = Tree::new(state());
        tree.show(&child_id());
        assert_eq!(tree.view(), tree.root_id());
    }

    #[test]
    fn the_tally_counts_who_is_waiting_and_the_dialog_takes_the_root_first() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        tree.apply(&child_frame(2, opened(child_permission())));
        assert_eq!(tree.tally().as_deref(), Some("1 agent · 1 needs you"));
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
        assert_eq!(
            agents
                .get(&bingo_sdk::ItemId::from_raw("itm_1"))
                .map(String::as_str),
            Some("reviewer · idle")
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
        assert_eq!(status_suffix(tree.sessions().last().expect("the room")), "");
        assert!(tree.tally().is_none(), "a room is not an agent at work");
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
        assert_eq!(
            tree.agents()
                .get(&ItemId::from_raw("itm_1"))
                .map(String::as_str),
            Some("#design")
        );
    }

    #[test]
    fn a_finished_child_says_done() {
        let mut tree = Tree::new(state());
        tree.apply(&child_frame(1, announced("reviewer")));
        tree.apply(&child_frame(2, started("trn_9")));
        tree.apply(&child_frame(
            3,
            completed("trn_9", bingo_sdk::TurnStatus::Completed),
        ));
        assert_eq!(
            tree.agents()
                .get(&bingo_sdk::ItemId::from_raw("itm_1"))
                .map(String::as_str),
            Some("reviewer · done")
        );
    }
}
