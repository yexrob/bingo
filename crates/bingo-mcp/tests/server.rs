//! What the plugin does against a real MCP server.
//!
//! The server is this crate's `echo_server` example: `cargo test` builds a
//! crate's examples, so the binary is always beside the test binary
//! (`target/<profile>/examples/echo_server` next to
//! `target/<profile>/deps/<test>`), with no build script and no second
//! manifest. Everything here dials it as a person's settings would.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_mcp::{Manager, McpCommand, McpSource, Server, Status};
use bingo_sdk::{
    Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogKind, ClientIdentity,
    CloseReason, Command, CommandContext, CommandOutcome, Delivery, Env, GatewayStream, HostApi,
    HostHandle, Input, IntentId, InteractionKind, ItemBody, ItemId, KernelError, OpenOptions,
    Prompter, SessionFilter, SessionId, SessionSelector, SessionSummary, Tool, ToolContext,
    ToolHost, ToolOutput, ToolSource, TurnId, View,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};

// ---------------------------------------------------------------- the server

/// The example binary, beside the test binary that is running.
fn echo_server() -> PathBuf {
    let Ok(test) = std::env::current_exe() else {
        panic!("a running test binary knows its own path");
    };
    let Some(profile) = test.parent().and_then(Path::parent) else {
        panic!("a test binary lives at target/<profile>/deps/<test>");
    };
    let server = profile.join("examples").join("echo_server");
    assert!(
        server.exists(),
        "cargo test builds this crate's examples; {} is missing",
        server.display()
    );
    server
}

fn stdio(command: impl Into<String>, args: &[&str]) -> Server {
    Server::Stdio {
        command: command.into(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        env: BTreeMap::new(),
        cwd: None,
    }
}

/// A manager over the named servers, logging into a directory of its own.
fn manager(servers: &[(&str, Server)], disabled: &[String]) -> (Arc<Manager>, tempfile::TempDir) {
    let Ok(data) = tempfile::tempdir() else {
        panic!("a temporary data directory");
    };
    let configured = servers
        .iter()
        .map(|(name, server)| ((*name).to_string(), server.clone()))
        .collect();
    let manager = Arc::new(Manager::new(
        configured,
        disabled,
        data.path().to_path_buf(),
    ));
    (manager, data)
}

/// One connected `test` server, dialled.
async fn connected() -> (Arc<Manager>, tempfile::TempDir) {
    let (manager, data) = manager(
        &[("test", stdio(echo_server().display().to_string(), &[]))],
        &[],
    );
    manager.dial_enabled().await;
    assert_eq!(
        manager.statuses().await,
        vec![("test".to_string(), Status::Connected { tools: 5 })],
        "the example server offers echo, noisy, boom, whereami and ask"
    );
    (manager, data)
}

async fn tool_named(manager: &Arc<Manager>, name: &str) -> Arc<dyn Tool> {
    let source = McpSource::new(Arc::clone(manager));
    source
        .tools()
        .await
        .into_iter()
        .find(|tool| tool.spec().name == name)
        .unwrap_or_else(|| panic!("{name} is not among this server's tools"))
}

/// Wait for a server to reach a state, or give up. Dialling is deliberately
/// asynchronous, so a test that asks about it polls rather than sleeps.
async fn settles(manager: &Arc<Manager>, name: &str, wanted: impl Fn(&Status) -> bool) -> Status {
    for _ in 0..600 {
        let statuses = manager.statuses().await;
        let status = statuses
            .iter()
            .find(|(server, _)| server == name)
            .map(|(_, status)| status.clone())
            .unwrap_or_else(|| panic!("{name} is not configured"));
        if wanted(&status) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{name} never settled");
}

fn connected_status(status: &Status) -> bool {
    matches!(status, Status::Connected { .. })
}

// ----------------------------------------------------------- the two doubles

/// A tool host nothing here reaches: an MCP call asks the kernel for nothing.
#[derive(Debug)]
struct NullHost;

#[async_trait]
impl Prompter for NullHost {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        Ok(Answer::Cancel)
    }
}

#[async_trait]
impl ToolHost for NullHost {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::from_raw("itm_test"))
    }
}

