//! The `Bash` tool: one shell command per call, in its own process group,
//! waited for or left running. The plugin also owns `!`, the same shell for
//! the person at the keyboard (`shell`).
//!
//! Under the three verbs are the bricks: the table that refuses a command
//! before anything is spawned (`reject`), the one that backgrounds a command
//! that could never finish (`endless`), the bounded `output`, the `log` a job
//! writes and the `sink` that chooses between the two, the `tail` the user
//! watches while the command works, the `jobs` table with a task per job
//! (`supervise`), what a job's end says (`notify`), and the process lifecycle
//! (`run`), which takes its directory, its interrupt and its progress sink
//! from whichever of the two asked.
//!
//! The traits fail closed except for `trusted`: a shell command is not read-only
//! and whether it may run at all is the permission policy's decision, made from
//! the `Command` subject.
//!
//! A person stopping the turn ends a running command, full stop: the kernel
//! drops the call's future and `run::Group` takes the process tree with it,
//! with nothing waited for and the output forfeit. What a half-run command
//! left in the working tree is the price of being able to stop it, and the
//! model is told the call was interrupted rather than what it wrote.
//! Interrupting a turn never touches a background job — a promoted command's
//! process left the call for a task of its own, and `KillShell` is what ends
//! one.
//!
//! See docs/plans/M1-provider-tools-gate.md, docs/plans/M16-background-work.md
//! and docs/adr/0018-background-commands.md.

mod endless;
mod jobs;
mod kill;
mod log;
mod notify;
mod output;
mod promote;
mod read;
mod reject;
mod run;
mod shell;
mod sink;
mod supervise;
mod tail;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    Command, Contribution, Plugin, PluginError, PluginManifest, Registrar, ResultLimit, Subject,
    Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::jobs::{Job, Jobs};
use crate::kill::KillShellTool;
use crate::log::Log;
use crate::notify::Conditions;
use crate::output::MAX_OUTPUT_CHARS;
use crate::promote::{PromoteCommand, Promotions};
use crate::read::BashOutputTool;
use crate::run::Running;
use crate::sink::Sink;

pub use shell::ShellCommand;

/// How long a command runs when the call does not say.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The longest a call may ask for. Past ten minutes the answer belongs in a
/// background job the model pulls from, not in a call the turn waits on.
const MAX_TIMEOUT_MS: u64 = 600_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashArgs {
    /// The command to run.
    pub command: String,
    /// Start it and answer at once with a job id, instead of waiting for it.
    pub background: Option<bool>,
    /// How long to let it run, in milliseconds, when the call waits. Defaults
    /// to 120000, and 600000 is the most that will be honoured. A background
    /// job has no timeout.
    pub timeout: Option<u64>,
    /// Words to watch a background job's output for. The first line holding
    /// one is sent to you as it happens.
    pub notify_on: Option<Vec<String>>,
    /// A regular expression to watch a background job's output for, matched a
    /// line at a time.
    pub notify_regex: Option<String>,
    /// Keep watching past the first hit, instead of stopping there. You are
    /// told at most once every thirty seconds; the lines that match in between
    /// are counted, and the count comes with the next notice. Needs
    /// `notify_on` or `notify_regex`.
    pub notify_all: Option<bool>,
    /// What the command does, in five to ten words, in active voice.
    pub description: Option<String>,
}

/// One shell command per call. The tool holds the job table and the open
/// promotions, because a call that is backgrounded — by its own flag, by the
/// table of commands that never end, or by a person mid-run — hands its
/// process to the first and stops listening to the second.
pub struct BashTool {
    jobs: Arc<Jobs>,
    promotions: Arc<Promotions>,
}

impl BashTool {
    pub fn new(jobs: Arc<Jobs>, promotions: Arc<Promotions>) -> Self {
        Self { jobs, promotions }
    }

    /// The command a call names, as the gate and the shell both see it.
    fn target(input: &Value) -> Option<String> {
        let args: BashArgs = serde_json::from_value(input.clone()).ok()?;
        Some(args.command)
    }

    /// Start it, file it and answer at once. `why` is the reason the tool
    /// backgrounded a call that did not ask to be.
    async fn detach(
        &self,
        command: &str,
        conditions: Conditions,
        why: Option<String>,
        cx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let log = self.open_log(cx).await?;
        let job = self.file(command, log.path().to_path_buf(), cx);
        let running = run::start(command, &cx.cwd, Sink::file(log))?;
        self.take(job.clone(), running, conditions, cx).await;
        Ok(ToolOutput::text(started(&job, why)))
    }

