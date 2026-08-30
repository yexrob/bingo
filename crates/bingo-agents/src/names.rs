//! Who a name means. An agent's name is its session's title and nothing else
//! (ADR-0010): the tree is the roster, so a name is resolved by asking the
//! host for the caller's children, never by keeping a list beside them.

use bingo_sdk::{ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary};

/// The name a child uses for the session that spawned it.
pub const PARENT: &str = "parent";

/// What a room's title starts with. A room is a session nobody answers
/// (ADR-0011 §1); it is written `#name` wherever a name is written, and no
/// agent may take a name that would read as one.
pub const ROOM: char = '#';

/// How many names one base may take before a caller is asked to pick another.
const MAX_NAMES: usize = 64;

/// The names a base may take, in the order they are tried: `reviewer`,
/// `reviewer-2`, `reviewer-3`…
pub fn candidates(base: &str) -> impl Iterator<Item = String> + '_ {
    (1..=MAX_NAMES).map(move |nth| match nth {
        1 => base.to_string(),
        nth => format!("{base}-{nth}"),
    })
}

/// The first name of a base that nothing in `taken` holds.
pub fn free(base: &str, taken: &[String]) -> Option<String> {
    candidates(base).find(|name| !taken.iter().any(|held| held == name))
}

/// A name that can be a title and a key segment both. Whitespace and `/`
/// would make `agent/<parent>/<name>` ambiguous, so they are refused here
/// rather than mangled into something the caller never asked for; `parent`
/// is refused because `resolve` would read it as the address it already is,
/// and a leading `#` because it would read as a room.
pub fn check(name: &str) -> Result<&str, KernelError> {
    let name = name.trim();
    let bad = name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace);
    if bad {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("{name:?} is not a name: one word, no slashes"),
        ));
    }
    if name == PARENT {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("{PARENT:?} is what a child calls whoever spawned it; pick another name"),
        ));
    }
    if name.starts_with(ROOM) {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("{name:?} reads as a room; an agent's name starts with a letter"),
        ));
    }
    Ok(name)
}

/// The children of `session`, as the host lists them.
pub async fn children(
    host: &HostHandle,
    session: &SessionId,
) -> Result<Vec<SessionSummary>, KernelError> {
    host.sessions(SessionFilter {
        parent: Some(session.clone()),
        ..SessionFilter::default()
    })
    .await
}

/// The agents among the caller's children: a room is a child too, and
/// answers nobody, so a roster leaves it out.
pub async fn agents(
    host: &HostHandle,
    session: &SessionId,
) -> Result<Vec<SessionSummary>, KernelError> {
    let mut children = children(host, session).await?;
    children.retain(|child| child.driver != bingo_sdk::Driver::Log);
    Ok(children)
}

/// The name a child answers to: its title, or its id when it has none.
pub fn name_of(child: &SessionSummary) -> &str {
    child.title.as_deref().unwrap_or_else(|| child.id.as_str())
}

/// The names of the caller's children, in the order the host lists them.
pub fn names_of(children: &[SessionSummary]) -> Vec<String> {
    children.iter().map(|c| name_of(c).to_string()).collect()
}

/// The session a `to` names, as the host lists it: a child of the caller by
/// name, a `#room` the caller can reach, or — for `parent` — the session that
/// spawned the caller. A leading `@` is how a person writes it and means the
/// same thing.
pub async fn resolve(
    host: &HostHandle,
    caller: &SessionId,
    to: &str,
) -> Result<SessionSummary, KernelError> {
    let to = to.trim().trim_start_matches('@');
    if to == PARENT {
        return parent(host, caller).await;
    }
    if to.starts_with(ROOM) {
        return room(host, caller, to).await;
    }
    child(host, caller, to).await
}

/// A child of the caller by name.
pub async fn child(
    host: &HostHandle,
    caller: &SessionId,
    name: &str,
) -> Result<SessionSummary, KernelError> {
    let children = children(host, caller).await?;
    named(&children, name).ok_or_else(|| unknown(name, &children))
}

/// A room by name: one the caller opened, else one beside it — a member and
/// the room it posts into are children of the same session.
async fn room(
    host: &HostHandle,
    caller: &SessionId,
    name: &str,
) -> Result<SessionSummary, KernelError> {
    let mut known = children(host, caller).await?;
    if let Some(room) = named(&known, name) {
        return Ok(room);
    }
    if let Ok(parent) = parent(host, caller).await {
        known.extend(children(host, &parent.id).await?);
    }
    named(&known, name).ok_or_else(|| unknown(name, &known))
}

/// The one of these sessions that answers to `name`.
pub fn named(sessions: &[SessionSummary], name: &str) -> Option<SessionSummary> {
    sessions.iter().find(|s| name_of(s) == name).cloned()
}

/// The session that spawned this one.
pub async fn parent(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    let all = host.sessions(SessionFilter::default()).await?;
    let link = pick(&all, session)?.parent.clone().ok_or_else(|| {
        KernelError::new(
            ErrorCode::SessionNotFound,
            "this session has no parent to write to",
        )
    })?;
    pick(&all, &link.session).cloned()
}

