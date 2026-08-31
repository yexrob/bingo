//! `ScheduleList`: the store as the model reads it — the same rows
//! `/schedule` shows a person, and the same line about who runs them.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::render;
use crate::schedules::Schedules;

const DESCRIPTION: &str = "\
Every schedule there is: its id, when it fires, when it next fires, whether \
it is enabled, and the head of what it says. The id is what ScheduleForget \
takes. It also says whether any process is running them — a schedule in a \
store nobody runs is a file, not an appointment.";

/// Listing takes nothing. The type exists so the model is handed a schema
/// rather than a free-form object.
#[derive(Debug, Deserialize, JsonSchema)]
struct List {}

#[derive(Debug)]
pub struct ScheduleListTool {
    schedules: Arc<Schedules>,
}

impl ScheduleListTool {
    pub fn new(schedules: Arc<Schedules>) -> Self {
        Self { schedules }
    }
}

#[async_trait]
impl Tool for ScheduleListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ScheduleList".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<List>(),
            meta: Default::default(),
        }
    }

    /// Reading a directory of small files touches nothing anyone else is
    /// using, and nothing outside the process.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    async fn call(&self, _input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let shelf = self.schedules.store().load();
        let view = render::view(&shelf, &self.schedules.holder(), &TimeZone::system());
        let mut out = ToolOutput::text(view.fold());
        out.display = Some(view);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::tests::entry;
    use crate::tests::{Fixture, text};
    use serde_json::json;

    async fn list(fixture: &Fixture) -> ToolOutput {
        ScheduleListTool::new(fixture.schedules.clone())
            .call(json!({}), &fixture.context())
            .await
            .expect("a listing")
    }

    #[tokio::test]
    async fn an_empty_store_says_so_and_says_who_would_run_it() {
        let fixture = Fixture::new();
        let said = text(&list(&fixture).await);
        assert!(said.contains("no schedules yet"), "{said}");
        assert!(said.contains("schedules: dormant"), "{said}");
    }

    #[tokio::test]
    async fn every_entry_is_a_row_the_model_can_name() {
        let fixture = Fixture::new();
        fixture
            .schedules
            .store()
            .save(&entry())
            .expect("an entry on the shelf");
        let out = list(&fixture).await;
        let said = text(&out);
        assert!(said.contains("abcd1234"), "{said}");
        assert!(said.contains("every 30m"), "{said}");
        assert!(
            said.contains("check whether the nightly build is green"),
            "{said}"
        );
        assert!(
            matches!(out.display, Some(bingo_sdk::View::Stack { .. })),
            "a surface draws the table it folded"
        );
    }

    #[test]
    fn the_spec_is_read_only() {
        let fixture = Fixture::new();
        let tool = ScheduleListTool::new(fixture.schedules.clone());
        assert_eq!(tool.spec().name, "ScheduleList");
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && traits.concurrency_safe);
        assert!(!traits.destructive && !traits.edit);
    }
}
