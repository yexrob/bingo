//! The host protocol's side of a run: Claude Code's multi-turn stdin.
//!
//! `input` is the wire — what a line means. This is what the run does with
//! one: a prompt is a turn, an interrupt stops the head turn, and a permission
//! the gate opens is asked of the host and answered by its verdict. The run
//! ends when stdin has closed and every prompt submitted has had its turn.
//!
//! Stdout carries the protocol here as well as the transcript: an
//! acknowledgement and a permission request are lines the host reads, not
//! prose, so they go there whatever the output format is. A host that wants
//! nothing else on stdout asks for `--output-format stream-json` too.

use std::collections::HashMap;
use std::io::{self, Write};

use bingo_sdk::{
    Activation, Answer, Event, Exit, Frame, Input, IntentId, IntentOutcome, Interaction,
    InteractionId, InteractionKind, InterruptScope, ItemBody, KernelError, Origin, SessionHandle,
    SessionState, TurnId, TurnStatus,
};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::input::{self, Decision, Line};
use crate::render::write_line;
use crate::{
    Attached, Next, SURFACE_ID, close_message, closed, error_report, exit_for, notice_report,
    refuse, stdio_error,
};

impl Attached<'_> {
    /// The host protocol: a turn per prompt, until stdin has closed and every
    /// prompt has been answered. A turn that fails ends its own line and
    /// nothing else, as Claude Code's does; the run reports it in the exit code
    /// once there is nothing left to read.
    pub(crate) async fn hosted(
        mut self,
        mut lines: mpsc::Receiver<String>,
        first: Option<String>,
        prompting: bool,
    ) -> Result<Exit, KernelError> {
        let mut host = Hosted::new(prompting, self.human());
        if let Some(prompt) = first {
            host.submit(&self.handle, prompt);
        }
        let mut open = true;
        while open || !host.settled() {
            tokio::select! {
                frame = self.events.next() => match frame {
                    Some(frame) => {
                        if !self.show(&frame)? {
                            continue;
                        }
                        match self.reaction(&frame, &mut host)? {
                            Next::Await => {}
                            Next::Resync => self.resync().await?,
                            Next::Exit(exit) => return Ok(exit),
                        }
                    }
                    None => return self.ended(),
                },
                line = lines.recv(), if open => match line {
                    Some(line) => self.take(&line, &mut host)?,
                    None => open = false,
                },
            }
        }
        Ok(host.exit())
    }

    fn reaction(&mut self, frame: &Frame, host: &mut Hosted) -> Result<Next, KernelError> {
        host.react(
            &frame.event,
            &self.snapshot,
            &self.handle,
            &mut *self.out,
            &mut *self.err,
        )
    }

    fn ended(&mut self) -> Result<Exit, KernelError> {
        let human = self.human();
        closed(
            "the event stream ended while stdin was open",
            &mut *self.err,
            human,
        )
    }

    /// One line the host wrote. A line this surface cannot read is a
    /// diagnostic, never the end of the run.
    fn take(&mut self, line: &str, host: &mut Hosted) -> Result<(), KernelError> {
        if line.trim().is_empty() {
            return Ok(());
        }
        let human = self.human();
        match input::parse_line(line) {
            Ok(parsed) => host.take(parsed, &self.handle, &mut *self.out, &mut *self.err),
            Err(e) => {
                let report = notice_report("INPUT_LINE_IGNORED", &e.to_string(), human);
                writeln!(self.err, "{report}").and_then(|()| self.err.flush())
            }
        }
        .map_err(stdio_error)
    }
}

/// The surface's side of a hosted run: the prompts it has submitted and not
/// been answered for, and the permission requests it is owed a verdict on. The
/// session's own state is the snapshot's; nothing here is a second copy of it.
#[derive(Debug)]
struct Hosted {
    /// Whether the host answers permission prompts (`--permission-prompt-tool
    /// stdio`). Without it they are refused, as they are for any run with
    /// nobody at the keyboard.
    prompting: bool,
    /// Stderr is a terminal: diagnostics are for a person.
    human: bool,
    /// Submissions with no completed turn behind them. The run ends when stdin
    /// has closed and this is empty.
    awaiting: Vec<IntentId>,
    /// Permission requests written and not yet answered, by request id, so a
    /// second prompt opened while one is pending is told apart from it.
    asked: HashMap<String, Asked>,
    /// The last turn or submission that did not succeed; the run's exit code.
    failure: Option<Exit>,
}

