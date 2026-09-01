//! `/room`: the rooms under this session, and the one word that opens another.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, HostHandle, KernelError,
    OpenOptions, SessionFilter, SessionId, SessionSelector, SessionState, View,
};
use jiff::Timestamp;

use crate::room::{self, Room};
use crate::{identity, mentions, name, owed, seat};

const HEADERS: [&str; 3] = ["room", "members", "owed"];

/// What a session with no rooms in it is told, which is also where a person
/// meets the holder's seat (ADR-0028).
const NONE: &str = "no rooms here; `/room <name> [member…]` opens one — name \
`parent` among the members to hear every post yourself, quietly, and be woken \
by one that says `@parent`";

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
        let members: Vec<String> = words.map(str::to_string).collect();
        seat::seat(&cx.host, &cx.session, &cx.cwd, name, &members).await?;
        Ok(CommandOutcome::Applied {
            message: Some(seat::receipt(&name::title(name), &members)),
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
        let read = read(&cx.host, &id).await;
        let members = read.as_ref().map(room::members_of).unwrap_or_default();
        let open = read.as_ref().map(mentions::of_state).unwrap_or_default();
        rows.push(vec![
            room.title,
            members.join(", "),
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

/// A room as its own journal has it: who is in it, and what its posts owe.
/// Both are read where they live rather than from the hook's fold, so `/room`
/// answers from the one fact and a room a hook never saw a frame of still
/// says what it has. A room that cannot be read says nothing rather than
/// guessing.
async fn read(host: &HostHandle, room: &SessionId) -> Option<SessionState> {
    let opened = host
        .open(
            SessionSelector::ById { id: room.clone() },
            identity(),
            OpenOptions::default(),
        )
        .await;
    opened.ok().map(|attachment| attachment.snapshot)
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

    /// What a person is told naming `parent` gets them (ADR-0028 §1–3).
    #[test]
    fn the_listing_says_what_seating_the_holder_gets_you() {
        assert!(NONE.contains("`parent` among the members"), "{NONE}");
        assert!(NONE.contains("quietly"), "{NONE}");
        assert!(NONE.contains("`@parent`"), "{NONE}");
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
