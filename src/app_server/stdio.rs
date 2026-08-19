//! The stdio transport: JSON-RPC 2.0, one message per line.
//!
//! Stdout carries protocol frames and nothing else; every diagnostic goes to
//! stderr. A malformed but bounded line is answered with the standard parse
//! error and mutates nothing. A line past the client ceiling, bytes that are not
//! UTF-8, or a stdout that stops taking frames cannot be framed past, so they
//! close the transport — and closing runs the same shutdown the client's own
//! `shutdown` runs: active work interrupted, open prompts failed closed,
//! everything persisted through the core's own path.
//!
//! Three tasks and one loop. A reader task turns bytes into lines, a writer task
//! turns frames into bytes, and the loop between them owns the connection: which
//! protocol was negotiated, which session is open, and which request is waiting
//! for which reply. The loop reads the core's frames before it reads the
//! client's, so a reply is always written before the events it caused.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::app::command::{AppCommand, AppQuery};
use crate::app::ids::{EpochId, SessionId};
use crate::app::snapshot::{SessionCloseReason, SessionSnapshot};
use crate::app::{AppCore, AppError, AppFrame, AppLink, AppReply, AppRequest, AttachRequest};
use crate::app_server::AppServerError;
use crate::app_server::protocol::envelope::{
    MAX_CLIENT_FRAME_BYTES, MAX_SERVER_FRAME_BYTES, NotificationFrame, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, RequestId, ResponseFrame,
};
use crate::app_server::protocol::error::{
    INTERNAL_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, ProtocolErrorKind, RpcError,
};
use crate::app_server::protocol::notifications::ServerNotification;
use crate::app_server::protocol::requests::{
    ActionExecuteResult, ActionListResult, AssetReadChunkResult, AssetRegisterPathResult,
    CatalogReadResult, ClientNotification, ClientRequest, ConfigReadResult, ConversationListResult,
    ConversationMarkReadResult, ConversationReadResult, ConversationSubmitResult, FrameLimits,
    InitializeParams, InitializeResult, InteractionRespondResult, ProtocolVersion, QueueReadResult,
    QueueReclaimTailResult, RequestMethod, ResourceReadResult, RespondStatus, ResponseResult,
    ServerInfo, SessionCloseResult, SessionDeleteResult, SessionListResult, SessionReadResult,
    SessionResumeResult, SessionStartResult, ShutdownResult, TurnInterruptResult,
};
use crate::app_server::session::{self, Bootstrap, Refusal, Started, Wanted};

/// How many frames may wait for a client that is not reading. Bounded, as the
/// spec requires of both directions; the core's frame channel is bounded behind
/// it.
const OUTBOUND_CAPACITY: usize = 1024;
/// How many lines may wait to be served. This is *the* inbound bound: the core
/// takes requests on an unbounded queue (B7b ruling ②), because half its
/// producers are in-process and cannot wait. A client on a socket is not one of
/// those, so the limit the spec asks for lives here, at the edge that has one.
const INBOUND_CAPACITY: usize = 64;
/// How many frames one write may carry. Batching is what keeps a token-sized
/// delta from costing a syscall of its own.
const BATCH_LIMIT: usize = 256;
/// How long one write may take before the transport is declared unusable.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the best-effort `CLIENT_TOO_SLOW` notice may take. Short on purpose:
/// the client has already stopped reading, and this is a courtesy, not a promise.
const LAST_WORD_TIMEOUT: Duration = Duration::from_secs(1);
/// The most text one coalesced frame may carry, well under the server ceiling.
const COALESCE_LIMIT: usize = 256 * 1024;
/// How long the connection waits, on its way out, for the answers to requests
/// the core already accepted.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

/// The experimental methods a client may opt into. Empty while nothing is
/// experimental — an opt-in this build does not know is refused rather than
/// ignored, because a client that asked for it will rely on it.
const EXPERIMENTAL: &[&str] = &[];

/// Serve one client on this process's stdin and stdout.
pub async fn serve() -> Result<(), AppServerError> {
    let boot = Bootstrap::resolve()?;
    run(tokio::io::stdin(), tokio::io::stdout(), boot).await
}

/// The transport, over any pair of streams. Real stdio in production, a pipe in
/// the tests, which is what lets the contract be tested at the frame level.
pub(crate) async fn run<R, W>(input: R, output: W, boot: Bootstrap) -> Result<(), AppServerError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (lines, mut inbound) = mpsc::channel(INBOUND_CAPACITY);
    let (frames, outbound) = mpsc::channel(OUTBOUND_CAPACITY);
    let (broken, mut faults) = mpsc::channel(1);
    let reader = tokio::spawn(read_lines(input, lines));
    let writer = tokio::spawn(write_frames(output, outbound, broken));
    let mut connection = Connection::new(boot, frames);
    let outcome = connection.pump(&mut inbound, &mut faults).await;
    // Whatever ended it, the session ends the way a session ends: interrupted,
    // failed closed, persisted. A transport that broke gets no farewell frames —
    // there is nobody reading them.
    connection
        .close_session(SessionCloseReason::Disconnected, outcome.is_ok())
        .await;
    drop(connection);
    reader.abort();
    let _ = writer.await;
    outcome
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// One line, or the reason there will not be another.
enum Inbound {
    Line(String),
    Fault(AppServerError),
}

/// What one fill of the read buffer produced.
enum Step {
    Eof,
    /// A newline was found; this many bytes are consumed.
    Line(usize),
    /// No newline yet; this many bytes are consumed.
    More(usize),
    Failed(String),
}

