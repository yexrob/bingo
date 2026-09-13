//! Opening a room, saying who is in it, and ending it. `/room design reviewer`
//! and a line in `.bingo/team.json` are the same act, so they are the same
//! function: the room titled `#design` under this session, and its membership
//! published whole into its journal.
//!
//! A room is opened for one purpose (ADR-0053 §1), and after that its roster
//! moves by its own verbs: `join` and `leave` derive the next roster from the
//! standing one and publish it whole, and `close` ends the room. Each of them
//! says so in the room by a post the caller signs, so the change is something
//! every member reads rather than something that happened to them.
//!
//! Two doors open one: a person's, which reseats a room that stands because a
//! person outranks the protocol (ADR-0025 §4), and an agent's, which refuses a
//! name that stands and names the verb that does what was meant.

use std::path::Path;

use bingo_sdk::{
    Driver, ErrorCode, HostHandle, ItemId, KernelError, OpenOptions, ParentLink, SessionFilter,
    SessionId, SessionSelector, SessionSpec, SessionState,
};
use jiff::Timestamp;
use serde_json::Value;

use crate::ear::{self, Seat};
use crate::room::{self, Closed, Opened, Room};
use crate::{PLUGIN, cursor, identity, name, post};

/// What a door asks for when it opens a room: the name, the one purpose it is
/// for, and the signing name of whoever is opening it (ADR-0053 §1).
#[derive(Clone, Copy, Debug)]
pub struct Opening<'a> {
    pub name: &'a str,
    pub purpose: Option<&'a str>,
    pub by: &'a str,
}

impl<'a> Opening<'a> {
    /// What a person's door asks for: a room they may give no purpose, opened
    /// under the name every member of it calls them by.
    pub fn person(name: &'a str, purpose: Option<&'a str>) -> Opening<'a> {
        Opening {
            name,
            purpose,
            by: name::PARENT,
        }
    }

    /// The opening as the room's journal keeps it.
    fn published(&self) -> Opened {
        Opened {
            purpose: self.purpose.map(str::to_string),
            by: self.by.to_string(),
        }
    }
}

/// The room of that name under `parent`, opened if there is none; either way
/// its roster afterwards is exactly `seats`, which are names and not sessions
/// — a role may be seated before anyone holds it — each wearing the ear the
/// door asked for.
///
/// This is the person's door and the team file's, and it stays the reset lever
/// a restart leans on (ADR-0053 §2). What it will not do is reopen a room that
/// was closed: closing is the one thing a person's `/room` cannot take back.
pub async fn seat(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    opening: Opening<'_>,
    seats: &[Seat],
) -> Result<SessionId, KernelError> {
    let opening = Opening {
        name: name::check(opening.name)?,
        ..opening
    };
    let title = name::title(opening.name);
    let Some(room) = standing(host, parent, &title).await? else {
        return found(host, parent, cwd, opening, seats).await;
    };
    let standing = room::read(host, &room).await;
    still_open(standing.as_ref(), &title)?;
    reseat(host, &room, &title, seats, standing).await?;
    Ok(room)
}

/// The same door with an agent on the other side of it (ADR-0021), and the one
/// difference ADR-0053 §2 makes: a name that stands is not reopened here. What
/// the caller meant is a verb on the room it already has, or another name for
/// another phase, and the refusal says which.
pub async fn open(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    opening: Opening<'_>,
    seats: &[Seat],
) -> Result<SessionId, KernelError> {
    let opening = Opening {
        name: name::check(opening.name)?,
        ..opening
    };
    let title = name::title(opening.name);
    if let Some(room) = standing(host, parent, &title).await? {
        return Err(taken(room::read(host, &room).await.as_ref(), &title));
    }
    found(host, parent, cwd, opening, seats).await
}

