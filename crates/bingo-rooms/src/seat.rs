//! Opening a room and saying who is in it. `/room design reviewer` and a line
//! in `.bingo/team.json` are the same act, so they are the same function: the
//! room titled `#design` under this session, and its membership published
//! whole into its journal.

use std::path::Path;

use bingo_sdk::{
    Driver, HostHandle, KernelError, OpenOptions, ParentLink, SessionFilter, SessionId,
    SessionSelector, SessionSpec,
};

use crate::{PLUGIN, identity, name, room};

/// The room of that name under `parent`, opened if there is none; either way
/// its membership afterwards is exactly `members`, which are names and not
/// sessions — a role may be seated before anyone holds it.
pub async fn seat(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    name: &str,
    members: &[String],
) -> Result<SessionId, KernelError> {
    let name = name::check(name)?;
    let title = name::title(name);
    let room = match standing(host, parent, &title).await? {
        Some(room) => room,
        None => open(host, parent, cwd, name, &title).await?,
    };
    host.extend(&room, PLUGIN, room::MEMBERS, room::payload(members))
        .await?;
    Ok(room)
}

/// What the caller is told once a room is seated: the room, and who is in it.
/// `/room` and `OpenRoom` are the same act (ADR-0021 §3), so they say the same
/// thing about it.
pub(crate) fn receipt(title: &str, members: &[String]) -> String {
    format!("{title}: {}", roster(members))
}

/// Who is in a room, as a person or a model reads it.
pub(crate) fn roster(members: &[String]) -> String {
    match members.is_empty() {
        true => "nobody yet".to_string(),
        false => members.join(", "),
    }
}

/// The room of that title already under this session, live or persisted.
async fn standing(
    host: &HostHandle,
    parent: &SessionId,
    title: &str,
) -> Result<Option<SessionId>, KernelError> {
    let children = host
        .sessions(SessionFilter {
            parent: Some(parent.clone()),
            ..SessionFilter::default()
        })
        .await?;
    Ok(children
        .into_iter()
        .find(|child| child.driver == Driver::Log && child.title.as_deref() == Some(title))
        .map(|child| child.id))
}

/// A new room: a session nobody answers, under this one. The attachment its
/// creation hands back is dropped — the room keeps running, and this plugin
/// reads it through the hook that observes every journal, not through a
/// stream it holds.
async fn open(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    name: &str,
    title: &str,
) -> Result<SessionId, KernelError> {
    let spec = SessionSpec {
        cwd: cwd.to_path_buf(),
        key: Some(format!("{}{parent}/{name}", room::KEY)),
        parent: Some(ParentLink {
            session: parent.clone(),
            // A room is opened by a person or by a project file, never by a
            // tool call (ADR-0011 §3).
            item: None,
        }),
        title: Some(title.to_string()),
        driver: Driver::Log,
        ..SessionSpec::default()
    };
    let attachment = host
        .open(
            SessionSelector::Create { spec },
            identity(),
            OpenOptions::default(),
        )
        .await?;
    Ok(attachment.session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::Room;
    use crate::tests::Fleet;

    fn members(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    async fn seated(fleet: &Fleet, parent: &SessionId, name: &str, who: &[&str]) -> SessionId {
        seat(
            &fleet.handle(),
            parent,
            Path::new("/work/project"),
            name,
            &members(who),
        )
        .await
        .expect("a room this crate can open")
    }

    #[tokio::test]
    async fn a_new_room_is_a_log_session_under_the_caller_with_a_key_of_its_own() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let room = seated(&fleet, &root, "design", &["reviewer", "scout"]).await;

        let spec = &fleet.created()[0];
        assert_eq!(spec.driver, Driver::Log);
        assert_eq!(spec.title.as_deref(), Some("#design"));
        assert_eq!(
            spec.key.as_deref(),
            Some(format!("rooms/{root}/design").as_str())
        );
        assert_eq!(spec.cwd, Path::new("/work/project"));
        let link = spec.parent.as_ref().expect("a room hangs under a session");
        assert_eq!(link.session, root);
        assert_eq!(link.item, None, "no tool call opened it");

        assert_eq!(
            fleet.members(&room),
            ["reviewer", "scout"],
            "the membership is published whole"
        );
        assert_eq!(
            Room::of(&fleet.summary(&room)).map(|r| r.title),
            Some("#design".into())
        );
    }

    #[tokio::test]
    async fn a_room_that_stands_is_reused_and_its_membership_replaced() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let first = seated(&fleet, &root, "design", &["reviewer", "scout"]).await;
        let again = seated(&fleet, &root, "design", &["scout"]).await;

        assert_eq!(first, again, "one room of that name under this session");
        assert_eq!(fleet.created().len(), 1, "the second call opened nothing");
        assert_eq!(fleet.members(&first), ["scout"]);
    }

    #[tokio::test]
    async fn a_room_without_members_has_none() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let room = seated(&fleet, &root, "design", &[]).await;
        assert!(fleet.members(&room).is_empty());
    }

    #[tokio::test]
    async fn a_name_that_is_not_one_opens_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let error = seat(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            "two words",
            &[],
        )
        .await
        .expect_err("a room name is one word");
        assert_eq!(error.code, bingo_sdk::ErrorCode::InvalidInput);
        assert!(fleet.created().is_empty());
    }

    #[tokio::test]
    async fn an_agent_of_the_same_name_is_not_a_room() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "#design");
        seated(&fleet, &root, "design", &["reviewer"]).await;
        assert_eq!(
            fleet.created().len(),
            1,
            "a session a model answers in is never reused as a room"
        );
    }
}