/// Turn bytes into lines, refusing anything that cannot be framed.
async fn read_lines<R: AsyncRead + Unpin>(input: R, out: mpsc::Sender<Inbound>) {
    let mut reader = BufReader::new(input);
    let mut line: Vec<u8> = Vec::new();
    loop {
        let step = {
            match reader.fill_buf().await {
                Err(error) => Step::Failed(error.to_string()),
                Ok([]) => Step::Eof,
                Ok(chunk) => match chunk.iter().position(|byte| *byte == b'\n') {
                    Some(at) => {
                        line.extend_from_slice(&chunk[..at]);
                        Step::Line(at + 1)
                    }
                    None => {
                        line.extend_from_slice(chunk);
                        Step::More(chunk.len())
                    }
                },
            }
        };
        match step {
            Step::Failed(detail) => {
                let _ = out
                    .send(Inbound::Fault(AppServerError::Framing { detail }))
                    .await;
                return;
            }
            // A last line with no newline is still a frame; after it, the stream
            // is over and dropping the sender is what says so.
            Step::Eof => {
                if !line.is_empty() {
                    let _ = deliver(&out, &mut line).await;
                }
                return;
            }
            Step::Line(taken) => {
                reader.consume(taken);
                if oversized(&line, &out).await || !deliver(&out, &mut line).await {
                    return;
                }
            }
            Step::More(taken) => {
                reader.consume(taken);
                if oversized(&line, &out).await {
                    return;
                }
            }
        }
    }
}

/// Whether the line already ran past the ceiling. Checked as it grows, so an
/// endless line is refused rather than buffered.
async fn oversized(line: &[u8], out: &mpsc::Sender<Inbound>) -> bool {
    if line.len() <= MAX_CLIENT_FRAME_BYTES {
        return false;
    }
    let _ = out
        .send(Inbound::Fault(AppServerError::FrameTooLarge {
            limit: MAX_CLIENT_FRAME_BYTES,
        }))
        .await;
    true
}

/// Hand one complete line on, or refuse the stream it came from.
async fn deliver(out: &mpsc::Sender<Inbound>, line: &mut Vec<u8>) -> bool {
    let bytes = std::mem::take(line);
    let Ok(text) = String::from_utf8(bytes) else {
        let _ = out
            .send(Inbound::Fault(AppServerError::Framing {
                detail: "the client stream is not UTF-8".to_string(),
            }))
            .await;
        return false;
    };
    let text = text.trim_end_matches('\r');
    // A blank line is whitespace between frames, not a frame.
    if text.trim().is_empty() {
        return true;
    }
    out.send(Inbound::Line(text.to_string())).await.is_ok()
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// One frame on its way out. Boxed because a response carries a whole snapshot
/// and a notification carries a whole item.
#[derive(Debug, Clone, PartialEq)]
enum Wire {
    Response(Box<ResponseFrame>),
    Notification(Box<ServerNotification>),
}

/// Serialize frames and write them, one batch per flush.
async fn write_frames<W: AsyncWrite + Unpin>(
    mut output: W,
    mut queue: mpsc::Receiver<Wire>,
    broken: mpsc::Sender<AppServerError>,
) {
    loop {
        let Some(first) = queue.recv().await else {
            let _ = output.flush().await;
            return;
        };
        let mut batch = vec![first];
        while batch.len() < BATCH_LIMIT {
            match queue.try_recv() {
                Ok(frame) => batch.push(frame),
                Err(_) => break,
            }
        }
        coalesce(&mut batch);
        let mut buffer = Vec::new();
        for frame in &batch {
            match encode(frame) {
                Ok(line) => {
                    buffer.extend_from_slice(line.as_bytes());
                    buffer.push(b'\n');
                }
                Err(fault) => {
                    let _ = broken.send(fault).await;
                    return;
                }
            }
        }
        match tokio::time::timeout(WRITE_TIMEOUT, write_all(&mut output, &buffer)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = broken
                    .send(AppServerError::Framing {
                        detail: error.to_string(),
                    })
                    .await;
                return;
            }
            Err(_) => {
                // Bounded backpressure and the write timeout both ran out. The
                // notice is best-effort: the client that could not read the
                // frames probably cannot read this one either.
                let notice = ResponseFrame::error(
                    RequestId::Null,
                    RpcError::application(ProtocolErrorKind::ClientTooSlow),
                );
                if let Ok(line) = serde_json::to_string(&notice) {
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\n');
                    let _ = tokio::time::timeout(LAST_WORD_TIMEOUT, write_all(&mut output, &bytes))
                        .await;
                }
                let _ = broken.send(AppServerError::ClientTooSlow).await;
                return;
            }
        }
    }
}

async fn write_all<W: AsyncWrite + Unpin>(output: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    output.write_all(bytes).await?;
    output.flush().await
}

/// One frame as its line, or the reason it cannot be one.
///
/// A response too large for the ceiling becomes the error that says so, which is
/// still an answer to the request that asked. A notification too large has no
/// such substitute — dropping a lifecycle event is exactly what the protocol
/// forbids — so it closes the transport instead.
fn encode(frame: &Wire) -> Result<String, AppServerError> {
    let line = match frame {
        Wire::Response(response) => serde_json::to_string(response.as_ref()),
        Wire::Notification(notification) => {
            serde_json::to_string(&NotificationFrame::new(notification.as_ref().clone()))
        }
    }
    .map_err(|error| AppServerError::Framing {
        detail: error.to_string(),
    })?;
    if line.len() < MAX_SERVER_FRAME_BYTES {
        return Ok(line);
    }
    match frame {
        Wire::Response(response) => {
            let refusal = ResponseFrame::error(
                response.id.clone(),
                RpcError::application(ProtocolErrorKind::FrameTooLarge),
            );
            serde_json::to_string(&refusal).map_err(|error| AppServerError::Framing {
                detail: error.to_string(),
            })
        }
        Wire::Notification(notification) => Err(AppServerError::Framing {
            detail: format!("{} exceeds the server frame ceiling", notification.method()),
        }),
    }
}

