//! The `Bash` tool: one shell command per call, in its own process group. The
//! plugin also owns `!`, the same shell for the person at the keyboard
//! (`shell`).
//!
//! Four bricks under both: the tables that refuse a command before anything is
//! spawned (`reject`), the bounded `output`, the `tail` the user watches while
//! the command works, and the process lifecycle (`run`), which takes its
//! directory, its interrupt and its progress sink from whichever of the two
//! asked.
//!
//! The traits fail closed except for `trusted`: a shell command is not read-only
//! and not concurrency-safe — whether it may run at all is the permission
//! policy's decision, made from the `Command` subject. Its interrupt is `Block`,
//! because a command that has already written to the working tree cannot be
//! taken back by dropping the future; the executor lets it finish and keeps what
//! it produced, so an interrupt kills the process group and reports the partial
//! output rather than a cancellation.
//!
//! See docs/plans/M1-provider-tools-gate.md.

mod output;
mod reject;
mod run;
mod shell;
mod tail;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    Command, Contribution, Interrupt, Plugin, PluginError, PluginManifest, Registrar, ResultLimit,
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

pub use shell::ShellCommand;

/// How long a command runs when the call does not say.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The longest a call may ask for. Past ten minutes the answer belongs in a file
/// the model reads, not in a call the turn waits on.
const MAX_TIMEOUT_MS: u64 = 600_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashArgs {
    /// The command to run.
    pub command: String,
    /// How long to let it run, in milliseconds. Defaults to 120000, and 600000
    /// is the most that will be honoured.
    pub timeout: Option<u64>,
    /// What the command does, in five to ten words, in active voice.
    pub description: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BashTool;

impl BashTool {
    /// The command a call names, as the gate and the shell both see it.
    fn target(input: &Value) -> Option<String> {
        let args: BashArgs = serde_json::from_value(input.clone()).ok()?;
        Some(args.command)
    }
}

/// The bound a call sets for itself, within the tool's own ceiling.
fn deadline(timeout: Option<u64>) -> Duration {
    Duration::from_millis(timeout.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS))
}