    /// Wait for it, unless a person takes it into the background first.
    async fn wait_for(
        &self,
        args: &BashArgs,
        conditions: Conditions,
        cx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let listening = self.promotions.listen(&cx.call_id);
        let timeout = deadline(args.timeout);
        let mut running = run::start(&args.command, &cx.cwd, Sink::buffer(MAX_OUTPUT_CHARS))?;
        let context = run::Context::of_call(cx, listening.token());
        match run::watch(&mut running, timeout, &context).await? {
            run::Stop::Over(over) => {
                let finished = run::conclude(running, over, timeout).await?;
                Ok(output::shape(
                    &args.command,
                    &finished.output,
                    finished.ended,
                ))
            }
            run::Stop::Promoted => self.hand_over(&args.command, running, conditions, cx).await,
        }
    }

    /// The same process, the same pipes and the same buffer, filed as a job:
    /// nothing restarts and the foreground timeout is gone with the wait.
    async fn hand_over(
        &self,
        command: &str,
        running: Running,
        conditions: Conditions,
        cx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let log = match self.open_log(cx).await {
            Ok(log) => log,
            // Nowhere to write is nowhere to background to. End it, and answer
            // with what it wrote, which is all there is left to save.
            Err(error) => {
                let finished =
                    run::conclude(running, run::Over::Interrupted, Duration::ZERO).await?;
                return Ok(ToolOutput::error(format!(
                    "{error}\n{}",
                    finished.output.trim_end()
                )));
            }
        };
        let job = self.file(command, log.path().to_path_buf(), cx);
        running.sink.lock().await.promote(log).await;
        self.take(job.clone(), running, conditions, cx).await;
        Ok(ToolOutput::text(promoted(&job)))
    }

    async fn open_log(&self, cx: &ToolContext) -> Result<Log, ToolError> {
        let dir = log::dir(&cx.env.data_dir);
        Log::create(&dir, &jobs::mint())
            .await
            .map_err(|e| ToolError::Failed(format!("no log could be made under {dir:?}: {e}")))
    }

    /// A job for a log that already exists; its id is the log's name.
    fn file(&self, command: &str, log: std::path::PathBuf, cx: &ToolContext) -> Arc<Job> {
        let job = Arc::new(Job::new(
            jobs::id_of(&log),
            command.to_string(),
            log,
            cx.session.clone(),
        ));
        self.jobs.file(job.clone());
        job
    }

    /// Hand the process to its own task and tell the rail the set has changed.
    async fn take(
        &self,
        job: Arc<Job>,
        running: Running,
        conditions: Conditions,
        cx: &ToolContext,
    ) {
        supervise::take(supervise::Watch {
            jobs: self.jobs.clone(),
            job,
            running,
            conditions,
            host: cx.host.clone(),
        });
        self.jobs.publish(&cx.host, &cx.session).await;
    }
}

/// The bound a call sets for itself, within the tool's own ceiling.
fn deadline(timeout: Option<u64>) -> Duration {
    Duration::from_millis(timeout.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS))
}

/// What a call gets back the moment its command is running in the background.
fn started(job: &Job, why: Option<String>) -> String {
    let backgrounded = why
        .map(|why| format!(" It was backgrounded although the call did not ask: {why}."))
        .unwrap_or_default();
    format!(
        "Started `{}` in the background as job {}.{backgrounded} You will be told when it ends, so \
         there is no reason to poll: `BashOutput` with id \"{}\" reads what it has written when you \
         want it, and `KillShell` ends it. Its log is {}.",
        jobs::head(&job.command),
        job.id,
        job.id,
        job.log.display()
    )
}

/// What a call gets back when a person took its command into the background.
fn promoted(job: &Job) -> String {
    format!(
        "`{}` was moved into the background as job {} while it ran: the same process, carrying on \
         with no timeout. You will be told when it ends; `BashOutput` with id \"{}\" reads what it \
         has written so far, and `KillShell` ends it. Its log is {}.",
        jobs::head(&job.command),
        job.id,
        job.id,
        job.log.display()
    )
}