/// Names added to a room by a caller that may (ADR-0053 §3). The room is told
/// before the roster changes under it: the post goes in first, so the seats
/// already there read who joined, and the joiners — seated at the head the room
/// had before it — are handed that line as the first thing they read.
pub async fn join(
    host: &HostHandle,
    id: &SessionId,
    room: &Room,
    seats: &[Seat],
    by: &str,
) -> Result<Vec<Seat>, KernelError> {
    let standing = read(host, id, &room.title).await?;
    still_open(Some(&standing), &room.title)?;
    let roster = joined(&room::roster_of(&standing), seats);
    let fresh = joining(Some(&standing), seats);
    let head = head_of(&standing);
    post::say(host, id, by, moved(by, "seated", &named(seats))).await?;
    declare(host, id, &roster).await?;
    start_reading(host, id, &room.title, head.as_ref(), &fresh).await;
    Ok(roster)
}

/// Names taken out of one. The roster goes out whole without them, what each of
/// them had retuned goes with them, the room reads that it happened — and each
/// of them is told, because nothing in a tree is taken away in silence.
pub async fn leave(
    host: &HostHandle,
    id: &SessionId,
    room: &Room,
    names: &[String],
    by: &str,
) -> Result<Vec<Seat>, KernelError> {
    let standing = read(host, id, &room.title).await?;
    still_open(Some(&standing), &room.title)?;
    let seated = room::roster_of(&standing);
    let leaving = leaving(&seated, names, &room.title)?;
    let roster: Vec<Seat> = seated
        .into_iter()
        .filter(|seat| !holds(&leaving, &seat.name))
        .collect();
    declare(host, id, &roster).await?;
    clear_ears(host, id, &leaving).await?;
    post::say(host, id, by, moved(by, "unseated", &leaving)).await?;
    tell_the_unseated(host, room, &leaving, by).await;
    Ok(roster)
}

/// The end of a room (ADR-0053 §4): a last line the caller signs, and then the
/// frame that says it is over. The post goes first, so it lands while the room
/// still takes one and the live ears on it hear it.
pub async fn close(
    host: &HostHandle,
    id: &SessionId,
    room: &Room,
    by: &str,
    why: Option<&str>,
) -> Result<(), KernelError> {
    let standing = read(host, id, &room.title).await?;
    still_open(Some(&standing), &room.title)?;
    post::say(host, id, by, closing(by, &room.title, why)).await?;
    let closed = Closed {
        at: Timestamp::now(),
        by: by.to_string(),
        why: why.map(str::to_string),
    };
    host.extend(id, PLUGIN, room::CLOSED, closed.payload())
        .await
}

/// A room this call opens: the session, the frame saying why it is there, and
/// the roster. It has said nothing yet, so every seat on it is level with it
/// and no cursor has to say so — and it carries no retuning to clear either.
async fn found(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    opening: Opening<'_>,
    seats: &[Seat],
) -> Result<SessionId, KernelError> {
    let room = mint(host, parent, cwd, opening.name).await?;
    host.extend(&room, PLUGIN, room::OPENED, opening.published().payload())
        .await?;
    declare(host, &room, seats).await?;
    Ok(room)
}

/// A room that already stands, seated again. Its journal answers three
/// questions here — what a seat retuned for itself, who it was already
/// seating, and where its head is — and one snapshot answers all three, so it
/// is read once and no read of it can disagree with another.
async fn reseat(
    host: &HostHandle,
    room: &SessionId,
    title: &str,
    seats: &[Seat],
    standing: Option<SessionState>,
) -> Result<(), KernelError> {
    // A roster is declared whole, and the ears with it: what a seat retuned
    // for itself under the roster before this one is written over here, so the
    // reseat is the reset lever it is meant to be (ADR-0029 §4).
    let retuned = standing
        .as_ref()
        .map(|standing| ear::ears_of(standing).retuned())
        .unwrap_or_default();
    clear_ears(host, room, &retuned).await?;
    let joining = joining(standing.as_ref(), seats);
    let head = standing.as_ref().and_then(head_of);
    declare(host, room, seats).await?;
    start_reading(host, room, title, head.as_ref(), &joining).await;
    Ok(())
}

/// The membership, published whole: the names and the ears in one payload.
async fn declare(host: &HostHandle, room: &SessionId, seats: &[Seat]) -> Result<(), KernelError> {
    host.extend(room, PLUGIN, room::MEMBERS, room::payload(seats))
        .await
}

