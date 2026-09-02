//! `BashOutput`: a window over a job's log.
//!
//! The model pulls as much as it wants, when it wants (ADR-0018 §2). Reading
//! costs a permission nothing — it is trusted and read-only, so no gate ever
//! stands between the model and output it already caused — and it bounds
//! itself, because the cursor line at the foot is the part that must survive.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ResultLimit, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::jobs::{Job, Jobs};
use crate::log;
use crate::output::MAX_OUTPUT_CHARS;

const DESCRIPTION: &str = "\
Read what a background shell command has written since you last looked. `id` \
is the job id `Bash` gave back — the start of it is enough, as long as it \
names only one job. `cursor` is the number the last read gave back; leave it \
out to read from the beginning. The answer says whether the job is still \
running and where to read on from, so pulling twice never repeats itself and \
never skips. This is how you follow a job: you are told when one ends and \
when a line you asked about appears, so there is no reason to poll.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutputArgs {
    /// The job's id, or enough of the start of it to name one job.
    pub id: String,
    /// Read on from here; the number the last read gave back.
    pub cursor: Option<u64>,
}

pub struct BashOutputTool {
    jobs: Arc<Jobs>,
}

impl BashOutputTool {
    pub fn new(jobs: Arc<Jobs>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for BashOutputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "BashOutput".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<OutputArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            // Reading a file this process wrote changes nothing and races with
            // nothing, and the cursor line at the foot must not be clipped off.
            read_only: true,
            trusted: true,
            concurrency_safe: true,
            result_limit: ResultLimit::SelfBounded,
            ..ToolTraits::default()
        }
    }

    fn subjects(&self, input: &Value, _cwd: &std::path::Path) -> Vec<Subject> {
        serde_json::from_value::<OutputArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.id }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: OutputArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let job = match self.jobs.find(&args.id) {
            Ok(job) => job,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        let from = args.cursor.unwrap_or(0);
        match log::window(&job.log, from, MAX_OUTPUT_CHARS).await {
            Ok(window) => Ok(ToolOutput::text(shape(&job, &window))),
            Err(e) => Ok(ToolOutput::error(format!(
                "job {}'s log at {} could not be read: {e}",
                job.id,
                job.log.display()
            ))),
        }
    }
}

/// The command, what is new, and where the next read starts. A job with
/// nothing new says so rather than answering with a blank.
fn shape(job: &Job, window: &log::Window) -> String {
    let body = match window.text.trim_end_matches('\n') {
        "" => "(nothing new)".to_string(),
        text => text.to_string(),
    };
    let more = if window.more { " · more waiting" } else { "" };
    format!(
        "$ {}\n{body}\n[job {} · {} · cursor {}{more}]",
        job.command,
        job.id,
        job.state().said(),
        window.cursor,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::State;
    use crate::log::Log;
    use crate::tests::context;

    use bingo_sdk::SessionId;
    use serde_json::json;

    /// A job whose log already holds `text`, filed in a fresh table.
    async fn filed(text: &str) -> (tempfile::TempDir, Arc<Jobs>, Arc<Job>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let jobs = Arc::new(Jobs::new());
        let mut log = Log::create(dir.path(), "written").await.expect("a log");
        log.write(text).await.expect("written");
        let job = Arc::new(Job::new(
            crate::jobs::id_of(log.path()),
            "cargo test".into(),
            log.path().to_path_buf(),
            SessionId::from_raw("ses_test"),
        ));
        jobs.file(job.clone());
        (dir, jobs, job)
    }

    fn text(out: &ToolOutput) -> String {
        out.parts[0].as_text().unwrap_or_default().to_string()
    }

    async fn read(jobs: &Arc<Jobs>, input: Value) -> ToolOutput {
        let (_host, cx) = context();
        BashOutputTool::new(jobs.clone())
            .call(input, &cx)
            .await
            .expect("the read answered")
    }

    #[tokio::test]
    async fn a_read_says_the_command_the_state_and_where_to_read_on() {
        let (_dir, jobs, job) = filed("one\ntwo\n").await;
        let out = read(&jobs, json!({ "id": &job.id })).await;
        assert!(!out.is_error);
        assert_eq!(
            text(&out),
            format!(
                "$ cargo test\none\ntwo\n[job {} · running · cursor 8]",
                job.id
            )
        );
    }

    /// Two pulls across two turns: the second starts where the first stopped.
    #[tokio::test]
    async fn a_cursor_reads_on_and_never_repeats_itself() {
        let (dir, jobs, job) = filed("first\n").await;
        let first = read(&jobs, json!({ "id": &job.id })).await;
        assert!(text(&first).contains("first"), "{}", text(&first));

        let mut log = Log::create(dir.path(), "written").await.expect("a log");
        log.write("first\nsecond\n").await.expect("written");
        let second = read(&jobs, json!({ "id": &job.id, "cursor": 6 })).await;
        assert!(text(&second).contains("second"), "{}", text(&second));
        assert!(!text(&second).contains("first"), "{}", text(&second));
    }

    #[tokio::test]
    async fn a_job_with_nothing_new_says_so_and_leaves_the_cursor_alone() {
        let (_dir, jobs, job) = filed("all of it\n").await;
        let out = read(&jobs, json!({ "id": &job.id, "cursor": 10 })).await;
        assert!(text(&out).contains("(nothing new)"), "{}", text(&out));
        assert!(text(&out).contains("cursor 10"), "{}", text(&out));
    }

    #[tokio::test]
    async fn a_finished_job_still_answers_and_says_how_it_ended() {
        let (_dir, jobs, job) = filed("done\n").await;
        job.finished(State::Exited { code: 2 });
        let out = read(&jobs, json!({ "id": &job.id })).await;
        assert!(!out.is_error, "a read of a failed job is still a read");
        assert!(text(&out).contains("exited with code 2"), "{}", text(&out));
    }

    #[tokio::test]
    async fn a_prefix_is_enough_and_an_id_nobody_has_is_an_error_result() {
        let (_dir, jobs, job) = filed("x\n").await;
        let by_prefix = read(&jobs, json!({ "id": &job.id[..3] })).await;
        assert!(!by_prefix.is_error, "{}", text(&by_prefix));

        let unknown = read(&jobs, json!({ "id": "zzzz" })).await;
        assert!(unknown.is_error);
        assert!(
            text(&unknown).contains("no job is called"),
            "{}",
            text(&unknown)
        );
    }

    #[tokio::test]
    async fn a_log_that_has_gone_is_an_error_result_naming_it() {
        let jobs = Arc::new(Jobs::new());
        let job = Arc::new(Job::new(
            "gonelog0".into(),
            "true".into(),
            std::path::PathBuf::from("/no/such/job.log"),
            SessionId::from_raw("ses_test"),
        ));
        jobs.file(job.clone());
        let out = read(&jobs, json!({ "id": &job.id })).await;
        assert!(out.is_error);
        assert!(text(&out).contains("/no/such/job.log"), "{}", text(&out));
    }

    #[test]
    fn the_tool_is_free_to_call_and_bounds_its_own_answer() {
        let tool = BashOutputTool::new(Arc::new(Jobs::new()));
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && traits.concurrency_safe);
        assert_eq!(traits.result_limit, ResultLimit::SelfBounded);
        assert_eq!(tool.spec().name, "BashOutput");
        assert_eq!(
            tool.subjects(&json!({"id": "ab12"}), std::path::Path::new("/")),
            [Subject::Name {
                name: "ab12".into()
            }]
        );
    }
}