/// The tool names the shell it actually runs, because `Bash` on its own primes
/// the model for a dialect the host may not have.
fn description() -> String {
    format!(
        "Execute a command in the local shell ({shell}) and return what it wrote together with \
         its exit code. stdout and stderr come back interleaved, under a `$ <command>` header \
         and above an `[Exited with code N]` line. Long output keeps its beginning and its end, \
         and says how much was dropped between them; redirect to a file and `Read` it when you \
         need all of it. Reading, listing and searching files is the file tools' job, not this \
         one's: a shell `cat`, `ls`, `find` or `grep` costs a permission and answers with less \
         than they do. Use this for building, testing and running programs.\n\n\
         The call waits for the command to exit, for {default} milliseconds unless `timeout` \
         says otherwise ({max} milliseconds at most). stdin is closed, so nothing can prompt: \
         a command that needs a terminal (a full-screen monitor, an editor, a pager, `sudo` \
         without `-n`, `ssh` without a remote command, a bare REPL) is refused with the reason \
         and the way round it, and so is a command that never ends on its own unless `timeout` \
         bounds it.",
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
            interrupt: Interrupt::Block,
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
        if args.timeout.is_none()
            && let Some(reason) = reject::periodic_reason(&args.command)
        {
            return Ok(ToolOutput::error(reason));
        }
        let finished = run::run(
            &args.command,
            deadline(args.timeout),
            &run::Context::of_call(cx),
        )
        .await?;
        Ok(output::shape(
            &args.command,
            &finished.output,
            finished.ended,
        ))
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tools.bash",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["tool:Bash", "command:!"],
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
        registrar.tool(Arc::new(BashTool) as Arc<dyn Tool>);
        registrar.add(Contribution::Command(
            Arc::new(ShellCommand) as Arc<dyn Command>
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
        Answer, AnswerSpec, CancellationToken, Contribution, Env, InteractionKind, ItemBody,
        ItemId, KernelError, Prompter, SessionId, ToolHost, TurnId,
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

    /// A call context in a scratch directory, and the host recording it.
    pub(crate) fn context() -> (Arc<RecordingHost>, ToolContext) {
        context_in(std::env::temp_dir())
    }

    pub(crate) fn context_in(cwd: PathBuf) -> (Arc<RecordingHost>, ToolContext) {
        let host = Arc::new(RecordingHost::default());
        let cx = ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd,
            cancel: CancellationToken::new(),
            env: Arc::new(Env {
                home: PathBuf::from("/tmp"),
                config_dir: PathBuf::from("/tmp"),
                data_dir: PathBuf::from("/tmp"),
            }),
            host: bingo_sdk::testing::NoHost::handle(),
            call: host.clone(),
        };
        (host, cx)
    }

    fn text(out: &ToolOutput) -> String {
        out.parts[0].as_text().expect("text").to_string()
    }

    #[test]
    fn the_plugin_registers_the_bash_tool_and_the_shell_command() {
        let mut registrar = Registrar::new(
            "bingo.tools.bash",
            Value::Null,
            bingo_sdk::Env::rooted("/tmp"),
        );
        BashPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 2);
        match &contributions[0] {
            Contribution::Tool(tool) => assert_eq!(tool.spec().name, "Bash"),
            other => panic!("expected a tool, got {other:?}"),
        }
        match &contributions[1] {
            Contribution::Command(command) => assert_eq!(command.spec().name, "!"),
            other => panic!("expected a command, got {other:?}"),
        }
        assert_eq!(BashPlugin.manifest().id, "bingo.tools.bash");
        assert_eq!(BashPlugin.manifest().provides, &["tool:Bash", "command:!"]);
    }

    #[test]
    fn the_traits_fail_closed_except_for_trust() {
        let traits = BashTool.traits(&Value::Null);
        assert!(traits.trusted);
        assert_eq!(traits.interrupt, Interrupt::Block);
        assert!(!traits.read_only);
        assert!(!traits.concurrency_safe);
        assert!(!traits.edit);
        assert!(!traits.destructive);
    }

    #[test]
    fn the_subject_is_the_command_a_rule_would_match() {
        let subjects = BashTool.subjects(&serde_json::json!({"command": "ls -la"}), Path::new("/"));
        assert_eq!(
            subjects,
            vec![Subject::Command {
                command: "ls -la".into()
            }]
        );
        assert!(BashTool.subjects(&Value::Null, Path::new("/")).is_empty());
    }

    #[test]
    fn nothing_about_a_command_is_a_decision_only_a_person_may_take() {
        assert!(BashTool.confirm(&Value::Null).is_none());
        assert!(BashTool.preview(&Value::Null, Path::new("/")).is_none());
    }

    #[test]
    fn the_spec_advertises_the_arguments_and_the_real_shell() {
        let spec = BashTool.spec();
        assert_eq!(spec.name, "Bash");
        assert!(
            spec.description.contains(run::shell()),
            "{}",
            spec.description
        );
        assert_eq!(spec.input_schema["type"], "object");
        for field in ["command", "timeout", "description"] {
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
        let (_host, cx) = context();
        let out = BashTool
            .call(serde_json::json!({"command": "echo hi"}), &cx)
            .await
            .expect("the call ran");
        assert_eq!(text(&out), "$ echo hi\nhi\n[Exited with code 0]");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn a_failing_command_is_an_error_result_with_its_code() {
        let (_host, cx) = context();
        let out = BashTool
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
        let (_host, cx) = context();
        let out = BashTool
            .call(serde_json::json!({"command": "top"}), &cx)
            .await
            .expect("the call answered");
        assert!(out.is_error);
        assert!(text(&out).contains("rejected"), "{}", text(&out));
        assert!(!text(&out).starts_with("$ "), "the command was run anyway");
    }

    #[tokio::test]
    async fn a_never_ending_command_is_refused_until_it_is_bounded() {
        let (_host, cx) = context();
        let refused = BashTool
            .call(serde_json::json!({"command": "tail -f /etc/hosts"}), &cx)
            .await
            .expect("the call answered");
        assert!(refused.is_error);
        assert!(text(&refused).contains("timeout"), "{}", text(&refused));
        assert!(
            !text(&refused).starts_with("$ "),
            "the command was run anyway"
        );

        let finite = BashTool
            .call(serde_json::json!({"command": "tail -n 1 /etc/hosts"}), &cx)
            .await
            .expect("the call ran");
        assert!(!finite.is_error, "{}", text(&finite));

        let bounded = BashTool
            .call(
                serde_json::json!({"command": "tail -f /etc/hosts", "timeout": 300}),
                &cx,
            )
            .await
            .expect("the call ran");
        assert!(text(&bounded).starts_with("$ "), "{}", text(&bounded));
    }

    #[tokio::test]
    async fn a_timeout_kills_the_command_and_says_how_long_it_waited() {
        let (_host, cx) = context();
        let out = BashTool
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
    async fn an_interrupt_keeps_the_output_the_command_had_produced() {
        let (_host, cx) = context();
        let cancel = cx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let out = BashTool
            .call(
                serde_json::json!({"command": "echo started; sleep 30"}),
                &cx,
            )
            .await
            .expect("a Block tool answers with what it had");
        assert!(out.is_error);
        assert!(text(&out).contains("started"), "{}", text(&out));
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        let (_host, cx) = context();
        let error = BashTool.call(serde_json::json!({}), &cx).await.err();
        assert!(matches!(error, Some(ToolError::InvalidInput(_))));
    }
}
