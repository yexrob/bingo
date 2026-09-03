//! Who an `@` in the composer can reach from the session on the screen.
//!
//! Nothing here is state and nothing is published: the set is derived from the
//! tree the surface already holds, at the keystroke that asks for it. A surface
//! may not import a plugin (ADR-0001), so what is written down here is the same
//! rule the plugins apply on the other side of the line, and no more.
//!
//! From a session a model answers, that is its own children: an `@name` at the
//! head of a submitted line is redirected to the child of *this* session by
//! that name, and no further, so a name it could not deliver to is not offered.
//! A room answers nobody (ADR-0011 §1) and is posted into rather than asked, so
//! from a room it is the room's own roster instead: a mention in a post names a
//! seat and opens a debt against it (ADR-0022), and `@all` names every seat but
//! the poster — a room word, offered nowhere else. A rostered name nobody holds
//! yet is offered all the same — the debt it opens is chased when somebody
//! does.

use bingo_sdk::{Driver, SessionId, SessionState};

use crate::seats;
use crate::tree::Tree;

/// The names the `@` dropdown offers beside the paths, in the order the tree
/// and the roster list them.
pub fn targets(tree: &Tree) -> Vec<String> {
    let viewed = tree.viewed();
    match viewed.summary.driver {
        Driver::Log => roster(viewed),
        Driver::Model => agents(tree, &viewed.summary.id),
    }
}

/// The agents this session's own `@name` reaches: the children of it this
/// attachment carries that answer a model and have a name to be called by.
///
/// A room under the same session is left out. A line addressed to one would
/// arrive — a room is a child like any other — but a room is somewhere a person
/// posts rather than somebody they write to, and its own view, where the
/// composer says so, is one keystroke away.
fn agents(tree: &Tree, session: &SessionId) -> Vec<String> {
    tree.sessions()
        .filter(|state| child_of(state, session))
        .filter(|state| state.summary.driver != Driver::Log)
        .filter_map(|state| state.summary.title.clone())
        .collect()
}

/// Whether this session hangs directly under that one. Nothing is its own
/// parent, so the session on the screen is left out by the same question.
fn child_of(state: &SessionState, session: &SessionId) -> bool {
    state
        .summary
        .parent
        .as_ref()
        .is_some_and(|link| &link.session == session)
}

/// The word a post uses for everyone in the room but whoever wrote it
/// (ADR-0022 §1). It leads the roster because a word nobody offers is a word
/// nobody finds.
const EVERYONE: &str = "all";

/// Who a post in this room may name: the word for all of them, then the seats
/// on its roster, less the holder — which is the person typing and not
/// somebody to write to.
fn roster(room: &SessionState) -> Vec<String> {
    let seated = seated(room);
    match offers_everyone(&seated) {
        true => [vec![EVERYONE.to_string()], seated].concat(),
        false => seated,
    }
}

/// The seats a post may name one by one.
fn seated(room: &SessionState) -> Vec<String> {
    seats::members(room)
        .into_iter()
        .filter(|name| !name.eq_ignore_ascii_case(seats::HOLDER))
        .collect()
}

/// Whether the word for everyone is one of them: a room with nobody to reach
/// has nothing for it to mean, and a room that seats a member of that name
/// spends it on the member — a real name is never shadowed (ADR-0022 §1).
fn offers_everyone(seated: &[String]) -> bool {
    !seated.is_empty()
        && !seated
            .iter()
            .any(|name| name.eq_ignore_ascii_case(EVERYONE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// A root with two agents under it and a room seating them both.
    fn team() -> Tree {
        folded_tree(vec![
            child_frame(1, announced("reviewer")),
            agent_frame(3, 2, agent_announced(3, "watcher")),
            log_frame(3, log_announced("#design")),
            log_frame(
                4,
                extended(
                    "bingo.rooms",
                    "members",
                    roster_payload(&["reviewer", "watcher", "parent"], &[]),
                ),
            ),
        ])
    }

    /// `all` is a room word: a session a model answers has no roster for it to
    /// mean, so it is not offered there.
    #[test]
    fn a_session_offers_the_agents_under_it_and_not_the_room_beside_them() {
        assert_eq!(targets(&team()), vec!["reviewer", "watcher"]);
    }

    /// A child of a child is not this session's to address: the redirect hook
    /// reaches one step down, so the dropdown offers one step down.
    #[test]
    fn only_the_children_of_the_session_in_view_are_offered() {
        let mut tree = team();
        tree.show(&child_id());
        assert_eq!(
            targets(&tree),
            Vec::<String>::new(),
            "the reviewer started nobody"
        );
    }

    /// A room offers its roster instead: a mention in a post names a seat, and
    /// the holder is the person writing it. The word for all of them leads,
    /// which is how a person meets it.
    #[test]
    fn a_room_offers_all_then_its_seats_without_the_holder() {
        let mut tree = team();
        tree.show(&log_id());
        assert_eq!(targets(&tree), vec!["all", "reviewer", "watcher"]);
    }

    /// A room that seats somebody called `all` spends the word on them, and
    /// offers it once: a real name is never shadowed (ADR-0022 §1).
    #[test]
    fn a_room_that_seats_a_member_named_all_offers_the_member_and_no_word() {
        let mut tree = folded_tree(vec![
            log_frame(1, log_announced("#design")),
            log_frame(
                2,
                extended(
                    "bingo.rooms",
                    "members",
                    roster_payload(&["reviewer", "all"], &[]),
                ),
            ),
        ]);
        tree.show(&log_id());
        assert_eq!(targets(&tree), vec!["reviewer", "all"]);
    }

    /// A name nobody holds yet is on the roster and is offered: mentioning it
    /// opens a debt that is chased when somebody does (ADR-0022).
    #[test]
    fn a_rostered_name_nobody_holds_is_offered_all_the_same() {
        let mut tree = folded_tree(vec![
            log_frame(1, log_announced("#design")),
            log_frame(
                2,
                extended(
                    "bingo.rooms",
                    "members",
                    roster_payload(&["nobody-yet"], &[]),
                ),
            ),
        ]);
        tree.show(&log_id());
        assert_eq!(targets(&tree), vec!["all", "nobody-yet"]);
    }

    /// A room whose journal carries no roster this surface recognises seats
    /// nobody, and the dropdown says so by offering nothing — the word for all
    /// of them included, since there is nobody for it to reach.
    #[test]
    fn a_room_with_no_roster_offers_no_name() {
        let mut tree = folded_tree(vec![log_frame(1, log_announced("#design"))]);
        tree.show(&log_id());
        assert_eq!(targets(&tree), Vec::<String>::new());
    }
}
