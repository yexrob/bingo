//! The board: which session's list a call works on, and who the room that
//! holds it can see.
//!
//! Without `in`, a call means the caller's own list — byte for byte what it
//! meant before there was a board, and not one host read more (ADR-0023 §4).
//! With it, the list belongs to a room the caller can reach: a `Log` session
//! by that title among its children or beside it, which is the reach a post to
//! `#name` already has (ADR-0021). Structural reachability is the whole gate,
//! and a name outside it is a worded error the model corrects.
//!
//! The walk over the tree is this crate's own. `bingo-agents` and
//! `bingo-rooms` each have one, a plugin may not import a plugin, and the
//! duplication is recorded for the sdk sweep rather than shared through a
//! trait nobody has asked for yet (ADR-0023).

use bingo_sdk::{
    Driver, ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// What a room's title starts with (ADR-0011 §1). `in` is written the way a
/// post's address is, and `design` and `#design` mean the same room.
const ROOM: char = '#';

/// The board a call names. It is flattened into each tool's arguments, so the
/// word and what it means are written once for all four.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct In {
    /// The room whose shared board to work on, as `#name` — a room you opened
    /// or one beside you. Leave it out for this session's own list.
    #[serde(default, rename = "in")]
    pub room: Option<String>,
}

impl In {
    /// The room named, if one was: a blank name is no name.
    pub fn name(&self) -> Option<&str> {
        self.room
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// What a person types after `/tasks`: nothing, or `in #room`.
    pub fn spoken(args: &str) -> Result<Self, KernelError> {
        let words: Vec<&str> = args.split_whitespace().collect();
        match words[..] {
            [] => Ok(Self::default()),
            ["in", room] => Ok(Self {
                room: Some(room.to_string()),
            }),
            _ => Err(KernelError::new(
                ErrorCode::InvalidInput,
                "`/tasks` takes nothing, or `in #room`",
            )),
        }
    }
}

/// Where a call's list lives, and the tree the answer came from — so a
/// listing can go on to ask who is here without reading the tree a second
/// time.
#[derive(Debug)]
pub struct Board {
    pub session: SessionId,
    /// The tree the room was found in. A session's own list is found without
    /// looking at anything, and asserts nothing about who its owners are.
    tree: Option<Vec<SessionSummary>>,
}

impl Board {
    /// Who a listing of this board may say is here: nothing at all for a
    /// session's own list, where a name is a note the doer wrote itself.
    pub fn present(&self) -> Option<Vec<String>> {
        let tree = self.tree.as_ref()?;
        Some(around(tree, &self.session))
    }
}

/// The board this call works on.
pub async fn of(host: &HostHandle, caller: &SessionId, board: &In) -> Result<Board, KernelError> {
    let Some(name) = board.name() else {
        return Ok(Board {
            session: caller.clone(),
            tree: None,
        });
    };
    let wanted = title(name);
    let tree = tree(host).await?;
    let reachable = reachable(&tree, caller);
    let room = named(&reachable, &wanted).ok_or_else(|| unreachable(&wanted, &reachable))?;
    Ok(Board {
        session: room.id,
        tree: Some(tree),
    })
}

/// The name a claim is stamped with: the caller's own title, read from the
/// session the call came from, so the model never states who it is and cannot
/// claim onto a teammate (ADR-0023 §2). A session with no title is the
/// person's own; it has no name to sign with, and a guess would be worse than
/// a refusal.
pub async fn claimant(host: &HostHandle, caller: &SessionId) -> Result<String, KernelError> {
    let tree = tree(host).await?;
    pick(&tree, caller)
        .and_then(|own| own.title.clone())
        .ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                "this session has no name of its own to claim with; name the doer with `owner`",
            )
        })
}

/// Every session the host knows. There is no filter for one id and the walk
/// asks about several at once, so the tree is read once and looked at here.
async fn tree(host: &HostHandle) -> Result<Vec<SessionSummary>, KernelError> {
    host.sessions(SessionFilter::default()).await
}

