//! `ScheduleForget`: remove one entry. Destructive, and the card shows the
//! file that would go — there is no cap on entries and no gc (ADR-0019),
//! so this and the visible table are the whole of the pruning.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Preview, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::schedules::Schedules;
use crate::{diff, tools};

const DESCRIPTION: &str = "\
Delete one schedule, so it never fires again. The turns it has already run \
are transcripts of their own and are not touched. Use it for a schedule that \
was wrong; a schedule that should stop for now is one to delete and write \
again, since there is nothing else to remember about it.";

#[derive(Debug, Deserialize, JsonSchema)]
struct Forget {
    /// The schedule to delete; a unique prefix of its id is enough.
    id: String,
}

/// Removes one file.
#[derive(Debug)]
pub struct ScheduleForgetTool {
    schedules: Arc<Schedules>,
}

impl ScheduleForgetTool {
    pub fn new(schedules: Arc<Schedules>) -> Self {
        Self { schedules }
    }
}

#[async_trait]
impl Tool for ScheduleForgetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ScheduleForget".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Forget>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::destructive()
    }

    /// What would go: the whole file, as a removal.
    fn preview(&self, input: &Value, _cwd: &Path) -> Option<Preview> {
        let args: Forget = serde_json::from_value(input.clone()).ok()?;
        let shelf = self.schedules.store().load();
        let entry = tools::find(&shelf, &args.id).ok()?;
        Some(Preview::Diff {
            unified: diff::unified(
                &self.schedules.store().path(&entry.id),
                &entry.document().ok()?,
                "",
            ),
        })
    }

    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: Forget =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let shelf = self.schedules.store().load();
        let entry = match tools::find(&shelf, &args.id) {
            Ok(entry) => entry,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        self.schedules
            .store()
            .delete(&entry.id)
            .map_err(tools::failed)?;
        self.schedules.changed();
        Ok(ToolOutput::text(format!(
            "Forgot {}: {}, {}",
            entry.id,
            entry.spec,
            crate::render::head(&entry.text, 48)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;
    use crate::entry::tests::entry;
    use crate::tests::{Fixture, files, text};
    use serde_json::json;

    fn shelved(fixture: &Fixture, ids: &[&str]) {
        for id in ids {
            fixture
                .schedules
                .store()
                .save(&Entry {
                    id: (*id).to_string(),
                    ..entry()
                })
                .expect("an entry");
        }
    }

    async fn forget(fixture: &Fixture, id: &str) -> ToolOutput {
        ScheduleForgetTool::new(fixture.schedules.clone())
            .call(json!({ "id": id }), &fixture.context())
            .await
            .expect("an answer")
    }

    #[tokio::test]
    async fn one_entry_goes_and_the_others_stay() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111", "bbbb2222"]);
        let out = forget(&fixture, "aaaa").await;
        assert!(!out.is_error, "{out:?}");
        assert!(text(&out).contains("aaaa1111"), "{}", text(&out));
        assert_eq!(files(&fixture.dir()), ["bbbb2222.json"]);
    }

    #[tokio::test]
    async fn an_ambiguous_prefix_deletes_nothing() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111", "aaaa2222"]);
        let out = forget(&fixture, "aaaa").await;
        assert!(out.is_error);
        assert!(text(&out).contains("Give more of the id"), "{}", text(&out));
        assert_eq!(files(&fixture.dir()).len(), 2);
    }

    #[tokio::test]
    async fn an_id_nobody_has_is_an_answer_and_not_a_failure() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111"]);
        let out = forget(&fixture, "zzzz").await;
        assert!(out.is_error);
        assert!(text(&out).contains("ScheduleList"), "{}", text(&out));
        assert_eq!(files(&fixture.dir()).len(), 1);
    }

    #[test]
    fn the_card_shows_the_file_that_would_go() {
        let fixture = Fixture::new();
        shelved(&fixture, &["aaaa1111"]);
        let tool = ScheduleForgetTool::new(fixture.schedules.clone());
        let Some(Preview::Diff { unified }) = tool.preview(&json!({"id": "aaaa"}), &fixture.cwd())
        else {
            panic!("a deletion shows what goes");
        };
        assert!(unified.contains("aaaa1111.json"), "{unified}");
        assert!(unified.contains(r#"-  "spec": "every 30m""#), "{unified}");
        assert_eq!(files(&fixture.dir()).len(), 1, "a preview never writes");
    }

    #[test]
    fn the_spec_is_destructive() {
        let fixture = Fixture::new();
        let tool = ScheduleForgetTool::new(fixture.schedules.clone());
        assert_eq!(tool.spec().name, "ScheduleForget");
        let traits = tool.traits(&Value::Null);
        assert!(traits.destructive && traits.trusted);
        assert!(!traits.read_only && !traits.edit);
    }
}