/// `/mcp` asks the host nothing, so every answer here would be a bug.
struct UnusedHost;

#[async_trait]
impl HostApi for UnusedHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        unreachable!("/mcp reads no session list")
    }

    async fn open(
        &self,
        _selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        unreachable!("/mcp opens no session")
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        unreachable!("/mcp closes no session")
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        unreachable!("/mcp deletes no session")
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("this double delivers nothing")
    }

    async fn extend(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double extends nothing")
    }

    async fn signal(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double signals nothing")
    }

    async fn catalog(&self, _kind: CatalogKind) -> Result<Catalog, KernelError> {
        unreachable!("/mcp reads no catalog")
    }

    fn gateway_events(&self) -> GatewayStream {
        unreachable!("/mcp watches no gateway")
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A door that answers one question from a script and keeps the interaction it
/// was put through, so a test reads both halves of the round trip (M53).
#[derive(Debug)]
struct ScriptedDoor {
    answer: Answer,
    asked: std::sync::Mutex<Vec<InteractionKind>>,
}

impl ScriptedDoor {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<InteractionKind> {
        self.asked.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Prompter for ScriptedDoor {
    async fn ask(
        &self,
        kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(kind);
        }
        Ok(self.answer.clone())
    }
}

#[async_trait]
impl ToolHost for ScriptedDoor {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::from_raw("itm_test"))
    }
}

fn tool_context(cancel: CancellationToken) -> ToolContext {
    ToolContext {
        call_id: "call_1".into(),
        session: SessionId::from_raw("ses_test"),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: PathBuf::from("/work"),
        cancel,
        env: Arc::new(Env::rooted("/tmp")),
        host: bingo_sdk::testing::NoHost::handle(),
        call: Arc::new(NullHost),
    }
}

fn command_context() -> CommandContext {
    CommandContext {
        session: SessionId::from_raw("ses_test"),
        cwd: PathBuf::from("/work"),
        host: HostHandle(Arc::new(UnusedHost)),
    }
}

/// One call whose door answers the server's question with `answer`; the tool
/// result is the `ElicitResult` the server received, as JSON.
async fn elicited(answer: Answer) -> (Value, Vec<InteractionKind>) {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__ask").await;
    let door = ScriptedDoor::new(answer);
    let context = ToolContext {
        call: Arc::clone(&door) as Arc<dyn ToolHost>,
        ..tool_context(CancellationToken::new())
    };
    let output = match tool.call(json!({ "text": "go" }), &context).await {
        Ok(output) => output,
        Err(error) => panic!("the ask tool answered: {error}"),
    };
    let text = text_of(&output);
    match serde_json::from_str(&text) {
        Ok(result) => (result, door.asked()),
        Err(error) => panic!("the server's result is json ({error}): {text}"),
    }
}

async fn call(tool: &Arc<dyn Tool>, input: Value) -> ToolOutput {
    match tool
        .call(input, &tool_context(CancellationToken::new()))
        .await
    {
        Ok(output) => output,
        Err(error) => panic!("{}: {error}", tool.spec().name),
    }
}

fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(|part| match part {
            bingo_sdk::ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------- the set

#[tokio::test]
async fn a_connected_server_s_tools_carry_its_name_and_its_schema() {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__echo").await;
    let spec = tool.spec();
    assert_eq!(spec.name, "mcp__test__echo");
    assert_eq!(spec.description, "Return the text it was given.");
    assert_eq!(spec.meta["server"], json!("test"));
    assert_eq!(spec.input_schema["type"], json!("object"));
    assert!(
        spec.input_schema.get("$schema").is_none(),
        "the dialect marker never reaches a model: {}",
        spec.input_schema
    );

    let names: Vec<String> = McpSource::new(Arc::clone(&manager))
        .tools()
        .await
        .iter()
        .map(|tool| tool.spec().name)
        .collect();
    assert_eq!(
        names,
        [
            "mcp__test__echo",
            "mcp__test__noisy",
            "mcp__test__boom",
            "mcp__test__whereami",
            "mcp__test__ask"
        ],
        "in the order the server listed them"
    );
}

#[tokio::test]
async fn calling_a_tool_returns_what_the_server_answered() {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__echo").await;
    let output = call(&tool, json!({ "text": "hello from the other side" })).await;
    assert_eq!(text_of(&output), "hello from the other side");
    assert!(!output.is_error);
}

/// A server's `elicitation/create` reaches the session waiting on the call
/// that raised it, as one form card naming the server, and what the person
/// chose reaches the server as `accept` with the content (M53).
#[tokio::test]
async fn a_servers_question_becomes_a_form_card_and_the_answer_reaches_it() {
    let (result, asked) = elicited(Answer::Form {
        answers: vec![
            Answer::Choice {
                ids: vec!["sqlite".into()],
                other: None,
            },
            Answer::Text {
                text: "keep it small".into(),
            },
        ],
    })
    .await;
    assert_eq!(
        result,
        json!({
            "action": "accept",
            "content": { "store": "sqlite", "note": "keep it small" }
        })
    );

    assert_eq!(asked.len(), 1, "one card for the whole schema");
    let InteractionKind::Form { title, questions } = &asked[0] else {
        panic!("expected a form, got {:?}", asked[0]);
    };
    assert_eq!(
        title.as_deref(),
        Some("test: Please say how it should be built"),
        "the card names the server that is asking"
    );
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].header.as_deref(), Some("Store"));
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|option| (option.id.as_str(), option.label.as_str()))
            .collect::<Vec<_>>(),
        vec![("postgres", "Postgres"), ("sqlite", "SQLite")]
    );
    assert!(
        !questions[0].free_text,
        "a named value is chosen, never typed"
    );
    assert!(
        questions[1].free_text && questions[1].options.is_empty(),
        "a plain string is answered in words"
    );
}

/// Leaving the card is `cancel`; a question the server required and nobody
/// answered is `decline`. Both reach the server, which is what lets it decide
/// whether to offer an alternative or ask again later.
#[tokio::test]
async fn leaving_the_card_cancels_and_an_unanswered_requirement_declines() {
    let (cancelled, _) = elicited(Answer::Cancel).await;
    assert_eq!(cancelled, json!({ "action": "cancel" }));

    let (declined, _) = elicited(Answer::Form {
        answers: vec![Answer::Cancel, Answer::Cancel],
    })
    .await;
    assert_eq!(
        declined,
        json!({ "action": "decline" }),
        "the store was required"
    );
}

/// Nobody at the session is the fail-closed fate a question already meets
/// there (ADR-0039 §2): the server hears a decline rather than waiting.
#[tokio::test]
async fn a_call_with_nobody_to_ask_declines() {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__ask").await;
    let output = call(&tool, json!({ "text": "go" })).await;
    let text = text_of(&output);
    assert_eq!(
        serde_json::from_str::<Value>(&text).ok(),
        Some(json!({ "action": "cancel" })),
        "the null door leaves the card: {text}"
    );
}

#[tokio::test]
async fn a_server_that_answers_with_is_error_says_so() {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__boom").await;
    let output = call(&tool, json!({ "text": "no" })).await;
    assert!(output.is_error, "isError crosses the boundary");
    assert_eq!(text_of(&output), "boom: no");
}

/// A stdio server is spawned where its settings said, with the environment
/// its settings gave it and nothing the child had to guess.
#[tokio::test]
async fn a_child_is_spawned_in_the_directory_and_environment_it_was_given() {
    let Ok(data) = tempfile::tempdir() else {
        panic!("a temporary data directory");
    };
    let Ok(work) = tempfile::tempdir() else {
        panic!("a temporary working directory");
    };
    let Ok(work_path) = work.path().canonicalize() else {
        panic!("a real working directory");
    };
    let server = Server::Stdio {
        command: echo_server().display().to_string(),
        args: Vec::new(),
        env: BTreeMap::from([("BINGO_MCP_MARKER".to_string(), "seen".to_string())]),
        cwd: Some(work_path.clone()),
    };
    let manager = Arc::new(Manager::new(
        BTreeMap::from([("test".to_string(), server)]),
        &[],
        data.path().to_path_buf(),
    ));
    manager.dial_enabled().await;

    let tool = tool_named(&manager, "mcp__test__whereami").await;
    let output = call(&tool, json!({ "text": "" })).await;
    assert_eq!(
        text_of(&output),
        format!("{}\nseen", work_path.display()),
        "the child ran where it was told, with the environment it was given"
    );
}