/// A permission request a host owes an answer to.
#[derive(Debug)]
struct Asked {
    interaction: InteractionId,
    /// What the tool was asked to do, so a rewritten `updatedInput` is caught.
    input: Value,
}

impl Hosted {
    fn new(prompting: bool, human: bool) -> Self {
        Self {
            prompting,
            human,
            awaiting: Vec::new(),
            asked: HashMap::new(),
            failure: None,
        }
    }

    fn submit(&mut self, handle: &SessionHandle, text: String) {
        let intent = IntentId::mint();
        self.awaiting.push(intent.clone());
        handle.submit(intent, Input::text(text, Origin::surface(SURFACE_ID)));
    }

    /// Every prompt submitted has had its turn.
    fn settled(&self) -> bool {
        self.awaiting.is_empty()
    }

    fn exit(&self) -> Exit {
        self.failure.unwrap_or(Exit { code: 0 })
    }

    /// One line the host wrote, carried out.
    fn take(
        &mut self,
        line: Line,
        handle: &SessionHandle,
        out: &mut (dyn Write + Send),
        err: &mut (dyn Write + Send),
    ) -> io::Result<()> {
        match line {
            Line::User { text } => {
                self.submit(handle, text);
                Ok(())
            }
            // The turn is stopped before the acknowledgement is written, so a
            // host that reads one knows the interruption was asked for.
            Line::Interrupt { request_id } => {
                handle.interrupt(IntentId::mint(), InterruptScope::Head);
                write_line(&input::control_ok(&request_id).to_string(), out)
            }
            Line::Decision {
                request_id,
                decision,
            } => self.decided(&request_id, decision, handle, err),
            Line::Unsupported {
                request_id,
                subtype,
            } => {
                let why = format!("this surface answers no `{subtype}` control request");
                write_line(&input::control_error(&request_id, &why).to_string(), out)
            }
        }
    }

    /// The host's verdict on a call the gate stopped.
    fn decided(
        &mut self,
        request_id: &str,
        decision: Decision,
        handle: &SessionHandle,
        err: &mut (dyn Write + Send),
    ) -> io::Result<()> {
        let Some(asked) = self.asked.remove(request_id) else {
            let text = format!("no permission request `{request_id}` is open");
            let report = notice_report("UNKNOWN_REQUEST", &text, self.human);
            return writeln!(err, "{report}").and_then(|()| err.flush());
        };
        handle.answer(
            IntentId::mint(),
            asked.interaction,
            answer_for(decision, &asked.input),
            Activation::Programmatic,
        );
        Ok(())
    }

    /// Everything a frame asks of a hosted run once it has been rendered.
    fn react(
        &mut self,
        event: &Event,
        state: &SessionState,
        handle: &SessionHandle,
        out: &mut (dyn Write + Send),
        err: &mut (dyn Write + Send),
    ) -> Result<Next, KernelError> {
        match event {
            Event::InteractionOpened { interaction } => self
                .opened(interaction, state, handle, out)
                .map_err(stdio_error),
            // The prompt went away with its turn; no verdict can arrive for it.
            Event::InteractionResolved { id, .. } | Event::InteractionCancelled { id, .. } => {
                self.asked.retain(|_, asked| &asked.interaction != id);
                Ok(Next::Await)
            }
            Event::TurnCompleted { turn, status, .. } => {
                self.completed(turn, status, state);
                Ok(Next::Await)
            }
            Event::IntentAck { intent, outcome } => self.acked(intent, outcome, err),
            Event::Lagged { .. } => Ok(Next::Resync),
            Event::SessionClosed { reason } => {
                closed(&close_message(reason), err, self.human).map(Next::Exit)
            }
            _ => Ok(Next::Await),
        }
    }