/// The room's own journal, which is the one authority on who is in it, what it
/// is for and whether it still stands. A room that cannot be read is not one a
/// verb may write to blind, so this is where the three of them stop.
async fn read(host: &HostHandle, id: &SessionId, title: &str) -> Result<SessionState, KernelError> {
    room::read(host, id).await.ok_or_else(|| {
        KernelError::new(
            ErrorCode::SessionNotFound,
            format!("{title} could not be read"),
        )
    })
}

/// The names this call adds to the roster: the ones the room was not already
/// seating. A reseat is a roster and not a join, so a seat that was already
/// there keeps reading where it left off — including a seat that has read
/// nothing yet, whose backlog a restart must not sweep away.
fn joining(standing: Option<&SessionState>, seats: &[Seat]) -> Vec<String> {
    let held = standing.map(room::members_of).unwrap_or_default();
    seats
        .iter()
        .map(|seat| seat.name.clone())
        .filter(|seat| !held.iter().any(|member| name::same(member, seat)))
        .collect()
}

/// The roster a join leaves: everyone the room was already seating, each in the
/// place it had and wearing the ear this call gives it where it gives one, and
/// the names it adds after them (ADR-0053 §3).
fn joined(standing: &[Seat], joining: &[Seat]) -> Vec<Seat> {
    let mut roster = standing.to_vec();
    for seat in joining {
        match roster
            .iter_mut()
            .find(|held| name::same(&held.name, &seat.name))
        {
            Some(held) => held.ear = seat.ear,
            None => roster.push(seat.clone()),
        }
    }
    roster
}

/// The names a leave takes out, spelled as the roster spells them. A name the
/// room is not seating is refused rather than passed over: an unseat that
/// quietly did nothing would read exactly like one that worked.
fn leaving(seated: &[Seat], names: &[String], title: &str) -> Result<Vec<String>, KernelError> {
    names
        .iter()
        .map(|name| seated_as(seated, name, title))
        .collect()
}

fn seated_as(seated: &[Seat], name: &str, title: &str) -> Result<String, KernelError> {
    seated
        .iter()
        .find(|seat| name::same(&seat.name, name))
        .map(|seat| seat.name.clone())
        .ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                format!("{title} is not seating {name}; it seats {}", roster(seated)),
            )
        })
}

/// Whether a name is one of these.
fn holds(names: &[String], name: &str) -> bool {
    names.iter().any(|held| name::same(held, name))
}

/// The names a roster asks for, in the order it asks for them.
fn named(seats: &[Seat]) -> Vec<String> {
    seats.iter().map(|seat| seat.name.clone()).collect()
}

/// What the room is told a verb did, in the caller's own name (ADR-0053 §3).
fn moved(by: &str, verb: &str, names: &[String]) -> String {
    format!("{by} {verb} {}", names.join(", "))
}

/// The last line a room reads, which is the one that ends it.
fn closing(by: &str, title: &str, why: Option<&str>) -> String {
    match why {
        Some(why) => format!("{by} closed {title}: {why}"),
        None => format!("{by} closed {title}"),
    }
}

/// Nothing in a tree is taken away in silence (ADR-0053 §3): a seat that has
/// left is told once, by name, which room it was in and who took it out — so
/// the turn it is in ends rather than going on working for a room it can no
/// longer read.
async fn tell_the_unseated(host: &HostHandle, room: &Room, leaving: &[String], by: &str) {
    for member in leaving {
        let Some(seat) = post::seat_of(host, room, member).await else {
            tracing::debug!(room = %room.title, member, "nobody here answers to that name");
            continue;
        };
        post::nudge(host, &seat, &room.title, unseated(&room.title, by)).await;
    }
}

/// What that seat is told.
fn unseated(title: &str, by: &str) -> String {
    format!(
        "You were unseated from {title} by {by}. The room no longer reaches you and nothing is \
         owed for it; end your turn unless something of yours is unfinished."
    )
}

