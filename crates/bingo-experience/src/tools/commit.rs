//! `ExperienceCommit`: write a playbook down, or revise the one already
//! there. There is no propose tool — `preview` renders the file this call
//! would write, so the permission card *is* the proposal (ADR-0014 §4).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Preview, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, View, input_schema,
};
use jiff::Timestamp;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::entry::{Entry, Status};
use crate::store::{Library, Shelf};
use crate::{diff, frontmatter, render};

const DESCRIPTION: &str = "\
Write down a playbook this project taught you: when this happens (trigger), do \
this (steps), check it worked (verify). Commit one only after you have done \
the thing and seen it work, and keep it procedural — a fact about the project \
belongs in its memory, not here. Without `id` the same trigger, summary and \
steps revise the entry that already says this instead of forking it; with \
`id` (a unique prefix of one is enough) it revises that entry, which keeps \
its outcomes and the day it was first written. The person sees the file this \
would write before it is written.";

/// What a file with no id yet is shown as on the card: the id is minted when
/// the entry is written, not when the card is drawn.
const UNNAMED: &str = "<new>";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Commit {
    /// The entry to revise; a unique prefix of its id is enough. Leave it out
    /// to write a new one.
    #[serde(default)]
    id: Option<String>,
    /// What brings this playbook to mind, in the words that would be used at
    /// the time: an error message, a symptom, a kind of task.
    trigger: Vec<String>,
    /// One line naming the pattern.
    summary: String,
    /// What to do, in order.
    steps: Vec<String>,
    /// How to tell it worked.
    #[serde(default)]
    verify: Option<String>,
    /// Anything that does not fit the steps: what it depends on, what it cost.
    #[serde(default)]
    notes: Option<String>,
    /// `retired` keeps an entry and its history but stops it being recalled.
    #[serde(default)]
    status: Option<Status>,
}

/// The playbook, before it is known whether it is a new entry or a revision.
struct Draft {
    id: Option<String>,
    trigger: Vec<String>,
    summary: String,
    steps: Vec<String>,
    verify: Option<String>,
    notes: String,
    status: Option<Status>,
}

impl Commit {
    fn draft(self) -> Result<Draft, ToolError> {
        let draft = Draft {
            id: self.id,
            trigger: cleaned(self.trigger),
            summary: self.summary.trim().to_string(),
            steps: cleaned(self.steps),
            verify: self.verify.and_then(one_of),
            notes: self.notes.unwrap_or_default().trim().to_string(),
            status: self.status,
        };
        if draft.summary.is_empty() || draft.trigger.is_empty() || draft.steps.is_empty() {
            return Err(ToolError::InvalidInput(
                "an experience needs a summary, at least one trigger and at least one step: \
                 when this happens, do this"
                    .into(),
            ));
        }
        Ok(draft)
    }
}

impl Draft {
    /// Whether this is the entry, already written down.
    fn is(&self, entry: &Entry) -> bool {
        self.trigger == entry.trigger && self.summary == entry.summary && self.steps == entry.steps
    }

    /// A new entry. Its id is minted when it is written.
    fn new_entry(self, now: Timestamp) -> Entry {
        Entry {
            id: String::new(),
            status: self.status.unwrap_or_default(),
            trigger: self.trigger,
            summary: self.summary,
            steps: self.steps,
            verify: self.verify,
            created: now,
            outcomes: Vec::new(),
            notes: self.notes,
        }
    }

    /// The same entry, said again: the id, the day it was written and every
    /// outcome it has been given survive the edit.
    fn onto(self, before: &Entry) -> Entry {
        Entry {
            id: before.id.clone(),
            status: self.status.unwrap_or(before.status),
            trigger: self.trigger,
            summary: self.summary,
            steps: self.steps,
            verify: self.verify,
            created: before.created,
            outcomes: before.outcomes.clone(),
            notes: self.notes,
        }
    }
}

enum Plan {
    Create(Entry),
    Revise { before: Entry, after: Entry },
}

/// Which entry this call is about: the one it names, else the one that
/// already says the same thing, else a new one.
fn plan(draft: Draft, shelf: &Shelf, now: Timestamp) -> Result<Plan, String> {
    let existing = match &draft.id {
        Some(prefix) => Some(super::find(&shelf.entries, prefix)?.clone()),
        None => shelf.entries.iter().find(|entry| draft.is(entry)).cloned(),
    };
    Ok(match existing {
        Some(before) => Plan::Revise {
            after: draft.onto(&before),
            before,
        },
        None => Plan::Create(draft.new_entry(now)),
    })
}

