//! The smallest MCP client that can prove the bridge is a real MCP server.
//!
//! Written out rather than taken from a library on purpose: the scripted agent
//! stands in for Claude Code and Codex, and what it must show is that ordinary
//! newline-delimited JSON-RPC over a spawned child's stdio reaches bingo's
//! tools. A client built from the same crate as the server would prove the two
//! halves agree with each other and nothing more.
//!
//! Three requests and one notification is the whole of it: `initialize`,
//! `notifications/initialized`, `tools/list`, `tools/call`. Anything the
//! server says that was not asked for is remembered by method name, which is
//! how `notifications/tools/list_changed` is seen to have arrived.

use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

type Failed = Box<dyn std::error::Error>;

/// A protocol version this client speaks. The server answers with its own; a
/// scripted agent has no opinion about which one wins.
const VERSION: &str = "2025-06-18";

/// One dialled MCP server: the child, the two ends of its stdio, and what it
/// has said unprompted.
pub struct Server {
    /// Held so the child lives as long as the conversation does; dropping this
    /// closes its pipes.
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next: u64,
    /// Every notification the server sent, in the order it sent them.
    pub heard: Vec<String>,
}

impl Server {
    /// Spawn a stdio server row exactly as it was written — its command, its
    /// arguments, its environment — and shake hands.
    pub async fn dial(row: &Value) -> Result<Self, Failed> {
        let mut child = spawned(row)?;
        let stdin = child.stdin.take().ok_or("the server has no stdin")?;
        let stdout = child.stdout.take().ok_or("the server has no stdout")?;
        let mut server = Server {
            _child: child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next: 1,
            heard: Vec::new(),
        };
        server.hello().await?;
        Ok(server)
    }

    async fn hello(&mut self) -> Result<(), Failed> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": VERSION,
                "capabilities": {},
                "clientInfo": { "name": "bingo-fake-acp-agent", "version": "1" }
            }),
        )
        .await?;
        self.notify("notifications/initialized").await
    }

    pub async fn list(&mut self) -> Result<Value, Failed> {
        self.request("tools/list", json!({})).await
    }

    pub async fn call(&mut self, name: &str, arguments: &Value) -> Result<Value, Failed> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// One request, and the reading of everything that arrives until its own
    /// answer does. A `tools/call` that the far side refuses is an answer with
    /// `isError` in it, not a transport fault, so it comes back here.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, Failed> {
        let id = self.next;
        self.next += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        loop {
            let line = self
                .lines
                .next_line()
                .await?
                .ok_or("the MCP server closed mid-conversation")?;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] == json!(id) {
                return Ok(answered(message));
            }
            if let Some(said) = message["method"].as_str() {
                self.heard.push(said.to_string());
            }
        }
    }

    async fn notify(&mut self, method: &str) -> Result<(), Failed> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": {} }))
            .await
    }

    async fn send(&mut self, message: Value) -> Result<(), Failed> {
        self.stdin.write_all(message.to_string().as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

/// What a reply carried, whichever half it was. A protocol error is handed
/// back as itself rather than raised: a scripted agent's job is to record what
/// it was told.
fn answered(message: Value) -> Value {
    match message.get("error") {
        Some(error) => json!({ "error": error }),
        None => message["result"].clone(),
    }
}

fn spawned(row: &Value) -> Result<Child, Failed> {
    let mut command = Command::new(row["command"].as_str().ok_or("a row names a command")?);
    for arg in strings(&row["args"]) {
        command.arg(arg);
    }
    for pair in row["env"].as_array().into_iter().flatten() {
        if let (Some(name), Some(value)) = (pair["name"].as_str(), pair["value"].as_str()) {
            command.env(name, value);
        }
    }
    Ok(command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
