//! A scripted ACP agent, in Rust, so the contract tests need no node.
//!
//! `BINGO_FAKE_ACP_SCRIPT` names a JSON file; the same fake drives the loop,
//! the restore ladder and the black-box, because what it advertises and what
//! it streams are both the script's to say. `BINGO_FAKE_ACP_LOG` names a file
//! this appends one JSON line to per message received, which is how a test
//! asserts what the client actually sent — that a resume was tried before a
//! load, that a first prompt names a transcript file, that a cancel arrived.
//!
//! The script:
//!
//! ```json
//! {
//!   "sessionId": "acp-fake-1",
//!   "capabilities": { "loadSession": true, "resume": true, "image": false },
//!   "authRequired": false,
//!   "replay": [ { "sessionUpdate": "user_message_chunk", "content": {…} } ],
//!   "turns": [
//!     {
//!       "permission": { "toolCall": { "toolCallId": "c1", "title": "Edit" },
//!                       "options": [ { "optionId": "no", "name": "No",
//!                                      "kind": "reject_once" } ] },
//!       "elicitation": { "mode": "form", "sessionId": "…",
//!                        "requestedSchema": {…}, "message": "…" },
//!       "updates": [ { "sessionUpdate": "agent_message_chunk", "content": {…} } ],
//!       "stopReason": "end_turn",
//!       "usage": { "totalTokens": 3, "inputTokens": 2, "outputTokens": 1 },
//!       "awaitCancel": false,
//!       "thenExit": false
//!     }
//!   ]
//! }
//! ```

use std::path::PathBuf;

use bingo_provider_acp::method;
use bingo_provider_acp::wire::{self, Body, Envelope, Reply};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, Stdin, Stdout};

type Failed = Box<dyn std::error::Error>;

const SCRIPT: &str = "BINGO_FAKE_ACP_SCRIPT";
const LOG: &str = "BINGO_FAKE_ACP_LOG";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Script {
    #[serde(default = "default_session")]
    session_id: String,
    #[serde(default)]
    capabilities: Capabilities,
    /// `session/new` refuses, the way an adapter with no login does.
    #[serde(default)]
    auth_required: bool,
    /// What `session/load` replays before it answers.
    #[serde(default)]
    replay: Vec<Value>,
    #[serde(default)]
    turns: Vec<Turn>,
}

