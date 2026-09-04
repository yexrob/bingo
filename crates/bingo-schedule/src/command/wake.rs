//! `/wake`: what the model set itself to come back to, and the person's way
//! to end it (ADR-0019 §8).
//!
//! The store is the record: this reads it, and `off` takes one file away.
//! There is nothing here to keep in step with the status line, which reads
//! the same pending wake published from the same entry.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError, View};
use jiff::tz::TimeZone;

use crate::entry::Entry;
use crate::schedules::Schedules;
use crate::{render, wake};

/// The one word this command takes.
const OFF: &str = "off";

/// What a session with nothing pending says.
const NONE: &str = "no wake is standing on this session";

#[derive(Debug)]
pub struct WakeCommand {
    schedules: Arc<Schedules>,
}

impl WakeCommand {
    pub fn new(schedules: Arc<Schedules>) -> Self {
        Self { schedules }
    }

    /// The wake standing on this session, read from the store every time.
    fn standing(&self, cx: &CommandContext) -> Option<Entry> {
        let shelf = self.schedules.store().load();
        wake::pending(&shelf, &cx.session).cloned()
    }

    /// When it comes and what it will say — the two things a person wants of
    /// a wake they did not set themselves.
    fn shown(&self, standing: Option<Entry>) -> View {
        let Some(entry) = standing else {
            return View::Text { text: NONE.into() };
        };
        View::KeyValue {
            rows: vec![
                (
                    "when".into(),
                    render::when(entry.next_fire(&TimeZone::system()).as_ref()),
                ),
                ("note".into(), entry.text),
            ],
        }
    }

    /// End it: one file goes, and the pending wake is taken back from every
    /// surface reading it.
    async fn off(&self, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let Some(entry) = self.standing(cx) else {
            return Ok(applied(NONE.to_string()));
        };
        if let Err(e) = self.schedules.store().delete(&entry.id) {
            return Ok(applied(format!("the wake is still there: {e}")));
        }
        self.schedules.changed();
        wake::publish(&cx.host, &cx.session, None).await;
        Ok(applied(format!(
            "the wake set for {} is off",
            render::when(entry.next_fire(&TimeZone::system()).as_ref())
        )))
    }
}

fn applied(message: String) -> CommandOutcome {
    CommandOutcome::Applied {
        message: Some(message),
    }
}

#[async_trait]
impl Command for WakeCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "wake".into(),
            aliases: Vec::new(),
            hint: "the wake the model set, and `off` to end it".into(),
            args: ArgSpec::Free { hint: OFF.into() },
            // Not read-only, and instant anyway: a person ending a loop must
            // be able to end it while the loop is running, and waiting for
            // the turn to end is exactly the moment the next wake comes. What
            // it touches is one file in the agent's own store, which no turn
            // holds open.
            instant: true,
            family: "schedule".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        match args.trim() {
            "" => Ok(CommandOutcome::View {
                view: self.shown(self.standing(cx)),
            }),
            OFF => self.off(cx).await,
            other => Ok(applied(format!(
                "`/wake {other}` is not a word here: `/wake` shows the wake that stands, \
                 `/wake {OFF}` ends it"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::tests::Fixture;
    use bingo_sdk::SessionId;
    use jiff::{SignedDuration, Timestamp};

    /// A wake standing on the fixture's own session, five minutes out.
    fn standing(fixture: &Fixture) -> Entry {
        let entry = wake::entry(
            "aaaa1111".into(),
            &fixture.command().session,
            &fixture.cwd(),
            "look at the build again".into(),
            Timestamp::now(),
            SignedDuration::from_mins(5),
        );
        fixture.schedules.store().save(&entry).expect("a wake");
        entry
    }

    async fn run(fixture: &Fixture, args: &str) -> CommandOutcome {
        WakeCommand::new(fixture.schedules.clone())
            .run(args, &fixture.command())
            .await
            .expect("an outcome")
    }

    fn folded(outcome: CommandOutcome) -> String {
        match outcome {
            CommandOutcome::View { view } => view.fold(),
            CommandOutcome::Applied { message } => message.unwrap_or_default(),
            other => panic!("/wake says a view or a message, not {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_session_with_nothing_standing_says_so() {
        let fixture = Fixture::new();
        assert_eq!(folded(run(&fixture, "").await), NONE);
        assert_eq!(folded(run(&fixture, OFF).await), NONE);
    }

    #[tokio::test]
    async fn the_pending_wake_is_shown_with_when_it_comes_and_what_it_says() {
        let fixture = Fixture::new();
        standing(&fixture);
        let shown = folded(run(&fixture, "").await);
        assert!(shown.contains("look at the build again"), "{shown}");
        assert!(shown.contains("when"), "{shown}");
    }

    #[tokio::test]
    async fn off_takes_the_file_away_and_leaves_the_rest_of_the_store_alone() {
        let fixture = Fixture::new();
        standing(&fixture);
        fixture
            .schedules
            .store()
            .save(&entry())
            .expect("a schedule of a person's own");
        let said = folded(run(&fixture, " off ").await);
        assert!(said.starts_with("the wake set for"), "{said}");
        assert_eq!(
            fixture.shelf().ids(),
            ["abcd1234"],
            "a person's own schedule is not a wake"
        );
        assert_eq!(
            fixture.host.extended().last().map(|told| told.3.clone()),
            Some(serde_json::Value::Null),
            "and the pending wake is taken back from the status line"
        );
    }

    #[tokio::test]
    async fn another_sessions_wake_is_not_this_ones() {
        let fixture = Fixture::new();
        let theirs = wake::entry(
            "bbbb2222".into(),
            &SessionId::from_raw("ses_elsewhere"),
            &fixture.cwd(),
            "not yours".into(),
            Timestamp::now(),
            SignedDuration::from_mins(5),
        );
        fixture.schedules.store().save(&theirs).expect("a wake");
        assert_eq!(folded(run(&fixture, "").await), NONE);
        assert_eq!(fixture.shelf().ids(), ["bbbb2222"], "and off leaves it");
    }

    #[tokio::test]
    async fn a_word_it_does_not_know_changes_nothing_and_says_what_it_takes() {
        let fixture = Fixture::new();
        standing(&fixture);
        let said = folded(run(&fixture, "never").await);
        assert!(said.contains("/wake off"), "{said}");
        assert_eq!(fixture.shelf().ids(), ["aaaa1111"], "nothing was ended");
    }

    #[test]
    fn the_spec_runs_now_and_takes_one_word() {
        let fixture = Fixture::new();
        let spec = WakeCommand::new(fixture.schedules.clone()).spec();
        assert_eq!(spec.name, "wake");
        assert!(spec.aliases.is_empty());
        assert!(spec.instant, "a loop is ended while it is running");
        assert_eq!(spec.args, ArgSpec::Free { hint: OFF.into() });
        assert_eq!(spec.family, "schedule");
    }
}