    /// A permission the host answers, or the refusal a run with nobody to ask
    /// gives. Questions and every other kind stay refused: this protocol has a
    /// shape for a tool call and for nothing else.
    fn opened(
        &mut self,
        interaction: &Interaction,
        state: &SessionState,
        handle: &SessionHandle,
        out: &mut (dyn Write + Send),
    ) -> io::Result<Next> {
        let InteractionKind::Permission {
            tool,
            session_scope,
            ..
        } = &interaction.kind
        else {
            return Ok(decline(
                interaction,
                handle,
                "this surface cannot answer that",
            ));
        };
        if !self.prompting {
            return Ok(decline(interaction, handle, "no permission prompt tool"));
        }
        let request_id = IntentId::mint().to_string();
        let input = tool_input(interaction, state);
        let request = input::can_use_tool(&request_id, tool, &input, session_scope.as_deref());
        write_line(&request.to_string(), out)?;
        self.asked.insert(
            request_id,
            Asked {
                interaction: interaction.id.clone(),
                input,
            },
        );
        Ok(Next::Await)
    }

    /// A submission the kernel refused will never become a turn, and one it
    /// applied at once (a command) will not either.
    fn acked(
        &mut self,
        intent: &IntentId,
        outcome: &IntentOutcome,
        err: &mut (dyn Write + Send),
    ) -> Result<Next, KernelError> {
        match outcome {
            IntentOutcome::Rejected { error } => {
                let report = error_report(error.code, &error.message, self.human);
                writeln!(err, "{report}").map_err(stdio_error)?;
                if self.forget(intent) {
                    self.failure = Some(Exit { code: 1 });
                }
            }
            IntentOutcome::Applied { .. } => {
                self.forget(intent);
            }
            IntentOutcome::TurnStarted { .. } | IntentOutcome::Queued { .. } => {}
        }
        Ok(Next::Await)
    }

    /// Every prompt this turn carried has been answered.
    fn completed(&mut self, turn: &TurnId, status: &TurnStatus, state: &SessionState) {
        let exit = exit_for(status);
        if exit.code != 0 {
            self.failure = Some(exit);
        }
        self.awaiting.retain(|intent| !carried(state, turn, intent));
    }

    /// Drop a submission from the wait; `true` when it was one.
    fn forget(&mut self, intent: &IntentId) -> bool {
        let waiting = self.awaiting.len();
        self.awaiting.retain(|other| other != intent);
        self.awaiting.len() != waiting
    }
}

/// Whether the turn carried the item that intent submitted. The folded state is
/// the only place that ties a turn to the intents behind its inputs — an input
/// queued behind a running turn is acknowledged once, and the turn that finally
/// carries it is not the one it was queued at.
fn carried(state: &SessionState, turn: &TurnId, intent: &IntentId) -> bool {
    state
        .items
        .iter()
        .any(|item| item.turn.as_ref() == Some(turn) && item.intent.as_ref() == Some(intent))
}

/// What the tool was asked to do: the call the interaction is about, read from
/// the folded state, because the interaction itself carries only a summary.
fn tool_input(interaction: &Interaction, state: &SessionState) -> Value {
    interaction
        .item
        .as_ref()
        .and_then(|id| state.item(id))
        .and_then(|item| match &item.body {
            ItemBody::ToolCall { input, .. } => Some(input.clone()),
            _ => None,
        })
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
}

/// The kernel runs the call the gate stopped or none at all, so a host that
/// answers with a different input is asking for a call this surface cannot make.
fn answer_for(decision: Decision, input: &Value) -> Answer {
    match decision {
        Decision::Allow {
            updated_input: Some(updated),
        } if &updated != input => Answer::Deny {
            feedback: Some("this surface cannot rewrite a tool call's input".into()),
        },
        Decision::Allow { .. } => Answer::AllowOnce,
        Decision::Deny { message } => Answer::Deny { feedback: message },
    }
}

