//! `/experience`: the library as a person reads it — one row per entry, in
//! the order the prompt's index shows them, and a line for any file that was
//! meant to be an entry and could not be read.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError, View};
use jiff::Timestamp;

use crate::render;
use crate::store::{Library, Shelf};

/// What a person is told when this project has taught the agent nothing yet.
const NONE: &str = "no experience for this project yet";

#[derive(Debug)]
pub struct ExperienceCommand {
    library: Arc<Library>,
}

impl ExperienceCommand {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl Command for ExperienceCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "experience".into(),
            aliases: Vec::new(),
            hint: "what this project has taught the agent".into(),
            args: ArgSpec::None,
            // Reading a directory of small files touches nothing a turn is using.
            instant: true,
            family: "experience".into(),
        }
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let shelf = self.library.load(&cx.cwd);
        Ok(CommandOutcome::View {
            view: view(&shelf, Timestamp::now()),
        })
    }
}

fn view(shelf: &Shelf, now: Timestamp) -> View {
    let mut parts = Vec::new();
    if shelf.is_empty() {
        parts.push(View::Text { text: NONE.into() });
    } else {
        parts.push(View::Table {
            headers: render::HEADERS.map(str::to_string).to_vec(),
            rows: render::by_worth(shelf.entries.iter())
                .into_iter()
                .map(|entry| render::row(entry, now))
                .collect(),
        });
    }
    parts.extend(unreadable(shelf));
    match parts.len() {
        1 => parts.remove(0),
        _ => View::Stack { children: parts },
    }
}

/// A file that was meant to be an entry says so here, and nowhere else: a
/// store is hand-editable, and a silent skip is how a person loses one.
fn unreadable(shelf: &Shelf) -> Option<View> {
    if shelf.unreadable.is_empty() {
        return None;
    }
    let lines = shelf
        .unreadable
        .iter()
        .map(|bad| format!("{}: {}", bad.file, bad.why))
        .collect::<Vec<_>>()
        .join("\n");
    Some(View::Text {
        text: format!(
            "{} file(s) could not be read:\n{lines}",
            shelf.unreadable.len()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::entry::{Entry, Outcome, Record, Status};
    use crate::tests::Fixture;

    fn shelved(fixture: &Fixture, entry: Entry) {
        fixture
            .library
            .save(&fixture.cwd(), &entry)
            .expect("an entry");
    }

    async fn table(fixture: &Fixture) -> View {
        let outcome = ExperienceCommand::new(fixture.library.clone())
            .run("", &fixture.command())
            .await
            .expect("a view");
        match outcome {
            CommandOutcome::View { view } => view,
            other => panic!("/experience answers a view, not {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_library_is_one_line() {
        let fixture = Fixture::new();
        assert_eq!(table(&fixture).await, View::Text { text: NONE.into() });
    }

    #[tokio::test]
    async fn every_entry_is_a_row_in_the_order_the_prompt_shows_them() {
        let fixture = Fixture::new();
        shelved(
            &fixture,
            Entry {
                id: "aaaa1111".into(),
                ..entry()
            },
        );
        shelved(
            &fixture,
            Entry {
                id: "bbbb2222".into(),
                summary: "restart the daemon".into(),
                status: Status::Retired,
                outcomes: vec![Record {
                    outcome: Outcome::Helpful,
                    at: Timestamp::UNIX_EPOCH,
                    evidence: "it came back".into(),
                }],
                ..entry()
            },
        );
        let View::Table { headers, rows } = table(&fixture).await else {
            panic!("a library is a table");
        };
        assert_eq!(headers, render::HEADERS);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0][..4],
            [
                "aaaa1111",
                "active",
                "clear the target directory",
                "+0 / -0"
            ],
            "an active entry comes before a retired one with more to show for it"
        );
        assert_eq!(
            rows[1][..4],
            ["bbbb2222", "retired", "restart the daemon", "+1 / -0"]
        );
    }

    #[tokio::test]
    async fn a_file_that_could_not_be_read_is_said_out_loud() {
        let fixture = Fixture::new();
        shelved(&fixture, entry());
        std::fs::write(fixture.dir().join("broken.md"), "not an entry\n").expect("wrote");

        let View::Stack { children } = table(&fixture).await else {
            panic!("a shelf with a broken file is a table and a notice");
        };
        assert!(matches!(children[0], View::Table { .. }));
        let View::Text { text } = &children[1] else {
            panic!("the notice is text");
        };
        assert!(text.contains("broken.md"), "{text}");
        assert!(text.contains("frontmatter"), "{text}");
    }

    #[test]
    fn the_spec_runs_now_and_takes_nothing() {
        let fixture = Fixture::new();
        let spec = ExperienceCommand::new(fixture.library.clone()).spec();
        assert_eq!(spec.name, "experience");
        assert!(spec.instant, "reading a directory never waits for a turn");
        assert_eq!(spec.args, ArgSpec::None);
        assert_eq!(spec.family, "experience");
    }
}
