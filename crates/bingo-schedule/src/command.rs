//! `/schedule`: the store as a person reads it — one row per entry, and the
//! line that says whether anything here will fire them (ADR-0019 §7).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError};
use jiff::tz::TimeZone;

use crate::render;
use crate::schedules::Schedules;

#[derive(Debug)]
pub struct ScheduleCommand {
    schedules: Arc<Schedules>,
}

impl ScheduleCommand {
    pub fn new(schedules: Arc<Schedules>) -> Self {
        Self { schedules }
    }
}

#[async_trait]
impl Command for ScheduleCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "schedule".into(),
            aliases: vec!["schedules".into()],
            hint: "what fires later, and whether anything will fire it".into(),
            args: ArgSpec::None,
            // Reading a directory of small files touches nothing a turn is
            // using.
            instant: true,
            family: "schedule".into(),
        }
    }

    async fn run(&self, _args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        Ok(CommandOutcome::View {
            view: render::view(
                &self.schedules.store().load(),
                &self.schedules.holder(),
                self.schedules.trouble().as_deref(),
                &TimeZone::system(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;
    use crate::entry::tests::entry;
    use crate::tests::Fixture;
    use bingo_sdk::View;

    async fn view(fixture: &Fixture) -> View {
        let outcome = ScheduleCommand::new(fixture.schedules.clone())
            .run("", &fixture.command())
            .await
            .expect("a view");
        match outcome {
            CommandOutcome::View { view } => view,
            other => panic!("/schedule answers a view, not {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_empty_store_folds_to_two_lines() {
        let fixture = Fixture::new();
        assert_eq!(
            view(&fixture).await.fold(),
            "no schedules yet\nschedules: dormant — no runner holds this store"
        );
    }

    #[tokio::test]
    async fn every_entry_is_a_row_and_the_holder_is_named_under_them() {
        let fixture = Fixture::new();
        for id in ["bbbb2222", "aaaa1111"] {
            fixture
                .schedules
                .store()
                .save(&Entry {
                    id: id.into(),
                    ..entry()
                })
                .expect("an entry");
        }
        let folded = view(&fixture).await.fold();
        let rows: Vec<&str> = folded.lines().collect();
        assert!(rows[0].starts_with("id · spec · next fire"), "{folded}");
        assert!(rows[1].starts_with("aaaa1111 · every 30m · "), "{folded}");
        assert!(rows[2].starts_with("bbbb2222 · every 30m · "), "{folded}");
        assert!(rows[3].starts_with("schedules: dormant"), "{folded}");
    }

    #[test]
    fn the_spec_runs_now_and_takes_nothing() {
        let fixture = Fixture::new();
        let spec = ScheduleCommand::new(fixture.schedules.clone()).spec();
        assert_eq!(spec.name, "schedule");
        assert_eq!(spec.aliases, ["schedules"]);
        assert!(spec.instant, "reading a directory never waits for a turn");
        assert_eq!(spec.args, ArgSpec::None);
        assert_eq!(spec.family, "schedule");
    }
}
