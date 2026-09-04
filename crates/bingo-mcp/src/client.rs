//! The client half of one connection: what it tells a server it can do, and
//! who a question from that server reaches.
//!
//! A server may only raise an `elicitation/create` while it is handling a
//! request of ours, so the call in flight is the one that asked for it — and
//! that call is the door onto the session waiting on it (ADR-0039 §1). A
//! [`Guard`] holds the door open for exactly as long as one `tools/call`, and
//! a question that arrives with nothing in flight reaches nobody and is
//! declined.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bingo_sdk::{HostHandle, Level, ToolHost};
use rmcp::model::{
    ClientInfo, ElicitRequestParams, ElicitResult, ElicitationCapability,
    FormElicitationCapability, Implementation, UrlElicitationCapability,
};
use rmcp::service::{RequestContext, RoleClient};
use rmcp::{ClientHandler, ErrorData as McpError};

use crate::elicitation;

/// One `tools/call` in flight: the door onto the session that is waiting, and
/// the host a notice about it goes to.
#[derive(Clone)]
struct Waiting {
    id: u64,
    call: Arc<dyn ToolHost>,
    host: HostHandle,
}

/// Who a server's question reaches, for as long as one of our calls is in
/// flight on its connection.
pub struct Asker {
    server: String,
    /// The calls in flight, newest last. A session's MCP calls never overlap —
    /// an unknown tool is not concurrency-safe (ADR-0009 §2) — but two
    /// sessions may be talking to one server, so the newest is the one asked.
    calls: Mutex<Vec<Waiting>>,
    next: AtomicU64,
}

impl std::fmt::Debug for Asker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asker")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl Asker {
    pub fn new(server: &str) -> Self {
        Self {
            server: server.to_string(),
            calls: Mutex::new(Vec::new()),
            next: AtomicU64::new(0),
        }
    }

    /// Hold the door open for one call. Dropping the guard closes it, whether
    /// the call answered, failed or was interrupted.
    pub fn during(self: &Arc<Self>, call: Arc<dyn ToolHost>, host: HostHandle) -> Guard {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.held().push(Waiting { id, call, host });
        Guard {
            asker: Arc::clone(self),
            id,
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Vec<Waiting>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn waiting(&self) -> Option<Waiting> {
        self.held().last().cloned()
    }
}

/// One call's hold on the door.
pub struct Guard {
    asker: Arc<Asker>,
    id: u64,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.asker.held().retain(|waiting| waiting.id != self.id);
    }
}

/// The client one server sees.
#[derive(Debug)]
pub struct Client {
    asker: Arc<Asker>,
}

impl Client {
    pub fn new(asker: Arc<Asker>) -> Self {
        Self { asker }
    }

    /// Put a server's question to whoever is waiting on the call that raised
    /// it, and answer the server with what came back.
    async fn elicit(&self, request: ElicitRequestParams) -> ElicitResult {
        let Some(waiting) = self.asker.waiting() else {
            // Nothing of ours is in flight, so nobody asked for this and
            // nobody is waiting to be asked.
            tracing::warn!(
                server = self.asker.server,
                "an elicitation arrived outside a call; declining it"
            );
            return elicitation::declined();
        };
        match request {
            ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => self.form(&waiting, &message, requested_schema).await,
            ElicitRequestParams::UrlElicitationParams { message, url, .. } => {
                self.url(&waiting, &message, &url).await
            }
            _ => elicitation::declined(),
        }
    }

    async fn form(
        &self,
        waiting: &Waiting,
        message: &str,
        schema: rmcp::model::ElicitationSchema,
    ) -> ElicitResult {
        let schema = match serde_json::to_value(&schema) {
            Ok(schema) => schema,
            Err(error) => return self.refused(waiting, &error.to_string()).await,
        };
        let form = match elicitation::form(&self.asker.server, message, &schema) {
            Ok(form) => form,
            Err(why) => return self.refused(waiting, &why).await,
        };
        match waiting.call.ask(form.kind(), elicitation::answers()).await {
            Ok(answer) => form.result(&answer),
            // Nobody could be asked: the fail-closed fate a question meets
            // where no surface can answer it (ADR-0039 §2).
            Err(_) => elicitation::declined(),
        }
    }

    async fn url(&self, waiting: &Waiting, message: &str, url: &str) -> ElicitResult {
        let kind = elicitation::url_kind(&self.asker.server, message, url);
        match waiting.call.ask(kind, elicitation::url_answers()).await {
            Ok(answer) => elicitation::url_result(&answer),
            Err(_) => elicitation::declined(),
        }
    }

    /// A request nobody can be asked, because the schema is not one the spec
    /// allows. The server hears a decline and the person hears why, since a
    /// silent decline looks to both like a server that hung.
    async fn refused(&self, waiting: &Waiting, why: &str) -> ElicitResult {
        let _ = waiting
            .host
            .notice(
                Level::Warn,
                "MCP_ELICITATION_REFUSED",
                &format!(
                    "{} asked something this client cannot put: {why}",
                    self.asker.server
                ),
            )
            .await;
        elicitation::declined()
    }
}

impl ClientHandler for Client {
    /// Both modes of the elicitation capability, declared at the handshake
    /// (spec, Capabilities). Schema validation is not claimed: what is sent
    /// back is checked against the property's type, not against the whole
    /// schema's bounds.
    fn get_info(&self) -> ClientInfo {
        // Every type here is `#[non_exhaustive]`, so what this client declares
        // is set on the defaults rather than spelled as a literal.
        let mut info = ClientInfo::default();
        info.capabilities.elicitation = Some(
            ElicitationCapability::new()
                .with_form(FormElicitationCapability::new())
                .with_url(UrlElicitationCapability::new()),
        );
        info.client_info = Implementation::new("bingo", env!("CARGO_PKG_VERSION"));
        info
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        Ok(self.elicit(request).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_declares_both_modes_of_elicitation() {
        let client = Client::new(Arc::new(Asker::new("files")));
        let capability = client
            .get_info()
            .capabilities
            .elicitation
            .expect("the capability is declared");
        assert!(capability.form.is_some());
        assert!(capability.url.is_some());
        let json = serde_json::to_value(&capability).expect("json");
        assert_eq!(json, serde_json::json!({ "form": {}, "url": {} }));
        assert_eq!(client.get_info().client_info.name, "bingo");
    }

    #[tokio::test]
    async fn a_question_with_nothing_of_ours_in_flight_reaches_nobody() {
        let client = Client::new(Arc::new(Asker::new("files")));
        let result = client
            .elicit(ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "who are you?".into(),
                requested_schema: serde_json::from_value(serde_json::json!({
                    "type": "object",
                    "properties": { "name": { "type": "string" } }
                }))
                .expect("a schema"),
            })
            .await;
        assert_eq!(
            result.action,
            rmcp::model::ElicitationAction::Decline,
            "a server may only ask while it is answering us"
        );
    }

    #[test]
    fn a_guard_holds_the_door_for_one_call_and_no_longer() {
        let asker = Arc::new(Asker::new("files"));
        assert!(asker.waiting().is_none());
        let host = bingo_sdk::testing::NoHost::handle();
        let door = || crate::tests::Scripted::new(Vec::new()) as Arc<dyn ToolHost>;
        let outer = asker.during(door(), host.clone());
        let inner = asker.during(door(), host);
        assert!(asker.waiting().is_some());
        drop(inner);
        assert!(
            asker.waiting().is_some(),
            "the call still in flight keeps the door"
        );
        drop(outer);
        assert!(asker.waiting().is_none());
    }
}
