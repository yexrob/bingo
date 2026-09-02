//! The client half of an ACP conversation: one line in, one line out.
//!
//! Generic over the transport, so the tests drive it over an in-memory duplex
//! and the child process is a separate concern ([`crate::child`]). Two tasks: a
//! reader that turns lines into answers, updates and questions, and a writer
//! that serialises everything going the other way. Nothing here knows what a
//! session is — that is [`crate::pool`]'s job.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use agent_client_protocol_schema::rpc::RequestId;
use agent_client_protocol_schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, Error as RpcError,
    RequestPermissionRequest, RequestPermissionResponse, SessionNotification,
};
use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::AcpError;
use crate::method::{self, Call, Incoming, Notify};
use crate::wire::{self, Body, Envelope, Reply};

/// What the agent starts a line about. The connection answers nothing on its
/// own: it hands the question over and writes back what it is told.
#[async_trait]
pub trait Client: Send + Sync + 'static {
    /// A turn's stream. Notifications get no reply, so this cannot fail.
    async fn update(&self, notification: SessionNotification);

    /// The agent asking whether it may do something.
    async fn permission(
        &self,
        request: RequestPermissionRequest,
    ) -> Result<RequestPermissionResponse, RpcError>;

    /// The agent asking this client to collect something from a person.
    async fn elicitation(
        &self,
        request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, RpcError>;
}

type Pending = Arc<Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, RpcError>>>>>;

/// One adapter's conversation. Dropping it ends both tasks and, with the
/// child's pipes, the child.
pub struct Connection {
    outgoing: mpsc::UnboundedSender<Body>,
    pending: Pending,
    next_id: AtomicI64,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Connection {
    /// Start reading and writing. `client` answers what the agent asks.
    pub fn spawn<R, W>(reader: R, writer: W, client: Arc<dyn Client>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing, queue) = mpsc::unbounded_channel();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let tasks = vec![
            tokio::spawn(write_lines(writer, queue)),
            tokio::spawn(read_lines(
                reader,
                pending.clone(),
                client,
                outgoing.clone(),
            )),
        ];
        Self {
            outgoing,
            pending,
            next_id: AtomicI64::new(1),
            tasks,
        }
    }

    /// Send a request and wait for the answer that carries its id.
    pub async fn call<C: Call>(&self, params: C) -> Result<C::Response, AcpError> {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let body = serde_json::to_value(params).map_err(AcpError::protocol)?;
        let answer = self.ask(id, C::METHOD, body).await?;
        serde_json::from_value(answer)
            .map_err(|e| AcpError::protocol(format!("{}: {e}", C::METHOD)))
    }

    /// Send a notification. Nothing answers it, so nothing waits.
    pub fn notify<N: Notify>(&self, params: N) -> Result<(), AcpError> {
        let body = serde_json::to_value(params).map_err(AcpError::protocol)?;
        self.outgoing
            .send(wire::notification(N::METHOD, body))
            .map_err(|_| AcpError::transport("the adapter is gone"))
    }

    async fn ask(&self, id: RequestId, method: &str, params: Value) -> Result<Value, AcpError> {
        let (answered, answer) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), answered);
        if self
            .outgoing
            .send(wire::request(id.clone(), method, params))
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(AcpError::transport("the adapter is gone"));
        }
        match answer.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(refusal)) => Err(AcpError::Refused(refusal)),
            Err(_) => Err(AcpError::transport(format!("{method}: nothing answered"))),
        }
    }
}

async fn write_lines<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut queue: mpsc::UnboundedReceiver<Body>,
) {
    while let Some(body) = queue.recv().await {
        let Ok(line) = wire::line(body) else { continue };
        if writer.write_all(line.as_bytes()).await.is_err()
            || writer.write_all(b"\n").await.is_err()
            || writer.flush().await.is_err()
        {
            return;
        }
    }
}

/// Every pending call is failed when the pipe closes, so a turn whose adapter
/// died ends with a transport error instead of waiting for ever.
async fn read_lines<R: AsyncRead + Unpin>(
    reader: R,
    pending: Pending,
    client: Arc<dyn Client>,
    outgoing: mpsc::UnboundedSender<Body>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        // A line that is not a message is the adapter's noise on stdout, not
        // an answer to anything; dropping it keeps the conversation alive.
        if let Ok(envelope) = serde_json::from_str::<Envelope>(&line) {
            dispatch(envelope.into_inner(), &pending, &client, &outgoing).await;
        }
    }
    pending.lock().await.clear();
}

async fn dispatch(
    body: Body,
    pending: &Pending,
    client: &Arc<dyn Client>,
    outgoing: &mpsc::UnboundedSender<Body>,
) {
    match body {
        Body::Reply(reply) => settle(reply, pending).await,
        Body::Notification(note) => {
            observe(&note.method, note.params.unwrap_or(Value::Null), client).await;
        }
        Body::Request(asked) => {
            let answer = answer(
                &asked.method,
                asked.params.unwrap_or(Value::Null),
                client.clone(),
            )
            .await;
            let _ = outgoing.send(match answer {
                Ok(result) => wire::result(asked.id, result),
                Err(error) => wire::failed(asked.id, error),
            });
        }
    }
}

