//! Which room a caller means, and whether it is that caller's to change.
//!
//! A room reaches the session it hangs under and that session's other children
//! (ADR-0021 §1–2), so those are the only two trees a `room` argument can name
//! one in: the caller's own first, then the tree it stands in — the order
//! `bingo-agents` resolves a `#room` in, so a name means the nearer room
//! wherever it is written.
//!
//! Who may move a roster is a narrower question than who may post in it
//! (ADR-0053 §5): the session the room hangs under, and whoever opened it. A
//! peer of a shared room reads it and posts in it; it does not reseat what it
//! did not open.

use bingo_sdk::{ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary};

use crate::name;
use crate::room::{self, Opened, Room};

/// The room a `room` argument names, whether it was written `design` or
/// `#design`. A name nothing here answers to is refused with the rooms this
/// caller could have written instead.
pub async fn resolve(
    host: &HostHandle,
    caller: &SessionSummary,
    asked: &str,
) -> Result<(SessionId, Room), KernelError> {
    let title = name::title(name::check(asked.trim().trim_start_matches('#'))?);
    let reachable = reachable(host, caller).await;
    reachable
        .iter()
        .find(|(_, room)| room.title == title)
        .cloned()
        .ok_or_else(|| unreachable(&title, &reachable))
}

/// The room one of the room's own verbs names, with everything settled that
/// has to be settled before anything is written: which room, what its journal
/// says it is, and that this caller may change it (ADR-0053 §5). Whether it
/// still stands is the verb's own to refuse, in the words every door uses.
pub async fn entered(
    host: &HostHandle,
    caller: &SessionSummary,
    asked: &str,
) -> Result<(SessionId, Room), KernelError> {
    let (id, room) = resolve(host, caller, asked).await?;
    let state = room::read(host, &id).await.ok_or_else(|| {
        KernelError::new(
            ErrorCode::SessionNotFound,
            format!("{} could not be read", room.title),
        )
    })?;
    may(caller, &room, Opened::of_state(&state).as_ref())?;
    Ok((id, room.seated(&state)))
}

/// Whether this caller may move that roster: the session the room hangs under
/// is the holder, and the name the opening frame carries is the opener. A
/// caller that is neither is told what it may do instead.
pub fn may(
    caller: &SessionSummary,
    room: &Room,
    opened: Option<&Opened>,
) -> Result<(), KernelError> {
    let holder = caller.id == room.parent;
    let opener = opened.is_some_and(|opened| name::same(&name::signed_by(caller), &opened.by));
    match holder || opener {
        true => Ok(()),
        false => Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!(
                "{} is not yours to change: only the session it hangs under and whoever opened \
                 it seat, unseat or close a room. Post in it with `SendMessage`, or open a room \
                 of your own for the work you mean to convene.",
                room.title
            ),
        )),
    }
}

/// The caller's own summary. There is no filter for one id, so this is the
/// list the host has, read once.
pub async fn own(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    host.sessions(SessionFilter::default())
        .await?
        .into_iter()
        .find(|summary| &summary.id == session)
        .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
}

/// Every room this caller can reach: the ones under it, then the ones beside
/// it. A room the caller opened shadows a peer's of the same name, because the
/// nearer session is what an address means.
async fn reachable(host: &HostHandle, caller: &SessionSummary) -> Vec<(SessionId, Room)> {
    let mut around = room::under(host, &caller.id).await.unwrap_or_default();
    if let Some(parent) = caller.parent.as_ref() {
        around.extend(room::under(host, &parent.session).await.unwrap_or_default());
    }
    around
}

