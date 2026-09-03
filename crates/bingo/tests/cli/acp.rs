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
    /// servers.
    fn configured(agent: &Path, script: Value, row: Value, beside: Value) -> Self {
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
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.methods().iter().any(|m| m == method) {
            assert!(
                Instant::now() < deadline,
                "the agent never heard {method}; it heard {:?}",
                self.methods()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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

fn bodies(out: &Output) -> Vec<ItemBody> {
    frames_of(out)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::ItemCompleted { item } => Some(item.body),
            _ => None,
        })
        .collect()
}

fn said(out: &Output) -> Vec<String> {
    bodies(out)
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::Assistant { text } => Some(text),
            _ => None,
        })
        .collect()
}

fn notices(out: &Output) -> Vec<(String, String)> {
    bodies(out)
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::Notice { code, text, .. } => Some((code, text)),
            _ => None,
        })
        .collect()
}

fn extensions(out: &Output) -> Vec<(String, String)> {
    frames_of(out)
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
    assert_eq!(said(&out), ["Hello there."]);

    let reasoning: Vec<(String, bool)> = bodies(&out)
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
        !bodies(&out)
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
        extensions(&out),
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
    assert!(notices(&first).is_empty(), "a first session is no fall");

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
        notices(&resumed).is_empty(),
        "a resume that worked says nothing: {:?}",
        notices(&resumed)
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
        notices(&loaded)
            .iter()
            .any(|(code, _)| code == "ACP_RESTORE"),
        "a fall is said: {:?}",
        notices(&loaded)
    );
    assert!(
        !said(&loaded).iter().any(|text| text.contains("replayed")),
        "the replay is swallowed, not journaled twice: {:?}",
        said(&loaded)
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

/// ADR-0035 §5: permissions are the adapter's own. A question that arrives
/// anyway is refused with the agent's own reject option, one notice names the
/// row where the answer belongs, and the turn goes on.
#[test]
fn a_permission_question_is_refused_and_one_notice_names_the_row() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-6",
            "capabilities": { "resume": true },
            "turns": [{
                "permission": {
                    "toolCall": { "toolCallId": "c1", "title": "Edit src/lib.rs", "kind": "edit" },
                    "options": [
                        { "optionId": "allow-once", "name": "Yes", "kind": "allow_once" },
                        { "optionId": "reject", "name": "No", "kind": "reject_once" }
                    ]
                },
                "updates": [chunk("Left it alone.")],
                "stopReason": "end_turn"
            }]
        }),
    );
    let out = adapter.turn("edit it");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(said(&out), ["Left it alone."], "the turn went on");

    let answered = adapter
        .first("permission/answered")
        .expect("the agent got an answer");
    assert_eq!(answered["outcome"]["outcome"], "selected");
    assert_eq!(answered["outcome"]["optionId"], "reject");

    let all = notices(&out);
    let asked: Vec<&String> = all
        .iter()
        .filter(|(code, _)| code == "ACP_ASKED")
        .map(|(_, text)| text)
        .collect();
    assert_eq!(asked.len(), 1, "said once: {all:?}");
    assert!(
        asked[0].contains("acp.adapters.scripted"),
        "the notice names the row: {}",
        asked[0]
    );
}

/// ADR-0035 §3: an adapter that died between turns is replaced, not asked. The
/// replacement climbs back into the same agent session from the journal's own
/// pointer, and the person is told a child went.
#[test]
fn an_adapter_that_died_between_turns_is_replaced_and_said() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-7",
            "capabilities": { "resume": true },
            "turns": [
                { "updates": [chunk("First.")], "stopReason": "end_turn", "thenExit": true },
                one_turn(vec![chunk("Second.")])
            ]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("one");
    host.until("result");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    // The dead child's script starts again from its first turn, so the second
    // bingo turn is answered "First." by a new agent — which is the point:
    // it was answered at all.
    assert_eq!(ended.results().len(), 2, "{:?}", ended.types());
    assert_eq!(
        adapter.methods(),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "initialize",
            "session/resume",
            "session/prompt"
        ],
        "a second handshake, and back into the same agent session"
    );
}

// ------------------------------------------------- the tool bridge (ADR-0036)