async fn settle(reply: Reply, pending: &Pending) {
    let (id, outcome) = match reply {
        Reply::Result { id, result } => (id, Ok(result)),
        Reply::Error { id, error } => (id, Err(error)),
    };
    if let Some(waiting) = pending.lock().await.remove(&id) {
        let _ = waiting.send(outcome);
    }
}

/// A notification nothing can answer. An update whose shape this build does
/// not know — a variant a newer adapter ships ahead of the schema — is
/// dropped: a turn must not end because the agent said something extra.
async fn observe(method: &str, params: Value, client: &Arc<dyn Client>) {
    if let Ok(Incoming::Update(notification)) = method::incoming(method, params) {
        client.update(*notification).await;
    }
}

async fn answer(method: &str, params: Value, client: Arc<dyn Client>) -> Result<Value, RpcError> {
    match method::incoming(method, params) {
        Ok(Incoming::Permission(request)) => encoded(client.permission(*request).await),
        Ok(Incoming::Elicitation(request)) => encoded(client.elicitation(*request).await),
        // ADR-0035 §6: `fs/*` and `terminal/*` are declared unsupported, so
        // the agent is told rather than left waiting.
        Ok(_) => Err(RpcError::method_not_found()),
        Err(bad) => Err(RpcError::invalid_params().data(bad.to_string())),
    }
}

