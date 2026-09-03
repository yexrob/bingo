//! The MCP server one accepted stream carries.
//!
//! `rmcp`'s server half does the protocol; what is written here is the four
//! things ADR-0036 needs of it — `initialize`, `tools/list`, `tools/call` and
//! `notifications/tools/list_changed` — each of them one hop onto
//! [`Doors`](super::doors::Doors) and back through [`shape`](super::shape).
//! Nothing here decides anything: what may be called and what a call does are
//! bingo's to say.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::ServiceExt;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ErrorData, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo,
};
use rmcp::service::{Peer, RequestContext, RoleServer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;

use super::doors::Doors;
use super::shape;

/// What the agent is told this server is for. One line: the tools themselves
/// carry their own descriptions, and the preamble of the first prompt is
/// where a turn's own words go (ADR-0036 §5).
const INSTRUCTIONS: &str = "bingo's shared tools. A call is served by the turn that is asking, \
                            so it only works while bingo is waiting on your answer.";

/// One conversation's server: the doors it calls through, and a serial that
/// makes the call ids it mints its own.
struct Bridged {
    doors: Arc<dyn Doors>,
    conversation: u64,
    calls: AtomicU64,
}

impl Bridged {
    /// The id this call is journaled under. MCP's own request id never reaches
    /// a handler, so the bridge mints one; the conversation's serial keeps two
    /// adapters on one bingo session from minting the same.
    fn next_call_id(&self) -> String {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        format!("acp_{}_{call}", self.conversation)
    }

    async fn answer(&self, request: CallToolRequestParams) -> CallToolResponse {
        let call = shape::asked(request, self.next_call_id());
        match self.doors.call(call).await {
            Ok(output) => shape::answered(output).into(),
            Err(refused) => shape::refused(&refused).into(),
        }
    }
}

impl ServerHandler for Bridged {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("bingo", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let offer = self.doors.offer().await;
        Ok(ListToolsResult::with_all_items(
            offer.iter().map(shape::offered).collect(),
        ))
    }

    /// Never `Err`: a refusal is an answer the agent can read and go on from,
    /// and a protocol error would end the conversation instead (ADR-0036 §2).
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(self.answer(request).await)
    }
}

/// Speak MCP on this stream until either end stops. Returns when the
/// conversation is over, which is what frees the token to be dialled again.
pub async fn serve<S>(doors: Arc<dyn Doors>, changed: Arc<Notify>, conversation: u64, stream: S)
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let bridged = Bridged {
        doors,
        conversation,
        calls: AtomicU64::new(0),
    };
    let Ok(running) = bridged.serve(stream).await else {
        return;
    };
    let announcing = tokio::spawn(announce(running.peer().clone(), changed));
    let _ = running.waiting().await;
    announcing.abort();
}

/// `CatalogChanged` reaching the agent (ADR-0036 §1). Repeated changes
/// collapse into one notification, which is what `list_changed` means: ask
/// again, not "here is the diff".
async fn announce(peer: Peer<RoleServer>, changed: Arc<Notify>) {
    loop {
        changed.notified().await;
        if peer.notify_tool_list_changed().await.is_err() {
            return;
        }
    }
}
