//! `/team`: the roles this project declares, and which of them are seated.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError, SessionSummary,
    View,
};

use crate::team::file::{self, Role};
use crate::{list, names};

/// The columns a team has: what was declared, and what is running.
pub const HEADERS: [&str; 4] = ["role", "agent", "session", "state"];

/// A role nobody has seated yet has no session to name.
const NOT_SEATED: &str = "not seated";

/// A role that names no definition and is its own.
const NO_DEFINITION: &str = "-";

#[derive(Debug, Default, Clone, Copy)]
pub struct TeamCommand;

#[async_trait]
impl Command for TeamCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "team".into(),
            aliases: Vec::new(),
            hint: "the roles this project seats".into(),
            args: ArgSpec::None,
            // Reading a file and the session tree touches nothing a turn uses.
            instant: true,
            family: "agents".into(),
        }
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let Some(team) = file::of(&cx.cwd)? else {
            return Ok(CommandOutcome::View {
                view: View::Text {
                    text: format!(
                        "This project seats no team; {} would name its roles.",
                        file::path_in(&cx.cwd).display()
                    ),
                },
            });
        };
        let seated = names::children(&cx.host, &cx.session).await?;
        Ok(CommandOutcome::View {
            view: View::Table {
                headers: HEADERS.map(str::to_string).to_vec(),
                rows: rows(&team.roles, &seated),
            },
        })
    }
}

fn rows(roles: &[Role], seated: &[SessionSummary]) -> Vec<Vec<String>> {
    roles
        .iter()
        .map(|role| row(role, names::named(seated, &role.name).as_ref()))
        .collect()
}

/// A role as a row: what it was declared as, and the session seating it here.
fn row(role: &Role, seated: Option<&SessionSummary>) -> Vec<String> {
    vec![
        role.name.clone(),
        role.agent.clone().unwrap_or_else(|| NO_DEFINITION.into()),
        seated.map(|s| s.id.to_string()).unwrap_or_default(),
        seated.map_or(NOT_SEATED, list::state).to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Tree, command_context};

    const TWO: &str = r#"{"roles":[
        { "name": "reviewer", "agent": "reviewer" },
        { "name": "scout" }
    ]}"#;

    async fn run(cwd: &std::path::Path, fleet: &Fleet, session: &bingo_sdk::SessionId) -> View {
        let cx = CommandContext {
            cwd: cwd.to_path_buf(),
            ..command_context(session, fleet)
        };
        match TeamCommand.run("", &cx).await.expect("a view") {
            CommandOutcome::View { view } => view,
            other => panic!("a team is a view: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_table_names_every_declared_role_and_what_is_seated() {
        let tree = Tree::new();
        let cwd = tree.team("work", TWO);
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.set_busy(&reviewer, true);

        let View::Table { headers, rows } = run(&cwd, &fleet, &root).await else {
            panic!("a team is a table");
        };
        assert_eq!(headers, HEADERS);
        assert_eq!(
            rows,
            [
                vec![
                    "reviewer".to_string(),
                    "reviewer".into(),
                    reviewer.to_string(),
                    "busy".into()
                ],
                vec![
                    "scout".to_string(),
                    "-".into(),
                    String::new(),
                    "not seated".into()
                ],
            ]
        );
    }

    #[tokio::test]
    async fn a_project_with_no_team_file_is_told_where_one_would_be() {
        let tree = Tree::new();
        let fleet = Fleet::default();
        let root = fleet.root();
        let View::Text { text } = run(&tree.cwd(), &fleet, &root).await else {
            panic!("a project with no team says so in one line");
        };
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains(".bingo/team.json"), "{text}");
    }

    #[test]
    fn the_spec_runs_now_and_takes_nothing() {
        let spec = TeamCommand.spec();
        assert_eq!(spec.name, "team");
        assert!(spec.instant, "reading a file never waits for a turn");
        assert_eq!(spec.args, ArgSpec::None);
        assert_eq!(spec.family, "agents");
    }
}