/// Where a seat joining a room starts reading (ADR-0034 §2): at the room's
/// head, so what was said before it was seated is not a backlog it owes. The
/// cursor is a register in the room's own journal, so a name nobody holds yet
/// is seated with one exactly like a name somebody does.
async fn start_reading(
    host: &HostHandle,
    room: &SessionId,
    title: &str,
    head: Option<&ItemId>,
    joining: &[String],
) {
    let Some(head) = head else {
        return;
    };
    for member in joining {
        if let Err(error) = cursor::advance(host, room, member, head).await {
            tracing::debug!(room = %title, member, %error, "a seat was not told where to start reading");
        }
    }
}

/// The last thing said in a room, which is where a seat joining it starts.
fn head_of(standing: &SessionState) -> Option<ItemId> {
    standing
        .items
        .iter()
        .filter_map(crate::mentions::Post::of)
        .next_back()
        .map(|post| post.id)
}

/// Retunings cleared, each where it was written — one register per seat — so a
/// `Listen` that lands beside this call is settled by journal order rather than
/// by clobbering a shared value. A reseat clears every one the room carries; an
/// unseat clears the ones that left with their seats.
async fn clear_ears(
    host: &HostHandle,
    room: &SessionId,
    members: &[String],
) -> Result<(), KernelError> {
    for member in members {
        host.extend(room, PLUGIN, &ear::kind(member), Value::Null)
            .await?;
    }
    Ok(())
}

/// What the caller is told once a room is seated: the room, and who is in it.
/// `/room` and the room's own verbs are the same act on one roster, so they all
/// say the same thing about it.
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

/// A room that has ended takes nothing more (ADR-0053 §4). Every door comes
/// through here — the person's, the agent's, and the three verbs — so a closed
/// room is refused in one wording and by one rule.
fn still_open(standing: Option<&SessionState>, title: &str) -> Result<(), KernelError> {
    match standing.is_some_and(room::closed_of) {
        true => Err(shut(title, standing.and_then(Closed::of_state))),
        false => Ok(()),
    }
}

/// What a door is told about a closed name.
fn shut(title: &str, closed: Option<Closed>) -> KernelError {
    KernelError::new(
        ErrorCode::InvalidInput,
        format!(
            "{title} is closed{}: a closed room is not reopened and its name is not taken \
             again. Open another name for the work that follows.",
            closed.map(ended_by).unwrap_or_default()
        ),
    )
}

/// The rest of that sentence, where the closing frame says anything.
fn ended_by(closed: Closed) -> String {
    match closed.why {
        Some(why) => format!(" — {} closed it: {why}", closed.by),
        None => format!(" — {} closed it", closed.by),
    }
}

/// What an agent is told when the name it asked for is one that stands
/// (ADR-0053 §2): what the room is for, who is in it, and the verb that does
/// what was meant.
fn taken(standing: Option<&SessionState>, title: &str) -> KernelError {
    if let Err(closed) = still_open(standing, title) {
        return closed;
    }
    let seats = standing.map(room::roster_of).unwrap_or_default();
    KernelError::new(
        ErrorCode::InvalidInput,
        format!(
            "{title} already stands, {}, seating {}. A room is opened for one purpose and its \
             name is not reopened: `Seat` adds to it, `Unseat` takes a name out of it, \
             `CloseRoom` ends it, and work of another kind is another room under another name.",
            for_what(standing.and_then(room::purpose_of).as_deref()),
            roster(&seats)
        ),
    )
}