/// A name as a room wears it.
fn title(name: &str) -> String {
    match name.starts_with(ROOM) {
        true => name.to_string(),
        false => format!("{ROOM}{name}"),
    }
}

/// The rooms a caller may name: the ones it opened, then the ones beside it —
/// a member and the room it posts into are children of the same session.
fn reachable(tree: &[SessionSummary], caller: &SessionId) -> Vec<SessionSummary> {
    let mut rooms = rooms_under(tree, caller);
    if let Some(parent) = parent_of(tree, caller) {
        rooms.extend(rooms_under(tree, &parent));
    }
    rooms
}

/// The rooms a session holds. Only a `Log` session: a session that answers is
/// somebody's own list, never a board.
fn rooms_under(tree: &[SessionSummary], parent: &SessionId) -> Vec<SessionSummary> {
    tree.iter()
        .filter(|session| session.driver == Driver::Log && is_child(session, parent))
        .cloned()
        .collect()
}

/// Every name that could have reached this board: the room's own parent, and
/// everything beside the room. A claim can come from nowhere else, so an owner
/// none of these answers to is a name nobody here holds — which is what a
/// listing says at read time, writing nothing (ADR-0023 §3). Rooms are left
/// out: a board is not a doer.
fn around(tree: &[SessionSummary], board: &SessionId) -> Vec<String> {
    let Some(parent) = parent_of(tree, board) else {
        return Vec::new();
    };
    let mut names: Vec<String> = tree
        .iter()
        .filter(|session| session.driver != Driver::Log && is_child(session, &parent))
        .filter_map(|session| session.title.clone())
        .collect();
    names.extend(pick(tree, &parent).and_then(|holder| holder.title.clone()));
    names
}

fn is_child(session: &SessionSummary, parent: &SessionId) -> bool {
    session
        .parent
        .as_ref()
        .is_some_and(|link| &link.session == parent)
}

fn parent_of(tree: &[SessionSummary], id: &SessionId) -> Option<SessionId> {
    Some(pick(tree, id)?.parent.as_ref()?.session.clone())
}

fn pick<'a>(tree: &'a [SessionSummary], id: &SessionId) -> Option<&'a SessionSummary> {
    tree.iter().find(|session| &session.id == id)
}

fn named(rooms: &[SessionSummary], title: &str) -> Option<SessionSummary> {
    rooms
        .iter()
        .find(|room| room.title.as_deref() == Some(title))
        .cloned()
}