#[tokio::test]
async fn a_call_the_turn_cancelled_never_waits_for_the_server() {
    let (manager, _data) = connected().await;
    let tool = tool_named(&manager, "mcp__test__echo").await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = tool
        .call(json!({ "text": "hi" }), &tool_context(cancel))
        .await
        .expect_err("a cancelled turn cancels the call");
    assert!(matches!(error, bingo_sdk::ToolError::Cancelled), "{error}");
}

/// Whatever the server claimed and whatever the model sends, an MCP tool is
/// never read-only, never trusted and never concurrency-safe: the gate has to
/// ask about every call (ADR-0009 §2).
#[tokio::test]
async fn an_mcp_tool_never_earns_a_trait_the_gate_would_trust() {
    let (manager, _data) = connected().await;
    let tools = McpSource::new(Arc::clone(&manager)).tools().await;
    let inputs = prop_oneof![
        Just(Value::Null).boxed(),
        Just(json!({})).boxed(),
        any::<String>()
            .prop_map(|text| json!({ "text": text }))
            .boxed(),
        any::<bool>().prop_map(Value::Bool).boxed(),
        any::<i64>().prop_map(|n| json!(n)).boxed(),
        proptest::collection::vec(any::<String>(), 0..4)
            .prop_map(|items| json!(items))
            .boxed(),
    ];
    let mut runner = TestRunner::new(Config::default());
    runner
        .run(&inputs, |input| {
            for tool in &tools {
                let traits = tool.traits(&input);
                prop_assert!(!traits.read_only, "{}", tool.spec().name);
                prop_assert!(!traits.trusted, "{}", tool.spec().name);
                prop_assert!(!traits.concurrency_safe, "{}", tool.spec().name);
                prop_assert!(tool.subjects(&input, Path::new("/work")).is_empty());
            }
            Ok(())
        })
        .expect("no input ever makes a server's tool trusted");
}

#[tokio::test]
async fn a_server_s_stderr_lands_in_its_log_and_never_on_the_screen() {
    let (manager, data) = connected().await;
    let log = data.path().join("logs").join("mcp-test.log");
    let banner = std::fs::read_to_string(&log).expect("the log the child writes into");
    assert!(banner.contains("echo_server: ready"), "{banner}");

    let tool = tool_named(&manager, "mcp__test__noisy").await;
    call(&tool, json!({ "text": "" })).await;
    let written = std::fs::read_to_string(&log).expect("the log");
    assert!(
        written.contains("echo_server: noisy was called"),
        "{written}"
    );
}

/// Two servers, one of which never answers: the one that landed is offered
/// while the other is still on its way, and start never waited for either.
#[tokio::test]
async fn a_server_that_hangs_holds_up_neither_the_others_nor_the_source() {
    let (manager, _data) = manager(
        &[
            ("slow", stdio("sleep", &["30"])),
            ("test", stdio(echo_server().display().to_string(), &[])),
        ],
        &[],
    );
    let source = McpSource::new(Arc::clone(&manager));
    assert!(
        source.tools().await.is_empty(),
        "nothing has landed before the dial"
    );

    let dialling = Arc::clone(&manager);
    let dial = tokio::spawn(async move { dialling.dial_enabled().await });
    settles(&manager, "test", connected_status).await;

    assert_eq!(
        source.tools().await.len(),
        5,
        "the server that answered is offered while the other is still dialling"
    );
    assert_eq!(
        manager
            .statuses()
            .await
            .iter()
            .find(|(name, _)| name == "slow")
            .map(|(_, status)| status.clone()),
        Some(Status::Connecting),
        "a server that has not answered is still on its way"
    );
    dial.abort();
}

