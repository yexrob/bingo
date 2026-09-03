//! The `mcpServers` a session opens with (ADR-0036 §§3–4).
//!
//! Two kinds of row, and they cross for opposite reasons. Ours points the
//! agent back at this run — a stdio row whose command is this binary in its
//! proxy mode, with the address and the token in its environment — and is how
//! bingo's own tools reach it at all. Theirs are the servers a person already
//! configured for bingo, handed over so the agent dials them itself: one hop
//! instead of two, their own env and auth, and their tools off the bridge so
//! nothing is served twice.
//!
//! Pure: every function here takes what it needs and answers a row or a reason
//! there is none. Nothing dials, nothing reads settings, nothing spawns.

use std::path::Path;

use agent_client_protocol_schema::v1::{
    EnvVariable, HttpHeader, McpCapabilities, McpServer, McpServerHttp, McpServerStdio,
};
use serde_json::Value;

use crate::bridge::{ADDRESS_VAR, Address, PROXY_MODE, TOKEN_VAR, Token};

/// What the bridge's own row is called on the agent's side. One word, because
/// it is what the agent prefixes its tool names with in its own logs.
pub const OURS: &str = "bingo";

/// The row that points the agent back at this run. Stdio, the one transport
/// every agent must speak; the token rides the environment because argv is
/// world-readable (ADR-0036 §3).
pub fn ours(exe: &Path, address: &Address, token: &Token) -> McpServer {
    McpServer::Stdio(
        McpServerStdio::new(OURS, exe)
            .args(vec![PROXY_MODE.to_string()])
            .env(vec![
                EnvVariable::new(ADDRESS_VAR, address.as_str()),
                EnvVariable::new(TOKEN_VAR, token.as_str()),
            ]),
    )
}

/// Why a person's row did not cross, in the words a notice is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped(pub String);

/// One of a person's own rows, as the agent will dial it. The row is the shape
/// `bingo-mcp` answers with, which is the shape they wrote on disk.
///
/// Two rows do not cross. A transport this protocol has no word for is one; so
/// is an http row to an agent whose handshake did not claim http, because a
/// row an agent cannot take is a row it would refuse the whole `session/new`
/// over.
pub fn theirs(name: &str, row: &Value, mcp: &McpCapabilities) -> Result<McpServer, Skipped> {
    match transport(row) {
        "stdio" => stdio(name, row),
        "http" if mcp.http => http(name, row),
        "http" => Err(Skipped(format!(
            "the MCP server `{name}` is http, and this agent's handshake does not \
             claim http servers; it was not forwarded"
        ))),
        other => Err(Skipped(format!(
            "the MCP server `{name}` speaks {other}, which ACP has no server row \
             for; it was not forwarded"
        ))),
    }
}

/// A row whose transport is unwritten is a child process, the way it is
/// unwritten on disk.
fn transport(row: &Value) -> &str {
    row["type"].as_str().unwrap_or("stdio")
}

fn stdio(name: &str, row: &Value) -> Result<McpServer, Skipped> {
    let command = row["command"].as_str().ok_or_else(|| {
        Skipped(format!(
            "the MCP server `{name}` names no command to run; it was not forwarded"
        ))
    })?;
    Ok(McpServer::Stdio(
        McpServerStdio::new(name, command)
            .args(strings(&row["args"]))
            .env(pairs(&row["env"], EnvVariable::new)),
    ))
}

fn http(name: &str, row: &Value) -> Result<McpServer, Skipped> {
    let url = row["url"].as_str().ok_or_else(|| {
        Skipped(format!(
            "the MCP server `{name}` names no url to dial; it was not forwarded"
        ))
    })?;
    Ok(McpServer::Http(
        McpServerHttp::new(name, url).headers(pairs(&row["headers"], HttpHeader::new)),
    ))
}