/// What a room says it is for, inside a sentence about it.
fn for_what(purpose: Option<&str>) -> String {
    match purpose {
        Some(purpose) => format!("opened for {purpose}"),
        None => "opened without a purpose".to_string(),
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
async fn mint(
    host: &HostHandle,
    parent: &SessionId,
    cwd: &Path,
    name: &str,
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
        title: Some(name::title(name)),
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
    use crate::tests::Fleet;
    use bingo_sdk::{Delivery, Input};

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
        opened_for(fleet, parent, Opening::person(name, None), seats).await
    }

    async fn opened_for(
        fleet: &Fleet,
        parent: &SessionId,
        opening: Opening<'_>,
        seats: &[Seat],
    ) -> SessionId {
        seat(
            &fleet.handle(),
            parent,
            Path::new("/work/project"),
            opening,
            seats,
        )
        .await
        .expect("a room this crate can open")
    }

    /// The room as a verb is handed one: what its summary says, which is all
    /// `join`, `leave` and `close` are told before they read its journal.
    fn room_of(fleet: &Fleet, id: &SessionId) -> Room {
        Room::of(&fleet.summary(id)).expect("a room this crate can read")
    }

    /// What was said into the room: a post is a delivery into the room's own
    /// session, signed by whoever made it.
    fn posts(fleet: &Fleet, room: &SessionId) -> Vec<(String, Option<String>)> {
        fleet
            .delivered()
            .into_iter()
            .filter(|(to, ..)| to == room)
            .filter_map(|(_, input, _)| match input {
                Input::Text { text, origin, .. } => Some((text, origin.principal)),
                _ => None,
            })
            .collect()
    }

    /// What one session was told, and how.
    fn told(fleet: &Fleet, seat: &SessionId) -> Vec<(String, Delivery)> {
        fleet
            .delivered()
            .into_iter()
            .filter(|(to, ..)| to == seat)
            .filter_map(|(_, input, delivery)| match input {
                Input::Text { text, .. } => Some((text, delivery)),
                _ => None,
            })
            .collect()
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

    /// ADR-0053 §1: a room is opened for one purpose, and the frame that says
    /// so names whoever opened it. A person's door may give no purpose and
    /// still says the name, because that is what the roster rule reads.
    #[tokio::test]
    async fn a_new_room_says_what_it_is_for_and_who_opened_it() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let room = opened_for(
            &fleet,
            &root,
            Opening::person("design", Some("settle the storage layout")),
            &members(&["scout"]),
        )
        .await;
        assert_eq!(
            Opened::of_state(&fleet.state(&room)),
            Some(Opened {
                purpose: Some("settle the storage layout".into()),
                by: "parent".into(),
            })
        );
        assert_eq!(
            room_of(&fleet, &room).seated(&fleet.state(&room)).purpose,
            Some("settle the storage layout".into())
        );

        let bare = seated(&fleet, &root, "standup", &[]).await;
        assert_eq!(
            Opened::of_state(&fleet.state(&bare)),
            Some(Opened {
                purpose: None,
                by: "parent".into(),
            }),
            "a person may give none"
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
            Opening::person("two words", None),
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

    /// The agent's door (ADR-0053 §2): a name that stands is not reopened, and
    /// the refusal carries what stands and the verb that does what was meant.
    #[tokio::test]
    async fn the_agent_s_door_refuses_a_name_that_stands_and_names_the_verbs() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let opening = Opening {
            name: "design",
            purpose: Some("settle the storage layout"),
            by: "reviewer",
        };
        let room = open(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            opening,
            &members(&["scout"]),
        )
        .await
        .expect("a room an agent may open");
        assert_eq!(
            Opened::of_state(&fleet.state(&room)).map(|opened| opened.by),
            Some("reviewer".into()),
            "opened in the caller's own name"
        );

        let refused = open(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            opening,
            &members(&["reviewer"]),
        )
        .await
        .expect_err("a standing name is not reopened");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        for said in [
            "#design already stands",
            "settle the storage layout",
            "scout",
            "`Seat`",
            "`Unseat`",
            "`CloseRoom`",
        ] {
            assert!(refused.message.contains(said), "{said}: {refused}");
        }
        assert_eq!(fleet.created().len(), 1, "and nothing was opened");
        assert_eq!(fleet.members(&room), ["scout"], "nor reseated");
    }

    /// A room a person opened without a purpose still refuses the agent's
    /// door, and says it has none rather than pretending to one.
    #[tokio::test]
    async fn a_standing_room_that_was_opened_without_a_purpose_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        seated(&fleet, &root, "design", &[]).await;
        let refused = open(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            Opening::person("design", Some("something else")),
            &[],
        )
        .await
        .expect_err("a standing name");
        assert!(
            refused.message.contains("opened without a purpose"),
            "{refused}"
        );
        assert!(refused.message.contains("nobody yet"), "{refused}");
    }

    /// A room that has ended is reopened by no door, the person's included
    /// (ADR-0053 §4).
    #[tokio::test]
    async fn a_closed_name_is_refused_at_both_doors() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated(&fleet, &root, "design", &["scout"]).await;
        close(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            "parent",
            Some("it shipped"),
        )
        .await
        .expect("a room this crate can close");

        for refused in [
            seat(
                &fleet.handle(),
                &root,
                Path::new("/work/project"),
                Opening::person("design", None),
                &members(&["scout"]),
            )
            .await
            .expect_err("a person's door"),
            open(
                &fleet.handle(),
                &root,
                Path::new("/work/project"),
                Opening::person("design", Some("again")),
                &[],
            )
            .await
            .expect_err("an agent's door"),
        ] {
            assert_eq!(refused.code, ErrorCode::InvalidInput);
            assert!(refused.message.contains("#design is closed"), "{refused}");
            assert!(refused.message.contains("it shipped"), "{refused}");
            assert!(
                refused.message.contains("not reopened"),
                "the rule is said: {refused}"
            );
        }
        assert_eq!(fleet.created().len(), 1, "and nothing was opened");
        assert_eq!(fleet.members(&id), ["scout"], "nor reseated");
    }

    /// ADR-0053 §3: the roster is derived from the standing one and published
    /// whole, the room reads that it happened, and the seat that joins starts
    /// at the head the room had.
    #[tokio::test]
    async fn a_join_appends_to_the_standing_roster_and_the_room_reads_it() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated_with(&fleet, &root, "design", &[Seat::named("reviewer")]).await;
        fleet.post(
            &id,
            "the build is green",
            Some("reviewer"),
            crate::tests::ts(),
        );
        let head = fleet.state(&id).items.last().map(|item| item.id.clone());

        let roster = join(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            &[Seat::live("scout"), Seat::named("watcher")],
            "parent",
        )
        .await
        .expect("a join this crate can make");

        assert_eq!(
            roster.iter().map(Seat::said).collect::<Vec<String>>(),
            ["reviewer", "scout:0", "watcher"],
            "the standing seat keeps its place and the new ones follow it"
        );
        assert_eq!(fleet.members(&id), ["reviewer", "scout", "watcher"]);
        assert_eq!(
            posts(&fleet, &id),
            [(
                "parent seated scout, watcher".to_string(),
                Some("parent".to_string())
            )],
            "the room reads who joined, in the caller's name"
        );
        assert_eq!(
            cursor::of_state(&fleet.state(&id), "scout"),
            head,
            "a seat joins at the head the room had"
        );
        assert_eq!(
            cursor::of_state(&fleet.state(&id), "reviewer"),
            None,
            "and a seat that was already there keeps reading where it was"
        );
    }

    /// A name the room already seats keeps its place; where the call gives it
    /// an ear, the ear is what changes.
    #[tokio::test]
    async fn a_join_that_names_a_standing_seat_retunes_it_rather_than_seating_it_twice() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated_with(&fleet, &root, "design", &[Seat::named("scout")]).await;

        let roster = join(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            &[Seat::live("Scout")],
            "parent",
        )
        .await
        .expect("a join");
        assert_eq!(roster, [Seat::live("scout")], "one seat, in its own place");
        assert_eq!(fleet.members(&id), ["scout"]);
        assert_eq!(fleet.ears(&id).of("scout"), Ear::Live);
    }

    /// The other half of §3: the roster goes out without the names, what they
    /// retuned goes with them, the room reads it, and each of them is told.
    #[tokio::test]
    async fn an_unseat_publishes_the_roster_without_the_name_and_tells_the_seat() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let id = seated(&fleet, &root, "design", &["scout", "reviewer"]).await;
        fleet
            .handle()
            .extend(
                &id,
                PLUGIN,
                &ear::kind("scout"),
                ear::register(Ear::Patient(ear::FLOOR)),
            )
            .await
            .expect("a seat retunes its own ear");

        let roster = leave(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            &["Scout".to_string()],
            "parent",
        )
        .await
        .expect("an unseat this crate can make");

        assert_eq!(roster, [Seat::live("reviewer")]);
        assert_eq!(fleet.members(&id), ["reviewer"]);
        assert!(
            fleet.ears(&id).retuned().is_empty(),
            "the retuning left with the seat"
        );
        assert_eq!(
            posts(&fleet, &id),
            [(
                "parent unseated scout".to_string(),
                Some("parent".to_string())
            )],
            "spelled as the roster spelled it"
        );
        assert_eq!(
            told(&fleet, &scout),
            [(
                "You were unseated from #design by parent. The room no longer reaches you and \
                 nothing is owed for it; end your turn unless something of yours is unfinished."
                    .to_string(),
                Delivery::Wake
            )]
        );
    }

    #[tokio::test]
    async fn an_unseat_of_a_name_the_room_is_not_seating_is_refused_and_changes_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated(&fleet, &root, "design", &["scout"]).await;
        let refused = leave(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            &["reviewer".to_string()],
            "parent",
        )
        .await
        .expect_err("a name the room is not seating");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(
            refused.message.contains("#design is not seating reviewer"),
            "{refused}"
        );
        assert!(refused.message.contains("scout"), "{refused}");
        assert_eq!(fleet.members(&id), ["scout"], "and the roster stands");
        assert!(posts(&fleet, &id).is_empty(), "nothing was said in it");
    }

    /// ADR-0053 §4: a last line the caller signs, and then the frame that ends
    /// it. The post is first, so it lands while the room still takes one.
    #[tokio::test]
    async fn a_close_posts_the_last_line_and_then_says_the_room_has_ended() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated(&fleet, &root, "design", &["scout"]).await;

        close(
            &fleet.handle(),
            &id,
            &room_of(&fleet, &id),
            "parent",
            Some("it shipped"),
        )
        .await
        .expect("a room this crate can close");

        assert_eq!(
            posts(&fleet, &id),
            [(
                "parent closed #design: it shipped".to_string(),
                Some("parent".to_string())
            )]
        );
        let state = fleet.state(&id);
        assert!(room::closed_of(&state));
        let closed = Closed::of_state(&state).expect("what the closing said");
        assert_eq!(closed.by, "parent");
        assert_eq!(closed.why.as_deref(), Some("it shipped"));
        assert!(
            room_of(&fleet, &id).seated(&state).closed,
            "and the room reads as closed"
        );
    }

    #[tokio::test]
    async fn a_close_without_a_reason_says_only_that_it_closed() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated(&fleet, &root, "design", &[]).await;
        let room = room_of(&fleet, &id);
        close(&fleet.handle(), &id, &room, "reviewer", None)
            .await
            .expect("a room this crate can close");
        assert_eq!(
            posts(&fleet, &id),
            [(
                "reviewer closed #design".to_string(),
                Some("reviewer".to_string())
            )]
        );
        assert_eq!(
            Closed::of_state(&fleet.state(&id)).and_then(|closed| closed.why),
            None
        );
    }

    /// A closed room takes no verb either: there is nothing left to seat, to
    /// unseat or to close again.
    #[tokio::test]
    async fn a_closed_room_takes_no_verb() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = seated(&fleet, &root, "design", &["scout"]).await;
        let room = room_of(&fleet, &id);
        let host = fleet.handle();
        close(&host, &id, &room, "parent", None)
            .await
            .expect("a room this crate can close");
        let after = posts(&fleet, &id).len();

        let refusals = [
            join(&host, &id, &room, &members(&["reviewer"]), "parent")
                .await
                .expect_err("seating a closed room")
                .message,
            leave(&host, &id, &room, &["scout".to_string()], "parent")
                .await
                .expect_err("unseating in a closed room")
                .message,
            close(&host, &id, &room, "parent", None)
                .await
                .expect_err("closing it twice")
                .message,
        ];
        for refused in refusals {
            assert!(refused.contains("#design is closed"), "{refused}");
        }
        assert_eq!(fleet.members(&id), ["scout"], "the roster is as it was");
        assert_eq!(posts(&fleet, &id).len(), after, "and nothing more was said");
    }
}
