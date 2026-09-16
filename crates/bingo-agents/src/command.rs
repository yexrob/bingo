//! `/agents`: the sub-agents this session started, in the same columns the
//! model reads from `ListAgents`; and the person's spelling of the two
//! verbs that end one — `/agents stop <name>`, `/agents dismiss <name>`
//! (ADR-0060). A command is the person's own lever and passes no gate.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, KernelError, View,
};

use crate::{dismiss, list, names, stop};

/// The two words after `/agents` that do something.
const STOP: &str = "stop";
const DISMISS: &str = "dismiss";

#[derive(Debug, Default, Clone, Copy)]
pub struct AgentsCommand;

#[async_trait]
impl Command for AgentsCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "agents".into(),
            aliases: Vec::new(),
            hint: "the sub-agents this session started, or stop or dismiss one".into(),
            args: ArgSpec::Free {
                hint: format!("[{STOP} <name> | {DISMISS} <name>]"),
            },
            // Reading the session tree touches nothing a turn is using, and
            // the two verbs reach another session's actor, never this one's.
            instant: true,
            family: "agents".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let mut words = args.split_whitespace();
        let Some(verb) = words.next() else {
            return roster(cx).await;
        };
        let name = words.next();
        let message = match verb {
            STOP => stop::stop(&cx.host, &cx.session, named(verb, name)?).await?,
            DISMISS => dismiss::dismiss(&cx.host, &cx.session, named(verb, name)?).await?,
            other => {
                return Err(KernelError::new(
                    ErrorCode::InvalidInput,
                    format!(
                        "`/agents {other}` is not a verb: `/agents {STOP} <name>` or `/agents {DISMISS} <name>`"
                    ),
                ));
            }
        };
        Ok(CommandOutcome::Applied {
            message: Some(message),
        })
    }
}

/// The name a verb needs, or the sentence that says it does.
fn named<'a>(verb: &str, name: Option<&'a str>) -> Result<&'a str, KernelError> {
    name.ok_or_else(|| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!("`/agents {verb} <name>` needs the agent to {verb}"),
        )
    })
}

/// The table, or the sentence that says there is nobody to put in it.
async fn roster(cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let children = names::agents(&cx.host, &cx.session).await?;
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

    #[tokio::test]
    async fn dismiss_deletes_an_idle_child_and_stop_refuses_one() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        let cx = command_context(&root, &fleet);

        let error = AgentsCommand
            .run("stop reviewer", &cx)
            .await
            .expect_err("nothing is running");
        assert!(error.message.contains("reviewer is idle"), "{error}");

        let outcome = AgentsCommand
            .run("dismiss reviewer", &cx)
            .await
            .expect("an idle child is dismissed");
        let CommandOutcome::Applied { message } = outcome else {
            panic!("a receipt");
        };
        assert!(
            message
                .unwrap_or_default()
                .starts_with("reviewer dismissed"),
            "the same receipt the tool gives"
        );
        assert_eq!(fleet.deleted(), vec![reviewer]);
    }

    #[tokio::test]
    async fn a_verb_without_a_name_and_a_word_that_is_no_verb_are_told_the_shape() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let cx = command_context(&root, &fleet);
        let error = AgentsCommand.run("stop", &cx).await.expect_err("no name");
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(error.message.contains("/agents stop <name>"), "{error}");
        let error = AgentsCommand
            .run("fire reviewer", &cx)
            .await
            .expect_err("no such verb");
        assert!(error.message.contains("is not a verb"), "{error}");
    }

    #[test]
    fn the_spec_runs_now_and_names_the_two_verbs() {
        let spec = AgentsCommand.spec();
        assert_eq!(spec.name, "agents");
        assert!(spec.instant, "reading the tree never waits for a turn");
        assert_eq!(
            spec.args,
            ArgSpec::Free {
                hint: "[stop <name> | dismiss <name>]".into()
            }
        );
        assert_eq!(spec.family, "agents");
    }
}