/// Refuse an interaction nobody here can answer, in the narrowest way it takes.
fn decline(interaction: &Interaction, handle: &SessionHandle, why: &str) -> Next {
    handle.answer(
        IntentId::mint(),
        interaction.id.clone(),
        refuse(interaction, why),
        Activation::Programmatic,
    );
    Next::Await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use bingo_sdk::ItemStatus;
    use serde_json::json;

    use crate::drive;
    use crate::tests::{
        Run, TestConsole, TestHost, frame, locked, options, permission, question, tool_call,
    };

    /// The arguments a host drives this surface with.
    fn hosted_args(prompt_tool: Option<&str>) -> Value {
        let mut args = json!({ "inputFormat": "stream-json" });
        if let Some(tool) = prompt_tool
            && let Some(object) = args.as_object_mut()
        {
            object.insert("permissionPromptTool".into(), json!(tool));
        }
        args
    }

    fn user_line(text: &str) -> String {
        json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "parent_tool_use_id": Value::Null,
            "session_id": "ses_1",
        })
        .to_string()
    }

    fn control_response(request_id: &str, payload: Value) -> String {
        json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": payload },
        })
        .to_string()
    }

    async fn play_hosted(frames: Vec<Frame>, console: &mut TestConsole, args: Value) -> Run {
        let (host, session) = TestHost::live(frames);
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let exit = drive(&host, options(None, args), console, &mut out, &mut err).await;
        Run {
            exit,
            out: String::from_utf8_lossy(&out).into_owned(),
            err: String::from_utf8_lossy(&err).into_owned(),
            session,
        }
    }

    fn json_lines(rendered: &str) -> Vec<Value> {
        rendered
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect()
    }

    /// Two prompts, two turns, and the exit once stdin has closed and both have
    /// been answered.
    #[tokio::test]
    async fn every_user_line_is_a_turn_of_its_own() {
        let mut console = TestConsole::hosted(&[user_line("first"), user_line("second")]);
        let run = play_hosted(vec![], &mut console, hosted_args(None)).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(run.session.prompts(), ["first", "second"]);
        assert!(
            run.session.submitted().iter().all(
                |input| matches!(input, Input::Text { origin, .. } if origin.surface == "print")
            )
        );
    }

    /// `--print "prompt" --input-format stream-json`: the argument is the first
    /// turn, the lines are the rest.
    #[tokio::test]
    async fn the_prompt_argument_is_the_first_turn_of_a_hosted_run() {
        let mut console = TestConsole::hosted(&[user_line("second")]);
        let (host, session) = TestHost::live(vec![]);
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let opts = options(Some("first"), hosted_args(None));
        let exit = drive(&host, opts, &mut console, &mut out, &mut err).await;
        assert_eq!(exit, Ok(Exit { code: 0 }));
        assert_eq!(session.prompts(), ["first", "second"]);
    }

    /// Every turn writes its own `result` line, and stdout carries nothing else.
    #[tokio::test]
    async fn each_turn_writes_one_result_line_and_no_prose() {
        let mut console = TestConsole::hosted(&[user_line("first"), user_line("second")]);
        let mut args = hosted_args(None);
        if let Some(object) = args.as_object_mut() {
            object.insert("outputFormat".into(), json!("stream-json"));
        }
        let run = play_hosted(vec![], &mut console, args).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        let types: Vec<&str> = json_lines(&run.out)
            .iter()
            .filter_map(|line| line["type"].as_str().map(str::to_owned))
            .map(|owned| match owned.as_str() {
                "system" => "system",
                "result" => "result",
                _ => "other",
            })
            .collect();
        assert_eq!(types, ["system", "result", "result"], "{}", run.out);
    }

    #[tokio::test]
    async fn a_junk_line_is_a_diagnostic_and_the_run_goes_on() {
        let lines = ["{oh no".to_string(), "   ".to_string(), user_line("first")];
        let mut console = TestConsole::hosted(&lines);
        let run = play_hosted(vec![], &mut console, hosted_args(None)).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert_eq!(run.session.prompts(), ["first"]);
        assert_eq!(run.err.lines().count(), 1, "{}", run.err);
        assert!(
            run.err.starts_with("[notice] INPUT_LINE_IGNORED not JSON:"),
            "{}",
            run.err
        );
    }

    #[tokio::test]
    async fn an_interrupt_stops_the_running_turn_and_is_acknowledged() {
        let request = json!({
            "type": "control_request",
            "request_id": "req_9",
            "request": { "subtype": "interrupt" },
        })
        .to_string();
        let mut console = TestConsole::hosted(&[request]);
        let run = play_hosted(vec![], &mut console, hosted_args(None)).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert!(matches!(
            run.session.interrupts().as_slice(),
            [InterruptScope::Head]
        ));
        assert_eq!(
            json_lines(&run.out),
            vec![json!({
                "type": "control_response",
                "response": { "subtype": "success", "request_id": "req_9", "response": {} },
            })]
        );
    }

    /// A host must never wait for an answer that will not come.
    #[tokio::test]
    async fn a_control_request_this_surface_cannot_serve_is_refused() {
        let request = json!({
            "type": "control_request",
            "request_id": "req_10",
            "request": { "subtype": "initialize" },
        })
        .to_string();
        let mut console = TestConsole::hosted(&[request]);
        let run = play_hosted(vec![], &mut console, hosted_args(None)).await;
        let line = &json_lines(&run.out)[0];
        assert_eq!(line["response"]["subtype"], json!("error"));
        assert_eq!(line["response"]["request_id"], json!("req_10"));
    }

    #[tokio::test]
    async fn a_verdict_for_no_open_request_is_a_diagnostic() {
        let mut console =
            TestConsole::hosted(&[control_response("req_11", json!({ "behavior": "allow" }))]);
        let run = play_hosted(vec![], &mut console, hosted_args(Some("stdio"))).await;
        assert_eq!(run.exit, Ok(Exit { code: 0 }));
        assert!(run.session.answers().is_empty());
        assert!(run.err.contains("[notice] UNKNOWN_REQUEST"), "{}", run.err);
    }

    /// Yield until the condition holds; `false` when it never does, so a run
    /// that does nothing fails the test instead of hanging it.
    async fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        for _ in 0..10_000 {
            if ready() {
                return true;
            }
            tokio::task::yield_now().await;
        }
        false
    }

    /// A stdout the test reads while the run is still writing to it, which is
    /// what a host on the other end of the pipe does.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&locked(&self.0)).into_owned()
        }

        fn request(&self) -> Option<Value> {
            self.text()
                .lines()
                .find(|line| line.contains("can_use_tool"))
                .and_then(|line| serde_json::from_str(line).ok())
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            locked(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The permission the gate opens over a `Read` call the state knows about.
    fn asking() -> Vec<Frame> {
        vec![
            frame(
                1,
                Event::ItemStarted {
                    item: tool_call("itm_2", "Read", None, ItemStatus::Running),
                },
            ),
            frame(
                2,
                Event::InteractionOpened {
                    interaction: permission(Some("Read(//tmp)")),
                },
            ),
        ]
    }

    /// Drive a hosted run under `--permission-prompt-tool stdio` whose host
    /// answers the request it is written with `verdict`, then closes stdin.
    async fn prompted(verdict: Value) -> (Value, Vec<(InteractionId, Answer, Activation)>) {
        let (host, session) = TestHost::live(asking());
        let (mut console, lines) = TestConsole::fed();
        let mut out = Shared::default();
        let mut err: Vec<u8> = Vec::new();
        let written = out.clone();
        let answered = Arc::clone(&session);
        let asked = tokio::spawn(async move {
            if !wait_until(|| written.request().is_some()).await {
                return None;
            }
            let request = written.request()?;
            let id = request["request_id"].as_str()?.to_owned();
            lines.send(control_response(&id, verdict)).await.ok()?;
            wait_until(|| !answered.answers().is_empty()).await;
            Some(request)
        });
        let opts = options(None, hosted_args(Some("stdio")));
        let exit = drive(&host, opts, &mut console, &mut out, &mut err).await;
        assert_eq!(exit, Ok(Exit { code: 0 }));
        let request = asked
            .await
            .expect("the host task")
            .expect("a permission request on stdout");
        (request, session.answers())
    }

    #[tokio::test]
    async fn a_permission_is_asked_of_the_host_and_an_allow_reaches_the_kernel() {
        let (request, answers) = prompted(json!({ "behavior": "allow" })).await;
        assert_eq!(request["request"]["subtype"], json!("can_use_tool"));
        assert_eq!(request["request"]["tool_name"], json!("Read"));
        assert_eq!(
            request["request"]["input"],
            json!({ "file_path": "Cargo.toml" }),
            "the call the gate stopped, read from the folded state"
        );
        assert_eq!(
            request["request"]["permission_suggestions"][0]["rules"][0]["toolName"],
            json!("Read")
        );
        assert_eq!(
            answers.as_slice(),
            &[(
                InteractionId::from_raw("int_1"),
                Answer::AllowOnce,
                Activation::Programmatic,
            )]
        );
    }

    #[tokio::test]
    async fn a_deny_carries_the_hosts_message_back_to_the_model() {
        let verdict = json!({ "behavior": "deny", "message": "not that file" });
        let (_, answers) = prompted(verdict).await;
        assert_eq!(
            answers[0].1,
            Answer::Deny {
                feedback: Some("not that file".into())
            }
        );
    }

    /// A host that answers with a different input is asking for a call the
    /// kernel was never stopped on.
    #[tokio::test]
    async fn an_allow_that_rewrites_the_call_is_a_denial() {
        let verdict = json!({ "behavior": "allow", "updatedInput": { "file_path": "other.toml" } });
        let (_, answers) = prompted(verdict).await;
        assert!(matches!(answers[0].1, Answer::Deny { .. }), "{answers:?}");
    }

    #[test]
    fn an_echoed_input_is_the_same_call_and_still_an_allow() {
        let input = json!({ "file_path": "Cargo.toml" });
        assert_eq!(
            answer_for(
                Decision::Allow {
                    updated_input: Some(input.clone())
                },
                &input
            ),
            Answer::AllowOnce
        );
        assert_eq!(
            answer_for(
                Decision::Allow {
                    updated_input: None
                },
                &input
            ),
            Answer::AllowOnce
        );
        assert_eq!(
            answer_for(Decision::Deny { message: None }, &input),
            Answer::Deny { feedback: None }
        );
    }

    /// Without the flag there is nobody to ask, exactly as for a run with no
    /// terminal.
    #[tokio::test]
    async fn without_a_prompt_tool_a_permission_is_refused() {
        let (host, session) = TestHost::live(asking());
        let (mut console, lines) = TestConsole::fed();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let answered = Arc::clone(&session);
        let closer = tokio::spawn(async move {
            let refused = wait_until(|| !answered.answers().is_empty()).await;
            drop(lines);
            refused
        });
        let opts = options(None, hosted_args(None));
        let exit = drive(&host, opts, &mut console, &mut out, &mut err).await;
        assert!(closer.await.expect("the host task"), "nothing was answered");
        assert_eq!(exit, Ok(Exit { code: 0 }));
        assert_eq!(
            session.answers()[0].1,
            Answer::Deny {
                feedback: Some("no permission prompt tool".into())
            }
        );
        assert_eq!(out, b"", "nobody was asked");
    }

    /// The protocol has a shape for a tool call and for nothing else.
    #[tokio::test]
    async fn a_question_stays_refused_even_when_the_host_answers_permissions() {
        let (host, session) = TestHost::live(vec![frame(
            1,
            Event::InteractionOpened {
                interaction: question(&[("a", "Cargo.toml")]),
            },
        )]);
        let (mut console, lines) = TestConsole::fed();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let answered = Arc::clone(&session);
        let closer = tokio::spawn(async move {
            let refused = wait_until(|| !answered.answers().is_empty()).await;
            drop(lines);
            refused
        });
        let opts = options(None, hosted_args(Some("stdio")));
        let exit = drive(&host, opts, &mut console, &mut out, &mut err).await;
        assert!(closer.await.expect("the host task"), "nothing was answered");
        assert_eq!(exit, Ok(Exit { code: 0 }));
        assert_eq!(session.answers()[0].1, Answer::Cancel);
        assert_eq!(out, b"", "nobody was asked");
    }
}
