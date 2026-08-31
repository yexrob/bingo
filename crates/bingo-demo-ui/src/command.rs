//! The three commands the board answers to. `/board` is typed; `board.tick`
//! and `board.reset` are what the buttons fire, and a button fires a command
//! by name through `Input::Action` (ADR-0008 §1, ADR-0013 §3) — the same
//! entry a person's line uses, so nothing new reaches the kernel.

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError};

use crate::board::{Board, RESET, TICK};
use crate::journal;

/// What a command does to the board before publishing it again.
#[derive(Clone, Copy, Debug)]
enum Change {
    /// Publish whatever is there, so a session that has none gets one.
    Show,
    Tick,
    Reset,
}

/// One command: its name, what it says in the menu, and what it changes.
#[derive(Clone, Copy, Debug)]
pub struct BoardCommand {
    name: &'static str,
    hint: &'static str,
    change: Change,
}

/// `/board`: put the board on the screen, making one if this session has none.
pub const SHOW: BoardCommand = BoardCommand {
    name: "board",
    hint: "the demo board (ADR-0013)",
    change: Change::Show,
};

/// What the `Tick` button fires.
pub const TICK_COMMAND: BoardCommand = BoardCommand {
    name: TICK,
    hint: "move the board on one",
    change: Change::Tick,
};

/// What the `Reset` button fires; it also takes the progress bar off the rail.
pub const RESET_COMMAND: BoardCommand = BoardCommand {
    name: RESET,
    hint: "start the board over",
    change: Change::Reset,
};

#[async_trait]
impl Command for BoardCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.name.into(),
            aliases: Vec::new(),
            hint: self.hint.into(),
            args: ArgSpec::None,
            // Reading and publishing touch nothing a turn is using.
            instant: true,
            family: "demo".into(),
        }
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let board = self.changed(cx).await?;
        journal::write(&cx.host, &cx.session, &board).await?;
        Ok(CommandOutcome::Applied {
            message: Some(self.said()),
        })
    }
}

impl BoardCommand {
    /// The board this command leaves behind.
    async fn changed(&self, cx: &CommandContext) -> Result<Board, KernelError> {
        if let Change::Reset = self.change {
            // A signal is removed by publishing nothing under its kind.
            cx.host
                .signal(
                    &cx.session,
                    journal::PLUGIN,
                    journal::PROGRESS,
                    serde_json::Value::Null,
                )
                .await?;
            return Ok(Board::default());
        }
        let mut board = journal::read(&cx.host, &cx.session).await?;
        if let Change::Tick = self.change {
            board.tick();
        }
        Ok(board)
    }

    fn said(&self) -> String {
        match self.change {
            Change::Show => "board published (ctrl+t to pin it)".into(),
            Change::Tick => "board moved on".into(),
            Change::Reset => "board reset".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::State;
    use crate::tests::{Journals, command_context};

    #[tokio::test]
    async fn showing_the_board_publishes_a_fresh_one_and_tick_moves_it_on() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = command_context(&session, &journals);

        let said = SHOW.run("", &cx).await.expect("published");
        assert_eq!(
            said,
            CommandOutcome::Applied {
                message: Some("board published (ctrl+t to pin it)".into())
            }
        );
        let board = journal::read(&journals.handle(), &session)
            .await
            .expect("read back");
        assert_eq!(board, Board::default());

        TICK_COMMAND.run("", &cx).await.expect("ticked");
        let moved = journal::read(&journals.handle(), &session)
            .await
            .expect("read back");
        assert_eq!(moved.rows[0].state, State::Running);
        assert_ne!(moved, board, "the table a person sees changed");
    }

    #[tokio::test]
    async fn a_reset_starts_over_and_takes_the_progress_bar_away() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = command_context(&session, &journals);
        SHOW.run("", &cx).await.expect("published");
        TICK_COMMAND.run("", &cx).await.expect("ticked");

        RESET_COMMAND.run("", &cx).await.expect("reset");
        assert_eq!(
            journal::read(&journals.handle(), &session)
                .await
                .expect("read back"),
            Board::default()
        );
        assert_eq!(
            journals.signals(),
            vec![(journal::PROGRESS.to_string(), serde_json::Value::Null)],
            "a null payload is what removes a live kind"
        );
    }

    #[test]
    fn every_command_runs_now_and_takes_nothing() {
        for command in [SHOW, TICK_COMMAND, RESET_COMMAND] {
            let spec = command.spec();
            assert!(spec.instant, "publishing never waits for a turn");
            assert_eq!(spec.args, ArgSpec::None);
            assert_eq!(spec.family, "demo");
        }
        assert_eq!(TICK_COMMAND.spec().name, TICK, "the button fires by name");
    }
}
