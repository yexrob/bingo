//! What one ACP session is given so it can act, not only read (ADR-0036).
//!
//! Three things, made together because each depends on the last: which of a
//! person's own MCP servers the agent will dial itself, the doors that serve
//! everything those servers do not, and the `mcpServers` rows that carry both
//! across `session/new`. Made together, and let go together: dropping a
//! [`Crossing`] closes the conversation its token named.
//!
//! The person's rows are read through the service `bingo-mcp` registers, not
//! from settings: a key belongs to the plugin that claimed it, and a second
//! reading of one would be a second answer to what a person configured
//! (ADR-0031 §1).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use agent_client_protocol_schema::v1::{AgentCapabilities, McpCapabilities, McpServer};
use bingo_sdk::{HostHandle, Level, ServiceHandle, SessionId};
use serde_json::{Map, Value};

use crate::bridge::{Bridge, Token};
use crate::config::Adapter;
use crate::error::AcpError;
use crate::servers;
use crate::shared::Shared;

/// The service `bingo-mcp` registers its configured rows under, and the one
/// method it speaks. Two strings across a plugin line, which is what a service
/// key is; the rows themselves are never read from settings here.
const ROWS: &str = "mcp.servers";
const SERVERS: &str = "servers";

/// The code a person sees when a row of theirs did not cross.
const SKIPPED: &str = "ACP_MCP";

/// What the first prompt of a bridged conversation says, before what the
/// person asked. A tool in the hand is no tool if nobody says it is there
/// (ADR-0036 §5).
pub const SAYS: &str = "\
You have bingo's own tools over MCP, from the server named `bingo`. They are \
how you act in this house rather than only answer in it: posting to a room, \
starting or waiting on work, whatever this session offers. Call `tools/list` \
on that server to see them — the set can change mid-conversation, and you \
will be told when it does. Two things to know. A call is served by the turn \
that is waiting on you, so it only works while you are answering; after your \
turn ends it is refused. And a call that waits on somebody else's work may \
wait a long time — nothing times it out, and it ends when this turn is \
interrupted.";

/// One session's way back into bingo. Dropping it dismisses the token, so a
/// conversation that is over stops being an address.
pub struct Crossing {
    bridge: Arc<Bridge>,
    token: Token,
    /// The doors the bridge serves this conversation through. Held so the
    /// provider can hand each request's tool list to them.
    pub doors: Arc<Shared>,
    /// The rows `session/new` carries: ours first, then the person's own.
    pub servers: Vec<McpServer>,
}

impl Drop for Crossing {
    fn drop(&mut self) {
        self.bridge.dismiss(&self.token);
    }
}

/// Open one. The capabilities are the agent's own handshake: they decide which
/// of a person's rows it can be handed at all.
pub async fn open(
    bridge: &Arc<Bridge>,
    host: &HostHandle,
    session: &SessionId,
    named: (&str, &Adapter),
    capabilities: &AgentCapabilities,
    exe: &Path,
) -> Result<Crossing, AcpError> {
    let (name, adapter) = named;
    let (theirs, forwarded) = forwarded(host, adapter, &capabilities.mcp_capabilities).await;
    let doors = Shared::new(
        host.clone(),
        session.clone(),
        name,
        adapter.tools.clone(),
        forwarded,
    );
    let token = bridge.admit(doors.clone() as Arc<dyn crate::bridge::Doors>)?;
    let mut servers = vec![servers::ours(exe, bridge.address(), &token)];
    servers.extend(theirs);
    Ok(Crossing {
        bridge: bridge.clone(),
        token,
        doors,
        servers,
    })
}

/// The person's rows the agent will dial itself, and the names of the servers
/// they came from — which is what leaves the bridge's offer, so that nothing
/// is served twice (ADR-0036 §4).
///
/// A row that did not cross is said, by name: a server a person configured and
/// never sees anywhere is worse than one that failed loudly.
async fn forwarded(
    host: &HostHandle,
    adapter: &Adapter,
    mcp: &McpCapabilities,
) -> (Vec<McpServer>, BTreeSet<String>) {
    if !adapter.forward_mcp {
        return (Vec::new(), BTreeSet::new());
    }
    let mut crossed = Vec::new();
    let mut names = BTreeSet::new();
    for (name, row) in rows(host).await {
        match servers::theirs(&name, &row, mcp) {
            Ok(server) => {
                crossed.push(server);
                names.insert(name.clone());
                say(host, servers::homeless(&name, &row)).await;
            }
            Err(skipped) => say(host, Some(skipped.0)).await,
        }
    }
    (crossed, names)
}

async fn say(host: &HostHandle, line: Option<String>) {
    if let Some(line) = line {
        let _ = host.notice(Level::Warn, SKIPPED, &line).await;
    }
}