/// Merge adjacent append deltas for one item.
///
/// Only text and reasoning deltas, only when they are adjacent in the queue —
/// which is what makes them adjacent in the stream — and only for the same item.
/// Nothing is dropped: the merged frame carries the run's last sequence number,
/// says which sequence the run began at, and holds the whole of its text. A
/// lifecycle, interaction, replacement, or terminal event is never a candidate.
fn coalesce(frames: &mut Vec<Wire>) {
    let mut merged: Vec<Wire> = Vec::with_capacity(frames.len());
    for frame in frames.drain(..) {
        if let (Some(Wire::Notification(previous)), Wire::Notification(current)) =
            (merged.last_mut(), &frame)
            && merge(previous, current)
        {
            continue;
        }
        merged.push(frame);
    }
    *frames = merged;
}

/// Fold `current` into `previous` when they are one item's consecutive appends.
fn merge(previous: &mut ServerNotification, current: &ServerNotification) -> bool {
    use ServerNotification::{ItemReasoningDelta, ItemTextDelta};
    let (before, after) = match (&mut *previous, current) {
        (ItemTextDelta(before), ItemTextDelta(after))
        | (ItemReasoningDelta(before), ItemReasoningDelta(after)) => (before, after),
        _ => return false,
    };
    if before.body.conversation_id != after.body.conversation_id
        || before.body.turn_id != after.body.turn_id
        || before.body.item_id != after.body.item_id
        || before.event.session_id != after.event.session_id
        || before.body.delta.len() + after.body.delta.len() > COALESCE_LIMIT
    {
        return false;
    }
    let started_at = before.event.coalesced_from.unwrap_or(before.event.seq);
    before.body.delta.push_str(&after.body.delta);
    before.body.delta_seq = after.body.delta_seq;
    before.event = after.event.clone();
    before.event.coalesced_from = Some(started_at);
    true
}

// ---------------------------------------------------------------------------
// The connection
// ---------------------------------------------------------------------------

/// How far the handshake has come. A session notification before `initialized`
/// is impossible by construction: nothing but `initialize` is served until the
/// client says it is ready, so there is no session to publish about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    New,
    Negotiated,
    Ready,
}

/// The core this connection is talking to, and what it is.
struct Live {
    core: AppCore,
    link: AppLink,
    session_id: Option<SessionId>,
    /// The transcript this session writes. Held for as long as the session is
    /// open, because holding it is what holds its lock: a second process may not
    /// write the session this one is reading.
    transcript: Option<crate::transcript::Transcript>,
    /// A core with no session of its own, kept so the catalogs can answer before
    /// a client has chosen one (Amendment #7). It is never snapshot-read, so it
    /// never publishes an event.
    lobby: bool,
}

/// A request the core is still answering, and the wire call waiting for it.
struct Waiting {
    id: RequestId,
    method: RequestMethod,
}

/// What the loop should do next.
enum Flow {
    Continue,
    Stop,
}

/// What one turn of the loop was woken by.
enum Woken {
    Broken(AppServerError),
    Frame(Option<AppFrame>),
    Client(Option<Inbound>),
}

struct Connection {
    boot: Bootstrap,
    /// This process's run. Every resource identifier belongs to an epoch and
    /// dies with it; a session start or resume mints its own and says so in the
    /// snapshot it answers with.
    epoch: EpochId,
    out: mpsc::Sender<Wire>,
    phase: Phase,
    session: Option<Live>,
    next_call: u64,
    waiting: HashMap<crate::app::RequestId, Waiting>,
    in_flight: HashSet<RequestId>,
    /// The open session answered `session/close` and is on its way out. Its link
    /// is drained where the loop can see it, rather than inside the frame
    /// handler that noticed.
    retiring: bool,
}

impl Connection {
    fn new(boot: Bootstrap, out: mpsc::Sender<Wire>) -> Self {
        Self {
            boot,
            epoch: EpochId::mint(),
            out,
            phase: Phase::New,
            session: None,
            next_call: 0,
            waiting: HashMap::new(),
            in_flight: HashSet::new(),
            retiring: false,
        }
    }

    /// The loop. The core's frames are read before the client's lines, so a
    /// reply is written before the events it caused and a flood of events cannot
    /// be overtaken by the next request.
    async fn pump(
        &mut self,
        inbound: &mut mpsc::Receiver<Inbound>,
        faults: &mut mpsc::Receiver<AppServerError>,
    ) -> Result<(), AppServerError> {
        loop {
            let woken = tokio::select! {
                biased;
                fault = faults.recv() => match fault {
                    Some(fault) => Woken::Broken(fault),
                    None => Woken::Broken(AppServerError::Framing {
                        detail: "the writer stopped".to_string(),
                    }),
                },
                frame = next_frame(&mut self.session) => Woken::Frame(frame),
                line = inbound.recv() => Woken::Client(line),
            };
            let flow = match woken {
                Woken::Broken(fault) => return Err(fault),
                Woken::Frame(Some(frame)) => self.on_frame(frame).await?,
                // The actor ended without being asked to. Whatever it published
                // on the way out has already been written; what is left is a
                // connection with no session.
                Woken::Frame(None) => {
                    self.session = None;
                    self.abandon().await?;
                    self.open_lobby().await;
                    Flow::Continue
                }
                Woken::Client(Some(Inbound::Line(line))) => self.on_line(line).await?,
                Woken::Client(Some(Inbound::Fault(fault))) => return Err(fault),
                // EOF: the shutdown policy runs on the way out.
                Woken::Client(None) => Flow::Stop,
            };
            if self.retiring {
                self.retiring = false;
                self.retire(true).await?;
                self.open_lobby().await;
            }
            if matches!(flow, Flow::Stop) {
                return self.settle().await;
            }
        }
    }

