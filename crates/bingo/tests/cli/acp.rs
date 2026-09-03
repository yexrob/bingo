//! Black-box: an ACP adapter answering as a model (ADR-0035), driven through
//! the real binary against the scripted agent the plugin ships.
//!
//! Nothing here knows anything about the plugin's insides: a settings row, a
//! prompt, the frames that came out, and the log the agent wrote of every
//! message it was actually sent.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use bingo_sdk::{ContentPart, ItemBody};
use serde_json::{Value, json};

use super::*;

/// What becomes of a question the agent asks — allowed, refused, or put to a
/// person (ADR-0039) — is its own story and reads on its own terms.
mod asking;

/// The tool bridge is its own scenario file: what ADR-0036 added is a
/// conversation the other way, and it reads on its own terms.
mod bridge;

/// And so is the catalogue answering before anybody has said a word to the
/// agent: what a cold `/models refresh` finds is its own story (M44).
mod catalogue;

/// So is the pair of knobs: `/think` and `/model` reaching the agent is its
/// own story (ADR-0037), and it reads on its own terms.
mod knobs;

/// And so is a child dying: three ways of meeting one rule, which read
/// together or not at all.
mod life;

/// The scripted agent is a binary of another crate, built beside this one.
/// `cargo test --workspace` and CI build it; a bare `cargo test -p bingo` does
/// not, and these tests say so once rather than failing on a file nobody in
/// that invocation asked for.
fn fake_agent() -> Option<&'static Path> {
    static AGENT: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        let path = Path::new(env!("CARGO_BIN_EXE_bingo")).with_file_name(format!(
            "bingo-fake-acp-agent{}",
            std::env::consts::EXE_SUFFIX
        ));
        if !path.exists() {
            eprintln!(
                "the ACP black-box is skipped: {} is not built. Run the suite \
                 as `cargo test --workspace`.",
                path.display()
            );
            return None;
        }
        Some(path)
    });
    AGENT.as_deref()
}

/// One configured adapter: a home to run in, the script the agent obeys, and
/// the log it appends every message it received to.
struct Scripted {
    home: tempfile::TempDir,
    script: PathBuf,
    log: PathBuf,
    settings: PathBuf,
}

impl Scripted {
    fn new(agent: &Path, script: Value) -> Self {
        Self::configured(agent, script, json!({}), json!({}))
    }

    /// One adapter, with whatever else the scenario needs written onto its row
    /// and beside it — a `tools` list, a `forwardMcp`, a person's own MCP
    /// servers. On a machine that already knows what this adapter serves: see
    /// [`Scripted::asked_before`].
    fn configured(agent: &Path, script: Value, row: Value, beside: Value) -> Self {
        let scripted = Self::written(agent, script, row, beside);
        scripted.asked_before();
        scripted
    }

    /// The same adapter on a machine that has never asked it anything — which
    /// is what the catalogue's own scenarios are about (`super::catalogue`).
    fn cold(agent: &Path, script: Value, row: Value) -> Self {
        Self::written(agent, script, row, json!({}))
    }

    fn written(agent: &Path, script: Value, row: Value, beside: Value) -> Self {
        let home = tempfile::tempdir().unwrap();
        let scripted = Scripted {
            script: home.path().join("acp-script.json"),
            log: home.path().join("acp-log.jsonl"),
            settings: home.path().join("settings.json"),
            home,
        };
        scripted.obeys(script);
        let mut adapter = json!({
            "command": agent,
            "env": {
                "BINGO_FAKE_ACP_SCRIPT": scripted.script,
                "BINGO_FAKE_ACP_LOG": scripted.log,
            }
        });
        merge(&mut adapter, row);
        let mut settings = json!({ "acp": { "adapters": { "scripted": adapter } } });
        merge(&mut settings, beside);
        std::fs::write(&scripted.settings, settings.to_string()).unwrap();
        scripted
    }

    /// What the agent does from the next spawn on. Rewriting this between runs
    /// is how one conversation meets an adapter that has changed its mind
    /// about what it can restore.
    fn obeys(&self, script: Value) {
        std::fs::write(&self.script, script.to_string()).unwrap();
    }

