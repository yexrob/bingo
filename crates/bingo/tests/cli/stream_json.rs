//! The Claude Code envelope (ADR-0007 §8): a host that drives
//! `claude -p --output-format stream-json` drives bingo the same way, and with
//! `--input-format stream-json` it drives it for as long as it likes.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, ChildStdout};

use serde_json::{Value, json};

use super::*;

fn lines_of(out: &Output) -> Vec<serde_json::Value> {
    stdout(out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

#[test]
fn stream_json_is_init_then_messages_then_result_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\n").unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"text":"Looking."},{"toolCall":{"name":"Read","input":{"file_path":"notes.txt"}}}]},
            {"steps":[{"text":"One line."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--output-format", "stream-json", "--cwd"])
        .arg(dir.path())
        .arg("what is in notes.txt?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let lines = lines_of(&out);
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "system",
            "assistant",
            "assistant",
            "user",
            "assistant",
            "result"
        ],
        "{}",
        stdout(&out)
    );
    assert_eq!(lines[0]["subtype"], "init");
    assert!(
        lines[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "Read")
    );
    let session = lines[0]["session_id"].as_str().unwrap();
    assert!(lines.iter().all(|l| l["session_id"] == session));
    assert_eq!(lines[2]["message"]["content"][0]["type"], "tool_use");
    assert_eq!(lines[3]["message"]["content"][0]["type"], "tool_result");
    let result = lines.last().unwrap();
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], "One line.");
    assert_eq!(result["num_turns"], 2);
}

#[test]
fn a_failed_turn_is_a_result_line_with_errors() {
    let script =
        script(r#"{"responses":[{"steps":[{"error":{"kind":"auth","message":"bad key"}}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", tempfile::tempdir().unwrap().path())
        .args(["--print", "--output-format", "stream-json", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    let lines = lines_of(&out);
    let result = lines.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["is_error"], true);
    assert!(
        result["errors"][0].as_str().unwrap().contains("bad key"),
        "{result}"
    );
    assert!(
        result.get("result").is_none(),
        "no result text on the error arm"
    );
}

// ---- the host protocol on stdin -----------------------------------------

/// The binary as a host drives it: one JSON line at a time in, one out.
struct Host {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Every line read so far, so what the run said before the test looked is
    /// still part of the transcript it ends with.
    lines: Vec<Value>,
}

/// What was left when stdin closed.
struct Ended {
    lines: Vec<Value>,
    err: String,
    code: Option<i32>,
}

impl Ended {
    fn types(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|line| line["type"].as_str().unwrap_or("?"))
            .collect()
    }

    fn results(&self) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["type"] == "result")
            .collect()
    }
}

impl Host {
    fn start(cmd: &mut Command) -> Self {
        let mut child = cmd.stdin(Stdio::piped()).spawn().expect("the binary runs");
        let stdin = child.stdin.take().expect("a pipe");
        let stdout = BufReader::new(child.stdout.take().expect("a pipe"));
        Self {
            child,
            stdin,
            stdout,
            lines: Vec::new(),
        }
    }

    fn send(&mut self, line: &Value) {
        writeln!(self.stdin, "{line}").expect("the run reads stdin");
        self.stdin.flush().expect("the run reads stdin");
    }

    fn prompt(&mut self, text: &str) {
        self.send(&json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "parent_tool_use_id": Value::Null,
        }));
    }

    /// The next line; `None` at the end of stdout. Every line is JSON or the
    /// test fails: that is the contract this mode keeps.
    fn line(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).expect("utf-8 stdout") == 0 {
            return None;
        }
        let value: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        self.lines.push(value.clone());
        Some(value)
    }

    /// Read until a line of this type; the run is certainly past it by then.
    fn until(&mut self, kind: &str) -> Value {
        loop {
            let line = self
                .line()
                .unwrap_or_else(|| panic!("stdout ended before a {kind} line"));
            if line["type"] == kind {
                return line;
            }
        }
    }

    fn answer(&mut self, request: &Value, verdict: Value) {
        self.send(&json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request["request_id"],
                "response": verdict,
            },
        }));
    }

    fn interrupt(&mut self) {
        self.send(&json!({
            "type": "control_request",
            "request_id": "req_host_1",
            "request": { "subtype": "interrupt" },
        }));
    }

    /// Close stdin and collect what the run had left to say.
    fn finish(self) -> Ended {
        let Host {
            mut child,
            stdin,
            mut stdout,
            mut lines,
        } = self;
        drop(stdin);
        let mut line = String::new();
        while stdout.read_line(&mut line).expect("utf-8 stdout") > 0 {
            lines.push(serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}")));
            line.clear();
        }
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            stderr.read_to_string(&mut err).expect("utf-8 stderr");
        }
        let code = child.wait().expect("the binary exits").code();
        Ended { lines, err, code }
    }
}

fn hosted(dir: &std::path::Path, script: &tempfile::NamedTempFile, extra: &[&str]) -> Command {
    let mut cmd = bingo();
    cmd.env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir)
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--cwd",
        ])
        .arg(dir)
        .args(extra);
    cmd
}

