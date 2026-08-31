//! The long connection: dial, listen, ack, ping, and come back (ADR-0016 §6).
//!
//! The socket itself is the only stateful thing in the Feishu adapter, and
//! everything it decides comes from a pure brick: the URL and the intervals
//! from [`bootstrap`], the bytes from [`frame`], the reassembly from
//! [`chunks`], the meaning from [`event`]. What is left here is the loop.
//!
//! Two rules earn their comments. The read deadline is re-armed on **every**
//! inbound frame, pongs included, because an outbound ping proves nothing
//! about the inbound path and a laptop that slept has a socket that looks
//! open and is not. And the dialled URL is single-use: every reconnect
//! re-runs the bootstrap.

use std::time::{SystemTime, UNIX_EPOCH};

use bingo_sdk::CancellationToken;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::api::Api;
use super::bootstrap::{ClientConfig, Refusal, handshake};
use super::chunks::Chunks;
use super::event::{Seen, heard};
use super::frame::{self, Frame, Method, header, kind};
use crate::adapter::{Inbox, Incoming};
use crate::error::ChannelError;

/// How a connection ended, and what the ladder should do about it.
enum Ended {
    /// The surface is stopping.
    Cancelled,
    /// It was up and went away: the ladder starts again from the jitter.
    Dropped(String),
    /// It never came up: the next rung of the ladder.
    Refused(String),
    /// Nothing this process does will fix it.
    Fatal(String),
}

