//! `bingo serve --stdio`: the kernel's `HostApi` with an envelope.
//!
//! One task reads lines and dispatches them; one task writes. Every response
//! and every notification goes through the same channel, so their order on the
//! wire is the order they were produced and needs no other rule. A forwarder is
//! started only after its `session/open` reply is already queued, which is what
//! makes "the snapshot precedes the frames" true by construction (ADR-0007).

use std::collections::HashMap;

use bingo_sdk::{
    Attachment, ClientIdentity, CloseReason, ErrorCode, Exit, FrameStream, HostHandle, KernelError,
    SessionHandle, SessionId,
};
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodecError};

use crate::codec::{
    self, INVALID_PARAMS, INVALID_REQUEST, Id, KERNEL_ERROR, METHOD_NOT_FOUND, Message,
    PARSE_ERROR, Request, Response, RpcError,
};
use crate::methods::{
    AnswerParams, CatalogParams, Empty, EventsParams, HistoryParams, InitializeParams,
    InitializeResult, InterruptParams, ListParams, ListResult, OpenParams, OpenResult,
    SessionParams, SubmitParams, name,
};
use crate::session::{Forwarder, Pump};

/// Enough to absorb a burst of frames without letting a slow reader grow it
/// without bound; a full channel is backpressure on the session's forwarder.
const OUT_CAPACITY: usize = 256;

/// Serve one client until it says `shutdown` or the input ends.
pub async fn serve<R, W>(host: HostHandle, reader: R, writer: W) -> Result<Exit, KernelError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, queue) = mpsc::channel(OUT_CAPACITY);
    let writing = tokio::spawn(write_lines(writer, queue));
    let mut server = Server::new(host, out);
    let read = server.read_lines(reader).await;
    // Drops the forwarders and the last sender, so the writer drains and ends.
    drop(server);
    let written = writing.await.map_err(|error| {
        KernelError::new(
            ErrorCode::Internal,
            format!("the rpc writer failed: {error}"),
        )
    })?;
    let exit = read?;
    written?;
    Ok(exit)
}

async fn write_lines<W>(writer: W, mut queue: Receiver<Message>) -> Result<(), KernelError>
where
    W: AsyncWrite + Unpin + Send,
{
    let mut sink = FramedWrite::new(writer, codec::lines());
    while let Some(message) = queue.recv().await {
        let line = serde_json::to_string(&message).map_err(|error| {
            KernelError::new(
                ErrorCode::Internal,
                format!("unserialisable reply: {error}"),
            )
        })?;
        sink.send(line).await.map_err(broken_pipe)?;
    }
    Ok(())
}

fn broken_pipe(error: LinesCodecError) -> KernelError {
    KernelError::new(
        ErrorCode::Internal,
        format!("the rpc transport failed: {error}"),
    )
}

/// What a method answers, and the subscription to start once it is queued.
struct Reply {
    result: Value,
    then: Option<Start>,
}

impl Reply {
    fn of<T: Serialize>(value: &T) -> Result<Reply, RpcError> {
        Ok(Reply {
            result: encode(value)?,
            then: None,
        })
    }

    fn empty() -> Result<Reply, RpcError> {
        Reply::of(&Empty {})
    }

    fn then(self, start: Start) -> Reply {
        Reply {
            then: Some(start),
            ..self
        }
    }
}

/// A stream that must not produce a notification before the reply is on the wire.
enum Start {
    Session {
        session: SessionId,
        events: FrameStream,
        handle: SessionHandle,
    },
    Gateway(bingo_sdk::GatewayStream),
}

struct Server {
    host: HostHandle,
    out: Sender<Message>,
    /// `Some` once `initialize` succeeded, and the identity every `open` carries:
    /// the handshake and who is asking are one fact.
    client: Option<ClientIdentity>,
    open: HashMap<SessionId, Forwarder>,
    gateway: Option<Pump>,
    /// Set by `shutdown`; the loop stops once the reply is queued.
    exit: Option<Exit>,
}