/// What the agent was offered on the bridge, from the `tools/list` it logged.
fn offered(adapter: &Scripted) -> Vec<String> {
    let listed = adapter
        .first("mcp/tools")
        .expect("the agent listed the bridge");
    listed["tools"]
        .as_array()
        .expect("a list of tools")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A script whose agent dials the bridge and calls one tool mid-turn.
fn bridged(calls: Vec<Value>, updates: Vec<Value>) -> Value {
    json!({
        "sessionId": "acp-bridge",
        "capabilities": { "resume": true },
        "mcp": true,
        "turns": [{ "mcp": calls, "updates": updates, "stopReason": "end_turn" }]
    })
}

/// The offer is the session's own tool set, less the hands the agent brought
/// (ADR-0036 §1). Nothing in `bingo-provider-acp` names the tools that cross:
/// what is asserted here is that the house's tools arrived and the machine's
/// did not.
#[test]
fn the_bridge_offers_the_sessions_tools_and_not_the_agents_own_hands() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(agent, bridged(Vec::new(), vec![chunk("Listed.")]));
    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let offered = offered(&adapter);
    assert!(
        offered.iter().any(|name| name == "SendMessage"),
        "the house's own tools cross: {offered:?}"
    );
    for brought in [
        "Read",
        "Write",
        "Edit",
        "Bash",
        "WebFetch",
        "SpawnAgent",
        "AskUserQuestion",
    ] {
        assert!(
            !offered.iter().any(|name| name == brought),
            "{brought} is the agent's own hand and does not cross: {offered:?}"
        );
    }
}

/// What the agent got back from one bridged call.
fn answer(adapter: &Scripted) -> Value {
    adapter
        .first("mcp/called")
        .expect("the agent called the bridge")["answer"]
        .clone()
}

fn answered_text(answered: &Value) -> String {
    answered["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// Every frame of every session this run journaled, wherever it lives. A
/// bridged call is journaled under the ACP member's own turn, and that member
/// is a sub-session with a journal of its own.
fn journaled(home: &Path) -> Vec<Frame> {
    let mut frames = Vec::new();
    collect(&home.join(".bingo/data/sessions"), &mut frames);
    frames
}

fn collect(dir: &Path, into: &mut Vec<Frame>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            into.extend(
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok()),
            );
        }
    }
}

/// ADR-0036 §2: a bridged call is the turn's call. An ACP member spawned by a
/// root posts to that root mid-turn; the post reaches the root's journal, and
/// the tool item sits under the member's own turn wearing the mark that says
/// no model asked for it.
#[test]
fn a_bridged_call_posts_to_the_parent_and_is_journaled_under_the_turn() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        bridged(
            vec![json!({
                "tool": "SendMessage",
                "arguments": { "to": "parent", "text": "the member spoke" }
            })],
            vec![chunk("Posted.")],
        ),
    );
    let root = adapter.home.path().join("root.json");
    std::fs::write(
        &root,
        json!({ "responses": [
            { "steps": [{ "toolCall": { "name": "SpawnAgent", "input": {
                "prompt": "say something upward", "name": "member",
                "provider": "scripted", "model": "agent", "background": false
            }}}]},
            { "steps": [{ "text": "root done" }]}
        ]})
        .to_string(),
    )
    .unwrap();

    let out = run_within(
        adapter
            .bingo(&[])
            .env("BINGO_FAKE_SCRIPT", &root)
            .arg("spawn the member"),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let answered = answer(&adapter);
    assert_eq!(
        answered["isError"],
        json!(false),
        "the call ran: {answered}"
    );
    assert!(
        answered_text(&answered).contains("parent"),
        "and the tool's own receipt came back: {answered}"
    );

    let posts: Vec<String> = bodies(&out)
        .into_iter()
        .filter_map(|body| match body {
            ItemBody::User { parts, .. } => {
                Some(parts.iter().filter_map(ContentPart::as_text).collect())
            }
            _ => None,
        })
        .collect();
    assert!(
        posts.iter().any(|text: &String| text == "the member spoke"),
        "the post is in the root's own journal: {posts:?}"
    );

    let calls: Vec<(String, bool)> = journaled(adapter.home.path())
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall { name, .. } if name == "SendMessage" => {
                    Some((name.clone(), item.external()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [("SendMessage".to_string(), true)],
        "one tool item, under the member's turn, marked as none of the model's"
    );
}

/// What the agent got back from the last bridged call it logged.
fn last_answer(adapter: &Scripted) -> Value {
    adapter
        .heard()
        .into_iter()
        .filter(|line| line["method"] == "mcp/called")
        .next_back()
        .expect("the agent called the bridge")["params"]["answer"]
        .clone()
}

/// ADR-0036 §2, fail closed: a bridged call is the turn's call, so one made
/// when this session is running no turn is refused with a reason. The agent
/// reads it as an MCP error result and goes on — the next turn is served by
/// the same child.
#[test]
fn a_call_with_no_turn_in_flight_is_refused_with_a_reason() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-late",
            "capabilities": { "resume": true },
            "mcp": true,
            "turns": [
                {
                    "updates": [chunk("Done.")],
                    "stopReason": "end_turn",
                    "mcpAfter": [{ "tool": "ListAgents", "arguments": {} }]
                },
                { "updates": [chunk("Second.")], "stopReason": "end_turn" }
            ]
        }),
    );
    let mut host = stream_json::Host::start(&mut adapter.hosted());
    host.prompt("one");
    host.until("result");
    adapter.wait_for("mcp/called");
    host.prompt("two");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let answered = last_answer(&adapter);
    assert_eq!(answered["isError"], json!(true), "{answered}");
    assert!(
        answered_text(&answered).contains("no turn is in flight"),
        "the kernel's own reason reaches the agent: {answered}"
    );
    let answers = ended.results();
    assert_eq!(answers.len(), 2, "{:?}", ended.types());
    assert_eq!(
        answers[1]["result"], "Second.",
        "and the refusal cost the child nothing"
    );
}