/// Stay connected until told to stop, or until the peer refuses in a way no
/// retry can help.
pub async fn listen(
    api: &Api,
    app_secret: &str,
    me: &str,
    inbox: &Inbox,
    cancel: &CancellationToken,
) -> Result<(), ChannelError> {
    let mut config = ClientConfig::default();
    let mut attempt = 0u32;
    // Outlives every connection, unlike the reassembly beside it: see `Inbound`.
    let mut seen = Seen::default();
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        match once(api, app_secret, me, inbox, cancel, &mut config, &mut seen).await {
            Ended::Cancelled => return Ok(()),
            Ended::Fatal(why) => return Err(ChannelError::Refused(why)),
            Ended::Dropped(why) => {
                tracing::warn!(%why, "the feishu long connection dropped");
                attempt = 0;
            }
            Ended::Refused(why) => {
                tracing::warn!(%why, "the feishu long connection was refused");
                attempt = attempt.saturating_add(1);
            }
        }
        let wait = config.backoff(attempt, entropy());
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// One bootstrap, one dial, one connection's worth of listening.
async fn once(
    api: &Api,
    app_secret: &str,
    me: &str,
    inbox: &Inbox,
    cancel: &CancellationToken,
    config: &mut ClientConfig,
    seen: &mut Seen,
) -> Ended {
    let url = match api.endpoint(app_secret).await {
        Ok((url, fresh)) => {
            *config = fresh;
            url
        }
        Err(ChannelError::Refused(why)) => return Ended::Fatal(why),
        Err(error) => return Ended::Refused(error.to_string()),
    };
    let socket = match tokio_tungstenite::connect_async(&url).await {
        Ok((socket, _)) => socket,
        Err(error) => return refused(error),
    };
    pump(socket, config, me, inbox, cancel, seen).await
}

/// A refused upgrade says why in three response headers and no body.
fn refused(error: WsError) -> Ended {
    let WsError::Http(response) = &error else {
        return Ended::Refused(format!("the feishu socket: {error}"));
    };
    let value = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
    match handshake(
        value("Handshake-Status"),
        value("Handshake-Msg"),
        value("Handshake-Autherrcode"),
    ) {
        Refusal::Fatal(why) => Ended::Fatal(why),
        Refusal::Retry(why) => Ended::Refused(why),
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Writer = futures::stream::SplitSink<Socket, Message>;
type Reader = futures::stream::SplitStream<Socket>;

/// One connection, listened to until it ends, and then closed properly
/// whatever ended it.
async fn pump(
    socket: Socket,
    config: &mut ClientConfig,
    me: &str,
    inbox: &Inbox,
    cancel: &CancellationToken,
    seen: &mut Seen,
) -> Ended {
    let (mut writer, mut reader) = socket.split();
    let ended = listening(&mut writer, &mut reader, config, me, inbox, cancel, seen).await;
    farewell(&mut writer).await;
    ended
}

async fn listening(
    writer: &mut Writer,
    reader: &mut Reader,
    config: &mut ClientConfig,
    me: &str,
    inbox: &Inbox,
    cancel: &CancellationToken,
    seen: &mut Seen,
) -> Ended {
    let mut inbound = Inbound::new(me, seen);
    let mut pings = 0u64;
    let mut ping_at = Instant::now() + config.ping_interval;
    let mut quiet_at = Instant::now() + config.read_deadline();
    loop {
        let message = tokio::select! {
            _ = cancel.cancelled() => return Ended::Cancelled,
            _ = tokio::time::sleep_until(ping_at) => {
                pings += 1;
                if let Err(error) = writer.send(binary(&ping(pings))).await {
                    return Ended::Dropped(format!("the ping could not be written: {error}"));
                }
                ping_at = Instant::now() + config.ping_interval;
                continue;
            }
            _ = tokio::time::sleep_until(quiet_at) => {
                return Ended::Dropped("nothing arrived for two ping intervals".into());
            }
            message = reader.next() => message,
        };
        let Some(frame) = read(message) else {
            return Ended::Dropped("the peer hung up".into());
        };
        let step = inbound.absorb(frame, config);
        // The deadline is re-armed on everything, and the intervals may have
        // just been hot-updated by a pong.
        quiet_at = Instant::now() + config.read_deadline();
        ping_at = ping_at.min(Instant::now() + config.ping_interval);
        if let Some(ended) = act(writer, step, inbox, cancel, quiet_at).await {
            return ended;
        }
    }
}

/// What one absorbed frame asks for: an ack written back, and an event handed
/// to the surface. `Some` is the connection ending.
async fn act(
    writer: &mut Writer,
    step: Step,
    inbox: &Inbox,
    cancel: &CancellationToken,
    quiet_at: Instant,
) -> Option<Ended> {
    if let Some(reply) = step.reply
        && let Err(error) = writer.send(binary(&reply)).await
    {
        return Some(Ended::Dropped(format!(
            "the ack could not be written: {error}"
        )));
    }
    let event = step.deliver?;
    // Handing an event on must never blind the socket. While this task is
    // parked on a full downstream channel, nothing polls the read deadline,
    // the ping timer or the cancellation — so a stalled session is
    // indistinguishable from a healthy connection, no further ack goes out,
    // the peer drops us, and the reconnect ladder never runs because the pump
    // never returns. That is the wedge no reconnect can recover from, which is
    // why this wait is armed like every other one here.
    tokio::select! {
        posted = inbox.post(event) => posted.is_err().then_some(Ended::Cancelled),
        _ = cancel.cancelled() => Some(Ended::Cancelled),
        _ = tokio::time::sleep_until(quiet_at) => Some(Ended::Dropped(
            "the session could not take an event within the read deadline".into(),
        )),
    }
}

/// Say goodbye before letting the socket go.
///
/// Feishu counts a long connection against the app until the server times the
/// corpse out, and the limit is fatal (`1000040350`): a run that reconnects a
/// few times without closing cleanly can exhaust the budget and then be
/// refused for good. A close frame is what makes a restart recover promptly.
async fn farewell(writer: &mut Writer) {
    let _ = writer.send(Message::Close(None)).await;
    let _ = writer.close().await;
}

/// The frame in a message, or `None` when the socket is finished with. Text,
/// pings and closes are the transport's own business, not the protocol's.
fn read(message: Option<Result<Message, WsError>>) -> Option<Frame> {
    match message? {
        Ok(Message::Binary(bytes)) => match frame::decode(&bytes) {
            Ok(frame) => Some(frame),
            Err(error) => {
                tracing::warn!(%error, "a frame feishu sent could not be decoded");
                Some(Frame::default())
            }
        },
        Ok(Message::Close(_)) | Err(_) => None,
        Ok(_) => Some(Frame::default()),
    }
}

/// What one inbound frame asks for: something written back, something handed
/// to the surface, or neither.
#[derive(Default)]
struct Step {
    reply: Option<Frame>,
    deliver: Option<Incoming>,
}

/// The reassembly state of one connection, and the dedupe ring of the whole
/// run.
///
/// The chunks belong to the connection: a half-assembled message on a socket
/// that is gone will never be finished. The `Seen` ring does not — Feishu
/// redelivers anything it was not acked for, so a ring that started empty on
/// every reconnect would replay each of those as a brand new message and
/// submit the same prompt twice.
struct Inbound<'a> {
    me: &'a str,
    chunks: Chunks,
    seen: &'a mut Seen,
}

impl<'a> Inbound<'a> {
    fn new(me: &'a str, seen: &'a mut Seen) -> Self {
        Self {
            me,
            chunks: Chunks::default(),
            seen,
        }
    }

    fn absorb(&mut self, frame: Frame, config: &mut ClientConfig) -> Step {
        let arrived = std::time::Instant::now();
        self.chunks.expire(arrived);
        let Some(whole) = self.chunks.absorb(frame, arrived) else {
            return Step::default();
        };
        match whole.kind() {
            Some(kind::PING) => Step {
                reply: Some(pong(whole.seq_id)),
                ..Step::default()
            },
            Some(kind::PONG) => {
                *config = config.updated(&payload(&whole));
                Step::default()
            }
            // A card callback on a connection that never asked for one is a
            // stale console switch, not an answer (ADR-0016 §6).
            Some(kind::CARD) => {
                tracing::debug!("feishu sent a card frame; this build does not use them");
                Step::default()
            }
            _ => self.event(whole, arrived),
        }
    }

    /// Acked within three seconds, whatever else happens to it. The ack's
    /// payload doubles as the `{}` a card callback must be answered with.
    fn event(&mut self, whole: Frame, arrived: std::time::Instant) -> Step {
        let deliver = heard(&whole.payload, self.me)
            .filter(|heard| self.seen.first(&heard.id))
            .and_then(|heard| heard.incoming);
        Step {
            reply: Some(frame::ack(&whole, arrived.elapsed())),
            deliver,
        }
    }
}

fn payload(frame: &Frame) -> Value {
    serde_json::from_slice(&frame.payload).unwrap_or(Value::Null)
}

fn binary(frame: &Frame) -> Message {
    Message::Binary(frame::encode(frame).into())
}

fn ping(seq: u64) -> Frame {
    control(seq, kind::PING)
}

fn pong(seq: u64) -> Frame {
    control(seq, kind::PONG)
}

fn control(seq: u64, what: &str) -> Frame {
    let mut frame = Frame {
        seq_id: seq,
        method: Method::Control,
        ..Frame::default()
    };
    frame.set_header(header::TYPE, what);
    frame
}

/// Enough randomness to keep a fleet from reconnecting in step. The clock is
/// the only entropy this crate needs, and it costs no dependency.
fn entropy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| u64::from(since.subsec_nanos()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