/// How the caller signs a message: its own name, or `parent` for a session
/// that has no title, which is what a child calls whoever spawned it.
pub async fn speaker(host: &HostHandle, session: &SessionId) -> String {
    match own(host, session).await {
        Ok(summary) => summary.title.unwrap_or_else(|| PARENT.to_string()),
        Err(_) => PARENT.to_string(),
    }
}

/// The caller's own summary. There is no filter for one id, so this is the
/// list the host has, read once.
pub async fn own(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    let all = host.sessions(SessionFilter::default()).await?;
    pick(&all, session).cloned()
}

fn pick<'a>(all: &'a [SessionSummary], id: &SessionId) -> Result<&'a SessionSummary, KernelError> {
    all.iter()
        .find(|summary| &summary.id == id)
        .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
}

/// What a caller could have written instead. Agents and rooms are both
/// sessions of the tree and both answer to a name, so both are named here.
fn unknown(name: &str, known: &[SessionSummary]) -> KernelError {
    let (rooms, agents): (Vec<String>, Vec<String>) = names_of(known)
        .into_iter()
        .partition(|name| name.starts_with(ROOM));
    let mut here = Vec::new();
    if !agents.is_empty() {
        here.push(format!("the agents running are: {}", agents.join(", ")));
    }
    if !rooms.is_empty() {
        here.push(format!("the rooms here are: {}", rooms.join(", ")));
    }
    let message = match here.is_empty() {
        true => format!("nothing is called {name}, and no agent or room is here"),
        false => format!("nothing is called {name}; {}", here.join("; ")),
    };
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fleet;

    #[test]
    fn the_first_name_of_a_base_is_the_base_itself() {
        assert_eq!(free("reviewer", &[]).as_deref(), Some("reviewer"));
    }

    #[test]
    fn a_name_a_sibling_holds_gets_the_next_suffix() {
        let taken = vec!["reviewer".to_string(), "reviewer-2".to_string()];
        assert_eq!(free("reviewer", &taken).as_deref(), Some("reviewer-3"));
    }

    #[test]
    fn a_base_runs_out_of_names_rather_than_looping() {
        let taken: Vec<String> = candidates("r").collect();
        assert_eq!(taken.len(), MAX_NAMES);
        assert_eq!(taken[1], "r-2");
        assert_eq!(free("r", &taken), None);
    }

    #[test]
    fn a_name_that_would_break_the_key_or_the_address_is_refused() {
        assert_eq!(check(" reviewer "), Ok("reviewer"));
        for bad in ["", "  ", "a/b", "two words", PARENT, "#design"] {
            assert!(check(bad).is_err(), "{bad:?} was accepted");
        }
        let error = check(PARENT).expect_err("a child named parent could never be written to");
        assert!(error.message.contains("pick another name"), "{error}");
        let error = check("#design").expect_err("an agent is not a room");
        assert!(error.message.contains("reads as a room"), "{error}");
    }

    #[tokio::test]
    async fn a_name_resolves_among_the_caller_s_children_and_parent_upwards() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let host = fleet.handle();

        assert_eq!(resolve(&host, &root, "reviewer").await.unwrap().id, child);
        assert_eq!(resolve(&host, &root, "@reviewer").await.unwrap().id, child);
        assert_eq!(resolve(&host, &child, PARENT).await.unwrap().id, root);
    }

    #[tokio::test]
    async fn a_room_resolves_from_the_session_that_holds_it_and_from_a_member() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        let design = fleet.room(&root, "#design");
        let host = fleet.handle();

        let from_root = resolve(&host, &root, "#design").await.unwrap();
        assert_eq!(from_root.id, design);
        assert_eq!(from_root.driver, bingo_sdk::Driver::Log);
        let from_member = resolve(&host, &reviewer, "#design").await.unwrap();
        assert_eq!(from_member.id, design, "a room is a sibling of its members");
    }

    #[tokio::test]
    async fn a_name_nobody_has_says_which_agents_and_rooms_are_here() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        fleet.room(&root, "#design");
        let host = fleet.handle();

        let error = resolve(&host, &root, "nobody")
            .await
            .expect_err("no such agent");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("reviewer"), "{error}");
        assert!(error.message.contains("#design"), "{error}");

        let error = resolve(&host, &root, "#nowhere")
            .await
            .expect_err("no such room");
        assert!(error.message.contains("#design"), "{error}");
    }

    #[tokio::test]
    async fn the_root_signs_as_parent_and_a_child_by_its_own_name() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let host = fleet.handle();

        assert_eq!(speaker(&host, &root).await, PARENT);
        assert_eq!(speaker(&host, &child).await, "reviewer");
        assert!(parent(&host, &root).await.is_err(), "the root has none");
        assert_eq!(parent(&host, &child).await.unwrap().id, root);
    }
}