impl Server {
    fn new(host: HostHandle, out: Sender<Message>) -> Self {
        Self {
            host,
            out,
            client: None,
            open: HashMap::new(),
            gateway: None,
            exit: None,
        }
    }

    async fn read_lines<R>(&mut self, reader: R) -> Result<Exit, KernelError>
    where
        R: AsyncRead + Unpin + Send,
    {
        let mut lines = FramedRead::new(reader, codec::lines());
        while self.exit.is_none() {
            match lines.next().await {
                None => break,
                Some(Ok(line)) => self.line(line).await?,
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    self.fail(None, RpcError::new(PARSE_ERROR, "the line is too long"))
                        .await?;
                }
                Some(Err(error)) => return Err(broken_pipe(error)),
            }
        }
        Ok(self.exit.unwrap_or(Exit { code: 0 }))
    }

    /// A line the client sent: bad JSON is -32700, a shape that is not JSON-RPC
    /// is -32600, and either way the server goes on.
    async fn line(&mut self, line: String) -> Result<(), KernelError> {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            return self
                .fail(None, RpcError::new(PARSE_ERROR, "the line is not json"))
                .await;
        };
        match serde_json::from_value::<Message>(value) {
            Ok(Message::Request(request)) => self.request(request).await,
            // A client sends no notifications and no responses; JSON-RPC says a
            // notification is never answered, so both are dropped.
            Ok(other) => {
                tracing::debug!(?other, "ignoring a message that is not a request");
                Ok(())
            }
            Err(error) => {
                self.fail(None, RpcError::new(INVALID_REQUEST, error.to_string()))
                    .await
            }
        }
    }

    async fn request(&mut self, request: Request) -> Result<(), KernelError> {
        let Request {
            id, method, params, ..
        } = request;
        match self.dispatch(&method, params).await {
            Ok(reply) => {
                self.send(Message::Response(Response::ok(id, reply.result)))
                    .await?;
                if let Some(start) = reply.then {
                    self.start(start);
                }
                Ok(())
            }
            Err(error) => self.fail(Some(id), error).await,
        }
    }

    async fn dispatch(&mut self, method: &str, params: Value) -> Result<Reply, RpcError> {
        if method != name::INITIALIZE && self.client.is_none() {
            return Err(
                KernelError::new(ErrorCode::NotInitialized, "call initialize first").into(),
            );
        }
        match method {
            name::INITIALIZE => self.initialize(params),
            name::SHUTDOWN => self.shutdown(params),
            name::SESSION_LIST => self.list(params).await,
            name::SESSION_OPEN => self.open(params).await,
            name::SESSION_CLOSE => self.close(params).await,
            name::SESSION_DELETE => self.delete(params).await,
            name::SESSION_HISTORY => self.history(params).await,
            name::SESSION_EVENTS => self.events(params).await,
            name::SESSION_SUBMIT => self.submit(params),
            name::SESSION_INTERRUPT => self.interrupt(params),
            name::SESSION_ANSWER => self.answer(params),
            name::CATALOG_READ => self.catalog(params),
            name::GATEWAY_SUBSCRIBE => self.subscribe(params),
            unknown => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("no such method: {unknown}"),
            )),
        }
    }

    fn initialize(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: InitializeParams = parse(params)?;
        if self.client.is_some() {
            return Err(RpcError::new(INVALID_REQUEST, "already initialized"));
        }
        self.client = Some(params.client);
        Reply::of(&InitializeResult::current())
    }

    fn shutdown(&mut self, params: Value) -> Result<Reply, RpcError> {
        let Empty {} = parse(params)?;
        self.exit = Some(Exit { code: 0 });
        Reply::empty()
    }

    async fn list(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: ListParams = parse(params)?;
        let sessions = self.host.sessions(params.filter).await?;
        Reply::of(&ListResult { sessions })
    }

    async fn open(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: OpenParams = parse(params)?;
        let who = self.who()?;
        let Attachment {
            session,
            snapshot,
            events,
            handle,
        } = self.host.open(params.selector, who).await?;
        let reply = Reply::of(&OpenResult {
            session: session.clone(),
            snapshot,
        })?;
        Ok(reply.then(Start::Session {
            session,
            events,
            handle,
        }))
    }

    async fn close(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: SessionParams = parse(params)?;
        self.open.remove(&params.session);
        self.host
            .close(&params.session, CloseReason::Client)
            .await?;
        Reply::empty()
    }

    async fn delete(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: SessionParams = parse(params)?;
        self.open.remove(&params.session);
        self.host.delete(&params.session).await?;
        Reply::empty()
    }

    async fn history(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: HistoryParams = parse(params)?;
        let chunk = self.port(&params.session)?.history(params.page).await?;
        Reply::of(&chunk)
    }

    /// Resync: the frames after `since`, then live, on a forwarder that replaces
    /// the one this session already had.
    async fn events(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: EventsParams = parse(params)?;
        let handle = self.port(&params.session)?;
        let events = handle.events_since(params.since).await?;
        Ok(Reply::empty()?.then(Start::Session {
            session: params.session,
            events,
            handle,
        }))
    }

    fn submit(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: SubmitParams = parse(params)?;
        self.port(&params.session)?
            .submit(params.intent, params.input);
        Reply::empty()
    }

    fn interrupt(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: InterruptParams = parse(params)?;
        self.port(&params.session)?
            .interrupt(params.intent, params.scope);
        Reply::empty()
    }

    fn answer(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: AnswerParams = parse(params)?;
        self.port(&params.session)?.answer(
            params.intent,
            params.interaction,
            params.answer,
            params.activation,
        );
        Reply::empty()
    }

    fn catalog(&mut self, params: Value) -> Result<Reply, RpcError> {
        let params: CatalogParams = parse(params)?;
        Reply::of(&self.host.catalog(params.kind))
    }

    fn subscribe(&mut self, params: Value) -> Result<Reply, RpcError> {
        let Empty {} = parse(params)?;
        if self.gateway.is_some() {
            return Reply::empty();
        }
        let events = self.host.gateway_events();
        Ok(Reply::empty()?.then(Start::Gateway(events)))
    }

    /// After the reply is queued, never before.
    fn start(&mut self, start: Start) {
        match start {
            Start::Session {
                session,
                events,
                handle,
            } => {
                let pump = Pump::spawn(name::EVENT, events, self.out.clone());
                // Replacing drops the old forwarder, which stops its task.
                self.open.insert(session, Forwarder::new(handle, pump));
            }
            Start::Gateway(events) => {
                self.gateway = Some(Pump::spawn(name::GATEWAY_EVENT, events, self.out.clone()));
            }
        }
    }

    /// A write reaches an actor only through a session this client has open.
    fn port(&self, session: &SessionId) -> Result<SessionHandle, RpcError> {
        self.open
            .get(session)
            .map(|forwarder| forwarder.handle.clone())
            .ok_or_else(|| {
                KernelError::new(
                    ErrorCode::SessionNotFound,
                    format!("session {session} is not open on this connection"),
                )
                .into()
            })
    }

    fn who(&self) -> Result<ClientIdentity, RpcError> {
        self.client.clone().ok_or_else(|| {
            KernelError::new(ErrorCode::NotInitialized, "call initialize first").into()
        })
    }

    async fn send(&self, message: Message) -> Result<(), KernelError> {
        self.out
            .send(message)
            .await
            .map_err(|_| KernelError::new(ErrorCode::Internal, "the rpc writer stopped"))
    }

    async fn fail(&self, id: Option<Id>, error: RpcError) -> Result<(), KernelError> {
        self.send(Message::Response(Response::failed(id, error)))
            .await
    }
}

/// Absent params are an empty object, so a method whose params are all optional
/// can be called without them.
fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    let params = if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    };
    serde_json::from_value(params).map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))
}

fn encode<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|error| RpcError::new(KERNEL_ERROR, format!("unserialisable result: {error}")))
}