/// What the caller could have written instead.
fn unreachable(title: &str, reachable: &[SessionSummary]) -> KernelError {
    let here: Vec<&str> = reachable
        .iter()
        .filter_map(|room| room.title.as_deref())
        .collect();
    let message = match here.is_empty() {
        true => format!(
            "no board called {title} is within reach; open a room first, or leave `in` out for this session's own list"
        ),
        false => format!(
            "no board called {title} is within reach; the rooms here are: {}",
            here.join(", ")
        ),
    };
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Journals;

    /// A root with a room under it and a member beside the room, which is the
    /// shape every board scenario is cut from.
    fn tree_of(journals: &Journals) -> (SessionId, SessionId, SessionId) {
        let root = journals.session();
        let room = journals.room(&root, "#design");
        let member = journals.child(&root, "reviewer");
        (root, room, member)
    }

    /// The board a call means, written as the model writes it.
    fn asked(name: &str) -> In {
        In {
            room: Some(name.into()),
        }
    }

    #[tokio::test]
    async fn without_a_name_the_list_is_the_caller_s_own() {
        let journals = Journals::new();
        let (root, ..) = tree_of(&journals);
        let board = of(&journals.handle(), &root, &In::default())
            .await
            .expect("the caller's own");
        assert_eq!(board.session, root);
        assert_eq!(board.present(), None, "a private list asserts nothing");
        assert_eq!(
            journals.session_reads(),
            0,
            "a private list costs no walk of the tree"
        );
    }

    #[tokio::test]
    async fn a_room_resolves_from_the_session_that_opened_it_and_from_a_member() {
        let journals = Journals::new();
        let (root, room, member) = tree_of(&journals);
        let host = journals.handle();
        for caller in [&root, &member] {
            for written in ["#design", "design", "  #design  "] {
                assert_eq!(
                    of(&host, caller, &asked(written))
                        .await
                        .expect("the room")
                        .session,
                    room,
                    "{written} from {caller}"
                );
            }
        }
    }

    /// One read of the tree answers both of a listing's questions.
    #[tokio::test]
    async fn a_board_and_who_is_on_it_cost_one_walk() {
        let journals = Journals::new();
        let (root, ..) = tree_of(&journals);
        let board = of(&journals.handle(), &root, &asked("#design"))
            .await
            .expect("the room");
        assert_eq!(board.present(), Some(vec!["reviewer".to_string()]));
        assert_eq!(journals.session_reads(), 1);
    }

    #[tokio::test]
    async fn a_name_out_of_reach_says_which_rooms_are_here() {
        let journals = Journals::new();
        let (root, ..) = tree_of(&journals);
        let error = of(&journals.handle(), &root, &asked("#nowhere"))
            .await
            .expect_err("no such room");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("#nowhere"), "{error}");
        assert!(error.message.contains("#design"), "{error}");
    }

    /// A room is a `Log` session; an agent by that name is not a board.
    #[tokio::test]
    async fn a_session_that_answers_is_never_a_board() {
        let journals = Journals::new();
        let root = journals.session();
        journals.child(&root, "#design");
        let error = of(&journals.handle(), &root, &asked("#design"))
            .await
            .expect_err("an agent is not a board");
        assert!(error.message.contains("no board"), "{error}");
    }

    #[tokio::test]
    async fn a_claim_is_stamped_with_the_caller_s_own_title() {
        let journals = Journals::new();
        let (root, _, member) = tree_of(&journals);
        let host = journals.handle();
        assert_eq!(
            claimant(&host, &member).await.expect("its own name"),
            "reviewer"
        );
        let error = claimant(&host, &root)
            .await
            .expect_err("a root has no name of its own");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("`owner`"), "{error}");
    }

    /// The names a board can see are the ones that could have claimed on it:
    /// the room's own holder and its siblings, nobody further out.
    #[tokio::test]
    async fn a_board_sees_the_room_s_holder_and_everything_beside_it() {
        let journals = Journals::new();
        let root = journals.session();
        let holder = journals.child(&root, "lead");
        journals.room(&holder, "#design");
        journals.child(&holder, "reviewer");
        journals.room(&holder, "#other");
        journals.child(&root, "elsewhere");

        let here = of(&journals.handle(), &holder, &asked("#design"))
            .await
            .expect("the room")
            .present()
            .expect("a board asserts who is here");
        assert!(here.contains(&"reviewer".to_string()), "{here:?}");
        assert!(here.contains(&"lead".to_string()), "the room's own holder");
        assert!(
            !here.contains(&"#other".to_string()),
            "a room is not a doer"
        );
        assert!(!here.contains(&"elsewhere".to_string()), "{here:?}");
    }

    #[test]
    fn a_person_writes_the_board_as_two_words_or_leaves_it_out() {
        assert!(In::spoken("").expect("nothing").name().is_none());
        assert_eq!(
            In::spoken("  in   #design ").expect("a board").name(),
            Some("#design")
        );
        for bad in ["#design", "in", "in #design #other", "of #design"] {
            let error = In::spoken(bad).expect_err("{bad}");
            assert_eq!(error.code, ErrorCode::InvalidInput, "{bad}");
            assert!(error.message.contains("in #room"), "{bad}: {error}");
        }
    }

    #[test]
    fn the_word_in_is_what_the_schema_carries() {
        let schema = bingo_sdk::input_schema::<In>();
        assert!(schema["properties"]["in"].is_object(), "{schema}");
        assert!(schema.get("required").is_none(), "a board is optional");
    }
}
