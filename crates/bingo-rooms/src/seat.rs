//! Opening a room and saying who is in it. `/room design reviewer` and a line
//! in `.bingo/team.json` are the same act, so they are the same function: the
//! room titled `#design` under this session, and its membership published
//! whole into its journal.

use std::path::Path;

use bingo_sdk::{
    Driver, HostHandle, ItemId, KernelError, OpenOptions, ParentLink, SessionFilter, SessionId,
    SessionSelector, SessionSpec,
};
use serde_json::Value;

use crate::ear::{self, Seat};
use crate::room::Room;
use crate::{PLUGIN, cursor, identity, name, post, room};

/// The room of that name under `parent`, opened if there is none; either way
/// its roster afterwards is exactly `seats`, which are names and not sessions
/// — a role may be seated before anyone holds it — each wearing the ear the
/// door asked for.
pub async fn seat(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    name: &str,
    seats: &[Seat],
) -> Result<SessionId, KernelError> {
    let name = name::check(name)?;
    let title = name::title(name);
    let room = match standing(host, parent, &title).await? {
        Some(room) => {
            // A roster is declared whole, and the ears with it: what a seat
            // retuned for itself under the roster before this one is written
            // over here, so the reseat is the reset lever it is meant to be
            // (ADR-0029 §4). A room this call is opening has none to clear.
            clear_retuned(host, &room).await?;
            room
        }
        None => open(host, parent, cwd, name, &title).await?,
    };
    let joining = joining(host, &room, seats).await;
    host.extend(&room, PLUGIN, room::MEMBERS, room::payload(seats))
        .await?;
    start_reading(host, &room, parent, &title, &joining).await;
    Ok(room)
}

/// The names this call adds to the roster: the ones the room was not already
/// seating. A reseat is a roster and not a join, so a seat that was already
/// there keeps reading where it left off — including a seat that has read
/// nothing yet, whose backlog a restart must not sweep away.
async fn joining(host: &HostHandle, room: &SessionId, seats: &[Seat]) -> Vec<String> {
    let held = room::read(host, room)
        .await
        .map(|state| room::members_of(&state))
        .unwrap_or_default();
    seats
        .iter()
        .map(|seat| seat.name.clone())
        .filter(|seat| !held.iter().any(|member| name::same(member, seat)))
        .collect()
}

/// Where a seat joining a room starts reading (ADR-0034 §2): at the room's
/// head, so what was said before it was seated is not a backlog it owes. Only
/// a seat that has never read this room is written to, and a name nobody holds
/// yet is left alone: when its session opens it starts at the head the room has
/// then, which is the same rule read off the one fact such a seat does leave
/// behind.
async fn start_reading(
    host: &HostHandle,
    id: &SessionId,
    parent: &SessionId,
    title: &str,
    joining: &[String],
) {
    let Some(head) = head_of(host, id).await else {
        return;
    };
    let room = Room {
        title: title.to_string(),
        parent: parent.clone(),
        members: joining.to_vec(),
        ears: Default::default(),
    };
    for member in joining {
        if let Some(seat) = post::seat_of(host, &room, member).await {
            open_at(host, &seat, title, &head).await;
        }
    }
}

/// The last thing said in a room, which is where a seat joining it starts.
async fn head_of(host: &HostHandle, room: &SessionId) -> Option<ItemId> {
    let state = room::read(host, room).await?;
    state
        .items
        .iter()
        .filter_map(crate::mentions::Post::of)
        .next_back()
        .map(|post| post.id)
}

/// One seat's cursor, written only if it has none.
async fn open_at(host: &HostHandle, seat: &SessionId, title: &str, head: &ItemId) {
    let read = room::read(host, seat)
        .await
        .and_then(|state| cursor::of_state(&state, title));
    if read.is_some() {
        return;
    }
    if let Err(error) = cursor::advance(host, seat, title, head).await {
        tracing::debug!(room = %title, %seat, %error, "a seat was not told where to start reading");
    }
}

/// Every retuning a standing room carries, cleared. Each is cleared where it
/// was written — one register per seat — so a `Listen` that lands beside this
/// call is settled by journal order rather than by clobbering a shared value.
async fn clear_retuned(host: &HostHandle, room: &SessionId) -> Result<(), KernelError> {
    let Some(state) = room::read(host, room).await else {
        return Ok(());
    };
    for member in ear::ears_of(&state).retuned() {
        host.extend(room, PLUGIN, &ear::kind(&member), Value::Null)
            .await?;
    }
    Ok(())
}

/// What the caller is told once a room is seated: the room, and who is in it.
/// `/room` and `OpenRoom` are the same act (ADR-0021 §3), so they say the same
/// thing about it.
pub(crate) fn receipt(title: &str, seats: &[Seat]) -> String {
    format!("{title}: {}", roster(seats))
}

/// Who is in a room, as a person or a model reads it: the names, and the sigil
/// on the ones that listen rather than answer.
pub(crate) fn roster(seats: &[Seat]) -> String {
    match seats.is_empty() {
        true => ear::NOBODY.to_string(),
        false => seats
            .iter()
            .map(Seat::said)
            .collect::<Vec<String>>()
            .join(", "),
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
    use crate::ear::Ear;
    use crate::room::Room;
    use crate::tests::Fleet;

    fn members(names: &[&str]) -> Vec<Seat> {
        names.iter().map(|n| Seat::live(n)).collect()
    }

    async fn seated(fleet: &Fleet, parent: &SessionId, name: &str, who: &[&str]) -> SessionId {
        seated_with(fleet, parent, name, &members(who)).await
    }

    async fn seated_with(
        fleet: &Fleet,
        parent: &SessionId,
        name: &str,
        seats: &[Seat],
    ) -> SessionId {
        seat(
            &fleet.handle(),
            parent,
            Path::new("/work/project"),
            name,
            seats,
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

    /// The roster carries the ears, and a reseat is the reset lever: what a
    /// seat retuned for itself is cleared where it was written (ADR-0029 §4).
    #[tokio::test]
    async fn a_reseat_declares_the_ears_whole_and_clears_what_a_seat_retuned() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let listening = [
            Seat::live("scout"),
            Seat {
                name: "parent".into(),
                ear: Ear::Patient(std::time::Duration::from_secs(120)),
            },
        ];
        let room = seated_with(&fleet, &root, "design", &listening).await;
        assert_eq!(fleet.ears(&room).of("parent"), listening[1].ear);

        fleet
            .handle()
            .extend(
                &room,
                PLUGIN,
                &ear::kind("scout"),
                ear::register(Ear::Patient(ear::FLOOR)),
            )
            .await
            .expect("a seat retunes its own ear");
        assert_eq!(fleet.ears(&room).of("scout"), Ear::Patient(ear::FLOOR));

        seated(&fleet, &root, "design", &["scout", "parent"]).await;
        let ears = fleet.ears(&room);
        assert_eq!(ears.of("scout"), Ear::Live, "the reseat is the reset lever");
        assert_eq!(ears.of("parent"), Ear::Live);
        assert!(ears.retuned().is_empty(), "and nothing lingers behind it");
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
