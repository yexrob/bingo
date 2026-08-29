//! `/agents`: the sub-agents this session started, in the same columns the
//! model reads from `ListAgents`.

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError, View};

use crate::{list, names};

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentsCommand;

#[async_trait]
impl Command for AgentsCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "agents".into(),
            aliases: Vec::new(),
            hint: "the sub-agents this session started".into(),
            args: ArgSpec::None,
            // Reading the session tree touches nothing a turn is using.
            instant: true,
            family: "agents".into(),
        }
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let children = names::children(&cx.host, &cx.session).await?;
        if children.is_empty() {
            return Ok(CommandOutcome::Applied {
                message: Some("no agents are running in this session".into()),
            });
        }
        Ok(CommandOutcome::View {
            view: View::Table {
                headers: list::HEADERS.map(str::to_string).to_vec(),
                rows: list::rows(&children),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, command_context};

    #[tokio::test]
    async fn the_table_names_every_child_and_what_it_is_doing() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);
        fleet.child(&root, "scout");

        let outcome = AgentsCommand
            .run("", &command_context(&root, &fleet))
            .await
            .expect("a table");
        let CommandOutcome::View {
            view: View::Table { headers, rows },
        } = outcome
        else {
            panic!("a roster is a table");
        };
        assert_eq!(headers, list::HEADERS);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "reviewer");
        assert_eq!(rows[0][1], reviewer.to_string());
        assert_eq!(rows[0][2], "busy");
        assert_eq!(rows[1][2], "idle");
    }

    #[tokio::test]
    async fn a_session_that_started_nothing_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let outcome = AgentsCommand
            .run("", &command_context(&root, &fleet))
            .await
            .expect("a message");
        assert_eq!(
            outcome,
            CommandOutcome::Applied {
                message: Some("no agents are running in this session".into())
            }
        );
    }

    #[test]
    fn the_spec_runs_now_and_takes_nothing() {
        let spec = AgentsCommand.spec();
        assert_eq!(spec.name, "agents");
        assert!(spec.instant, "reading the tree never waits for a turn");
        assert_eq!(spec.args, ArgSpec::None);
        assert_eq!(spec.family, "agents");
    }
}
