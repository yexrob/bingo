//! `ExperienceOutcome`: what happened when the playbook was followed. One
//! record, evidence required, and the status is never touched — recording an
//! outcome can promote nothing, which is the whole of the anti-self-
//! confirmation policy (ADR-0014).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Interrupt, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::entry::{Outcome, Record};
use crate::store::Library;
use crate::{render, tools};

const DESCRIPTION: &str = "\
Record what happened when you followed an experience: `helpful` if it worked, \
`harmful` if it made things worse. Evidence is required and must be something \
a person could check — the command that went green, the error that came back, \
the file that changed. One record per time you actually followed it; this \
never changes an entry's status, and an entry you disagree with is revised or \
retired with ExperienceCommit, not voted down.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Outcomes {
    /// The entry you followed; a unique prefix of its id is enough.
    id: String,
    outcome: Outcome,
    /// What a person could check to see this is true.
    evidence: String,
}

/// Appends one record to one entry.
#[derive(Debug)]
pub struct ExperienceOutcomeTool {
    library: Arc<Library>,
}

impl ExperienceOutcomeTool {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl Tool for ExperienceOutcomeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExperienceOutcome".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Outcomes>(),
            meta: Default::default(),
        }
    }

    /// A write, but never an edit to anything a person is working on: the
    /// gate asks in every mode but `bypassPermissions`, which is the point —
    /// an outcome a person never saw is a self-confirmation.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            trusted: true,
            interrupt: Interrupt::Cancel,
            ..ToolTraits::default()
        }
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: Outcomes =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let evidence = args.evidence.trim();
        if evidence.is_empty() {
            return Err(ToolError::InvalidInput(
                "evidence is required: say what a person could check".into(),
            ));
        }
        let shelf = self.library.load(&cx.cwd);
        let mut entry = match tools::find(&shelf.entries, &args.id) {
            Ok(entry) => entry.clone(),
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        entry.outcomes.push(Record {
            outcome: args.outcome,
            at: Timestamp::now(),
            evidence: evidence.to_string(),
        });
        self.library.save(&cx.cwd, &entry).map_err(tools::failed)?;
        Ok(ToolOutput::text(format!(
            "Recorded {} for {}",
            args.outcome.as_str(),
            render::line_with_status(&entry)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::entry::{Entry, Status};
    use crate::tests::{Fixture, text};
    use serde_json::json;

    fn shelved(fixture: &Fixture, entry: Entry) -> Entry {
        fixture
            .library
            .save(&fixture.cwd(), &entry)
            .expect("an entry");
        entry
    }

    async fn record(fixture: &Fixture, input: Value) -> Result<ToolOutput, ToolError> {
        ExperienceOutcomeTool::new(fixture.library.clone())
            .call(input, &fixture.context())
            .await
    }

    #[tokio::test]
    async fn a_record_is_appended_and_counted_but_never_written_as_a_count() {
        let fixture = Fixture::new();
        shelved(&fixture, entry());
        let out = record(
            &fixture,
            json!({"id": "abcd", "outcome": "helpful", "evidence": "cargo build went green"}),
        )
        .await
        .expect("a record");
        assert!(!out.is_error, "{out:?}");
        assert!(text(&out).contains("helpful 1"), "{}", text(&out));

        let entry = &fixture.shelf().entries[0];
        assert_eq!(entry.outcomes.len(), 1);
        assert_eq!(entry.outcomes[0].evidence, "cargo build went green");
        assert_eq!(entry.helpful(), 1);
        let file = std::fs::read_to_string(fixture.library.path(&fixture.cwd(), &entry.id))
            .expect("the file");
        assert!(!file.contains("helpful:"), "{file}");
    }

    #[tokio::test]
    async fn recording_an_outcome_never_changes_the_status() {
        let fixture = Fixture::new();
        for status in [Status::Active, Status::Retired] {
            let entry = shelved(&fixture, Entry { status, ..entry() });
            record(
                &fixture,
                json!({"id": &entry.id, "outcome": "harmful", "evidence": "it broke the build"}),
            )
            .await
            .expect("a record");
            assert_eq!(fixture.shelf().entries[0].status, status);
        }
    }

    #[tokio::test]
    async fn evidence_is_required() {
        let fixture = Fixture::new();
        shelved(&fixture, entry());
        for input in [
            json!({"id": "abcd", "outcome": "helpful"}),
            json!({"id": "abcd", "outcome": "helpful", "evidence": "   "}),
        ] {
            let error = record(&fixture, input).await.expect_err("no evidence");
            assert!(matches!(error, ToolError::InvalidInput(_)), "{error:?}");
        }
        assert!(fixture.shelf().entries[0].outcomes.is_empty());
    }

    #[tokio::test]
    async fn an_id_nobody_has_is_an_answer() {
        let fixture = Fixture::new();
        shelved(&fixture, entry());
        let out = record(
            &fixture,
            json!({"id": "zzzz", "outcome": "helpful", "evidence": "it worked"}),
        )
        .await
        .expect("an answer");
        assert!(out.is_error);
        assert!(text(&out).contains("ExperienceQuery"), "{}", text(&out));
        assert!(fixture.shelf().entries[0].outcomes.is_empty());
    }

    #[test]
    fn the_spec_is_a_write_a_person_sees() {
        let fixture = Fixture::new();
        let tool = ExperienceOutcomeTool::new(fixture.library.clone());
        assert_eq!(tool.spec().name, "ExperienceOutcome");
        let traits = tool.traits(&Value::Null);
        assert!(traits.trusted);
        assert!(!traits.read_only && !traits.edit && !traits.destructive);
    }
}
