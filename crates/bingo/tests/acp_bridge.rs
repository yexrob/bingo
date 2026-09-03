//! Black-box: MCP through the real `bingo acp-mcp-proxy` (ADR-0036 §3).
//!
//! The bridge's own tests speak to it over the socket directly. This one puts
//! the shipped binary in the middle, spawned exactly as a `session/new`
//! server row spawns it — stdio, the address and the token in its environment,
//! nothing in its argv — because the proxy is the half an agent actually runs.
//!
//! What is on the far side of the doors is worker Q's; here they are a double.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use bingo_provider_acp::bridge::doors::{Doors, Refused};
use bingo_provider_acp::bridge::{ADDRESS_VAR, Address, Bridge, TOKEN_VAR};
use bingo_sdk::{Env, ToolCall, ToolOutput, ToolSpec};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::json;
use tokio::sync::Mutex;

/// Doors that answer with what they were asked, so a round trip through two
/// processes can be seen to have carried the arguments.
struct Echo {
    seen: Mutex<Vec<ToolCall>>,
}

#[async_trait]
impl Doors for Echo {
    async fn offer(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "Shout".into(),
            description: "Say something out loud.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
            meta: serde_json::Map::new(),
        }]
    }

    async fn call(&self, call: ToolCall) -> Result<ToolOutput, Refused> {
        let said = call.input["text"].as_str().unwrap_or_default().to_string();
        self.seen.lock().await.push(call);
        match said.is_empty() {
            true => Err(Refused::new("Shout needs something to say")),
            false => Ok(ToolOutput::text(format!("posted: {said}"))),
        }
    }
}

/// The proxy as an agent spawns it: this binary, one hidden mode, and the two
/// words it needs in its environment rather than in its argv.
fn proxy(address: &Address, token: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_bingo"));
    command
        .arg("acp-mcp-proxy")
        .env(ADDRESS_VAR, address.as_str())
        .env(TOKEN_VAR, token);
    command
}

#[tokio::test]
async fn an_agent_reaches_the_shared_tools_through_the_spawned_proxy() {
    let home = tempfile::tempdir().expect("a temporary home");
    let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
    let bridge = Bridge::at(address.clone()).expect("it listens");
    let doors = Arc::new(Echo {
        seen: Mutex::new(Vec::new()),
    });
    let token = bridge.admit(doors.clone()).expect("a token");

    let transport = TokioChildProcess::new(proxy(&address, token.as_str())).expect("it spawns");
    let client = ().serve(transport).await.expect("the bridge answers");

    let offered = client.list_all_tools().await.expect("a list");
    assert_eq!(
        offered
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>(),
        ["Shout"]
    );

    let answered = client
        .call_tool(
            CallToolRequestParams::new("Shout").with_arguments(
                json!({ "text": "hello from the other side" })
                    .as_object()
                    .expect("an object")
                    .clone(),
            ),
        )
        .await
        .expect("a call is answered");
    assert_eq!(answered.is_error, Some(false));
    assert_eq!(
        answered
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<String>(),
        "posted: hello from the other side"
    );
    assert_eq!(doors.seen.lock().await.len(), 1);

    // A refusal crosses two processes as an answer, not as a broken pipe.
    let refused = client
        .call_tool(CallToolRequestParams::new("Shout"))
        .await
        .expect("a refusal is an answer");
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(
        refused
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<String>(),
        "Shout needs something to say"
    );

    let _ = client.cancel().await;
}

/// A proxy that names a token this run never minted is closed on, and its
/// client's handshake fails rather than hanging.
#[tokio::test]
async fn a_proxy_with_a_token_this_run_never_minted_gets_nothing() {
    let home = tempfile::tempdir().expect("a temporary home");
    let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
    let _bridge = Bridge::at(address.clone()).expect("it listens");

    let transport = TokioChildProcess::new(proxy(&address, "not-a-token")).expect("it spawns");
    assert!(
        ().serve(transport).await.is_err(),
        "the stream is closed, not answered"
    );
}
