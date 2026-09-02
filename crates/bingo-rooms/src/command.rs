//! `/room`: the rooms under this session, and the one word that opens another.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, HostHandle, KernelError,
    SessionFilter, SessionId, View,
};
use jiff::Timestamp;

use crate::ear::Seat;
use crate::room::{self, Room};
use crate::{mentions, name, owed, seat};

const HEADERS: [&str; 3] = ["room", "members", "owed"];

/// What a session with no rooms in it is told, which is also where a person
/// meets the holder's seat (ADR-0028) and the ear it can wear (ADR-0029).
const NONE: &str = "no rooms here; `/room <name> [member…]` opens one — name \
`parent` among the members to read the room yourself, and to owe an answer to a \
post that says `@parent`. A member reads the room at the head of its next turn: \
a bare `name` is woken when a post says `@name`, and once when the room has \
stood unread for 300s. Write `name:120` to say how long it may stand instead, \
or `name:0` for a seat every post wakes as it lands";

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
                hint: "[<name> [member…]]".into(),
            },
            // Opening a room touches nothing a running turn is using.
            instant: true,
            family: "rooms".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let mut words = args.split_whitespace();
        let Some(name) = words.next() else {
            return list(cx).await;
        };
        let seats: Vec<Seat> = words.map(Seat::read).collect::<Result<_, _>>()?;
        seat::seat(&cx.host, &cx.session, &cx.cwd, name, &seats).await?;
        Ok(CommandOutcome::Applied {
            message: Some(seat::receipt(&name::title(name), &seats)),
        })
    }
}

/// Every room under this session, and who is in each.
async fn list(cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let rooms = under(&cx.host, &cx.session).await?;
    if rooms.is_empty() {
        return Ok(CommandOutcome::View {
            view: View::Text { text: NONE.into() },
        });
    }
    let now = Timestamp::now();
    let mut rows = Vec::with_capacity(rooms.len());
    for (id, room) in rooms {
        let read = room::read(&cx.host, &id).await;
        let seats = read.as_ref().map(room::roster_of).unwrap_or_default();
        let open = read.as_ref().map(mentions::of_state).unwrap_or_default();
        rows.push(vec![
            room.title,
            seats
                .iter()
                .map(Seat::said)
                .collect::<Vec<String>>()
                .join(", "),
            owed::column(&open, now),
        ]);
    }
    Ok(CommandOutcome::View {
        view: View::Table {
            headers: HEADERS.map(str::to_string).to_vec(),
            rows,
        },
    })
}

/// The rooms a session holds, as the host lists its children.
async fn under(
    host: &HostHandle,
    session: &SessionId,
) -> Result<Vec<(SessionId, Room)>, KernelError> {
    let children = host
        .sessions(SessionFilter {
            parent: Some(session.clone()),
            ..SessionFilter::default()
        })
        .await?;
    Ok(children
        .into_iter()
        .filter_map(|child| Room::of(&child).map(|room| (child.id, room)))
        .collect())
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
            [["#design", "reviewer", ""], ["#standup", "", ""]],
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
        assert_eq!(rows[0][2], "scout 2m");
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
        assert_eq!(rows[0][1], "scout, parent:120");
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
        assert!(NONE.contains("`parent` among the members"), "{NONE}");
        assert!(NONE.contains("`@parent`"), "{NONE}");
        assert!(NONE.contains("stood unread for 300s"), "{NONE}");
        assert!(NONE.contains("`name:120`"), "{NONE}");
        assert!(NONE.contains("`name:0`"), "{NONE}");
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
