//! `/room`: the rooms under this session, the one word that opens another, and
//! the one that ends one.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, HostHandle,
    KernelError, SessionId, View,
};
use jiff::Timestamp;

use crate::ear::Seat;
use crate::room::{self, CLOSED, Room};
use crate::seat::Opening;
use crate::{door, mentions, name, owed, seat};

/// The columns a person reads a listing in; the page names each of them.
pub(crate) const HEADERS: [&str; 4] = ["room", "purpose", "members", "owed"];

/// What a session with no rooms in it is told, which is also where a person
/// meets the holder's seat (ADR-0028), the ear it can wear (ADR-0029) and the
/// word that ends a room (ADR-0053 §4).
const NONE: &str = "no rooms here; `/room <name> [member…]` opens one and \
`/room close <name>` ends one — name `parent` among the members to read the room \
yourself, and to owe an answer to a post that says `@parent`. A member reads the \
room at the head of its next turn: a bare `name` is woken when a post says \
`@name`, and once when the room has stood unread for 300s. Write `name:120` to \
say how long it may stand instead, or `name:0` for a seat every post wakes as it \
lands";

#[derive(Debug, Default, Clone, Copy)]
pub struct RoomCommand;

#[async_trait]
impl Command for RoomCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "room".into(),
            aliases: Vec::new(),
            hint: "the rooms under this session, or open one".into(),
            args: ArgSpec::Free {
                hint: "[<name> [member…] | close <name>]".into(),
            },
            // Opening a room touches nothing a running turn is using.
            instant: true,
            family: "rooms".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let mut words = args.split_whitespace();
        let Some(word) = words.next() else {
            return list(cx).await;
        };
        if name::same(word, name::CLOSE) {
            return ended(cx, words.next()).await;
        }
        let seats: Vec<Seat> = words.map(Seat::read).collect::<Result<_, _>>()?;
        let opening = Opening::person(word, None);
        seat::seat(&cx.host, &cx.session, &cx.cwd, opening, &seats).await?;
        Ok(CommandOutcome::Applied {
            message: Some(seat::receipt(&name::title(word), &seats)),
        })
    }
}

/// `/room close <name>`: the person's spelling of `CloseRoom` (ADR-0053 §4).
/// A person closes a room of their own; the rooms further down the tree are
/// closed by whoever holds or opened them.
async fn ended(cx: &CommandContext, name: Option<&str>) -> Result<CommandOutcome, KernelError> {
    let Some(name) = name else {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("`/room {} <name>` needs the room to close", name::CLOSE),
        ));
    };
    let title = name::title(name::check(name)?);
    let rooms = room::under(&cx.host, &cx.session).await?;
    let Some((id, room)) = rooms.iter().find(|(_, room)| room.title == title).cloned() else {
        return Err(door::unreachable(&title, &rooms));
    };
    seat::close(&cx.host, &id, &room, name::PARENT, None).await?;
    Ok(CommandOutcome::Applied {
        message: Some(format!("{title}: {CLOSED}")),
    })
}

/// Every room under this session, and who is in each.
async fn list(cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let rooms = room::under(&cx.host, &cx.session).await?;
    if rooms.is_empty() {
        return Ok(CommandOutcome::View {
            view: View::Text { text: NONE.into() },
        });
    }
    let now = Timestamp::now();
    let mut rows = Vec::with_capacity(rooms.len());
    for (id, room) in rooms {
        rows.push(row(&cx.host, &id, &room, now).await);
    }
    Ok(CommandOutcome::View {
        view: View::Table {
            headers: HEADERS.map(str::to_string).to_vec(),
            rows,
        },
    })
}

