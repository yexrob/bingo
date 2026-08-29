//! A scripted MCP server over stdio, for this crate's tests.
//!
//! It is an example rather than a fixture because `cargo test` builds a
//! crate's examples, which puts the binary beside the test binary with no
//! build script and no second manifest: `target/<profile>/examples/echo_server`
//! next to `target/<profile>/deps/<test>`.
//!
//! Three tools, one for each thing a client has to get right: `echo` returns
//! what it was given, `noisy` writes to stderr (which must reach a log and
//! never the screen), and `boom` answers with `isError`.

use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::io::stdio;
use serde_json::{Value, json};

/// Written before the first byte of protocol, so a test that dialled the
/// server can assert where its stderr went.
const BANNER: &str = "echo_server: ready";

/// What `noisy` writes when it is called.
const NOISE: &str = "echo_server: noisy was called";

struct EchoServer;

/// A one-string-property object schema, carrying the dialect marker a client
/// is expected to strip.
fn one_string(property: &str) -> Arc<JsonObject> {
    let mut properties = JsonObject::new();
    properties.insert(property.to_string(), json!({ "type": "string" }));
    let mut schema = JsonObject::new();
    schema.insert(
        "$schema".to_string(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert("required".to_string(), json!([property]));
    Arc::new(schema)
}

fn tools() -> Vec<Tool> {
    vec![
        Tool::new("echo", "Return the text it was given.", one_string("text")),
        Tool::new("noisy", "Write a line to stderr.", one_string("text")),
        Tool::new("boom", "Answer with isError.", one_string("text")),
        Tool::new(
            "whereami",
            "Return the working directory and the environment it was spawned with.",
            one_string("text"),
        ),
    ]
}

/// The environment variable `whereami` reports, so a test can tell whether a
/// server's configured `env` reached the child.
const MARKER: &str = "BINGO_MCP_MARKER";

fn whereami() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let marker = std::env::var(MARKER).unwrap_or_default();
    format!("{}\n{marker}", cwd.display())
}

fn argument(request: &CallToolRequestParams, name: &str) -> String {
    request
        .arguments
        .as_ref()
        .and_then(|arguments| arguments.get(name))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn answer(request: CallToolRequestParams) -> Result<CallToolResponse, ErrorData> {
    let text = argument(&request, "text");
    match request.name.as_ref() {
        "echo" => Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into()),
        "noisy" => {
            eprintln!("{NOISE}");
            Ok(CallToolResult::success(vec![ContentBlock::text("wrote to stderr")]).into())
        }
        "boom" => {
            Ok(CallToolResult::error(vec![ContentBlock::text(format!("boom: {text}"))]).into())
        }
        "whereami" => Ok(CallToolResult::success(vec![ContentBlock::text(whereami())]).into()),
        other => Err(ErrorData::invalid_params(
            format!("no tool named {other}"),
            None,
        )),
    }
}

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        std::future::ready(answer(request))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("{BANNER}");
    EchoServer.serve(stdio()).await?.waiting().await?;
    Ok(())
}