/// What a caller could have written instead. A caller in no room at all is
/// told where rooms come from, because there is nothing else to tell it.
pub(crate) fn unreachable(title: &str, reachable: &[(SessionId, Room)]) -> KernelError {
    let names: Vec<&str> = reachable
        .iter()
        .map(|(_, room)| room.title.as_str())
        .collect();
    let message = match names.is_empty() {
        true => format!(
            "there is no {title} you can reach: a room is opened by `OpenRoom` or `/room`, and \
             reaches the session it hangs under and that session's other children"
        ),
        false => format!(
            "there is no {title} you can reach; the rooms here are: {}",
            names.join(", ")
        ),
    };
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Seat;
    use crate::seat::{self, Opening};
    use crate::tests::Fleet;
    use std::path::Path;

    /// A root, the reviewer it started, and the scout beside the reviewer.
    async fn tree() -> (Fleet, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        (fleet, root, reviewer)
    }

    /// A room under `parent`, opened in `by`'s name for one purpose.
    async fn opened(fleet: &Fleet, parent: &SessionId, name: &str, by: &str) -> SessionId {
        seat::open(
            &fleet.handle(),
            parent,
            Path::new("/work/project"),
            Opening {
                name,
                purpose: Some("settle the storage layout"),
                by,
            },
            &[Seat::named("scout")],
        )
        .await
        .expect("a room this crate can open")
    }

    #[tokio::test]
    async fn a_room_argument_names_a_room_under_the_caller_before_one_beside_it() {
        let (fleet, root, reviewer) = tree().await;
        let beside = opened(&fleet, &root, "design", "parent").await;
        let mine = opened(&fleet, &reviewer, "design", "reviewer").await;
        let caller = fleet.summary(&reviewer);

        for asked in ["design", "#design", " design "] {
            let (id, room) = resolve(&fleet.handle(), &caller, asked)
                .await
                .expect("the room the caller means");
            assert_eq!(id, mine, "{asked}: the caller's own room comes first");
            assert_eq!(room.title, "#design");
        }

        let (id, _) = resolve(&fleet.handle(), &fleet.summary(&root), "design")
            .await
            .expect("the room under the root");
        assert_eq!(id, beside);
    }

    #[tokio::test]
    async fn a_name_no_room_here_answers_to_is_refused_with_the_ones_that_do() {
        let (fleet, root, reviewer) = tree().await;
        opened(&fleet, &root, "design", "parent").await;
        let refused = resolve(&fleet.handle(), &fleet.summary(&reviewer), "standup")
            .await
            .expect_err("no such room");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(
            refused.message.contains("no #standup you can reach"),
            "{refused}"
        );
        assert!(refused.message.contains("#design"), "{refused}");

        let alone = fleet.child(&fleet.root(), "stranger");
        let refused = resolve(&fleet.handle(), &fleet.summary(&alone), "standup")
            .await
            .expect_err("no room at all");
        assert!(
            refused.message.contains("`OpenRoom` or `/room`"),
            "a caller in no room is told where one comes from: {refused}"
        );
    }

    /// ADR-0053 §5: the holder and the opener, and nobody else.
    #[tokio::test]
    async fn only_the_session_it_hangs_under_and_the_one_that_opened_it_may_change_it() {
        let (fleet, root, reviewer) = tree().await;
        let id = opened(&fleet, &root, "design", "reviewer").await;
        let host = fleet.handle();

        for caller in [root.clone(), reviewer.clone()] {
            entered(&host, &fleet.summary(&caller), "design")
                .await
                .unwrap_or_else(|e| panic!("{caller} may change it: {e}"));
        }

        let scout = fleet.titled("scout").expect("the peer");
        let refused = entered(&host, &fleet.summary(&scout), "design")
            .await
            .expect_err("a peer reads and posts, and does not reseat");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(
            refused.message.contains("#design is not yours to change"),
            "{refused}"
        );
        assert!(refused.message.contains("`SendMessage`"), "{refused}");
        assert_eq!(id, fleet.titled("#design").expect("the room"));
    }

    /// What a verb is handed is the room as its own journal has it: the roster,
    /// the purpose and whether it still stands.
    #[tokio::test]
    async fn what_the_door_hands_back_is_the_room_its_journal_says_it_is() {
        let (fleet, root, _) = tree().await;
        opened(&fleet, &root, "design", "parent").await;
        let (_, room) = entered(&fleet.handle(), &fleet.summary(&root), "design")
            .await
            .expect("the room");
        assert_eq!(room.members, ["scout"]);
        assert_eq!(room.purpose.as_deref(), Some("settle the storage layout"));
        assert!(!room.closed);
    }
}