/// One room's line: what it is for, who is in it, and what it owes — all four
/// read off the one snapshot, so no column can disagree with another. A room
/// that has ended seats nobody any more and owes nothing (ADR-0053 §4), so the
/// column that would name its members says that instead.
async fn row(host: &HostHandle, id: &SessionId, room: &Room, now: Timestamp) -> Vec<String> {
    let read = room::read(host, id).await;
    let purpose = read.as_ref().and_then(room::purpose_of).unwrap_or_default();
    if read.as_ref().is_some_and(room::closed_of) {
        return vec![
            room.title.clone(),
            purpose,
            CLOSED.to_string(),
            String::new(),
        ];
    }
    let seats = read.as_ref().map(room::roster_of).unwrap_or_default();
    let open = read.as_ref().map(mentions::of_state).unwrap_or_default();
    vec![
        room.title.clone(),
        purpose,
        seats
            .iter()
            .map(Seat::said)
            .collect::<Vec<String>>()
            .join(", "),
        owed::column(&open, now),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, command_context};

    async fn typed(fleet: &Fleet, session: &SessionId, args: &str) -> CommandOutcome {
        RoomCommand
            .run(args, &command_context(session, fleet))
            .await
            .expect("a command this crate can run")
    }

    #[tokio::test]
    async fn opening_a_room_acks_with_the_room_and_who_is_in_it() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let outcome = typed(&fleet, &root, "design reviewer scout").await;
        assert_eq!(
            outcome,
            CommandOutcome::Applied {
                message: Some("#design: reviewer, scout".into())
            }
        );
        let room = fleet.titled("#design").expect("the room was opened");
        assert_eq!(fleet.members(&room), ["reviewer", "scout"]);
    }

    #[tokio::test]
    async fn a_room_nobody_is_in_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        assert_eq!(
            typed(&fleet, &root, "  design  ").await,
            CommandOutcome::Applied {
                message: Some("#design: nobody yet".into())
            }
        );
    }

    #[tokio::test]
    async fn the_table_names_every_room_under_this_session_and_its_members() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        typed(&fleet, &root, "design reviewer").await;
        typed(&fleet, &root, "standup").await;

        let CommandOutcome::View {
            view: View::Table { headers, rows },
        } = typed(&fleet, &root, "").await
        else {
            panic!("a roster is a table");
        };
        assert_eq!(headers, HEADERS);
        assert_eq!(
            rows,
            [["#design", "", "reviewer", ""], ["#standup", "", "", ""]],
            "a room nobody has asked anything in owes nothing"
        );
    }

    #[tokio::test]
    async fn the_owed_column_names_who_has_not_answered_and_for_how_long() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "scout");
        typed(&fleet, &root, "design scout").await;
        let room = fleet.titled("#design").expect("the room was opened");
        let asked = Timestamp::now() - jiff::SignedDuration::from_secs(120);
        fleet.post(&room, "@scout what does the log say?", None, asked);

        let CommandOutcome::View {
            view: View::Table { rows, .. },
        } = typed(&fleet, &root, "").await
        else {
            panic!("a roster is a table");
        };
        assert_eq!(rows[0][3], "scout 2m");
    }

    #[tokio::test]
    async fn a_session_with_no_rooms_says_so_in_one_line() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        assert_eq!(
            typed(&fleet, &root, "").await,
            CommandOutcome::View {
                view: View::Text { text: NONE.into() }
            },
            "an agent under this session is not a room"
        );
    }

    /// The patience door, end to end: the roster takes it, the journal keeps
    /// it, and the receipt and the listing both show it (ADR-0029 §2). A bare
    /// name is the default and reads back bare (ADR-0034 §6).
    #[tokio::test]
    async fn a_member_with_a_patience_beside_it_is_seated_wearing_that_ear() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "scout");
        assert_eq!(
            typed(&fleet, &root, "design scout parent:120").await,
            CommandOutcome::Applied {
                message: Some("#design: scout, parent:120".into())
            }
        );

        let room = fleet.titled("#design").expect("the room was opened");
        assert_eq!(fleet.members(&room), ["scout", "parent"]);
        assert_eq!(
            fleet.ears(&room).of("parent"),
            crate::ear::Ear::Patient(std::time::Duration::from_secs(120))
        );
        assert_eq!(
            fleet.ears(&room).of("scout"),
            crate::ear::Ear::default(),
            "a bare name took the default"
        );

        let CommandOutcome::View {
            view: View::Table { rows, .. },
        } = typed(&fleet, &root, "").await
        else {
            panic!("a roster is a table");
        };
        assert_eq!(rows[0][2], "scout, parent:120");
    }

    #[tokio::test]
    async fn a_patience_under_the_floor_is_refused_and_opens_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let error = RoomCommand
            .run("design parent:15", &command_context(&root, &fleet))
            .await
            .expect_err("the dead band");
        assert_eq!(error.code, bingo_sdk::ErrorCode::InvalidInput);
        assert!(
            error.message.contains("under thirty seconds of patience"),
            "{error}"
        );
        assert!(fleet.created().is_empty(), "nothing was opened");
    }

    /// What a person is told naming `parent` gets them (ADR-0028 §1–3), and
    /// what the number beside a name does (ADR-0029 §2, ADR-0034 §6).
    #[test]
    fn the_listing_says_what_seating_the_holder_gets_you() {
        assert!(NONE.contains("`/room close <name>` ends one"), "{NONE}");
        assert!(NONE.contains("`parent` among the members"), "{NONE}");
        assert!(NONE.contains("`@parent`"), "{NONE}");
        assert!(NONE.contains("stood unread for 300s"), "{NONE}");
        assert!(NONE.contains("`name:120`"), "{NONE}");
        assert!(NONE.contains("`name:0`"), "{NONE}");
    }

    /// ADR-0053 §1: a room a person opens may have a purpose, and the listing
    /// is where they read it back.
    #[tokio::test]
    async fn the_listing_says_what_each_room_is_for() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "scout");
        seat::seat(
            &fleet.handle(),
            &root,
            std::path::Path::new("/work/project"),
            Opening::person("design", Some("settle the storage layout")),
            &[Seat::live("scout")],
        )
        .await
        .expect("a room this crate can open");

        let CommandOutcome::View {
            view: View::Table { rows, .. },
        } = typed(&fleet, &root, "").await
        else {
            panic!("a roster is a table");
        };
        assert_eq!(
            rows,
            [["#design", "settle the storage layout", "scout:0", ""]]
        );
    }

    /// ADR-0053 §4: `/room close <name>` is the person's spelling of the verb.
    /// The room ends, the listing says so, and nothing is reopened under it.
    #[tokio::test]
    async fn closing_a_room_ends_it_and_the_listing_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        typed(&fleet, &root, "design scout").await;
        assert_eq!(
            typed(&fleet, &root, "close design").await,
            CommandOutcome::Applied {
                message: Some("#design: closed".into())
            }
        );

        let room = fleet.titled("#design").expect("the room");
        assert!(crate::room::closed_of(&fleet.state(&room)));
        let CommandOutcome::View {
            view: View::Table { rows, .. },
        } = typed(&fleet, &root, "").await
        else {
            panic!("a roster is a table");
        };
        assert_eq!(
            rows,
            [["#design", "", "closed", ""]],
            "a closed room seats nobody and owes nothing"
        );

        let error = RoomCommand
            .run("design scout", &command_context(&root, &fleet))
            .await
            .expect_err("a closed name is not reopened");
        assert!(error.message.contains("#design is closed"), "{error}");
    }

    #[tokio::test]
    async fn closing_needs_a_name_and_a_room_that_is_there() {
        let fleet = Fleet::default();
        let root = fleet.root();
        typed(&fleet, &root, "design").await;

        let bare = RoomCommand
            .run("close", &command_context(&root, &fleet))
            .await
            .expect_err("close what?");
        assert_eq!(bare.code, bingo_sdk::ErrorCode::InvalidInput);
        assert!(bare.message.contains("`/room close <name>`"), "{bare}");

        let missing = RoomCommand
            .run("close standup", &command_context(&root, &fleet))
            .await
            .expect_err("no such room");
        assert!(
            missing.message.contains("no #standup you can reach"),
            "{missing}"
        );
        assert!(missing.message.contains("#design"), "{missing}");
    }

    #[tokio::test]
    async fn a_name_that_is_not_one_is_refused_and_opens_nothing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let error = RoomCommand
            .run("de/sign reviewer", &command_context(&root, &fleet))
            .await
            .expect_err("a room name is one word, no slashes");
        assert_eq!(error.code, bingo_sdk::ErrorCode::InvalidInput);
        assert!(fleet.created().is_empty());
    }

    #[test]
    fn the_spec_runs_now_and_takes_a_name_and_names() {
        let spec = RoomCommand.spec();
        assert_eq!(spec.name, "room");
        assert!(spec.instant, "opening a room never waits for a turn");
        assert_eq!(spec.family, "rooms");
        assert!(matches!(spec.args, ArgSpec::Free { .. }));
    }
}
