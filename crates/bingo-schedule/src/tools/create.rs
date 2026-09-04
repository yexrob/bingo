//! `ScheduleCreate`: write one entry down. There is no propose tool —
//! `preview` renders the file this call would write, so the permission card
//! *is* the proposal (ADR-0019 §6).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Preview, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::entry::Entry;
use crate::schedules::Schedules;
use crate::{diff, render, tools};

/// What a file with no id yet is shown as on the card: the id is minted
/// when the entry is written, not when the card is drawn.
const UNNAMED: &str = "<new>";

const DESCRIPTION: &str = "\
Set work to happen later, or over and over: `every 30m`, `daily at 09:00` \
(the machine's own clock), `once at 2026-09-01T09:00:00-07:00`. When it \
fires, `text` is delivered as a turn on a session of the schedule's own — a \
prompt, or a `/command` line — and everything it does lands in that \
session's transcript. Nobody is watching it, so write a `text` that stands \
on its own and needs no answer from anyone. Schedules fire only while a \
bingo process is running; there is no daemon. The person sees the entry \
this would write before it is written.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Create {
    /// When: `every <n>s|m|h`, `daily at HH:MM` in local time, or `once at
    /// <RFC3339>`. Cron expressions are not a schedule here.
    spec: String,
    /// What to say when it fires: a prompt, or a `/command` line. It reads
    /// nothing of this conversation.
    text: String,
    /// The directory the scheduled turn works in; this session's by default.
    #[serde(default)]
    cwd: Option<PathBuf>,
    /// What the scheduled turn does when no rule decides: `default`,
    /// `acceptEdits`, `plan`, `bypassPermissions` or `dontAsk`. Nobody is
    /// there to answer a prompt, so `default` declines what would have
    /// asked.
    #[serde(default)]
    permission_mode: Option<String>,
}

impl Create {
    /// The entry this call would write, or why it is not one. The id is not
    /// minted here: a preview writes nothing, so it names nothing.
    fn entry(self, cwd: &Path, now: Timestamp) -> Result<Entry, String> {
        let text = self.text.trim().to_string();
        if text.is_empty() {
            return Err("a schedule with nothing to say would fire and do nothing".into());
        }
        Ok(Entry {
            id: UNNAMED.into(),
            spec: self
                .spec
                .parse()
                .map_err(|e: crate::SpecError| e.to_string())?,
            text,
            cwd: self.cwd.unwrap_or_else(|| cwd.to_path_buf()),
            // A schedule of a person's own fires on a session of its own.
            permission_mode: self.permission_mode,
            enabled: true,
            created: now,
            last_fired: None,
        })
    }
}

/// Writes one entry, and shows the file it would write first.
#[derive(Debug)]
pub struct ScheduleCreateTool {
    schedules: Arc<Schedules>,
}

impl ScheduleCreateTool {
    pub fn new(schedules: Arc<Schedules>) -> Self {
        Self { schedules }
    }

    fn planned(&self, input: &Value, cwd: &Path) -> Option<Entry> {
        serde_json::from_value::<Create>(input.clone())
            .ok()?
            .entry(cwd, Timestamp::now())
            .ok()
    }

    /// The file as it would read, against the empty page it is written on.
    fn unified(&self, entry: &Entry) -> String {
        diff::unified(
            &self.schedules.store().path(&entry.id),
            "",
            &entry.document().unwrap_or_default(),
        )
    }

    /// What the model is told: the id it can name this by, when it fires,
    /// and whether anything in this process will fire it.
    fn receipt(&self, entry: &Entry) -> String {
        format!(
            "Scheduled {}: {}, next {}. Schedules here are {}.",
            entry.id,
            entry.spec,
            render::when(entry.next_fire(&TimeZone::system()).as_ref()),
            self.schedules.holder()
        )
    }
}