    /// Answer what was already accepted, before the connection ends.
    ///
    /// A request the core took is a request the core answers: stopping in the
    /// middle of one would leave a client waiting for a reply that exists. It is
    /// bounded, because a core that has stopped answering must not hold the exit
    /// open.
    async fn settle(&mut self) -> Result<(), AppServerError> {
        while !self.waiting.is_empty() {
            match tokio::time::timeout(SETTLE_TIMEOUT, next_frame(&mut self.session)).await {
                Ok(Some(frame)) => {
                    self.on_frame(frame).await?;
                }
                Ok(None) | Err(_) => break,
            }
        }
        Ok(())
    }

    // -- frames from the core ------------------------------------------------

    async fn on_frame(&mut self, frame: AppFrame) -> Result<Flow, AppServerError> {
        match frame {
            AppFrame::Reply { id, result } => {
                let Some(waiting) = self.waiting.remove(&id) else {
                    return Ok(Flow::Continue);
                };
                self.in_flight.remove(&waiting.id);
                let response = match result {
                    Ok(reply) => match self.as_result(waiting.method, reply) {
                        Some(result) => ResponseFrame::result(waiting.id, result),
                        None => ResponseFrame::error(
                            waiting.id,
                            RpcError::standard(
                                INTERNAL_ERROR,
                                "The core answered with something this method cannot return.",
                            ),
                        ),
                    },
                    Err(error) => ResponseFrame::error(waiting.id, self.as_rpc_error(&error)),
                };
                self.emit(Wire::Response(Box::new(response))).await?;
                // A closed session is no longer this connection's, but its link
                // still has the events the closing published on it. Those are
                // terminal events, and terminal events are exactly the ones that
                // may not be dropped: the link drains, and the lobby takes over
                // when it ends.
                if waiting.method == RequestMethod::SessionClose {
                    self.retiring = true;
                }
                Ok(Flow::Continue)
            }
            AppFrame::Event(event) => {
                if self.phase != Phase::Ready {
                    return Ok(Flow::Continue);
                }
                self.emit(Wire::Notification(Box::new(ServerNotification::from(
                    *event,
                ))))
                .await?;
                Ok(Flow::Continue)
            }
        }
    }

    // -- lines from the client ------------------------------------------------