    /// A home that was already told what this adapter serves.
    ///
    /// A machine with no cached list has every provider asked the moment the
    /// host is built (ADR-0026 §4), and an ACP instance answers that by
    /// opening a session of its own (`bingo_provider_acp::probe`) — a second
    /// child beside the one the scenario is about, arriving at a moment
    /// nothing can pin down. Every scenario here but the catalogue's own is
    /// about what *one* conversation is told, so the cache is left looking
    /// freshly asked and the top-up finds nothing stale to ask.
    fn asked_before(&self) {
        let file = self.home.path().join(".bingo/data/served-models.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let served = json!({
            "scripted": {
                "fetched": jiff::Timestamp::now().to_string(),
                "models": [{ "id": "agent" }]
            }
        });
        std::fs::write(&file, served.to_string()).unwrap();
    }

    /// Forget what the agent heard, so the next run's log is only its own.
    fn forget(&self) {
        let _ = std::fs::remove_file(&self.log);
    }

    fn cwd(&self) -> &Path {
        self.home.path()
    }

    /// Every message the agent received, in order.
    fn heard(&self) -> Vec<Value> {
        let Ok(body) = std::fs::read_to_string(&self.log) else {
            return Vec::new();
        };
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.heard()
            .into_iter()
            .map(|line| line["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn first(&self, method: &str) -> Option<Value> {
        self.heard()
            .into_iter()
            .find(|line| line["method"] == method)
            .map(|line| line["params"].clone())
    }

    /// Wait until the agent has recorded `method`, or fail the scenario rather
    /// than hang the suite.
    fn wait_for(&self, method: &str) {
        wait_until(
            || self.methods().iter().any(|m| m == method),
            || {
                format!(
                    "the agent never heard {method}; it heard {:?}",
                    self.methods()
                )
            },
        );
    }

    /// The binary against this adapter's home, in this adapter's directory.
    fn base(&self) -> Command {
        let mut cmd = bingo();
        cmd.env("HOME", self.home.path())
            .arg("--settings")
            .arg(&self.settings)
            .args(["--print", "--cwd"])
            .arg(self.cwd());
        cmd
    }

    /// One run, streaming frames: the whole event stream, as any surface sees
    /// it.
    fn bingo(&self, extra: &[&str]) -> Command {
        let mut cmd = self.base();
        cmd.args(["--output-format", "json"]).args(extra);
        cmd
    }

    /// One run a host drives over stdin, speaking the Claude Code envelope —
    /// the only way to put two turns, or an interrupt, into one process.
    fn hosted(&self) -> Command {
        self.hosted_with(&[])
    }

    /// The same, answering in frames rather than the envelope: a scenario that
    /// has to read what a `/command` inside the run answered reads the stream
    /// every surface reads, not the host's summary of it.
    fn driven(&self, model: &str) -> Command {
        let mut cmd = self.base();
        cmd.args([
            "--input-format",
            "stream-json",
            "--output-format",
            "json",
            "--provider",
            "scripted",
            "--model",
            model,
        ]);
        cmd
    }

    /// The same, for a scenario whose agent calls a tool the gate would stop
    /// to ask about. Nobody is there to answer one in this mode, so a run that
    /// means to watch a call *run* has to say so.
    fn hosted_with(&self, extra: &[&str]) -> Command {
        let mut cmd = self.base();
        cmd.args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--provider",
            "scripted",
            "--model",
            "agent",
        ])
        .args(extra);
        cmd
    }

    /// A whole turn, start to finish, on a session of its own.
    fn turn(&self, said: &str) -> Output {
        run(self
            .bingo(&["--provider", "scripted", "--model", "agent"])
            .arg(said))
    }

    /// A turn that carries on the last conversation in this directory.
    fn again(&self, said: &str) -> Output {
        run(self.bingo(&["--continue"]).arg(said))
    }
}

/// Wait for something to become true, or fail the scenario rather than hang
/// the suite. Two processes and a background dial make most of the facts here
/// arrive when they arrive; what a scenario must never do is guess how long
/// that takes. The message is asked for only once the wait has run out, so it
/// can afford to go and look at what the world was doing instead.
fn wait_until(ready: impl Fn() -> bool, gave_up: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "{}", gave_up());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One object's keys written over another's, so a scenario says only what it
/// changes.
fn merge(into: &mut Value, from: Value) {
    let (Some(into), Some(from)) = (into.as_object_mut(), from.as_object()) else {
        return;
    };
    for (key, value) in from {
        match into.get_mut(key) {
            Some(held) if held.is_object() && value.is_object() => merge(held, value.clone()),
            _ => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The frames a run wrote. A `--print` run in stream-json input mode also
/// writes control responses, which are not frames; they are not events either.
fn frames_of(out: &Output) -> Vec<Frame> {
    stdout(out)
        .lines()
        .filter(|line| line.contains("\"event\""))
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

/// The frames a host-driven run wrote. `--output-format json` is the same
/// stream, arriving a line at a time through the host's own pipe, so what a
/// run said before the test looked is read the one way.
fn frames(ended: &stream_json::Ended) -> Vec<Frame> {
    ended
        .lines
        .iter()
        .filter(|line| line.get("event").is_some())
        .map(|line| {
            serde_json::from_str(&line.to_string()).unwrap_or_else(|e| panic!("{e}: {line}"))
        })
        .collect()
}

fn bodies(frames: Vec<Frame>) -> Vec<ItemBody> {
    frames
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::ItemCompleted { item } => Some(item.body),
            _ => None,
        })
        .collect()
}

fn said(frames: Vec<Frame>) -> Vec<String> {
    bodies(frames)
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::Assistant { text } => Some(text),
            _ => None,
        })
        .collect()
}

