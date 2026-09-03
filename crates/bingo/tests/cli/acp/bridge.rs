//! Black-box: bingo's shared tools served to an ACP agent as MCP
//! (ADR-0036), driven through the real binary, the real proxy and the
//! scripted agent's own MCP client.
//!
//! Nothing here knows anything about the bridge's insides: a settings row,
//! a prompt, the frames that came out, and the log the agent wrote of what
//! it was offered and what it got back.

use super::*;
/// What the agent was offered on the bridge, from the `tools/list` it logged.
fn offered(adapter: &Scripted) -> Vec<String> {
    // The last list, not the first: a scenario that waited for a tool to
    // arrive is asking about the list it waited for.
    let listed = adapter
        .heard()
        .into_iter()
        .rfind(|line| line["method"] == "mcp/tools")
        .expect("the agent listed the bridge")["params"]
        .clone();
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

    let posts: Vec<String> = bodies(frames_of(&out))
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
        .rfind(|line| line["method"] == "mcp/called")
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

/// A row the agent could not take does not cross, and is not dropped in
/// silence: ACP only allows an http server row to an agent whose handshake
/// claimed http, and the scripted one claims none. The person is told which
/// row stayed behind (ADR-0036 §4).
#[test]
fn a_row_this_agent_cannot_take_is_skipped_and_named() {
    let Some(agent) = fake_agent() else { return };
    let adapter = Scripted::configured(
        agent,
        bridged(Vec::new(), vec![chunk("Listed.")]),
        json!({}),
        // Nothing listens there; forwarding is decided by the row and the
        // handshake, never by whether the server answers.
        json!({ "mcpServers": { "remote": {
            "type": "http", "url": "http://127.0.0.1:9/mcp"
        }}}),
    );
    let out = adapter.turn("what have you got");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let rows = injected(&adapter);
    assert_eq!(rows.len(), 1, "only ours crossed: {rows:?}");
    assert_eq!(rows[0]["name"], "bingo");

    let all = notices(frames_of(&out));
    let said = coded(&all, "ACP_MCP");
    assert_eq!(said.len(), 1, "said once: {all:?}");
    assert!(
        said[0].contains("remote") && said[0].contains("http"),
        "the notice names the row and why: {}",
        said[0]
    );
}