    async fn on_line(&mut self, line: String) -> Result<Flow, AppServerError> {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            return self
                .fail(
                    RequestId::Null,
                    RpcError::standard(PARSE_ERROR, "Invalid JSON was received."),
                )
                .await;
        };
        let Some(object) = value.as_object() else {
            return self
                .fail(
                    RequestId::Null,
                    RpcError::standard(INVALID_REQUEST, "A frame is a JSON-RPC object."),
                )
                .await;
        };
        let id = match object.get("id") {
            Some(raw) => match serde_json::from_value::<RequestId>(raw.clone()) {
                Ok(id) => Some(id),
                Err(_) => {
                    return self
                        .fail(
                            RequestId::Null,
                            RpcError::standard(
                                INVALID_REQUEST,
                                "A request id is a number, a string, or null.",
                            ),
                        )
                        .await;
                }
            },
            None => None,
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return match id {
                Some(id) => {
                    self.fail(
                        id,
                        RpcError::standard(INVALID_REQUEST, "Every frame declares jsonrpc 2.0."),
                    )
                    .await
                }
                None => Ok(Flow::Continue),
            };
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return match id {
                Some(id) => {
                    self.fail(
                        id,
                        RpcError::standard(INVALID_REQUEST, "A frame names its method."),
                    )
                    .await
                }
                None => Ok(Flow::Continue),
            };
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = id else {
            // A notification is answered with nothing at all, including when it
            // is one this build does not know.
            if method == "initialized"
                && serde_json::from_value::<ClientNotification>(
                    json!({"method": "initialized", "params": params}),
                )
                .is_ok()
                && self.phase == Phase::Negotiated
            {
                self.phase = Phase::Ready;
            }
            return Ok(Flow::Continue);
        };
        let Some(known) = RequestMethod::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == method)
        else {
            return self
                .fail(
                    id,
                    RpcError::standard(METHOD_NOT_FOUND, "This build has no such method."),
                )
                .await;
        };
        if !self.in_flight.insert(id.clone()) {
            return self
                .fail(
                    id,
                    RpcError::standard(INVALID_REQUEST, "That request id is already in flight."),
                )
                .await;
        }
        let call = match serde_json::from_value::<ClientRequest>(
            json!({"method": method, "params": params}),
        ) {
            Ok(call) => call,
            Err(_) => return self.refused(id, ProtocolErrorKind::BadArgument).await,
        };
        self.dispatch(id, known, call).await
    }

    async fn dispatch(
        &mut self,
        id: RequestId,
        method: RequestMethod,
        call: ClientRequest,
    ) -> Result<Flow, AppServerError> {
        if method == RequestMethod::Initialize {
            let ClientRequest::Initialize(params) = call else {
                return self.refused(id, ProtocolErrorKind::BadArgument).await;
            };
            return self.initialize(id, params).await;
        }
        if self.phase != Phase::Ready {
            return self.refused(id, ProtocolErrorKind::NotInitialized).await;
        }
        match call {
            ClientRequest::Shutdown(_) => self.shutdown(id).await,
            ClientRequest::SessionStart(params) => {
                let wanted = Wanted {
                    cwd: params.cwd,
                    provider: params.provider,
                    model: params.model,
                    thinking: params.thinking,
                    permission_mode: params.permission_mode,
                };
                let started = session::start(&self.boot, &wanted);
                self.open(id, method, started).await
            }
            ClientRequest::SessionResume(params) => {
                let started = session::resume(&self.boot, &params.locator, &Wanted::default());
                self.open(id, method, started).await
            }
            other => self.forward(id, method, other).await,
        }
    }

    /// `initialize`: agree on a major, a minor, and what each side can do.
    async fn initialize(
        &mut self,
        id: RequestId,
        params: InitializeParams,
    ) -> Result<Flow, AppServerError> {
        if self.phase != Phase::New {
            return self
                .refused(id, ProtocolErrorKind::AlreadyInitialized)
                .await;
        }
        if params.protocol.min_minor > params.protocol.max_minor {
            return self.refused(id, ProtocolErrorKind::BadArgument).await;
        }
        // A major is spoken or it is not; within it, the highest minor both
        // sides know is the one that holds for the connection.
        if params.protocol.major != PROTOCOL_MAJOR || params.protocol.min_minor > PROTOCOL_MINOR {
            return self
                .refused_fatally(id, ProtocolErrorKind::ProtocolUnsupported)
                .await;
        }
        // A controlling client must be able to answer a prompt. Failing here is
        // the alternative to silently auto-denying one that has not happened yet.
        if !params.capabilities.interaction_response {
            return self
                .refused_fatally(id, ProtocolErrorKind::CapabilityRequired)
                .await;
        }
        if params
            .capabilities
            .experimental
            .iter()
            .any(|name| !EXPERIMENTAL.contains(&name.as_str()))
        {
            return self
                .refused_fatally(id, ProtocolErrorKind::CapabilityRequired)
                .await;
        }
        let result = InitializeResult {
            protocol: ProtocolVersion {
                major: PROTOCOL_MAJOR,
                // The minor both sides speak. This build knows exactly one, and
                // the window above was checked to contain it; when the build
                // knows more than one, this becomes the highest of them the
                // window still holds.
                minor: PROTOCOL_MINOR,
            },
            server: ServerInfo {
                name: "bingo".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                epoch: self.epoch.clone(),
            },
            limits: FrameLimits {
                max_client_frame_bytes: MAX_CLIENT_FRAME_BYTES as u64,
                max_server_frame_bytes: MAX_SERVER_FRAME_BYTES as u64,
            },
            capabilities: crate::app::SessionSetup::default().capabilities,
        };
        self.phase = Phase::Negotiated;
        self.in_flight.remove(&id);
        self.emit(Wire::Response(Box::new(ResponseFrame::result(
            id,
            ResponseResult::Initialize(result),
        ))))
        .await?;
        // The catalogs answer before a session exists, which is the job
        // `--inspect` used to have (Amendment #7).
        self.open_lobby().await;
        Ok(Flow::Continue)
    }

    /// `shutdown`: answer first, then close the session the way EOF closes it.
    async fn shutdown(&mut self, id: RequestId) -> Result<Flow, AppServerError> {
        // Everything the core already accepted is answered before the shutdown
        // result says what it stopped.
        self.settle().await?;
        let (interrupted_turns, denied_interactions) = self.open_work();
        self.in_flight.remove(&id);
        self.emit(Wire::Response(Box::new(ResponseFrame::result(
            id,
            ResponseResult::Shutdown(ShutdownResult {
                interrupted_turns,
                denied_interactions,
            }),
        ))))
        .await?;
        Ok(Flow::Stop)
    }

    /// `session/start` and `session/resume`: replace the actor.
    ///
    /// One `AppCore` is one session, so choosing another is not a question for
    /// the one that is running. The session that was open closes first — every
    /// identifier it minted dies with its epoch — and the snapshot the reply
    /// carries is the new session's first cut, which is where its notification
    /// stream begins.
    async fn open(
        &mut self,
        id: RequestId,
        method: RequestMethod,
        started: Result<Started, Refusal>,
    ) -> Result<Flow, AppServerError> {
        let started = match started {
            Ok(started) => started,
            Err(refusal) => {
                self.in_flight.remove(&id);
                let mut error = RpcError::application(refusal.kind);
                error.message = refusal.detail;
                return self.fail(id, error).await;
            }
        };
        // Everything the session that is about to be replaced already accepted
        // is answered first: replacing the actor under a request in flight would
        // strand it.
        self.settle().await?;
        self.close_session(SessionCloseReason::Replaced, true).await;
        let (mut live, snapshot) = match attach(started.core).await {
            Ok(attached) => attached,
            Err(error) => {
                self.in_flight.remove(&id);
                let error = self.as_rpc_error(&error);
                return self.fail(id, error).await;
            }
        };
        live.transcript = started.transcript;
        self.epoch = snapshot.session.epoch.clone();
        self.session = Some(live);
        self.in_flight.remove(&id);
        let result = match method {
            RequestMethod::SessionResume => {
                ResponseResult::SessionResume(SessionResumeResult { snapshot })
            }
            _ => ResponseResult::SessionStart(SessionStartResult { snapshot }),
        };
        self.emit(Wire::Response(Box::new(ResponseFrame::result(id, result))))
            .await?;
        Ok(Flow::Continue)
    }

    /// Everything the core answers. The reply comes back on the same channel the
    /// events do, which is what makes "response before its caused events" a fact
    /// of the ordering rather than a rule to remember.
    async fn forward(
        &mut self,
        id: RequestId,
        method: RequestMethod,
        call: ClientRequest,
    ) -> Result<Flow, AppServerError> {
        if needs_session(method) && !self.has_session() {
            return self.refused(id, ProtocolErrorKind::NoActiveSession).await;
        }
        let Some(request) = as_core_request(call) else {
            return self.refused(id, ProtocolErrorKind::BadArgument).await;
        };
        let Some(live) = &self.session else {
            return self.refused(id, ProtocolErrorKind::NoActiveSession).await;
        };
        let call_id = crate::app::RequestId(self.next_call);
        self.next_call = self.next_call.saturating_add(1);
        let sent = match request {
            Core::Command(command) => live.link.request(AppRequest::Command {
                id: call_id,
                command,
            }),
            Core::Query(query) => live.link.request(AppRequest::Query { id: call_id, query }),
        };
        if let Err(error) = sent {
            let error = self.as_rpc_error(&error);
            return self.fail(id, error).await;
        }
        self.waiting.insert(call_id, Waiting { id, method });
        Ok(Flow::Continue)
    }

    // -- sessions -------------------------------------------------------------

    fn has_session(&self) -> bool {
        self.session.as_ref().is_some_and(|live| !live.lobby)
    }

    /// How much was open when the shutdown started: what it interrupts, and what
    /// it fails closed.
    fn open_work(&self) -> (u32, u32) {
        let Some(live) = self.session.as_ref().filter(|live| !live.lobby) else {
            return (0, 0);
        };
        (
            live.core.turns().view().active_count() as u32,
            live.core.interactions().view().len() as u32,
        )
    }

    /// Close whatever session is open, through the core's own path: active work
    /// interrupted, open prompts failed closed, everything persisted.
    async fn close_session(&mut self, reason: SessionCloseReason, announce: bool) {
        let Some(core) = self.session.as_ref().map(|live| live.core.clone()) else {
            return;
        };
        core.close_with(reason).await;
        let _ = self.retire(announce).await;
    }

    /// Drain a session's link to its end and let go of it.
    ///
    /// What the closing published is still the client's to see, and terminal
    /// events are exactly the ones that may not be dropped — so the link is read
    /// out rather than dropped. A transport that is already broken is told not to
    /// try: there is nobody on the other end of it.
    async fn retire(&mut self, announce: bool) -> Result<(), AppServerError> {
        let Some(quiet) = self
            .session
            .as_ref()
            .map(|live| live.lobby || !announce || self.phase != Phase::Ready)
        else {
            return Ok(());
        };
        loop {
            match tokio::time::timeout(SETTLE_TIMEOUT, next_frame(&mut self.session)).await {
                Ok(Some(AppFrame::Event(_))) if quiet => continue,
                Ok(Some(frame)) => {
                    self.on_frame(frame).await?;
                }
                Ok(None) | Err(_) => break,
            }
        }
        self.session = None;
        self.retiring = false;
        self.abandon().await
    }

    /// Answer what the session that just ended will never answer now. A request
    /// the core accepted leaves with a reply either way.
    async fn abandon(&mut self) -> Result<(), AppServerError> {
        let mut abandoned: Vec<(crate::app::RequestId, Waiting)> = self.waiting.drain().collect();
        abandoned.sort_by_key(|(id, _)| id.0);
        for (_, waiting) in abandoned {
            self.in_flight.remove(&waiting.id);
            // A session-scoped call says so in its own vocabulary. One that
            // never needed a session cannot: `NO_ACTIVE_SESSION` is not among
            // the errors it declares, so it takes the standard one instead.
            let error = if needs_session(waiting.method) {
                self.application_error(ProtocolErrorKind::NoActiveSession)
            } else {
                RpcError::standard(
                    INTERNAL_ERROR,
                    "The session ended before this request was answered.",
                )
            };
            self.emit(Wire::Response(Box::new(ResponseFrame::error(
                waiting.id, error,
            ))))
            .await?;
        }
        Ok(())
    }

    /// A core with no session, so the catalogs keep answering between sessions.
    async fn open_lobby(&mut self) {
        if self.session.is_some() {
            return;
        }
        let settings =
            crate::settings::load_settings(&self.boot.user_dir, &self.boot.cwd).unwrap_or_default();
        let core = AppCore::start(crate::app::SessionSetup {
            cwd: self.boot.cwd.clone(),
            catalog: crate::app::catalog::CatalogSource::load(
                &self.boot.home,
                &self.boot.user_dir,
                &self.boot.cwd,
                settings,
            ),
            ..Default::default()
        });
        match core.attach(AttachRequest::new("app-server")) {
            Ok(link) => {
                self.session = Some(Live {
                    core,
                    link,
                    session_id: None,
                    transcript: None,
                    lobby: true,
                });
            }
            Err(error) => eprintln!("[bingo] warning: the catalogs are unavailable: {error}"),
        }
    }

    // -- writing --------------------------------------------------------------

    /// Put one frame on the outbound queue, bounded and with a deadline. A queue
    /// that stays full for the whole of it means the client stopped reading, and
    /// the writer says so on its own way out.
    async fn emit(&mut self, frame: Wire) -> Result<(), AppServerError> {
        match tokio::time::timeout(WRITE_TIMEOUT, self.out.send(frame)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(AppServerError::Framing {
                detail: "the writer stopped".to_string(),
            }),
            Err(_) => Err(AppServerError::ClientTooSlow),
        }
    }

    async fn fail(&mut self, id: RequestId, error: RpcError) -> Result<Flow, AppServerError> {
        self.in_flight.remove(&id);
        self.emit(Wire::Response(Box::new(ResponseFrame::error(id, error))))
            .await?;
        Ok(Flow::Continue)
    }

    async fn refused(
        &mut self,
        id: RequestId,
        kind: ProtocolErrorKind,
    ) -> Result<Flow, AppServerError> {
        let error = self.application_error(kind);
        self.fail(id, error).await
    }

    /// Say no, and end the connection. Only initialization does this: the two
    /// failures it can have are the ones the contract declares non-recoverable,
    /// and a client cannot usefully retry either on the same connection.
    async fn refused_fatally(
        &mut self,
        id: RequestId,
        kind: ProtocolErrorKind,
    ) -> Result<Flow, AppServerError> {
        self.refused(id, kind).await?;
        Err(AppServerError::Initialization { kind })
    }

    /// A declared error, told which session it happened in.
    fn application_error(&self, kind: ProtocolErrorKind) -> RpcError {
        let mut error = RpcError::application(kind);
        if let (Some(data), Some(session)) = (
            error.data.as_mut(),
            self.session
                .as_ref()
                .and_then(|live| live.session_id.clone()),
        ) {
            data.session_id = Some(session);
        }
        error
    }

    fn as_rpc_error(&self, error: &AppError) -> RpcError {
        match error {
            AppError::Refused(kind) => self.application_error(*kind),
            AppError::Stopped => RpcError::standard(
                INTERNAL_ERROR,
                "The session is no longer running on this connection.",
            ),
            // What is left of it is one submission disposition, which needs the
            // engine the transport has not attached yet (B7).
            AppError::Unserved(_) => self.application_error(ProtocolErrorKind::ActionUnavailable),
        }
    }

    /// The core's answer, as the method that asked for it returns.
    fn as_result(&self, method: RequestMethod, reply: AppReply) -> Option<ResponseResult> {
        Some(match (method, reply) {
            (RequestMethod::SessionList, AppReply::Sessions(sessions)) => {
                ResponseResult::SessionList(SessionListResult { sessions })
            }
            (RequestMethod::SessionRead, AppReply::Session(snapshot)) => {
                ResponseResult::SessionRead(SessionReadResult {
                    snapshot: *snapshot,
                })
            }
            (RequestMethod::SessionClose, AppReply::Accepted) => {
                ResponseResult::SessionClose(SessionCloseResult {
                    session_id: self
                        .session
                        .as_ref()
                        .and_then(|live| live.session_id.clone())
                        .unwrap_or_else(|| SessionId::new("")),
                })
            }
            (RequestMethod::SessionDelete, AppReply::Deleted { locator, deleted }) => {
                ResponseResult::SessionDelete(SessionDeleteResult { locator, deleted })
            }
            (RequestMethod::ConversationList, AppReply::Conversations(conversations)) => {
                ResponseResult::ConversationList(ConversationListResult { conversations })
            }
            (RequestMethod::ConversationRead, AppReply::Conversation(snapshot)) => {
                ResponseResult::ConversationRead(ConversationReadResult {
                    snapshot: *snapshot,
                })
            }
            (RequestMethod::ConversationMarkRead, AppReply::Marked(conversation)) => {
                ResponseResult::ConversationMarkRead(ConversationMarkReadResult {
                    conversation: *conversation,
                })
            }
            (RequestMethod::ConversationSubmit, AppReply::Submitted(disposition)) => {
                ResponseResult::ConversationSubmit(ConversationSubmitResult { disposition })
            }
            (RequestMethod::TurnInterrupt, AppReply::Interrupted { turn_id, accepted }) => {
                ResponseResult::TurnInterrupt(TurnInterruptResult { turn_id, accepted })
            }
            (RequestMethod::QueueRead, AppReply::Queue { entries, count }) => {
                ResponseResult::QueueRead(QueueReadResult { entries, count })
            }
            (RequestMethod::QueueReclaimTail, AppReply::Reclaimed { outcome, .. }) => {
                ResponseResult::QueueReclaimTail(QueueReclaimTailResult { outcome: *outcome })
            }
            (RequestMethod::InteractionRespond, AppReply::Responded { item_id }) => {
                ResponseResult::InteractionRespond(InteractionRespondResult {
                    status: RespondStatus::Accepted,
                    item_id,
                })
            }
            (RequestMethod::ActionList, AppReply::Actions { actions, revision }) => {
                ResponseResult::ActionList(ActionListResult { actions, revision })
            }
            (RequestMethod::ActionExecute, AppReply::Submitted(disposition)) => {
                ResponseResult::ActionExecute(ActionExecuteResult { disposition })
            }
            (RequestMethod::ConfigRead, AppReply::Config(config)) => {
                ResponseResult::ConfigRead(ConfigReadResult { config: *config })
            }
            (RequestMethod::CatalogRead, AppReply::Catalog(catalog)) => {
                ResponseResult::CatalogRead(CatalogReadResult { catalog: *catalog })
            }
            (RequestMethod::ResourceRead, AppReply::Resource(resource)) => {
                ResponseResult::ResourceRead(ResourceReadResult {
                    resource: *resource,
                })
            }
            (RequestMethod::AssetRegisterPath, AppReply::Asset(asset)) => {
                ResponseResult::AssetRegisterPath(AssetRegisterPathResult { asset: *asset })
            }
            (
                RequestMethod::AssetReadChunk,
                AppReply::AssetChunk {
                    data,
                    next_offset,
                    eof,
                },
            ) => ResponseResult::AssetReadChunk(AssetReadChunkResult {
                data,
                next_offset,
                eof,
            }),
            _ => return None,
        })
    }
}