fn default_session() -> String {
    "acp-fake-1".to_string()
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Capabilities {
    #[serde(default)]
    load_session: bool,
    #[serde(default)]
    resume: bool,
    #[serde(default)]
    image: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Turn {
    #[serde(default)]
    permission: Option<Value>,
    /// The other door: an agent that asks the client to collect something from
    /// a person. Declined, like the permission (ADR-0035 §5).
    #[serde(default)]
    elicitation: Option<Value>,
    #[serde(default)]
    updates: Vec<Value>,
    #[serde(default = "end_turn")]
    stop_reason: String,
    #[serde(default)]
    usage: Option<Value>,
    /// The turn hangs until a `session/cancel` arrives, so an interrupt has
    /// something to interrupt.
    #[serde(default)]
    await_cancel: bool,
    /// The agent answers this turn and then goes, the way a crashed adapter
    /// does. The next turn must find a new child.
    #[serde(default)]
    then_exit: bool,
}

fn end_turn() -> String {
    "end_turn".to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Failed> {
    let path = std::env::var(SCRIPT).map_err(|_| format!("{SCRIPT} names no file"))?;
    let script: Script = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let log = std::env::var(LOG).ok().map(PathBuf::from);
    Agent {
        script,
        log,
        turn: 0,
        out: tokio::io::stdout(),
    }
    .serve(BufReader::new(tokio::io::stdin()).lines())
    .await
}

struct Agent {
    script: Script,
    log: Option<PathBuf>,
    turn: usize,
    out: Stdout,
}

impl Agent {
    /// One line at a time. A prompt keeps reading from the same lines, because
    /// its permission answer and its cancel arrive on this pipe too.
    async fn serve(&mut self, mut lines: Lines<BufReader<Stdin>>) -> Result<(), Failed> {
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(envelope) = serde_json::from_str::<Envelope>(&line) else {
                continue;
            };
            match envelope.into_inner() {
                Body::Request(asked) => {
                    let params = asked.params.clone().unwrap_or(Value::Null);
                    self.record(&asked.method, &params).await?;
                    self.answer(&asked, params, &mut lines).await?;
                }
                Body::Notification(note) => {
                    self.record(&note.method, &note.params.unwrap_or(Value::Null))
                        .await?;
                }
                Body::Reply(_) => {}
            }
        }
        Ok(())
    }

    async fn answer(
        &mut self,
        asked: &agent_client_protocol_schema::rpc::Request<Value>,
        params: Value,
        lines: &mut Lines<BufReader<Stdin>>,
    ) -> Result<(), Failed> {
        let id = asked.id.clone();
        let outcome = match asked.method.as_ref() {
            method::INITIALIZE => Ok(self.handshake()),
            method::SESSION_NEW => self.open(),
            method::SESSION_LOAD => self.load().await?,
            method::SESSION_RESUME => self.resume(),
            method::SESSION_PROMPT => return self.prompt(id, &params, lines).await,
            _ => Err(refusal(-32601, "method not found")),
        };
        let body = match outcome {
            Ok(result) => wire::result(id, result),
            Err(error) => wire::failed(id, serde_json::from_value(error)?),
        };
        self.send(body).await
    }

    fn handshake(&self) -> Value {
        let caps = &self.script.capabilities;
        let mut session = json!({});
        if caps.resume {
            session["resume"] = json!({});
        }
        json!({
            "protocolVersion": 1,
            "agentCapabilities": {
                "loadSession": caps.load_session,
                "promptCapabilities": { "image": caps.image },
                "sessionCapabilities": session
            },
            "agentInfo": { "name": "bingo-fake-acp-agent", "version": "1" }
        })
    }

    /// An adapter with no credential refuses here, in the protocol's own code.
    fn open(&self) -> Result<Value, Value> {
        if self.script.auth_required {
            return Err(refusal(-32000, "Authentication required"));
        }
        Ok(json!({ "sessionId": self.script.session_id }))
    }

    /// A load replays what it holds before it answers. The client is expected
    /// to swallow that replay: the journal already has those turns.
    async fn load(&mut self) -> Result<Result<Value, Value>, Failed> {
        if !self.script.capabilities.load_session {
            return Ok(Err(refusal(-32601, "session/load is not here")));
        }
        for update in self.script.replay.clone() {
            self.update(update).await?;
        }
        Ok(Ok(json!({})))
    }

    fn resume(&self) -> Result<Value, Value> {
        if !self.script.capabilities.resume {
            return Err(refusal(-32601, "session/resume is not here"));
        }
        Ok(json!({}))
    }

    async fn prompt(
        &mut self,
        id: agent_client_protocol_schema::rpc::RequestId,
        params: &Value,
        lines: &mut Lines<BufReader<Stdin>>,
    ) -> Result<(), Failed> {
        let Some(turn) = self.script.turns.get(self.turn).map(script_turn) else {
            let body = wire::result(id, json!({ "stopReason": "end_turn" }));
            return self.send(body).await;
        };
        self.turn += 1;
        let _ = params;
        if let Some(mut request) = turn.permission {
            request["sessionId"] = json!(self.script.session_id);
            self.ask(
                method::SESSION_REQUEST_PERMISSION,
                "permission",
                request,
                lines,
            )
            .await?;
        }
        if let Some(request) = turn.elicitation {
            self.ask(method::ELICITATION_CREATE, "elicitation", request, lines)
                .await?;
        }
        for update in turn.updates {
            self.update(update).await?;
        }
        let stop = if turn.await_cancel {
            self.wait_for_cancel(lines).await?
        } else {
            turn.stop_reason
        };
        let mut answer = json!({ "stopReason": stop });
        if let Some(usage) = turn.usage {
            answer["usage"] = usage;
        }
        self.send(wire::result(id, answer)).await?;
        if turn.then_exit {
            std::process::exit(0);
        }
        Ok(())
    }

    /// A question the agent puts to the client, and the wait for its answer.
    /// What comes back is logged under `<what>/answered`, so a test can prove
    /// what reached the agent rather than what the client believes it sent.
    async fn ask(
        &mut self,
        method: &str,
        what: &str,
        params: Value,
        lines: &mut Lines<BufReader<Stdin>>,
    ) -> Result<(), Failed> {
        let id = agent_client_protocol_schema::rpc::RequestId::Str(format!("{what}-1"));
        self.send(wire::request(id, method, params)).await?;
        while let Some(line) = lines.next_line().await? {
            let Ok(envelope) = serde_json::from_str::<Envelope>(&line) else {
                continue;
            };
            match envelope.into_inner() {
                Body::Reply(Reply::Result { result, .. }) => {
                    return self.record(&format!("{what}/answered"), &result).await;
                }
                Body::Reply(Reply::Error { error, .. }) => {
                    let said = serde_json::to_value(error)?;
                    return self.record(&format!("{what}/refused"), &said).await;
                }
                Body::Notification(note) => {
                    self.record(&note.method, &note.params.unwrap_or(Value::Null))
                        .await?;
                }
                Body::Request(_) => {}
            }
        }
        Ok(())
    }

    /// Hold the turn open until the client says stop, then end the way ACP
    /// says a cancelled turn ends.
    async fn wait_for_cancel(
        &mut self,
        lines: &mut Lines<BufReader<Stdin>>,
    ) -> Result<String, Failed> {
        while let Some(line) = lines.next_line().await? {
            let Ok(envelope) = serde_json::from_str::<Envelope>(&line) else {
                continue;
            };
            if let Body::Notification(note) = envelope.into_inner() {
                let params = note.params.unwrap_or(Value::Null);
                self.record(&note.method, &params).await?;
                if note.method.as_ref() == method::SESSION_CANCEL {
                    return Ok("cancelled".to_string());
                }
            }
        }
        Ok("cancelled".to_string())
    }

    async fn update(&mut self, update: Value) -> Result<(), Failed> {
        let params = json!({ "sessionId": self.script.session_id, "update": update });
        self.send(wire::notification(method::SESSION_UPDATE, params))
            .await
    }

    async fn send(&mut self, body: Body) -> Result<(), Failed> {
        let line = wire::line(body)?;
        self.out.write_all(line.as_bytes()).await?;
        self.out.write_all(b"\n").await?;
        self.out.flush().await?;
        Ok(())
    }

    async fn record(&self, method: &str, params: &Value) -> Result<(), Failed> {
        let Some(path) = &self.log else {
            return Ok(());
        };
        let line = format!("{}\n", json!({ "method": method, "params": params }));
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

/// The script's turn, taken by value so the borrow of `self` ends here.
fn script_turn(turn: &Turn) -> Turn {
    Turn {
        permission: turn.permission.clone(),
        elicitation: turn.elicitation.clone(),
        updates: turn.updates.clone(),
        stop_reason: turn.stop_reason.clone(),
        usage: turn.usage.clone(),
        await_cancel: turn.await_cancel,
        then_exit: turn.then_exit,
    }
}

fn refusal(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}