#[test]
fn two_user_lines_are_two_turns_with_one_result_each() {
    let dir = tempfile::tempdir().unwrap();
    let script =
        script(r#"{"responses":[{"steps":[{"text":"First."}]},{"steps":[{"text":"Second."}]}]}"#);
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.prompt("one");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(
        ended.types(),
        ["system", "assistant", "result", "assistant", "result"]
    );
    let results = ended.results();
    assert_eq!(results[0]["result"], "First.");
    assert_eq!(results[1]["result"], "Second.");
    assert!(results.iter().all(|line| line["is_error"] == false));
    let session = &ended.lines[0]["session_id"];
    assert!(
        ended
            .lines
            .iter()
            .all(|line| &line["session_id"] == session)
    );
}

#[test]
fn stdin_closing_with_nothing_to_do_is_a_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let ended = Host::start(&mut hosted(dir.path(), &script, &[])).finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(ended.types(), ["system"], "only the preamble");
}

/// The turn sleeps for thirty seconds; the interrupt ends it at once.
#[test]
fn an_interrupt_ends_the_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[{"steps":[{"text":"working"},{"delay":{"ms":30000}},{"text":"late"}]}]}"#,
    );
    let started = std::time::Instant::now();
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.prompt("take your time");
    // The first message is out, so the turn is certainly running.
    host.until("assistant");
    host.interrupt();
    let ended = host.finish();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the interrupt did not end the turn"
    );
    assert_eq!(ended.code, Some(130), "stderr: {}", ended.err);
    let acknowledged = ended.lines.iter().any(|line| {
        line["type"] == "control_response" && line["response"]["request_id"] == "req_host_1"
    });
    assert!(acknowledged, "{:?}", ended.types());
    let result = ended.results()[0];
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["is_error"], true);
    assert_eq!(result.get("result"), None, "no result text on an error arm");
}

/// A call the default policy stops, so the host is asked about it.
fn write_script() -> tempfile::NamedTempFile {
    script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"new.txt","content":"x"}}}]},
            {"steps":[{"text":"Done."}]}
        ]}"#,
    )
}

#[test]
fn a_permission_is_asked_of_the_host_and_an_allow_runs_the_tool() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script();
    let mut host = Host::start(&mut hosted(
        dir.path(),
        &script,
        &["--permission-prompt-tool", "stdio"],
    ));
    host.prompt("write it");
    let request = host.until("control_request");
    assert_eq!(request["request"]["subtype"], "can_use_tool");
    assert_eq!(request["request"]["tool_name"], "Write");
    assert_eq!(request["request"]["input"]["file_path"], "new.txt");
    assert!(
        request["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    host.answer(&request, json!({ "behavior": "allow" }));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "x",
        "the allowed call did not run"
    );
    assert_eq!(ended.results()[0]["result"], "Done.");
}

#[test]
fn a_denied_permission_stops_the_tool_and_the_turn_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script();
    let mut host = Host::start(&mut hosted(
        dir.path(),
        &script,
        &["--permission-prompt-tool", "stdio"],
    ));
    host.prompt("write it");
    let request = host.until("control_request");
    host.answer(
        &request,
        json!({ "behavior": "deny", "message": "not that file" }),
    );
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert!(!dir.path().join("new.txt").exists(), "the denied call ran");
    let denial = ended
        .lines
        .iter()
        .find(|line| line["type"] == "user")
        .expect("the tool result reaches the host");
    let block = &denial["message"]["content"][0];
    assert_eq!(block["is_error"], true);
    assert!(
        block["content"].as_str().unwrap().contains("not that file"),
        "{block}"
    );
    assert_eq!(ended.results()[0]["result"], "Done.");
}

/// Without the flag there is nobody to ask: the prompt is refused, as it is
/// for any run with no terminal.
#[test]
fn without_the_prompt_tool_a_permission_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script();
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.prompt("write it");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert!(!dir.path().join("new.txt").exists());
    assert!(
        !ended.types().contains(&"control_request"),
        "{:?}",
        ended.types()
    );
}

#[test]
fn a_junk_line_is_a_diagnostic_and_the_next_prompt_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"Fine."}]}]}"#);
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    writeln!(host.stdin, "{{not json").unwrap();
    host.prompt("one");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert!(
        ended.err.contains("[notice] INPUT_LINE_IGNORED"),
        "{}",
        ended.err
    );
    assert_eq!(ended.results()[0]["result"], "Fine.");
}

#[test]
fn the_host_protocol_needs_the_headless_surface_and_a_way_to_answer() {
    let out = run(bingo().args(["--input-format", "stream-json", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("[error] code=INVALID_INPUT msg="),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("--print"), "{}", stderr(&out));

    let out = run(bingo().args(["--print", "--permission-prompt-tool", "stdio", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("--input-format stream-json"),
        "{}",
        stderr(&out)
    );
}

/// Claude Code keeps reading after a turn fails, and so does this: the failure
/// is one `result` line and the exit code at the end of stdin.
#[test]
fn a_failed_turn_does_not_end_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"error":{"kind":"auth","message":"bad key"}}]},
            {"steps":[{"text":"Recovered."}]}
        ]}"#,
    );
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.prompt("one");
    host.until("result");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(1), "stderr: {}", ended.err);
    let results = ended.results();
    assert_eq!(results.len(), 2, "{:?}", ended.types());
    assert_eq!(results[0]["is_error"], true);
    assert_eq!(results[1]["result"], "Recovered.");
    assert!(
        ended.err.contains("[error] code=AUTH_REQUIRED"),
        "{}",
        ended.err
    );
}