/// A mutation or a read, as the core takes it.
enum Core {
    Command(AppCommand),
    Query(AppQuery),
}

/// Whether a method needs a session of its own.
///
/// The four that do not are the ones a client makes on the way in: what there is
/// to choose from, and which sessions exist. A test keeps this in step with the
/// errors each method declares, so the table cannot drift from the manifest.
fn needs_session(method: RequestMethod) -> bool {
    !matches!(
        method,
        RequestMethod::CatalogRead
            | RequestMethod::SessionList
            | RequestMethod::SessionDelete
            | RequestMethod::AssetReadChunk
    )
}

/// The core call one wire request makes. `initialize`, `shutdown`,
/// `session/start`, and `session/resume` are the transport's own and are not
/// here.
fn as_core_request(call: ClientRequest) -> Option<Core> {
    Some(match call {
        ClientRequest::SessionList(params) => Core::Query(AppQuery::ListSessions {
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::SessionRead(_) => Core::Query(AppQuery::ReadSession),
        ClientRequest::SessionClose(_) => Core::Command(AppCommand::CloseSession),
        ClientRequest::SessionDelete(params) => Core::Command(AppCommand::DeleteSession {
            locator: params.locator,
        }),
        ClientRequest::ConversationList(params) => Core::Query(AppQuery::ListConversations {
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::ConversationRead(params) => Core::Query(AppQuery::ReadConversation {
            conversation_id: params.conversation_id,
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::ConversationMarkRead(params) => Core::Command(AppCommand::MarkRead {
            conversation_id: params.conversation_id,
            last_item_id: params.last_item_id,
            last_room_seq: params.last_room_seq,
            expected_revision: params.expected_revision,
        }),
        ClientRequest::ConversationSubmit(params) => Core::Command(AppCommand::Submit {
            conversation_id: params.conversation_id,
            input: params.input,
        }),
        ClientRequest::TurnInterrupt(params) => Core::Command(AppCommand::Interrupt {
            conversation_id: params.conversation_id,
            turn_id: params.turn_id,
        }),
        ClientRequest::QueueRead(params) => Core::Query(AppQuery::ReadQueue {
            conversation_id: params.conversation_id,
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::QueueReclaimTail(params) => Core::Command(AppCommand::ReclaimQueueTail {
            conversation_id: params.conversation_id,
            expected_revision: params.expected_revision,
        }),
        ClientRequest::InteractionRespond(params) => {
            Core::Command(AppCommand::RespondInteraction {
                interaction_id: params.interaction_id,
                activation: params.activation,
                decision: params.decision,
            })
        }
        ClientRequest::ActionList(params) => Core::Query(AppQuery::ListActions {
            origin_conversation_id: params.origin_conversation_id,
        }),
        ClientRequest::ActionExecute(params) => Core::Command(AppCommand::Execute {
            origin_conversation_id: params.origin_conversation_id,
            precondition: params.precondition,
            action: params.action,
        }),
        ClientRequest::ConfigRead(_) => Core::Query(AppQuery::ReadConfig),
        ClientRequest::CatalogRead(params) => Core::Query(AppQuery::ReadCatalog {
            catalog: params.catalog,
            provider: params.provider,
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::ResourceRead(params) => Core::Query(AppQuery::ReadResource {
            resource: params.resource,
            cursor: params.cursor,
            limit: params.limit,
        }),
        ClientRequest::AssetRegisterPath(params) => Core::Command(AppCommand::RegisterAsset {
            path: params.path,
            expected_mime: params.expected_mime,
            expected_sha256: params.expected_sha256,
        }),
        ClientRequest::AssetReadChunk(params) => Core::Query(AppQuery::ReadAssetChunk {
            asset_id: params.asset_id,
            offset: params.offset,
            length: params.length,
        }),
        ClientRequest::Initialize(_)
        | ClientRequest::Shutdown(_)
        | ClientRequest::SessionStart(_)
        | ClientRequest::SessionResume(_) => return None,
    })
}

/// Attach to a started core and take its first cut.
///
/// The snapshot read here is where this attachment's notification stream begins:
/// everything before it is in the snapshot, so replaying it would state the same
/// fact twice (spec "Architecture"; B2a ruling ①).
async fn attach(core: AppCore) -> Result<(Live, SessionSnapshot), AppError> {
    let mut link = core.attach(AttachRequest::new("app-server"))?;
    let id = crate::app::RequestId(0);
    link.request(AppRequest::Query {
        id,
        query: AppQuery::ReadSession,
    })?;
    loop {
        match link.recv().await {
            Some(AppFrame::Reply { result, .. }) => {
                let snapshot = match result? {
                    AppReply::Session(snapshot) => *snapshot,
                    _ => return Err(AppError::Stopped),
                };
                let session_id = Some(snapshot.session.id.clone());
                return Ok((
                    Live {
                        core,
                        link,
                        session_id,
                        transcript: None,
                        lobby: false,
                    },
                    snapshot,
                ));
            }
            // Impossible before the cut, and harmless if it were.
            Some(AppFrame::Event(_)) => continue,
            None => return Err(AppError::Stopped),
        }
    }
}

/// The next thing the core said, or nothing at all while there is no core.
async fn next_frame(session: &mut Option<Live>) -> Option<AppFrame> {
    match session {
        Some(live) => live.link.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "stdio/tests.rs"]
mod tests;
