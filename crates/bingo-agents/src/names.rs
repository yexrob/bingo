//! Who a name means. An agent's name is its session's title and nothing else
//! (ADR-0010): the tree is the roster, so a name is resolved by asking the
//! host for the caller's children, never by keeping a list beside them.

use bingo_sdk::{ErrorCode, HostHandle, KernelError, SessionFilter, SessionId, SessionSummary};

/// The name a child uses for the session that spawned it.
pub const PARENT: &str = "parent";

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
/// is refused because `resolve` would read it as the address it already is.
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

/// The name a child answers to: its title, or its id when it has none.
pub fn name_of(child: &SessionSummary) -> &str {
    child.title.as_deref().unwrap_or_else(|| child.id.as_str())
}

/// The names of the caller's children, in the order the host lists them.
pub fn names_of(children: &[SessionSummary]) -> Vec<String> {
    children.iter().map(|c| name_of(c).to_string()).collect()
}

/// The session a `to` names: a child of the caller by name, or — for `parent`
/// — the session that spawned the caller. A leading `@` is how a person
/// writes it and means the same thing.
pub async fn resolve(
    host: &HostHandle,
    caller: &SessionId,
    to: &str,
) -> Result<SessionId, KernelError> {
    let to = to.trim().trim_start_matches('@');
    if to == PARENT {
        return parent_of(host, caller).await;
    }
    child(host, caller, to).await
}

/// A child of the caller by name.
pub async fn child(
    host: &HostHandle,
    caller: &SessionId,
    name: &str,
) -> Result<SessionId, KernelError> {
    let children = children(host, caller).await?;
    match children.iter().find(|c| name_of(c) == name) {
        Some(child) => Ok(child.id.clone()),
        None => Err(unknown(name, &children)),
    }
}

/// The session that spawned this one.
pub async fn parent_of(host: &HostHandle, session: &SessionId) -> Result<SessionId, KernelError> {
    let parent = own(host, session).await?.parent;
    parent.map(|link| link.session).ok_or_else(|| {
        KernelError::new(
            ErrorCode::SessionNotFound,
            "this session has no parent to write to",
        )
    })
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
async fn own(host: &HostHandle, session: &SessionId) -> Result<SessionSummary, KernelError> {
    host.sessions(SessionFilter::default())
        .await?
        .into_iter()
        .find(|summary| &summary.id == session)
        .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
}

fn unknown(name: &str, children: &[SessionSummary]) -> KernelError {
    let known = names_of(children).join(", ");
    let message = if known.is_empty() {
        format!("no agent is called {name}, and none is running here")
    } else {
        format!("no agent is called {name}; the ones running are: {known}")
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
        for bad in ["", "  ", "a/b", "two words", PARENT] {
            assert!(check(bad).is_err(), "{bad:?} was accepted");
        }
        let error = check(PARENT).expect_err("a child named parent could never be written to");
        assert!(error.message.contains("pick another name"), "{error}");
    }

    #[tokio::test]
    async fn a_name_resolves_among_the_caller_s_children_and_parent_upwards() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let host = fleet.handle();

        assert_eq!(resolve(&host, &root, "reviewer").await.unwrap(), child);
        assert_eq!(resolve(&host, &root, "@reviewer").await.unwrap(), child);
        assert_eq!(resolve(&host, &child, PARENT).await.unwrap(), root);
    }

    #[tokio::test]
    async fn a_name_nobody_has_says_which_names_are_running() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        let error = resolve(&fleet.handle(), &root, "nobody")
            .await
            .expect_err("no such agent");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("reviewer"), "{error}");
    }

    #[tokio::test]
    async fn the_root_signs_as_parent_and_a_child_by_its_own_name() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        let host = fleet.handle();

        assert_eq!(speaker(&host, &root).await, PARENT);
        assert_eq!(speaker(&host, &child).await, "reviewer");
        assert!(parent_of(&host, &root).await.is_err(), "the root has none");
    }
}