/// ACP's stdio row has no working directory, so a row that named one loses it.
/// Said rather than silently dropped: a server that starts somewhere else is a
/// server that fails for a reason nobody can see.
pub fn homeless(name: &str, row: &Value) -> Option<String> {
    row["cwd"].as_str().map(|cwd| {
        format!(
            "the MCP server `{name}` was forwarded, but ACP's server row has no \
             working directory: it will run in the agent's, not in {cwd}"
        )
    })
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

/// A map of names to values, as the protocol's list of pairs. Ordered by name,
/// because a `BTreeMap` on the other side of the service is.
fn pairs<T>(value: &Value, of: fn(String, String) -> T) -> Vec<T> {
    value
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(name, value)| Some(of(name.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(server: &McpServer) -> Value {
        serde_json::to_value(server).expect("a row is json")
    }

    fn capable(http: bool) -> McpCapabilities {
        let mut mcp = McpCapabilities::new();
        mcp.http = http;
        mcp
    }

    /// The recorded row: this binary, its proxy mode, and the two words that
    /// say which run and which conversation — in `env`, never in `args`.
    #[test]
    fn our_row_points_the_agent_back_at_this_run() {
        let row = ours(
            Path::new("/opt/bingo"),
            &Address::from_raw("/tmp/1.sock"),
            &Token::from_raw("t0ken"),
        );
        assert_eq!(
            wire(&row),
            json!({
                "name": "bingo",
                "command": "/opt/bingo",
                "args": ["acp-mcp-proxy"],
                "env": [
                    { "name": "BINGO_ACP_BRIDGE_ADDRESS", "value": "/tmp/1.sock" },
                    { "name": "BINGO_ACP_BRIDGE_TOKEN", "value": "t0ken" }
                ]
            })
        );
        assert!(
            !wire(&row)["args"].to_string().contains("t0ken"),
            "the token is not in argv"
        );
    }

    /// A person's stdio row crosses as they wrote it: their command, their
    /// arguments, their environment.
    #[test]
    fn a_stdio_row_crosses_verbatim() {
        let row = theirs(
            "files",
            &json!({ "type": "stdio", "command": "npx", "args": ["-y", "files"],
                     "env": { "TOKEN": "s3cret" } }),
            &capable(false),
        )
        .expect("it crosses");
        assert_eq!(
            wire(&row),
            json!({
                "name": "files",
                "command": "npx",
                "args": ["-y", "files"],
                "env": [{ "name": "TOKEN", "value": "s3cret" }]
            })
        );
    }

    /// A row with no `type` is a child process, on disk and on the wire —
    /// where a stdio row is the untagged one, so it carries no `type` either.
    #[test]
    fn a_row_that_names_no_transport_is_a_child_process() {
        let row =
            theirs("files", &json!({ "command": "npx" }), &capable(false)).expect("it crosses");
        assert_eq!(
            wire(&row),
            json!({ "name": "files", "command": "npx", "args": [], "env": [] })
        );
    }

    /// An http row crosses to an agent that claims http — url and headers, the
    /// credentials with them (ADR-0036 §4, chosen and recorded).
    #[test]
    fn an_http_row_crosses_to_an_agent_that_claims_http() {
        let row = theirs(
            "remote",
            &json!({ "type": "http", "url": "https://mcp.example.com/mcp",
                     "headers": { "Authorization": "Bearer s3cret" } }),
            &capable(true),
        )
        .expect("it crosses");
        assert_eq!(
            wire(&row),
            json!({
                "type": "http",
                "name": "remote",
                "url": "https://mcp.example.com/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer s3cret" }]
            })
        );
    }

    /// And does not cross to one that does not: the protocol says an http row
    /// is only available when the handshake claimed it.
    #[test]
    fn an_http_row_an_agent_cannot_take_is_skipped_and_named() {
        let skipped = theirs(
            "remote",
            &json!({ "type": "http", "url": "https://mcp.example.com/mcp" }),
            &capable(false),
        )
        .expect_err("it does not cross");
        assert!(skipped.0.contains("remote"), "{}", skipped.0);
        assert!(skipped.0.contains("http"), "{}", skipped.0);
    }

    /// A transport ACP has no row for is skipped and named rather than guessed
    /// at (ADR-0036 §4).
    #[test]
    fn a_transport_with_no_server_row_is_skipped_and_named() {
        let skipped = theirs(
            "legacy",
            &json!({ "type": "sse", "url": "http://localhost:8000/sse" }),
            &capable(true),
        )
        .expect_err("it does not cross");
        assert!(skipped.0.contains("legacy"), "{}", skipped.0);
        assert!(skipped.0.contains("sse"), "{}", skipped.0);
    }

    #[test]
    fn a_row_with_nothing_to_dial_is_skipped_and_named() {
        for (row, word) in [
            (json!({ "type": "stdio" }), "command"),
            (json!({ "type": "http" }), "url"),
        ] {
            let skipped = theirs("broken", &row, &capable(true)).expect_err("it does not cross");
            assert!(skipped.0.contains(word), "{}", skipped.0);
        }
    }

    /// The one thing a forwarded row loses, said out loud.
    #[test]
    fn a_row_that_named_a_working_directory_is_told_it_lost_one() {
        let said = homeless("files", &json!({ "command": "npx", "cwd": "/work" }))
            .expect("a row with a cwd is worth a word");
        assert!(said.contains("files") && said.contains("/work"), "{said}");
        assert_eq!(homeless("files", &json!({ "command": "npx" })), None);
    }
}