fn notices(frames: Vec<Frame>) -> Vec<(String, String)> {
    bodies(frames)
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::Notice { code, text, .. } => Some((code, text)),
            _ => None,
        })
        .collect()
}

/// The notices of one code among everything that was said. A scenario about a
/// notice asks two things of it — that it was said, and that it was said once
/// — and keeps the whole list beside it, because what else was said is what
/// makes a failure readable.
fn coded<'a>(all: &'a [(String, String)], code: &str) -> Vec<&'a String> {
    all.iter()
        .filter(|(said, _)| said == code)
        .map(|(_, text)| text)
        .collect()
}

fn extensions(frames: Vec<Frame>) -> Vec<(String, String)> {
    frames
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::Extension { plugin, kind, .. } => Some((plugin, kind)),
            _ => None,
        })
        .collect()
}

fn chunk(text: &str) -> Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": text },
        "messageId": "m1"
    })
}

fn thought(text: &str) -> Value {
    json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": { "type": "text", "text": text }
    })
}

fn tool_call() -> Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "c1",
        "title": "Read src/lib.rs",
        "kind": "read",
        "status": "completed",
        "content": [{ "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }],
        "rawInput": { "file_path": "src/lib.rs" }
    })
}

fn one_turn(updates: Vec<Value>) -> Value {
    json!({ "updates": updates, "stopReason": "end_turn" })
}

/// The turn ADR-0035 §4 describes: what the agent said, what it thought, and
/// what it ran on its own machine — the last wearing the mark that says bingo
/// never touched it, and asking the loop for nothing.
#[test]
fn a_turn_through_an_adapter_streams_text_thought_and_the_agents_own_call() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-1",
            "capabilities": { "resume": true },
            "turns": [{
                "updates": [thought("weighing it"), chunk("Hello there."), tool_call()],
                "stopReason": "end_turn",
                "usage": { "totalTokens": 9, "inputTokens": 6, "outputTokens": 3 }
            }]
        }),
    );
    let out = adapter.turn("say hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(said(frames_of(&out)), ["Hello there."]);

    let reasoning: Vec<(String, bool)> = bodies(frames_of(&out))
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::Reasoning {
                text,
                provider_metadata,
            } => Some((
                text,
                provider_metadata
                    .get("acp")
                    .is_some_and(|acp| acp["external"] == Value::Bool(true)),
            )),
            _ => None,
        })
        .collect();
    assert!(
        reasoning
            .iter()
            .any(|(text, marked)| text == "weighing it" && !marked),
        "the agent's thinking is its own, and wears no mark: {reasoning:?}"
    );
    assert!(
        reasoning
            .iter()
            .any(|(text, marked)| text.contains("Read src/lib.rs") && *marked),
        "the agent's own call wears `acp.external`: {reasoning:?}"
    );
    assert!(
        !bodies(frames_of(&out))
            .iter()
            .any(|body| matches!(body, ItemBody::ToolCall { .. })),
        "and the loop was asked to run nothing"
    );
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new", "session/prompt"],
        "nothing else crossed the wire"
    );
}

/// ADR-0035 §3: one bingo session is one ACP session on one child. The second
/// turn is another `session/prompt` and nothing else, and the pointer to the
/// agent's own session is journaled once.
#[test]
fn two_turns_of_one_session_ride_one_child_and_one_agent_session() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-2",
            "capabilities": { "resume": true },
            "turns": [one_turn(vec![chunk("First.")]), one_turn(vec![chunk("Second.")])]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("one");
    // The first turn is over, so the second is a turn and not steering.
    host.until("result");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    let answers: Vec<&Value> = ended.results();
    assert_eq!(answers.len(), 2, "{:?}", ended.types());
    assert_eq!(answers[0]["result"], "First.");
    assert_eq!(answers[1]["result"], "Second.");

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/prompt"
        ],
        "one handshake, one agent session, two turns"
    );
}

/// The extension is the pointer to the agent's own session, written once and
/// never copied (ADR-0035 §3).
#[test]
fn the_agents_session_id_is_journaled_once_as_an_extension() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-3",
            "capabilities": { "resume": true },
            "turns": [one_turn(vec![chunk("First.")])]
        }),
    );
    let out = adapter.turn("hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        extensions(frames_of(&out)),
        [("bingo.acp".to_string(), "session:scripted".to_string())]
    );
    let written = frames_of(&out)
        .into_iter()
        .find_map(|frame| match frame.event {
            Event::Extension { payload, .. } => Some(payload),
            _ => None,
        })
        .expect("the pointer is on the stream");
    assert_eq!(written["sessionId"], "acp-3");
    assert_eq!(written["adapter"], "scripted");
}