/// The deadline itself, on a clock the test advances: a server that answers
/// nothing is a failure, with the reason a person reads in `/mcp`.
#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_fails_at_the_deadline() {
    let (manager, _data) = manager(&[("slow", stdio("sleep", &["30"]))], &[]);
    assert_eq!(
        manager.statuses().await,
        vec![("slow".to_string(), Status::Connecting)],
        "connecting, from the moment it was configured"
    );

    let started = tokio::time::Instant::now();
    manager.dial_enabled().await;
    let waited = started.elapsed();

    assert!(
        waited <= bingo_mcp::CONNECT_TIMEOUT + Duration::from_secs(1),
        "the dial took {waited:?}"
    );
    let statuses = manager.statuses().await;
    let Some((_, Status::Failed { why })) = statuses.first() else {
        panic!("a server that answers nothing failed: {statuses:?}");
    };
    assert!(why.contains("timed out"), "{why}");
}

#[tokio::test]
async fn disabling_a_server_takes_its_tools_out_of_the_source() {
    let (manager, _data) = connected().await;
    let source = McpSource::new(Arc::clone(&manager));
    assert_eq!(source.tools().await.len(), 5);

    let command = McpCommand::new(Arc::clone(&manager));
    let outcome = command
        .run("disable test", &command_context())
        .await
        .expect("a configured server");
    assert_eq!(
        outcome,
        CommandOutcome::Applied {
            message: Some("disabled test".into())
        }
    );
    assert!(source.tools().await.is_empty());
    assert_eq!(manager.statuses().await[0].1, Status::Disabled);
}

#[tokio::test]
async fn reconnecting_dials_the_server_again() {
    let (manager, _data) = connected().await;
    let command = McpCommand::new(Arc::clone(&manager));

    let outcome = command
        .run("reconnect test", &command_context())
        .await
        .expect("a configured server");
    assert_eq!(
        outcome,
        CommandOutcome::Applied {
            message: Some("dialling test again".into())
        },
        "the command answers when the dial began, not when it landed"
    );
    assert_eq!(
        settles(&manager, "test", connected_status).await,
        Status::Connected { tools: 5 }
    );
}

#[tokio::test]
async fn a_disabled_server_is_enabled_before_it_is_dialled_again() {
    let (manager, _data) = connected().await;
    let command = McpCommand::new(Arc::clone(&manager));
    command
        .run("disable test", &command_context())
        .await
        .expect("a configured server");

    let outcome = command
        .run("reconnect test", &command_context())
        .await
        .expect("a configured server");
    assert_eq!(
        outcome,
        CommandOutcome::Applied {
            message: Some("test is disabled; /mcp enable test first".into())
        }
    );

    let outcome = command
        .run("enable test", &command_context())
        .await
        .expect("a configured server");
    assert_eq!(
        outcome,
        CommandOutcome::Applied {
            message: Some("enabled test; dialling it".into())
        }
    );
    assert_eq!(
        settles(&manager, "test", connected_status).await,
        Status::Connected { tools: 5 }
    );
}

#[tokio::test]
async fn a_server_nobody_configured_is_refused() {
    let (manager, _data) = connected().await;
    let command = McpCommand::new(manager);
    let error = command
        .run("reconnect nothing", &command_context())
        .await
        .expect_err("no such server");
    assert_eq!(error.code, bingo_sdk::ErrorCode::InvalidInput);
    assert!(error.message.contains("configured: test"), "{error}");
}

#[tokio::test]
async fn the_table_says_what_every_server_is_doing() {
    let (manager, _data) = manager(
        &[
            ("off", stdio("sleep", &["30"])),
            ("test", stdio(echo_server().display().to_string(), &[])),
            ("waiting", stdio("sleep", &["30"])),
        ],
        &["off".to_string()],
    );
    let dialling = Arc::clone(&manager);
    let dial = tokio::spawn(async move { dialling.dial_enabled().await });
    settles(&manager, "test", connected_status).await;

    let outcome = McpCommand::new(Arc::clone(&manager))
        .run("", &command_context())
        .await
        .expect("a table");
    let CommandOutcome::View {
        view: View::Table { headers, rows },
    } = outcome
    else {
        panic!("/mcp with no argument is a table");
    };
    insta::assert_json_snapshot!(json!({ "headers": headers, "rows": rows }));
    dial.abort();
}