fn encoded<T: serde::Serialize>(answered: Result<T, RpcError>) -> Result<Value, RpcError> {
    answered.and_then(|answer| serde_json::to_value(answer).map_err(RpcError::into_internal_error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use agent_client_protocol_schema::v1::{
        InitializeRequest, RequestPermissionOutcome, SelectedPermissionOutcome, SessionUpdate,
    };
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{DuplexStream, Lines, ReadHalf, WriteHalf};

    /// The far end of the duplex, standing in for a scripted agent.
    struct Agent {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        writer: WriteHalf<DuplexStream>,
    }

    impl Agent {
        async fn read(&mut self) -> Value {
            let line = self
                .lines
                .next_line()
                .await
                .expect("the pipe is open")
                .expect("a line arrives");
            serde_json::from_str(&line).expect("the client writes JSON")
        }

        async fn write(&mut self, body: Value) {
            self.writer
                .write_all(format!("{body}\n").as_bytes())
                .await
                .expect("the pipe takes it");
        }

        fn hang_up(self) {
            drop(self);
        }
    }

    #[derive(Default)]
    struct Watcher {
        updates: Arc<Mutex<Vec<SessionNotification>>>,
        asked: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Client for Watcher {
        async fn update(&self, notification: SessionNotification) {
            self.updates.lock().await.push(notification);
        }

        async fn permission(
            &self,
            request: RequestPermissionRequest,
        ) -> Result<RequestPermissionResponse, RpcError> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            let picked = request
                .options
                .first()
                .expect("the agent offers at least one option")
                .option_id
                .clone();
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(picked)),
            ))
        }

        async fn elicitation(
            &self,
            _request: CreateElicitationRequest,
        ) -> Result<CreateElicitationResponse, RpcError> {
            self.asked.fetch_add(1, Ordering::Relaxed);
            Ok(crate::refusal::declined())
        }
    }

    fn pair() -> (Connection, Agent, Arc<Watcher>) {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (our_read, our_write) = tokio::io::split(ours);
        let (their_read, their_write) = tokio::io::split(theirs);
        let watcher = Arc::new(Watcher::default());
        let connection = Connection::spawn(our_read, our_write, watcher.clone());
        let agent = Agent {
            lines: BufReader::new(their_read).lines(),
            writer: their_write,
        };
        (connection, agent, watcher)
    }

    #[tokio::test]
    async fn a_call_is_answered_by_its_id() {
        let (connection, mut agent, _) = pair();
        let asking = tokio::spawn(async move {
            let request: InitializeRequest =
                serde_json::from_value(fixtures::initialize_request()).expect("a request");
            connection.call(request).await
        });
        let sent = agent.read().await;
        assert_eq!(sent["method"], method::INITIALIZE);
        assert_eq!(sent["params"]["protocolVersion"], 1);
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "id": sent["id"],
                "result": fixtures::initialize_response()
            }))
            .await;
        let answered = asking.await.expect("the task finishes").expect("an answer");
        assert!(answered.agent_capabilities.load_session);
    }

    /// Two questions in flight: the ids, not the order, decide which answer
    /// belongs to which.
    #[tokio::test]
    async fn answers_that_arrive_out_of_order_still_find_their_callers() {
        let (connection, mut agent, _) = pair();
        let connection = Arc::new(connection);
        let request = || -> InitializeRequest {
            serde_json::from_value(fixtures::initialize_request()).expect("a request")
        };
        let first = tokio::spawn({
            let connection = connection.clone();
            async move { connection.call(request()).await }
        });
        let one = agent.read().await;
        let second = tokio::spawn({
            let connection = connection.clone();
            async move { connection.call(request()).await }
        });
        let two = agent.read().await;
        assert_ne!(one["id"], two["id"], "each question gets its own id");
        agent
            .write(json!({ "jsonrpc": "2.0", "id": two["id"], "result": fixtures::initialize_response() }))
            .await;
        agent
            .write(json!({ "jsonrpc": "2.0", "id": one["id"], "error": { "code": -32000, "message": "no login" } }))
            .await;
        assert!(matches!(
            second.await.expect("the task finishes"),
            Ok(response) if response.agent_capabilities.load_session
        ));
        assert!(matches!(
            first.await.expect("the task finishes"),
            Err(AcpError::Refused(refusal)) if refusal.message == "no login"
        ));
    }

    #[tokio::test]
    async fn an_update_reaches_the_client_and_an_unknown_one_does_not_end_the_stream() {
        let (connection, mut agent, watcher) = pair();
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "method": method::SESSION_UPDATE,
                "params": fixtures::update_agent_message_chunk()
            }))
            .await;
        // `subagent_spawned` is a draft RFD `codex-acp` ships ahead of the
        // schema. A turn must not end because the agent said something extra.
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "method": method::SESSION_UPDATE,
                "params": fixtures::update_from_a_newer_adapter()
            }))
            .await;
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "method": method::SESSION_UPDATE,
                "params": fixtures::update_agent_thought_chunk()
            }))
            .await;
        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if watcher.updates.lock().await.len() == 2 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            seen.is_ok(),
            "both known updates arrive, the third is dropped"
        );
        let updates = watcher.updates.lock().await;
        assert!(matches!(
            updates[0].update,
            SessionUpdate::AgentMessageChunk(_)
        ));
        assert!(matches!(
            updates[1].update,
            SessionUpdate::AgentThoughtChunk(_)
        ));
        drop(connection);
    }

    /// Both doors an agent may knock on reach the client, and its answer goes
    /// back under the agent's own id. What the answer *is* is
    /// [`crate::refusal`]'s to decide; this is the routing.
    #[tokio::test]
    async fn a_question_the_agent_asks_reaches_the_client_and_is_answered_by_id() {
        let (connection, mut agent, watcher) = pair();
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "id": "perm-1",
                "method": method::SESSION_REQUEST_PERMISSION,
                "params": fixtures::request_permission()
            }))
            .await;
        let reply = agent.read().await;
        assert_eq!(reply["id"], "perm-1", "the agent's own id comes back");
        assert_eq!(reply["result"], fixtures::request_permission_selected());

        agent
            .write(json!({
                "jsonrpc": "2.0",
                "id": "elicit-1",
                "method": method::ELICITATION_CREATE,
                "params": fixtures::elicitation_create()
            }))
            .await;
        let reply = agent.read().await;
        assert_eq!(reply["id"], "elicit-1");
        assert_eq!(reply["result"], fixtures::elicitation_declined());
        assert_eq!(watcher.asked.load(Ordering::Relaxed), 2);
        drop(connection);
    }

    /// ADR-0035 §6: this client has no filesystem and no terminal, and the
    /// agent is told so rather than left waiting.
    #[tokio::test]
    async fn a_method_this_client_does_not_have_is_refused_not_ignored() {
        let (connection, mut agent, _) = pair();
        agent
            .write(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": method::FS_READ_TEXT_FILE,
                "params": { "sessionId": "s", "path": "/etc/passwd" }
            }))
            .await;
        let reply = agent.read().await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["error"]["code"], -32601);
        drop(connection);
    }

    /// A dead adapter must fail the turn, not hold it open for ever.
    #[tokio::test]
    async fn a_call_whose_adapter_died_fails_instead_of_waiting() {
        let (connection, mut agent, _) = pair();
        let asking = tokio::spawn(async move {
            let request: InitializeRequest =
                serde_json::from_value(fixtures::initialize_request()).expect("a request");
            connection.call(request).await
        });
        agent.read().await;
        agent.hang_up();
        assert!(matches!(
            asking.await.expect("the task finishes"),
            Err(AcpError::Transport(_))
        ));
    }

    #[tokio::test]
    async fn a_notification_goes_out_with_no_id() {
        let (connection, mut agent, _) = pair();
        let cancel = serde_json::from_value(fixtures::cancel_notification()).expect("a cancel");
        connection
            .notify::<CancelNotification>(cancel)
            .expect("it goes");
        let sent = agent.read().await;
        assert_eq!(sent["method"], method::SESSION_CANCEL);
        assert!(sent.get("id").is_none(), "a notification has no id");
        drop(connection);
    }

    use agent_client_protocol_schema::v1::CancelNotification;
}