/// ADR-0035 §6: an interrupt is one `session/cancel` and then the wait for the
/// agent to stop of its own accord. The child and the agent session outlive it
/// — the next turn is a prompt, not a second handshake.
#[test]
fn an_interrupt_cancels_the_turn_and_the_child_serves_the_next_one() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-4",
            "capabilities": { "resume": true },
            "turns": [
                { "updates": [chunk("working")], "awaitCancel": true },
                one_turn(vec![chunk("Second.")])
            ]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("go");
    adapter.wait_for("session/prompt");
    host.interrupt();
    adapter.wait_for("session/cancel");
    host.prompt("again");
    let ended = host.finish();

    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "session/cancel",
            "session/prompt"
        ],
        "the cancel is a notification, not a kill: stderr {}",
        ended.err
    );
    let answers = ended.results();
    assert_eq!(answers.len(), 2, "{:?}", ended.types());
    assert_eq!(
        answers[0]["is_error"], true,
        "the interrupted turn ended as one"
    );
    assert_eq!(
        answers[1]["result"], "Second.",
        "and the next turn rode the same child"
    );
}

/// The restore ladder, all three rungs, against one conversation whose adapter
/// changes what it can do between runs (ADR-0035 §3).
#[test]
fn the_restore_ladder_climbs_resume_then_load_then_a_file() {
    let Some(agent) = fake_agent() else { return };
    let answers = |turns: usize| -> Value {
        (0..turns.max(1))
            .map(|_| one_turn(vec![chunk("Answered.")]))
            .collect()
    };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-5",
            "capabilities": { "loadSession": true, "resume": true },
            "turns": answers(1)
        }),
    );
    let first = adapter.turn("the first question");
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert!(
        notices(frames_of(&first)).is_empty(),
        "a first session is no fall"
    );

    // The top rung: the agent kept the session and takes it back without
    // replaying, so nothing is said and nothing is opened.
    adapter.forget();
    let resumed = adapter.again("and again");
    assert_eq!(
        resumed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&resumed)
    );
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/resume", "session/prompt"]
    );
    assert!(
        notices(frames_of(&resumed)).is_empty(),
        "a resume that worked says nothing: {:?}",
        notices(frames_of(&resumed))
    );

    // One rung down: no resume, only a load, whose replay of turns the journal
    // already holds must reach nobody.
    adapter.obeys(json!({
        "sessionId": "acp-5",
        "capabilities": { "loadSession": true },
        "replay": [
            { "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "a replayed question" } },
            { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "a replayed answer" }, "messageId": "old" }
        ],
        "turns": answers(1)
    }));
    adapter.forget();
    let loaded = adapter.again("a third time");
    assert_eq!(loaded.status.code(), Some(0), "stderr: {}", stderr(&loaded));
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/load", "session/prompt"]
    );
    assert!(
        notices(frames_of(&loaded))
            .iter()
            .any(|(code, _)| code == "ACP_RESTORE"),
        "a fall is said: {:?}",
        notices(frames_of(&loaded))
    );
    assert!(
        !said(frames_of(&loaded))
            .iter()
            .any(|text| text.contains("replayed")),
        "the replay is swallowed, not journaled twice: {:?}",
        said(frames_of(&loaded))
    );

    // The bottom rung: the agent kept nothing, so it is handed the
    // conversation as a file and told to read it first.
    adapter.obeys(json!({
        "sessionId": "acp-5",
        "capabilities": {},
        "turns": answers(1)
    }));
    adapter.forget();
    let fresh = adapter.again("a fourth time");
    assert_eq!(fresh.status.code(), Some(0), "stderr: {}", stderr(&fresh));
    assert_eq!(
        adapter.methods(),
        ["initialize", "session/new", "session/prompt"],
        "neither door opened, so a new session did"
    );
    let prompt = adapter.first("session/prompt").expect("a prompt crossed");
    let text = prompt["prompt"][0]["text"].as_str().unwrap().to_string();
    let named = text
        .split_whitespace()
        .find(|word| word.ends_with(".md"))
        .expect("the first prompt names the file");
    let transcript = std::fs::read_to_string(named).expect("the file the agent was told to read");
    assert!(
        transcript.contains("the first question") && transcript.contains("a third time"),
        "the file holds the fold so far: {transcript}"
    );
    assert!(
        !transcript.contains("a replayed"),
        "and holds nothing the journal never had: {transcript}"
    );
}
