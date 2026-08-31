//! Where a room hangs, and therefore who hears it. A room fans out to the
//! other children of the session it hangs under (`post.rs`), so placement is
//! the whole of the audience question: under the caller a room reaches the
//! workers the caller started, under the caller's parent it reaches the caller
//! and its peers (ADR-0021 §1, §2).

use bingo_sdk::{ErrorCode, KernelError, SessionId, SessionSummary};

/// Which tree a room is opened in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Under the caller: the agents it started hear it. The unprivileged case.
    Caller,
    /// Under the caller's parent: the caller's peers hear it.
    Peers,
}

impl Placement {
    /// What `shared` asks for.
    pub fn of(shared: bool) -> Self {
        match shared {
            true => Placement::Peers,
            false => Placement::Caller,
        }
    }

    /// Where the room will hang, in the words the permission card shows. A
    /// person approving `shared` is approving an act on the parent's tree, so
    /// the phrase names that tree rather than the flag that asked for it.
    pub fn phrase(self) -> &'static str {
        match self {
            Placement::Caller => "under the caller",
            Placement::Peers => "under the caller's parent",
        }
    }
}

/// The session to seat the room under. A root asking for its peers is asking
/// for a tree that is not there; it is told so rather than quietly given a
/// room under itself, because the two reach different audiences.
pub fn under(caller: &SessionSummary, placement: Placement) -> Result<SessionId, KernelError> {
    match placement {
        Placement::Caller => Ok(caller.id.clone()),
        Placement::Peers => caller
            .parent
            .as_ref()
            .map(|link| link.session.clone())
            .ok_or_else(rootless),
    }
}

fn rootless() -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        "this session is a root: it has no parent to hang a shared room under. \
         Open the room without `shared` — under a root it already reaches every agent it has.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::summary;

    fn root() -> SessionSummary {
        summary("ses_root", None, None)
    }

    fn child() -> SessionSummary {
        summary(
            "ses_reviewer",
            Some("reviewer"),
            Some(SessionId::from_raw("ses_root")),
        )
    }

    #[test]
    fn shared_picks_the_parents_tree_and_the_default_picks_the_callers_own() {
        assert_eq!(Placement::of(false), Placement::Caller);
        assert_eq!(Placement::of(true), Placement::Peers);
    }

    #[test]
    fn a_room_hangs_under_the_caller_by_default() {
        let caller = child();
        assert_eq!(
            under(&caller, Placement::Caller).expect("a caller is its own tree"),
            caller.id
        );
        assert_eq!(
            under(&root(), Placement::Caller).expect("a root opens rooms as it always did"),
            SessionId::from_raw("ses_root")
        );
    }

    #[test]
    fn shared_hangs_it_under_the_session_that_started_the_caller() {
        assert_eq!(
            under(&child(), Placement::Peers).expect("a child has a parent"),
            SessionId::from_raw("ses_root")
        );
    }

    #[test]
    fn a_root_asking_for_its_peers_is_told_why_it_has_none() {
        let error = under(&root(), Placement::Peers).expect_err("a root has no peers");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("root"), "{error}");
        assert!(
            error.message.contains("without `shared`"),
            "the refusal says what to call instead: {error}"
        );
    }

    #[test]
    fn the_card_names_the_tree_a_room_will_hang_in() {
        assert_eq!(Placement::Caller.phrase(), "under the caller");
        assert_eq!(Placement::Peers.phrase(), "under the caller's parent");
    }
}