/// A row that names its own tools is offered those and nothing else, exclusion
/// included: on their own machine the person's word is the last one
/// (ADR-0036 §6).
#[test]
fn a_row_that_names_its_tools_is_offered_only_those() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        bridged(Vec::new(), vec![chunk("Listed.")]),
        json!({ "tools": ["SendMessage", "Read"] }),
        json!({}),
    );
    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let mut offered = offered(&adapter);
    offered.sort();
    assert_eq!(
        offered,
        ["Read", "SendMessage"],
        "the derivation stands aside, `Read` and all"
    );
}

/// A tool has no say in what an interrupt does, and neither does the wire it
/// arrived on: one `esc` ends the turn and every call in flight is dropped
/// where it stands. The row names `Bash` so there is something slow to be
/// interrupted in the middle of — an explicit list is the one way the
/// machine's own hands cross (ADR-0036 §6) — and what the agent gets back is
/// an answer saying so rather than a broken pipe.
#[test]
fn an_interrupt_drops_a_bridged_call_and_the_child_serves_the_next_turn() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-esc",
            "capabilities": { "resume": true },
            "mcp": true,
            "turns": [
                {
                    "mcp": [{ "tool": "Bash", "arguments": { "command": "sleep 30" } }],
                    "updates": [chunk("Back.")],
                    "stopReason": "end_turn"
                },
                { "updates": [chunk("Second.")], "stopReason": "end_turn" }
            ]
        }),
        json!({ "tools": ["Bash"] }),
        json!({}),
    );
    let mut host =
        stream_json::Host::start(&mut adapter.hosted_with(&["--dangerously-skip-permissions"]));
    host.prompt("go");
    adapter.wait_for("mcp/calling");
    host.interrupt();
    adapter.wait_for("mcp/called");
    host.prompt("again");
    // A run that was interrupted exits as one; what matters here is what it
    // said, and that it said it without failing.
    let ended = host.finish();
    assert!(!ended.err.contains("[error]"), "stderr: {}", ended.err);

    let answered = last_answer(&adapter);
    assert_eq!(
        answered["isError"],
        json!(true),
        "the dropped call is an answer the agent can read: {answered}"
    );
    assert!(
        answered_text(&answered).contains("interrupted"),
        "and it says what happened: {answered}"
    );

    let answers = ended.results();
    assert_eq!(answers.len(), 2, "{:?}", ended.types());
    assert_eq!(
        answers[0]["is_error"], true,
        "the interrupted turn ended as one"
    );
    assert_eq!(
        answers[1]["result"], "Second.",
        "and the child lived to serve the next"
    );
}

/// The example plugin this repository ships, which is what a person would
/// copy. It registers one tool, in a language that is not Rust; the kernel
/// files it as `plugin__wordcount__count`.
fn wordcount() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/wordcount")
}