/// What `bingo-mcp` says is configured now. Nothing when it is not in this
/// build, or has nothing to say: an ACP adapter with no MCP plugin beside it
/// forwards no rows and serves everything itself.
async fn rows(host: &HostHandle) -> Map<String, Value> {
    let Some(service) = host.service::<ServiceHandle>(ROWS) else {
        return Map::new();
    };
    let Ok(answered) = service.call(SERVERS, Value::Null).await else {
        return Map::new();
    };
    match &answered[SERVERS] {
        Value::Object(rows) => rows.clone(),
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::socket::Address;
    use crate::bridge::{Doors, Refused};
    use async_trait::async_trait;
    use bingo_sdk::testing::ServiceHost;
    use bingo_sdk::{ServiceError, ToolCall, ToolOutput, ToolSpec, WireService};
    use serde_json::json;

    /// The service `bingo-mcp` would have registered, answering what a person
    /// configured.
    struct Configured(Value);

    #[async_trait]
    impl WireService for Configured {
        async fn call(&self, _method: &str, _params: Value) -> Result<Value, ServiceError> {
            Ok(json!({ SERVERS: self.0 }))
        }
    }

    /// A host whose only service is the rows, or one with no MCP plugin in it
    /// at all.
    fn host(rows: Option<Value>) -> HostHandle {
        match rows {
            Some(rows) => ServiceHost::holding(
                ROWS,
                Arc::new(ServiceHandle::new(Arc::new(Configured(rows)))),
            ),
            None => ServiceHost::handle(),
        }
    }

    fn adapter(value: Value) -> Adapter {
        serde_json::from_value(value).expect("an adapter row")
    }

    fn capable(http: bool) -> AgentCapabilities {
        let mut capabilities = AgentCapabilities::new();
        capabilities.mcp_capabilities.http = http;
        capabilities
    }

    fn bridge() -> (Arc<Bridge>, tempfile::TempDir) {
        let home = tempfile::tempdir().expect("a temporary home");
        let address = Address::from_raw(home.path().join("b.sock").display().to_string());
        (Arc::new(Bridge::at(address).expect("it listens")), home)
    }

    fn rows() -> Value {
        json!({
            "files": { "type": "stdio", "command": "npx", "args": ["-y", "files"] },
            "remote": { "type": "http", "url": "https://mcp.example.com/mcp" }
        })
    }

    fn named(server: &McpServer) -> String {
        serde_json::to_value(server).expect("a row is json")["name"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn crossing(
        rows: Option<Value>,
        row: Value,
        http: bool,
    ) -> (Crossing, Arc<Bridge>, tempfile::TempDir) {
        let (bridge, home) = bridge();
        let crossing = open(
            &bridge,
            &host(rows),
            &SessionId::mint(),
            ("scripted", &adapter(row)),
            &capable(http),
            Path::new("/opt/bingo"),
        )
        .await
        .expect("it opens");
        (crossing, bridge, home)
    }

    /// The default: our row first, then every row of the person's the agent
    /// can take (ADR-0036 §4).
    #[tokio::test]
    async fn a_forwarding_row_carries_ours_and_theirs() {
        let (crossing, _bridge, _home) =
            crossing(Some(rows()), json!({ "command": "x" }), true).await;
        let names: Vec<String> = crossing.servers.iter().map(named).collect();
        assert_eq!(names, ["bingo", "files", "remote"]);
    }

    /// An http row an agent never claimed it could take does not cross, and
    /// its tools stay on the bridge, where they are still reachable.
    #[tokio::test]
    async fn a_row_the_agent_cannot_take_is_not_forwarded() {
        let (crossing, _bridge, _home) =
            crossing(Some(rows()), json!({ "command": "x" }), false).await;
        let names: Vec<String> = crossing.servers.iter().map(named).collect();
        assert_eq!(names, ["bingo", "files"]);
    }

    /// `forwardMcp: false` keeps a person's rows — and the credentials in them
    /// — home; only ours crosses, and the sourced tools ride the bridge.
    #[tokio::test]
    async fn a_row_that_forwards_nothing_carries_only_ours() {
        let (crossing, _bridge, _home) = crossing(
            Some(rows()),
            json!({ "command": "x", "forwardMcp": false }),
            true,
        )
        .await;
        let names: Vec<String> = crossing.servers.iter().map(named).collect();
        assert_eq!(names, ["bingo"]);
    }

    /// A build with no MCP plugin beside it has no rows to forward.
    #[tokio::test]
    async fn a_house_with_no_mcp_plugin_forwards_nothing() {
        let (crossing, _bridge, _home) = crossing(None, json!({ "command": "x" }), true).await;
        assert_eq!(crossing.servers.len(), 1);
    }

    /// A conversation that is let go stops being an address: the token it held
    /// names nothing, and the next one is another token.
    #[tokio::test]
    async fn a_crossing_that_is_dropped_dismisses_its_token() {
        let (crossing, bridge, _home) = crossing(None, json!({ "command": "x" }), false).await;
        let token = crossing.token.clone();
        drop(crossing);
        let again = bridge
            .admit(Arc::new(NoDoors) as Arc<dyn Doors>)
            .expect("a fresh token");
        assert_ne!(again.as_str(), token.as_str());
    }

    struct NoDoors;

    #[async_trait]
    impl Doors for NoDoors {
        async fn offer(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        async fn call(&self, _call: ToolCall) -> Result<ToolOutput, Refused> {
            Err(Refused::new("nothing"))
        }
    }
}