/// The tool names the shell it actually runs, because `Bash` on its own primes
/// the model for a dialect the host may not have.
fn description() -> String {
    format!(
        "Execute a command in the local shell ({shell}) and return what it wrote together with \
         its exit code. stdout and stderr come back interleaved, under a `$ <command>` header \
         and above an `[Exited with code N]` line. Long output keeps its beginning and its end, \
         and says how much was dropped between them. Reading, listing and searching files is the \
         file tools' job, not this one's: a shell `cat`, `ls`, `find` or `grep` costs a permission \
         and answers with less than they do. Use this for building, testing and running \
         programs.\n\n\
         Prefer `background: true` for anything you do not need the answer to in your very next \
         step — a server, a watcher, a long build or test run. It answers at once with a job id: \
         `BashOutput` reads what the job has written, `KillShell` ends it, and you are told when \
         it finishes, so waiting is a choice rather than the only way. `notify_on` and \
         `notify_regex` have you told the moment a line you care about appears. They tell you \
         once unless `notify_all: true` keeps them watching for the whole job, which tells you \
         again at most once every thirty seconds and counts the lines that matched in between. A \
         command that could never finish on its own — `watch`, `tail -f`, a loop with no end, a \
         trailing `&` — is backgrounded whatever the call said.\n\n\
         Without `background`, the call waits for the command to exit, for {default} milliseconds \
         unless `timeout` says otherwise ({max} milliseconds at most); a person watching may move \
         it into the background while it runs, and then the call answers with a job id instead. \
         stdin is closed, so nothing can prompt: a command that needs a terminal (a full-screen \
         monitor, an editor, a pager, `sudo` without `-n`, `ssh` without a remote command, a bare \
         REPL) is refused with the reason and the way round it.",
        shell = run::shell(),
        default = DEFAULT_TIMEOUT_MS,
        max = MAX_TIMEOUT_MS,
    )
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Bash".into(),
            description: description(),
            input_schema: input_schema::<BashArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            trusted: true,
            // Whether two commands may run at once is the model's judgment,
            // exactly as it is for two edits: it emitted them in one step. The
            // gate still serializes anything it does not allow outright, and a
            // long command belongs in the background (ADR-0018), not in a batch.
            concurrency_safe: true,
            // The tool caps its own output; the kernel's clip would take the
            // exit line with it.
            result_limit: ResultLimit::SelfBounded,
            ..ToolTraits::default()
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        Self::target(input)
            .map(|command| vec![Subject::Command { command }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: BashArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        if let Some(reason) = reject::interactive_reason(&args.command) {
            return Ok(ToolOutput::error(reason));
        }
        let conditions = match Conditions::new(
            args.notify_on.clone().unwrap_or_default(),
            args.notify_regex.clone(),
            args.notify_all.unwrap_or(false),
        ) {
            Ok(conditions) => conditions,
            Err(reason) => return Ok(ToolOutput::error(reason)),
        };
        let endless = endless::reason(&args.command);
        if args.background.unwrap_or(false) || endless.is_some() {
            return self.detach(&args.command, conditions, endless, cx).await;
        }
        self.wait_for(&args, conditions, cx).await
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: jobs::PLUGIN,
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:Bash",
        "tool:BashOutput",
        "tool:KillShell",
        "command:!",
        "command:bash.promote",
    ],
    requires: &[],
    config: None,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct BashPlugin;

#[async_trait]
impl Plugin for BashPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let jobs = Arc::new(Jobs::new());
        let promotions = Arc::new(Promotions::new());
        registrar.tool(Arc::new(BashTool::new(jobs.clone(), promotions.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(BashOutputTool::new(jobs.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(KillShellTool::new(jobs)) as Arc<dyn Tool>);
        registrar.add(Contribution::Command(
            Arc::new(ShellCommand) as Arc<dyn Command>
        ));
        registrar.add(Contribution::Command(
            Arc::new(PromoteCommand::new(promotions)) as Arc<dyn Command>,
        ));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Mutex;

    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, CommandContext, Contribution, Delivery, Env,
        HostApi, HostHandle, Input, IntentId, InteractionKind, ItemBody, ItemId, KernelError,
        Prompter, SessionId, ToolHost, TurnId,
    };

    /// A tool host that keeps every progress tail it is handed, which is all a
    /// shell command ever reaches for.
    #[derive(Debug, Default)]
    pub(crate) struct RecordingHost {
        tails: Mutex<Vec<String>>,
    }

    impl RecordingHost {
        pub(crate) fn tails(&self) -> Vec<String> {
            self.tails.lock().expect("the tails").clone()
        }
    }

    #[async_trait]
    impl Prompter for RecordingHost {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }

    #[async_trait]
    impl ToolHost for RecordingHost {
        fn progress(&self, _item: &ItemId, tail: String) {
            self.tails.lock().expect("the tails").push(tail);
        }

        async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
            Ok(ItemId::from_raw("itm_test"))
        }
    }

    /// The host as this crate's tests need it: what a job delivered, and what
    /// it published. Everything else is the sdk's `NoHost`, which answers with
    /// an error, because nothing here asks it anything.
    #[derive(Debug, Default)]
    pub(crate) struct Kernel {
        delivered: Mutex<Vec<(SessionId, String, Delivery)>>,
        signals: Mutex<Vec<(SessionId, String, Value)>>,
        /// Every delivery fails, as it does when the session has gone.
        pub(crate) closed: bool,
    }

    impl Kernel {
        pub(crate) fn handle(self: &Arc<Self>) -> HostHandle {
            HostHandle(self.clone())
        }

        pub(crate) fn delivered(&self) -> Vec<(SessionId, String, Delivery)> {
            self.delivered.lock().expect("the deliveries").clone()
        }

        pub(crate) fn signals(&self) -> Vec<(SessionId, String, Value)> {
            self.signals.lock().expect("the signals").clone()
        }
    }

    #[async_trait]
    impl HostApi for Kernel {
        async fn sessions(
            &self,
            filter: bingo_sdk::SessionFilter,
        ) -> Result<Vec<bingo_sdk::SessionSummary>, KernelError> {
            bingo_sdk::testing::NoHost.sessions(filter).await
        }

        async fn open(
            &self,
            selector: bingo_sdk::SessionSelector,
            who: bingo_sdk::ClientIdentity,
            options: bingo_sdk::OpenOptions,
        ) -> Result<bingo_sdk::Attachment, KernelError> {
            bingo_sdk::testing::NoHost
                .open(selector, who, options)
                .await
        }

        async fn close(
            &self,
            session: &SessionId,
            reason: bingo_sdk::CloseReason,
        ) -> Result<(), KernelError> {
            bingo_sdk::testing::NoHost.close(session, reason).await
        }

        async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
            bingo_sdk::testing::NoHost.delete(session).await
        }

        async fn deliver(
            &self,
            to: &SessionId,
            _intent: IntentId,
            input: Input,
            delivery: Delivery,
        ) -> Result<(), KernelError> {
            if self.closed {
                return Err(KernelError::new(
                    bingo_sdk::ErrorCode::SessionClosed,
                    "the session is closed",
                ));
            }
            let Input::Text { text, .. } = input else {
                return Err(KernelError::new(
                    bingo_sdk::ErrorCode::InvalidInput,
                    "a peer delivers text",
                ));
            };
            self.delivered
                .lock()
                .expect("the deliveries")
                .push((to.clone(), text, delivery));
            Ok(())
        }

        async fn extend(
            &self,
            session: &SessionId,
            plugin: &str,
            kind: &str,
            payload: Value,
        ) -> Result<(), KernelError> {
            bingo_sdk::testing::NoHost
                .extend(session, plugin, kind, payload)
                .await
        }

        async fn signal(
            &self,
            session: &SessionId,
            _plugin: &str,
            kind: &str,
            payload: Value,
        ) -> Result<(), KernelError> {
            self.signals.lock().expect("the signals").push((
                session.clone(),
                kind.to_string(),
                payload,
            ));
            Ok(())
        }

        async fn catalog(
            &self,
            kind: bingo_sdk::CatalogKind,
        ) -> Result<bingo_sdk::Catalog, KernelError> {
            bingo_sdk::testing::NoHost.catalog(kind).await
        }

        fn gateway_events(&self) -> bingo_sdk::GatewayStream {
            bingo_sdk::testing::NoHost.gateway_events()
        }

        fn service_any(&self, _key: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            None
        }
    }

    /// A call context in a scratch directory, and the host recording it.
    pub(crate) fn context() -> (Arc<RecordingHost>, ToolContext) {
        context_in(std::env::temp_dir())
    }

    pub(crate) fn context_in(cwd: PathBuf) -> (Arc<RecordingHost>, ToolContext) {
        let (host, cx) = call_context(cwd, bingo_sdk::testing::NoHost::handle());
        (host, cx)
    }

    /// A call whose host is a kernel double, for the paths that deliver and
    /// publish. `data_dir` is the scratch directory too, so a job's log lands
    /// beside whatever the command wrote.
    pub(crate) fn call_context(
        cwd: PathBuf,
        host: HostHandle,
    ) -> (Arc<RecordingHost>, ToolContext) {
        let call = Arc::new(RecordingHost::default());
        let cx = ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd: cwd.clone(),
            cancel: CancellationToken::new(),
            env: Arc::new(Env {
                home: cwd.clone(),
                config_dir: cwd.clone(),
                data_dir: cwd,
            }),
            host,
            call: call.clone(),
        };
        (call, cx)
    }

    /// A command line as the kernel hands one to a command.
    pub(crate) fn command_context(cwd: PathBuf) -> CommandContext {
        CommandContext {
            session: SessionId::from_raw("ses_test"),
            cwd,
            host: bingo_sdk::testing::NoHost::handle(),
        }
    }

    /// The tool with a table and a promotion registry of its own.
    pub(crate) fn tool() -> (Arc<Jobs>, Arc<Promotions>, BashTool) {
        let jobs = Arc::new(Jobs::new());
        let promotions = Arc::new(Promotions::new());
        let tool = BashTool::new(jobs.clone(), promotions.clone());
        (jobs, promotions, tool)
    }

    fn text(out: &ToolOutput) -> String {
        out.parts[0].as_text().expect("text").to_string()
    }

    /// A scratch directory that is both the working directory and the data
    /// directory, so a job's log is under it.
    fn scratch() -> (tempfile::TempDir, Arc<Kernel>, ToolContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let kernel = Arc::new(Kernel::default());
        let (_call, cx) = call_context(dir.path().to_path_buf(), kernel.handle());
        (dir, kernel, cx)
    }

    /// The job the last `Bash` call filed.
    fn only_job(jobs: &Jobs) -> Arc<Job> {
        let running = jobs.running_in(&SessionId::from_raw("ses_test"));
        assert_eq!(running.len(), 1, "one job was filed");
        running[0].clone()
    }

    #[test]
    fn the_plugin_registers_three_verbs_and_two_commands() {
        let mut registrar = Registrar::new(
            "bingo.tools.bash",
            Value::Null,
            bingo_sdk::Env::rooted("/tmp"),
        );
        BashPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        let names: Vec<String> = contributions
            .iter()
            .map(|c| match c {
                Contribution::Tool(tool) => tool.spec().name.clone(),
                Contribution::Command(command) => command.spec().name.clone(),
                other => panic!("unexpected contribution {other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            ["Bash", "BashOutput", "KillShell", "!", "bash.promote"]
        );
        assert_eq!(BashPlugin.manifest().id, "bingo.tools.bash");
        assert_eq!(
            BashPlugin.manifest().provides,
            &[
                "tool:Bash",
                "tool:BashOutput",
                "tool:KillShell",
                "command:!",
                "command:bash.promote"
            ]
        );
    }

    #[test]
    fn the_traits_fail_closed_except_for_trust_and_running_together() {
        let (_jobs, _promotions, tool) = tool();
        let traits = tool.traits(&Value::Null);
        assert!(traits.trusted);
        assert!(!traits.read_only);
        assert!(
            traits.concurrency_safe,
            "two commands in one step run together"
        );
        assert!(!traits.edit);
        assert!(!traits.destructive);
    }

    #[test]
    fn the_subject_is_the_command_a_rule_would_match() {
        let (_jobs, _promotions, tool) = tool();
        let subjects = tool.subjects(&serde_json::json!({"command": "ls -la"}), Path::new("/"));
        assert_eq!(
            subjects,
            vec![Subject::Command {
                command: "ls -la".into()
            }]
        );
        assert!(tool.subjects(&Value::Null, Path::new("/")).is_empty());
    }

    #[test]
    fn nothing_about_a_command_is_a_decision_only_a_person_may_take() {
        let (_jobs, _promotions, tool) = tool();
        assert!(tool.confirm(&Value::Null).is_none());
        assert!(tool.preview(&Value::Null, Path::new("/")).is_none());
    }

    #[test]
    fn the_spec_advertises_the_arguments_the_real_shell_and_the_async_way() {
        let (_jobs, _promotions, tool) = tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "Bash");
        assert!(
            spec.description.contains(run::shell()),
            "{}",
            spec.description
        );
        assert!(
            spec.description.contains("background: true"),
            "the description leans async (ADR-0018 §1)"
        );
        assert_eq!(spec.input_schema["type"], "object");
        for field in [
            "command",
            "timeout",
            "description",
            "background",
            "notify_on",
            "notify_regex",
            "notify_all",
        ] {
            assert!(
                spec.input_schema["properties"][field].is_object(),
                "no {field} in the schema"
            );
        }
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["command"])
        );
    }

    #[test]
    fn a_call_bounds_itself_within_the_tool_s_ceiling() {
        assert_eq!(deadline(None), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(deadline(Some(5_000)), Duration::from_millis(5_000));
        assert_eq!(
            deadline(Some(u64::MAX)),
            Duration::from_millis(MAX_TIMEOUT_MS)
        );
    }

    #[tokio::test]
    async fn a_command_comes_back_in_the_shape_the_model_reads() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(serde_json::json!({"command": "echo hi"}), &cx)
            .await
            .expect("the call ran");
        assert_eq!(text(&out), "$ echo hi\nhi\n[Exited with code 0]");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn a_failing_command_is_an_error_result_with_its_code() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(serde_json::json!({"command": "exit 7"}), &cx)
            .await
            .expect("the call ran");
        assert!(out.is_error);
        assert!(
            text(&out).ends_with("[Exited with code 7]"),
            "{}",
            text(&out)
        );
    }

    #[tokio::test]
    async fn an_interactive_command_is_refused_without_being_run() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(serde_json::json!({"command": "top"}), &cx)
            .await
            .expect("the call answered");
        assert!(out.is_error);
        assert!(text(&out).contains("rejected"), "{}", text(&out));
        assert!(!text(&out).starts_with("$ "), "the command was run anyway");
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command_and_says_how_long_it_waited() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(
                serde_json::json!({"command": "sleep 30", "timeout": 200}),
                &cx,
            )
            .await
            .expect("the call ran");
        assert!(out.is_error);
        assert!(
            text(&out).ends_with("[Killed after 0.2s timeout]"),
            "{}",
            text(&out)
        );
    }

    #[tokio::test]
    /// What an interrupt does to a call, from the tool's side: the future is
    /// dropped, so there is no answer at all — the kernel writes the
    /// interruption marker instead. That the process tree goes with the drop
    /// is `run`'s own test; this one holds the tool to answering nothing.
    async fn a_dropped_call_answers_nothing() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let mut running = Box::pin(tool.call(
            serde_json::json!({ "command": "echo started; sleep 30" }),
            &cx,
        ));
        let held = tokio::time::timeout(Duration::from_millis(200), &mut running).await;
        assert!(
            held.is_err(),
            "a command that is still running answers nothing"
        );
        drop(running);
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        let (_jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let error = tool.call(serde_json::json!({}), &cx).await.err();
        assert!(matches!(error, Some(ToolError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn a_pattern_that_cannot_compile_is_an_error_result_and_runs_nothing() {
        let (jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(
                serde_json::json!({"command": "echo hi", "background": true, "notify_regex": "(unclosed"}),
                &cx,
            )
            .await
            .expect("the call answered");
        assert!(out.is_error);
        assert!(text(&out).contains("notify_regex"), "{}", text(&out));
        assert!(
            jobs.running_in(&cx.session).is_empty(),
            "nothing was started"
        );
    }

    /// An ongoing watch with nothing to watch for is refused the same way: a
    /// job that can never notify is worse than a call that comes back to be
    /// corrected (ADR-0018 §8).
    #[tokio::test]
    async fn notify_all_with_no_condition_is_an_error_result_and_runs_nothing() {
        let (jobs, _promotions, tool) = tool();
        let (_host, cx) = context();
        let out = tool
            .call(
                serde_json::json!({"command": "echo hi", "background": true, "notify_all": true}),
                &cx,
            )
            .await
            .expect("the call answered");
        assert!(out.is_error);
        assert!(text(&out).contains("notify_all"), "{}", text(&out));
        assert!(text(&out).contains("notify_on"), "{}", text(&out));
        assert!(
            jobs.running_in(&cx.session).is_empty(),
            "nothing was started"
        );
    }

    // ---- background (ADR-0018 §2) ----------------------------------------

    #[tokio::test]
    async fn a_background_call_answers_at_once_with_a_job_and_its_log() {
        let (dir, kernel, cx) = scratch();
        let (jobs, _promotions, tool) = tool();
        let out = tool
            .call(
                serde_json::json!({"command": "sleep 30", "background": true}),
                &cx,
            )
            .await
            .expect("the call answered");
        assert!(!out.is_error, "{}", text(&out));
        let job = only_job(&jobs);
        assert!(text(&out).contains(&job.id), "{}", text(&out));
        assert!(text(&out).contains("BashOutput"), "{}", text(&out));
        assert!(text(&out).contains("KillShell"), "{}", text(&out));
        assert!(
            job.log.starts_with(dir.path().join("bash")),
            "{:?}",
            job.log
        );
        assert!(job.log.exists(), "the log is there from the start");
        assert_eq!(job.state(), jobs::State::Running);
        // The rail hears about it the moment it is filed.
        let signals = kernel.signals();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].1, jobs::KIND);
        assert_eq!(signals[0].2["rows"][0][0], Value::String(job.id.clone()));
    }

    /// The whole of brick 3: it ends, the session is woken, and the rail is
    /// left with nothing.
    #[tokio::test]
    async fn a_job_that_ends_wakes_the_session_and_clears_the_rail() {
        let (_dir, kernel, cx) = scratch();
        let (jobs, _promotions, tool) = tool();
        tool.call(
            serde_json::json!({"command": "echo done; exit 3", "background": true}),
            &cx,
        )
        .await
        .expect("the call answered");
        let job = only_job(&jobs);
        assert_eq!(job.wait().await, jobs::State::Exited { code: 3 });
        // The task publishes and delivers after it settles the state.
        for _ in 0..200 {
            if !kernel.delivered().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let delivered = kernel.delivered();
        assert_eq!(delivered.len(), 1, "one message, at the end");
        assert_eq!(delivered[0].0, cx.session);
        assert_eq!(delivered[0].2, bingo_sdk::Delivery::Wake);
        assert!(
            delivered[0].1.contains("exited with code 3"),
            "{:?}",
            delivered[0].1
        );
        assert!(delivered[0].1.contains(&job.id), "{:?}", delivered[0].1);

        let last = kernel.signals().last().cloned().expect("a signal");
        assert_eq!(last.2, Value::Null, "no job is running, so no card");
        assert_eq!(
            std::fs::read_to_string(&job.log).expect("the log"),
            "done\n"
        );
    }

    #[tokio::test]
    async fn a_line_the_call_asked_about_wakes_the_session_before_the_end() {
        let (_dir, kernel, cx) = scratch();
        let (_jobs, _promotions, tool) = tool();
        tool.call(
            serde_json::json!({
                "command": "echo warming; echo BOOM; sleep 5",
                "background": true,
                "notify_on": ["BOOM"],
            }),
            &cx,
        )
        .await
        .expect("the call answered");
        for _ in 0..300 {
            if !kernel.delivered().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let delivered = kernel.delivered();
        assert_eq!(delivered.len(), 1, "the hit, and the job is still going");
        assert!(
            delivered[0].1.contains("still running"),
            "{:?}",
            delivered[0].1
        );
        assert!(delivered[0].1.contains("BOOM"), "{:?}", delivered[0].1);
    }

    /// Output growing is never news (ADR-0018 §4).
    #[tokio::test]
    async fn a_job_that_only_writes_wakes_nobody() {
        let (_dir, kernel, cx) = scratch();
        let (_jobs, _promotions, tool) = tool();
        tool.call(
            serde_json::json!({"command": "for i in 1 2 3 4 5; do echo line$i; sleep 0.05; done; sleep 5", "background": true}),
            &cx,
        )
        .await
        .expect("the call answered");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            kernel.delivered().is_empty(),
            "growth woke someone: {:?}",
            kernel.delivered()
        );
    }

    /// A session that has gone takes the message nowhere, and the log says so
    /// rather than the task failing (the plan's R-wake).
    #[tokio::test]
    async fn a_job_whose_session_is_gone_writes_that_into_its_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let kernel = Arc::new(Kernel {
            closed: true,
            ..Kernel::default()
        });
        let (_call, cx) = call_context(dir.path().to_path_buf(), kernel.handle());
        let (jobs, _promotions, tool) = tool();
        tool.call(
            serde_json::json!({"command": "echo bye", "background": true}),
            &cx,
        )
        .await
        .expect("the call answered");
        let job = only_job(&jobs);
        job.wait().await;
        for _ in 0..200 {
            let log = std::fs::read_to_string(&job.log).unwrap_or_default();
            if log.contains("nobody was told") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "the log never said the session was gone: {:?}",
            std::fs::read_to_string(&job.log)
        );
    }

    // ---- backgrounded unbidden (ADR-0018 §5) ------------------------------

    #[tokio::test]
    async fn a_command_that_could_never_finish_is_backgrounded_with_the_reason() {
        let (_dir, _kernel, cx) = scratch();
        let (jobs, _promotions, tool) = tool();
        let out = tool
            .call(
                serde_json::json!({"command": "tail -f /etc/hosts", "timeout": 300}),
                &cx,
            )
            .await
            .expect("the call answered");
        assert!(!out.is_error, "{}", text(&out));
        assert!(
            text(&out).contains("backgrounded although the call did not ask"),
            "{}",
            text(&out)
        );
        assert!(text(&out).contains("`tail -f`"), "{}", text(&out));
        let job = only_job(&jobs);
        assert_eq!(job.state(), jobs::State::Running, "and it is still going");
        job.ask_to_die();
        job.wait().await;
    }

    #[tokio::test]
    async fn an_ordinary_command_is_still_waited_for() {
        let (_dir, _kernel, cx) = scratch();
        let (jobs, _promotions, tool) = tool();
        let out = tool
            .call(serde_json::json!({"command": "tail -n 1 /etc/hosts"}), &cx)
            .await
            .expect("the call ran");
        assert!(text(&out).starts_with("$ "), "{}", text(&out));
        assert!(jobs.running_in(&cx.session).is_empty(), "nothing was filed");
    }

    // ---- promotion (ADR-0018 §6) -----------------------------------------

    #[tokio::test]
    async fn a_person_moves_a_running_command_into_the_background() {
        let (_dir, kernel, cx) = scratch();
        let (jobs, promotions, tool) = tool();
        let flipping = promotions.clone();
        tokio::spawn(async move {
            for _ in 0..300 {
                if flipping.promote("call_test") {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });

        let started = std::time::Instant::now();
        let out = tool
            .call(
                serde_json::json!({"command": "echo before; sleep 30", "timeout": 30000}),
                &cx,
            )
            .await
            .expect("the call answered early");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the call waited for the command anyway"
        );
        assert!(!out.is_error, "{}", text(&out));
        let job = only_job(&jobs);
        assert!(text(&out).contains(&job.id), "{}", text(&out));
        assert!(text(&out).contains("no timeout"), "{}", text(&out));

        // The same process carried on, and what it had written is at the head
        // of the log it now writes.
        assert_eq!(job.state(), jobs::State::Running);
        for _ in 0..200 {
            if std::fs::read_to_string(&job.log)
                .unwrap_or_default()
                .contains("before")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            std::fs::read_to_string(&job.log)
                .unwrap_or_default()
                .starts_with("before"),
            "the buffer did not follow the process"
        );
        assert!(
            kernel
                .signals()
                .iter()
                .any(|(_, kind, payload)| kind == jobs::KIND
                    && payload["rows"][0][0] == Value::String(job.id.clone())),
            "the rail was not told"
        );
        job.ask_to_die();
        job.wait().await;
    }

    #[tokio::test]
    async fn a_call_nobody_promotes_leaves_no_listener_behind() {
        let (_jobs, promotions, tool) = tool();
        let (_host, cx) = context();
        tool.call(serde_json::json!({"command": "echo hi"}), &cx)
            .await
            .expect("the call ran");
        assert!(promotions.open().is_empty(), "the call is over");
    }
}