fn python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The offer is derived, never listed: a tool nobody in `bingo-provider-acp`
/// has heard of — registered at boot by a plugin process, in Python — reaches
/// the bridge with no edit in that crate (ADR-0036 §1).
#[test]
fn a_tool_a_plugin_registered_reaches_the_bridge_with_no_edit_here() {
    let Some(agent) = fake_agent() else { return };
    if !python3() {
        eprintln!("the plugin's tool is skipped: no python3 here");
        return;
    }
    let adapter = Scripted::new(
        agent,
        json!({
            "sessionId": "acp-plugin",
            "capabilities": { "resume": true },
            "mcp": true,
            "turns": [{
                "mcpUntil": "plugin__wordcount__count",
                "updates": [chunk("Listed.")],
                "stopReason": "end_turn"
            }]
        }),
    );
    let installed = adapter.home.path().join(".bingo/plugins/wordcount");
    std::fs::create_dir_all(&installed).unwrap();
    for file in ["plugin.json", "main.py"] {
        std::fs::copy(wordcount().join(file), installed.join(file)).unwrap();
    }

    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let offered = offered(&adapter);
    assert!(
        offered
            .iter()
            .any(|name| name == "plugin__wordcount__count"),
        "a plugin's own tool is on the bridge: {offered:?}"
    );
}

/// The scripted MCP server `bingo-mcp` ships as an example, built beside this
/// test binary. A run of `cargo test -p bingo` alone does not build it.
fn echo_server() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_BIN_EXE_bingo"))
        .parent()?
        .join("examples")
        .join(format!("echo_server{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        eprintln!(
            "the forwarding scenarios are skipped: {} is not built. Run the \
             suite as `cargo test --workspace`.",
            path.display()
        );
        return None;
    }
    Some(path)
}

/// The rows a `session/new` carried.
fn injected(adapter: &Scripted) -> Vec<Value> {
    adapter.first("session/new").expect("a session was opened")["mcpServers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// ADR-0036 §4, the default: a person's own MCP rows ride `session/new`
/// verbatim, so the agent dials them itself — one hop, its own env — and the
/// tools those servers serve leave the bridge, because nothing is served
/// twice. The absence here is given its meaning by the scenario below, where
/// the same server under `forwardMcp: false` does reach the offer.
#[test]
fn a_persons_own_servers_are_forwarded_verbatim_and_leave_the_offer() {
    let Some(agent) = fake_agent() else { return };
    let Some(echo) = echo_server() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-forward",
            "capabilities": { "resume": true },
            "mcp": true,
            "turns": [{ "mcpList": true, "updates": [chunk("Listed.")], "stopReason": "end_turn" }]
        }),
        json!({}),
        json!({ "mcpServers": { "echo": {
            "command": echo, "args": ["--quiet"], "env": { "ECHO_TOKEN": "s3cret" }
        }}}),
    );
    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let rows = injected(&adapter);
    assert_eq!(rows.len(), 2, "ours and theirs: {rows:?}");
    assert_eq!(rows[0]["name"], "bingo");
    assert_eq!(
        rows[1],
        json!({
            "name": "echo",
            "command": echo,
            "args": ["--quiet"],
            "env": [{ "name": "ECHO_TOKEN", "value": "s3cret" }]
        }),
        "their row crosses as they wrote it, credentials and all"
    );

    let offered = offered(&adapter);
    assert!(
        !offered.iter().any(|name| name.starts_with("mcp__echo__")),
        "a server the agent dials itself is not served twice: {offered:?}"
    );
}

/// `forwardMcp: false` keeps a person's rows — and the credentials in them —
/// home. Nothing of theirs crosses, and the tools those servers serve ride the
/// bridge instead, gated and untrusted as ever (ADR-0009 §2, ADR-0036 §4).
#[test]
fn a_row_that_keeps_its_servers_home_serves_their_tools_on_the_bridge() {
    let Some(agent) = fake_agent() else { return };
    let Some(echo) = echo_server() else { return };
    let adapter = Scripted::configured(
        agent,
        json!({
            "sessionId": "acp-home",
            "capabilities": { "resume": true },
            "mcp": true,
            "turns": [{
                "mcpUntil": "mcp__echo__echo",
                "updates": [chunk("Listed.")],
                "stopReason": "end_turn"
            }]
        }),
        json!({ "forwardMcp": false }),
        json!({ "mcpServers": { "echo": { "command": echo } } }),
    );
    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let rows = injected(&adapter);
    assert_eq!(rows.len(), 1, "only ours crossed: {rows:?}");
    assert_eq!(rows[0]["name"], "bingo");
    assert!(
        !rows[0].to_string().contains("ECHO_TOKEN"),
        "nothing of theirs went with it: {rows:?}"
    );

    let offered = offered(&adapter);
    assert!(
        offered.iter().any(|name| name == "mcp__echo__echo"),
        "the server's tools ride the bridge instead: {offered:?}"
    );
}