#[async_trait]
impl Tool for ScheduleCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ScheduleCreate".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Create>(),
            meta: Default::default(),
        }
    }

    /// A write a person approves once, and `acceptEdits` takes as read: it
    /// touches one file of the agent's own and nothing else. What the
    /// scheduled turn then does is gated in that turn.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::edit()
    }

    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> {
        let unified = self.unified(&self.planned(input, cwd)?);
        (!unified.is_empty()).then_some(Preview::Diff { unified })
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: Create =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let mut entry = match args.entry(&cx.cwd, Timestamp::now()) {
            Ok(entry) => entry,
            // A spec the grammar cannot read is something the model rewrites.
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        entry.id = self.schedules.store().mint();
        self.schedules.store().save(&entry).map_err(tools::failed)?;
        self.schedules.changed();
        let mut out = ToolOutput::text(self.receipt(&entry));
        out.display = Some(bingo_sdk::View::Code {
            lang: Some("json".into()),
            text: entry.document().unwrap_or_default(),
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fixture, files, text};
    use serde_json::json;

    fn nightly() -> Value {
        json!({
            "spec": "daily at 09:00",
            "text": "check whether the nightly build is green",
        })
    }

    async fn create(fixture: &Fixture, input: Value) -> ToolOutput {
        ScheduleCreateTool::new(fixture.schedules.clone())
            .call(input, &fixture.context())
            .await
            .expect("a create")
    }

    #[tokio::test]
    async fn a_schedule_becomes_one_file_named_by_its_minted_id() {
        let fixture = Fixture::new();
        let out = create(&fixture, nightly()).await;
        assert!(!out.is_error, "{out:?}");
        let shelf = fixture.shelf();
        assert_eq!(shelf.entries.len(), 1);
        let entry = &shelf.entries[0];
        assert_eq!(entry.id.chars().count(), 8, "{}", entry.id);
        assert_eq!(entry.spec.to_string(), "daily at 09:00");
        assert_eq!(entry.cwd, fixture.cwd(), "this session's directory");
        assert!(entry.enabled && entry.last_fired.is_none());
        assert_eq!(files(&fixture.dir()), [format!("{}.json", entry.id)]);
        assert!(text(&out).contains(&entry.id), "{}", text(&out));
    }

    #[tokio::test]
    async fn the_receipt_says_when_it_fires_and_whether_anything_will_fire_it() {
        let fixture = Fixture::new();
        let said = text(&create(&fixture, nightly()).await);
        assert!(said.contains("daily at 09:00"), "{said}");
        assert!(
            said.contains("Schedules here are dormant — no runner holds this store."),
            "{said}"
        );
    }

    #[tokio::test]
    async fn a_call_may_name_its_own_directory_and_mode() {
        let fixture = Fixture::new();
        let mut asked = nightly();
        asked["cwd"] = json!("/elsewhere");
        asked["permissionMode"] = json!("acceptEdits");
        create(&fixture, asked).await;
        let entry = &fixture.shelf().entries[0];
        assert_eq!(entry.cwd, PathBuf::from("/elsewhere"));
        assert_eq!(entry.permission_mode.as_deref(), Some("acceptEdits"));
    }

    #[tokio::test]
    async fn a_spec_the_grammar_cannot_read_writes_nothing_and_says_what_one_is() {
        let fixture = Fixture::new();
        let mut asked = nightly();
        asked["spec"] = json!("*/5 * * * *");
        let out = create(&fixture, asked).await;
        assert!(out.is_error);
        assert!(text(&out).contains("daily at HH:MM"), "{}", text(&out));
        assert!(fixture.shelf().is_empty(), "nothing was written");
    }

    #[tokio::test]
    async fn a_schedule_with_nothing_to_say_is_refused() {
        let fixture = Fixture::new();
        let mut asked = nightly();
        asked["text"] = json!("   ");
        let out = create(&fixture, asked).await;
        assert!(out.is_error);
        assert!(fixture.shelf().is_empty());
    }

    #[test]
    fn the_card_shows_the_entry_it_would_write() {
        let fixture = Fixture::new();
        let tool = ScheduleCreateTool::new(fixture.schedules.clone());
        let Some(Preview::Diff { unified }) = tool.preview(&nightly(), &fixture.cwd()) else {
            panic!("a creation shows the file it would write");
        };
        assert!(unified.contains("<new>.json"), "{unified}");
        assert!(
            unified.contains(r#"+  "spec": "daily at 09:00""#),
            "{unified}"
        );
        assert!(unified.contains(r#"+  "enabled": true"#), "{unified}");
        assert!(files(&fixture.dir()).is_empty(), "a preview never writes");
    }

    #[test]
    fn a_card_is_not_drawn_for_a_call_that_would_be_refused() {
        let fixture = Fixture::new();
        let tool = ScheduleCreateTool::new(fixture.schedules.clone());
        assert!(
            tool.preview(&json!({"spec": "yearly", "text": "t"}), &fixture.cwd())
                .is_none()
        );
    }

    #[test]
    fn the_spec_is_an_edit_and_asks_before_it_writes() {
        let fixture = Fixture::new();
        let tool = ScheduleCreateTool::new(fixture.schedules.clone());
        let spec = tool.spec();
        assert_eq!(spec.name, "ScheduleCreate");
        assert!(spec.input_schema["properties"]["spec"]["description"].is_string());
        let traits = tool.traits(&Value::Null);
        assert!(traits.edit && traits.trusted);
        assert!(!traits.read_only && !traits.destructive);
    }
}
