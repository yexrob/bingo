//! `KillShell`: end a job's process group.
//!
//! The call asks; the job's own task does the killing, `SIGTERM` first and the
//! signal it cannot answer once the grace is spent (ADR-0018 §2). Nothing
//! durable is destroyed — the log stays and `BashOutput` still reads it — so
//! the tool is not `destructive`; it is not read-only either, so the gate asks
//! about it in the default mode, which is the right price for ending something
//! that is running.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::jobs::Jobs;

/// Longer than the grace the job's task gives the group plus the reaping after
/// it, so a call only gives up when something is truly stuck.
const WAIT: Duration = Duration::from_secs(8);

const DESCRIPTION: &str = "\
End a background shell command and everything it started. `id` is the job id \
`Bash` gave back — the start of it is enough, as long as it names only one \
job. The group is asked to stop and then made to; the answer says how it \
ended. Its log is kept, so `BashOutput` still reads what it wrote.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KillArgs {
    /// The job's id, or enough of the start of it to name one job.
    pub id: String,
}

pub struct KillShellTool {
    jobs: Arc<Jobs>,
}

impl KillShellTool {
    pub fn new(jobs: Arc<Jobs>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for KillShellTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "KillShell".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<KillArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            // Ending a process is a real change, but it destroys nothing that
            // outlives the run: the log is still there to be read.
            trusted: true,
            ..ToolTraits::default()
        }
    }

    fn subjects(&self, input: &Value, _cwd: &std::path::Path) -> Vec<Subject> {
        serde_json::from_value::<KillArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.id }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: KillArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let job = match self.jobs.find(&args.id) {
            Ok(job) => job,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        let already = job.state();
        if already.ended() {
            return Ok(ToolOutput::text(format!(
                "Job {} had already {}. Its log is {}.",
                job.named(),
                already.said(),
                job.log.display()
            )));
        }
        job.ask_to_die();
        let Ok(state) = tokio::time::timeout(WAIT, job.wait()).await else {
            return Ok(ToolOutput::error(format!(
                "Job {} was asked to stop and had not after {}s; `BashOutput` says where it got to.",
                job.named(),
                WAIT.as_secs()
            )));
        };
        Ok(ToolOutput::text(format!(
            "Job {} {}. Its log is {}, and `BashOutput` still reads it.",
            job.named(),
            state.said(),
            job.log.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Job, State};
    use crate::tests::context;

    use bingo_sdk::SessionId;
    use serde_json::json;

    fn filed(command: &str) -> (Arc<Jobs>, Arc<Job>) {
        let jobs = Arc::new(Jobs::new());
        let job = Arc::new(Job::new(
            crate::jobs::mint(),
            command.into(),
            std::path::PathBuf::from("/tmp/bash/job.log"),
            SessionId::from_raw("ses_test"),
        ));
        jobs.file(job.clone());
        (jobs, job)
    }

    fn text(out: &ToolOutput) -> String {
        out.parts[0].as_text().unwrap_or_default().to_string()
    }

    async fn kill(jobs: &Arc<Jobs>, id: &str) -> ToolOutput {
        let (_host, cx) = context();
        KillShellTool::new(jobs.clone())
            .call(json!({ "id": id }), &cx)
            .await
            .expect("the kill answered")
    }

    /// The call asks and waits; the job's own task is what ends it.
    #[tokio::test]
    async fn a_kill_asks_the_job_to_die_and_reports_how_it_went() {
        let (jobs, job) = filed("tail -f app.log");
        let watched = job.clone();
        tokio::spawn(async move {
            watched.killed().cancelled().await;
            watched.finished(State::Killed);
        });
        let out = kill(&jobs, &job.id).await;
        assert!(!out.is_error, "{}", text(&out));
        assert!(text(&out).contains("killed"), "{}", text(&out));
        assert!(text(&out).contains("tail -f app.log"), "{}", text(&out));
        assert!(text(&out).contains("BashOutput"), "{}", text(&out));
    }

    /// A program that answers `SIGTERM` and leaves cleanly has an exit code,
    /// and that is what the caller is told.
    #[tokio::test]
    async fn a_group_that_leaves_on_its_own_terms_reports_its_code() {
        let (jobs, job) = filed("sleep 30");
        let watched = job.clone();
        tokio::spawn(async move {
            watched.killed().cancelled().await;
            watched.finished(State::Exited { code: 0 });
        });
        assert!(
            kill(&jobs, &job.id).await.parts[0]
                .as_text()
                .is_some_and(|t| t.contains("exited with code 0"))
        );
    }

    #[tokio::test]
    async fn a_job_that_had_already_ended_is_said_so_and_nothing_is_signalled() {
        let (jobs, job) = filed("cargo build");
        job.finished(State::Exited { code: 0 });
        let out = kill(&jobs, &job.id).await;
        assert!(!out.is_error);
        assert!(
            text(&out).contains("had already exited with code 0"),
            "{}",
            text(&out)
        );
        assert!(!job.killed().is_cancelled(), "nothing was signalled");
    }

    #[tokio::test]
    async fn an_id_nobody_has_is_an_error_result() {
        let (jobs, _job) = filed("sleep 30");
        let out = kill(&jobs, "zzzz").await;
        assert!(out.is_error);
        assert!(text(&out).contains("no job is called"), "{}", text(&out));
    }

    #[test]
    fn killing_is_trusted_not_read_only_and_destroys_nothing_durable() {
        let tool = KillShellTool::new(Arc::new(Jobs::new()));
        let traits = tool.traits(&Value::Null);
        assert!(traits.trusted);
        assert!(!traits.read_only, "a kill is a change");
        assert!(!traits.destructive, "the log outlives the process");
        assert!(!traits.edit && !traits.concurrency_safe);
        assert_eq!(tool.spec().name, "KillShell");
        assert_eq!(
            tool.subjects(&json!({"id": "ab12"}), std::path::Path::new("/")),
            [Subject::Name {
                name: "ab12".into()
            }]
        );
    }
}