fn cleaned(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().filter_map(one_of).collect()
}

fn one_of(line: String) -> Option<String> {
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Writes one entry, and shows the file it would write first.
#[derive(Debug)]
pub struct ExperienceCommitTool {
    library: Arc<Library>,
}

impl ExperienceCommitTool {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }

    fn planned(&self, input: &Value, cwd: &Path) -> Option<Plan> {
        let draft = serde_json::from_value::<Commit>(input.clone())
            .ok()?
            .draft()
            .ok()?;
        plan(draft, &self.library.load(cwd), Timestamp::now()).ok()
    }

    /// The file as it would read, against the file as it reads now.
    fn unified(&self, cwd: &Path, plan: &Plan) -> String {
        match plan {
            Plan::Create(entry) => diff::unified(
                &self.library.path(cwd, UNNAMED),
                "",
                &frontmatter::to_markdown(entry),
            ),
            Plan::Revise { before, after } => diff::unified(
                &self.library.path(cwd, &after.id),
                &frontmatter::to_markdown(before),
                &frontmatter::to_markdown(after),
            ),
        }
    }

    fn write(&self, cwd: &Path, plan: Plan) -> Result<ToolOutput, ToolError> {
        let unified = self.unified(cwd, &plan);
        let (entry, how) = match plan {
            Plan::Create(mut entry) => {
                entry.id = self.library.mint(cwd);
                (entry, "Wrote")
            }
            Plan::Revise { after, .. } if unified.is_empty() => (after, "Unchanged:"),
            Plan::Revise { after, .. } => (after, "Revised"),
        };
        self.library.save(cwd, &entry).map_err(super::failed)?;
        let mut out = ToolOutput::text(format!("{how} {}", render::line_with_status(&entry)));
        if !unified.is_empty() {
            // A creation's diff was drawn against `<new>`; the file has a name now.
            out.display = Some(View::Diff {
                unified: unified.replace(UNNAMED, &entry.id),
            });
        }
        Ok(out)
    }
}

#[async_trait]
impl Tool for ExperienceCommitTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExperienceCommit".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Commit>(),
            meta: Default::default(),
        }
    }

    /// A write a person approves once, and `acceptEdits` takes as read: it
    /// touches one file of the agent's own and nothing else.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::edit()
    }

    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> {
        let unified = self.unified(cwd, &self.planned(input, cwd)?);
        (!unified.is_empty()).then_some(Preview::Diff { unified })
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let draft = serde_json::from_value::<Commit>(input)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?
            .draft()?;
        let shelf = self.library.load(&cx.cwd);
        match plan(draft, &shelf, Timestamp::now()) {
            Ok(plan) => self.write(&cx.cwd, plan),
            Err(message) => Ok(ToolOutput::error(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Outcome, Record};
    use crate::tests::{Fixture, files, text};
    use serde_json::json;

    fn playbook() -> Value {
        json!({
            "trigger": ["the build breaks after a dependency bump"],
            "summary": "clear the target directory",
            "steps": ["cargo clean", "cargo build"],
            "verify": "the build is green",
        })
    }

    async fn commit(fixture: &Fixture, input: Value) -> ToolOutput {
        ExperienceCommitTool::new(fixture.library.clone())
            .call(input, &fixture.context())
            .await
            .expect("a commit")
    }

    #[tokio::test]
    async fn a_playbook_becomes_one_file_named_by_its_minted_id() {
        let fixture = Fixture::new();
        let out = commit(&fixture, playbook()).await;
        assert!(!out.is_error, "{out:?}");
        let shelf = fixture.shelf();
        assert_eq!(shelf.entries.len(), 1);
        let entry = &shelf.entries[0];
        assert_eq!(entry.id.chars().count(), 8, "{}", entry.id);
        assert_eq!(entry.summary, "clear the target directory");
        assert_eq!(entry.steps, ["cargo clean", "cargo build"]);
        assert_eq!(entry.status, Status::Active);
        assert!(text(&out).contains(&entry.id), "{}", text(&out));
        assert_eq!(files(&fixture.dir()), [format!("{}.md", entry.id)]);
        assert!(matches!(out.display, Some(View::Diff { .. })));
    }

    #[tokio::test]
    async fn the_same_playbook_twice_revises_instead_of_forking() {
        let fixture = Fixture::new();
        commit(&fixture, playbook()).await;
        let first = fixture.shelf().entries[0].clone();

        let mut again = playbook();
        again["notes"] = json!("it is the incremental cache");
        let out = commit(&fixture, again).await;

        let shelf = fixture.shelf();
        assert_eq!(shelf.entries.len(), 1, "the entry forked");
        assert_eq!(shelf.entries[0].id, first.id, "the id changed");
        assert_eq!(shelf.entries[0].notes, "it is the incremental cache");
        assert!(text(&out).starts_with("Revised"), "{}", text(&out));
    }

    #[tokio::test]
    async fn revising_by_id_keeps_the_history_and_the_day_it_was_written() {
        let fixture = Fixture::new();
        commit(&fixture, playbook()).await;
        let mut first = fixture.shelf().entries[0].clone();
        first.outcomes.push(Record {
            outcome: Outcome::Helpful,
            at: Timestamp::UNIX_EPOCH,
            evidence: "it built".into(),
        });
        first.created = Timestamp::UNIX_EPOCH;
        fixture
            .library
            .save(&fixture.cwd(), &first)
            .expect("a history");

        let mut revised = playbook();
        // A prefix, not the whole id: what the index shows is what this takes.
        revised["id"] = json!(&first.id[..4]);
        revised["summary"] = json!("clear the target directory and the lockfile");
        revised["status"] = json!("retired");
        commit(&fixture, revised).await;

        let shelf = fixture.shelf();
        assert_eq!(shelf.entries.len(), 1);
        let entry = &shelf.entries[0];
        assert_eq!(entry.id, first.id);
        assert_eq!(entry.summary, "clear the target directory and the lockfile");
        assert_eq!(entry.status, Status::Retired);
        assert_eq!(entry.created, Timestamp::UNIX_EPOCH, "created was reset");
        assert_eq!(entry.helpful(), 1, "the history was lost");
    }

    #[tokio::test]
    async fn an_id_nobody_has_is_an_answer_and_writes_nothing() {
        let fixture = Fixture::new();
        let mut input = playbook();
        input["id"] = json!("nosuchid");
        let out = commit(&fixture, input).await;
        assert!(out.is_error);
        assert!(text(&out).contains("ExperienceQuery"), "{}", text(&out));
        assert!(fixture.shelf().is_empty());
    }

    #[tokio::test]
    async fn a_playbook_without_a_step_is_an_input_error() {
        let fixture = Fixture::new();
        let tool = ExperienceCommitTool::new(fixture.library.clone());
        for input in [
            json!({"trigger": ["x"], "summary": "y", "steps": []}),
            json!({"trigger": [], "summary": "y", "steps": ["z"]}),
            json!({"trigger": ["x"], "summary": "  ", "steps": ["z"]}),
        ] {
            let error = tool
                .call(input.clone(), &fixture.context())
                .await
                .expect_err("{input}");
            assert!(matches!(error, ToolError::InvalidInput(_)), "{error:?}");
        }
        assert!(fixture.shelf().is_empty());
    }

    #[test]
    fn the_card_shows_the_file_it_would_write() {
        let fixture = Fixture::new();
        let tool = ExperienceCommitTool::new(fixture.library.clone());
        let Some(Preview::Diff { unified }) = tool.preview(&playbook(), &fixture.cwd()) else {
            panic!("a commit proposes a file");
        };
        assert!(unified.contains("<new>.md"), "{unified}");
        assert!(
            unified.contains("+summary: \"clear the target directory\""),
            "{unified}"
        );
        assert!(unified.contains("+  - \"cargo clean\""), "{unified}");
        assert!(
            fixture.shelf().is_empty(),
            "a preview reads, it never writes"
        );
    }

    #[test]
    fn a_card_for_a_call_that_cannot_run_is_no_card() {
        let fixture = Fixture::new();
        let tool = ExperienceCommitTool::new(fixture.library.clone());
        assert!(
            tool.preview(&json!({"summary": "x"}), &fixture.cwd())
                .is_none()
        );
    }

    #[test]
    fn the_spec_is_an_approved_write() {
        let tool = ExperienceCommitTool::new(Arc::new(Library::new(Path::new("/nowhere"))));
        let spec = tool.spec();
        assert_eq!(spec.name, "ExperienceCommit");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema["properties"].get("trigger").is_some());
        let traits = tool.traits(&Value::Null);
        assert!(traits.edit && traits.trusted);
        assert!(!traits.read_only && !traits.destructive);
    }
}
