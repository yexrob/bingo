//! Feishu's long-connection frame, hand-decoded.
//!
//! Every message on the socket is one binary protobuf frame of eleven fields.
//! The format is documented nowhere; the shape below is read from the official
//! SDKs and is **stable by observation** (ADR-0016 §6), which is exactly why it
//! is a pure brick with byte fixtures: when the peer moves, one test fails and
//! one function changes, and nothing else in the crate has an opinion about
//! bytes at all.
//!
//! ```text
//! Header { 1: key string, 2: value string }
//! Frame  { 1: seq_id uint64, 2: log_id uint64, 3: service int32,
//!          4: method int32 (0 = control, 1 = data), 5: headers repeated Header,
//!          6: payload_encoding string, 7: payload_type string,
//!          8: payload bytes, 9: log_id_new string }
//! ```
//!
//! No `prost`: this is 150 lines of varints against a schema that will not
//! grow, and a code generator would cost a dependency tree to save them.

use std::time::Duration;

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_BYTES: u8 = 2;
const WIRE_FIXED32: u8 = 5;

/// Header keys the peer sends. `type` is the only one that changes what a
/// frame means; the rest are context or reassembly.
pub mod header {
    pub const TYPE: &str = "type";
    pub const MESSAGE_ID: &str = "message_id";
    /// How many parts this message was split into.
    pub const SUM: &str = "sum";
    /// This part's 0-based index.
    pub const SEQ: &str = "seq";
    /// Milliseconds the ack took, which the peer records as our latency.
    pub const BIZ_RT: &str = "biz_rt";
}

/// What `type` says a frame is. `card` is a stale-configuration artefact —
/// a card callback delivered over a connection that never asked for one — so
/// it is dropped rather than acted on (ADR-0016 §6).
pub mod kind {
    pub const EVENT: &str = "event";
    pub const CARD: &str = "card";
    pub const PING: &str = "ping";
    pub const PONG: &str = "pong";
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("the frame ended in the middle of a field")]
    Truncated,
    #[error("{0}")]
    Malformed(&'static str),
}

/// Proto3 omits a zero, so `Control` is what an absent field means — which
/// is why it is the default rather than an `Option` beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Method {
    #[default]
    Control,
    Data,
    /// A method this build does not know. Kept as it is so an ack echoes the
    /// frame the peer sent, not the one we would have sent.
    Other(i32),
}

impl Method {
    fn code(self) -> i32 {
        match self {
            Method::Control => 0,
            Method::Data => 1,
            Method::Other(code) => code,
        }
    }

    fn of(code: i32) -> Self {
        match code {
            0 => Method::Control,
            1 => Method::Data,
            other => Method::Other(other),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub seq_id: u64,
    pub log_id: u64,
    pub service: i32,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub payload_encoding: String,
    pub payload_type: String,
    pub payload: Vec<u8>,
    pub log_id_new: String,
}

impl Frame {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// A header's value as a number, when it is one.
    pub fn number(&self, key: &str) -> Option<usize> {
        self.header(key)?.parse().ok()
    }

    pub fn set_header(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        match self.headers.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = value,
            None => self.headers.push((key.to_string(), value)),
        }
    }

    pub fn kind(&self) -> Option<&str> {
        self.header(header::TYPE)
    }
}

/// The frame the peer must be sent back within three seconds of an event.
/// The `data` field is base64 of the JSON bytes — Go encodes a `[]byte` that
/// way and the peer's decoder insists on it (ADR-0016 §6).
pub fn ack(frame: &Frame, took: Duration) -> Frame {
    let mut ack = frame.clone();
    ack.set_header(header::BIZ_RT, took.as_millis().to_string());
    ack.payload = br#"{"code":200,"headers":{},"data":"e30="}"#.to_vec();
    ack
}

pub fn decode(bytes: &[u8]) -> Result<Frame, CodecError> {
    let mut reader = Reader::new(bytes);
    let mut frame = Frame::default();
    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, WIRE_VARINT) => frame.seq_id = reader.varint()?,
            (2, WIRE_VARINT) => frame.log_id = reader.varint()?,
            (3, WIRE_VARINT) => frame.service = reader.int32()?,
            (4, WIRE_VARINT) => frame.method = Method::of(reader.int32()?),
            (5, WIRE_BYTES) => frame.headers.push(decode_header(reader.slice()?)?),
            (6, WIRE_BYTES) => frame.payload_encoding = reader.text()?,
            (7, WIRE_BYTES) => frame.payload_type = reader.text()?,
            (8, WIRE_BYTES) => frame.payload = reader.slice()?.to_vec(),
            (9, WIRE_BYTES) => frame.log_id_new = reader.text()?,
            // A field this build does not know is the peer growing, not the
            // peer breaking: step over it and keep the rest.
            _ => reader.skip(wire)?,
        }
    }
    Ok(frame)
}

fn decode_header(bytes: &[u8]) -> Result<(String, String), CodecError> {
    let mut reader = Reader::new(bytes);
    let (mut key, mut value) = (String::new(), String::new());
    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, WIRE_BYTES) => key = reader.text()?,
            (2, WIRE_BYTES) => value = reader.text()?,
            _ => reader.skip(wire)?,
        }
    }
    Ok((key, value))
}

pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    put_uint(&mut out, 1, frame.seq_id);
    put_uint(&mut out, 2, frame.log_id);
    put_int(&mut out, 3, frame.service);
    put_int(&mut out, 4, frame.method.code());
    for (key, value) in &frame.headers {
        put_bytes(&mut out, 5, &encode_header(key, value));
    }
    put_text(&mut out, 6, &frame.payload_encoding);
    put_text(&mut out, 7, &frame.payload_type);
    if !frame.payload.is_empty() {
        put_bytes(&mut out, 8, &frame.payload);
    }
    put_text(&mut out, 9, &frame.log_id_new);
    out
}

fn encode_header(key: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_bytes(&mut out, 1, key.as_bytes());
    put_bytes(&mut out, 2, value.as_bytes());
    out
}

/// A zero is a proto3 default and is left out, as every encoder leaves it out.
fn put_uint(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return;
    }
    put_tag(out, field, WIRE_VARINT);
    put_varint(out, value);
}

/// An `int32`. A negative one is sign-extended to ten bytes, as protobuf
/// requires; none of these fields is ever negative in practice.
fn put_int(out: &mut Vec<u8>, field: u32, value: i32) {
    put_uint(out, field, i64::from(value) as u64);
}

fn put_text(out: &mut Vec<u8>, field: u32, text: &str) {
    if text.is_empty() {
        return;
    }
    put_bytes(out, field, text.as_bytes());
}

fn put_bytes(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    put_tag(out, field, WIRE_BYTES);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_tag(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, (u64::from(field) << 3) | u64::from(wire));
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// The next field number and wire type, or `None` at the end.
    fn tag(&mut self) -> Result<Option<(u32, u8)>, CodecError> {
        if self.at >= self.bytes.len() {
            return Ok(None);
        }
        let tag = self.varint()?;
        let field = u32::try_from(tag >> 3).map_err(|_| CodecError::Malformed("a field number"))?;
        Ok(Some((field, (tag & 0x7) as u8)))
    }

    fn varint(&mut self) -> Result<u64, CodecError> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.bytes.get(self.at).ok_or(CodecError::Truncated)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                return Err(CodecError::Malformed("a varint wider than 64 bits"));
            }
        }
    }

    /// A protobuf `int32`: negatives arrive sign-extended to ten bytes.
    fn int32(&mut self) -> Result<i32, CodecError> {
        Ok(self.varint()? as u32 as i32)
    }

    fn slice(&mut self) -> Result<&'a [u8], CodecError> {
        let length =
            usize::try_from(self.varint()?).map_err(|_| CodecError::Malformed("a length"))?;
        let end = self.at.checked_add(length).ok_or(CodecError::Truncated)?;
        let out = self.bytes.get(self.at..end).ok_or(CodecError::Truncated)?;
        self.at = end;
        Ok(out)
    }

    /// Text the peer sent. Invalid UTF-8 is replaced rather than refused: a
    /// mangled header is not worth dropping an event over.
    fn text(&mut self) -> Result<String, CodecError> {
        Ok(String::from_utf8_lossy(self.slice()?).into_owned())
    }

    fn skip(&mut self, wire: u8) -> Result<(), CodecError> {
        match wire {
            WIRE_VARINT => {
                self.varint()?;
            }
            WIRE_BYTES => {
                self.slice()?;
            }
            WIRE_FIXED64 => self.advance(8)?,
            WIRE_FIXED32 => self.advance(4)?,
            _ => return Err(CodecError::Malformed("an unknown wire type")),
        }
        Ok(())
    }

    fn advance(&mut self, bytes: usize) -> Result<(), CodecError> {
        self.at = self
            .at
            .checked_add(bytes)
            .filter(|at| *at <= self.bytes.len())
            .ok_or(CodecError::Truncated)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Frame {
        Frame {
            seq_id: 7,
            log_id: 42,
            service: 1,
            method: Method::Data,
            headers: vec![
                ("type".into(), "event".into()),
                ("message_id".into(), "om_1".into()),
                ("sum".into(), "1".into()),
                ("seq".into(), "0".into()),
            ],
            payload_encoding: "json".into(),
            payload_type: "im.message.receive_v1".into(),
            payload: br#"{"hello":"world"}"#.to_vec(),
            log_id_new: "lg_1".into(),
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The bytes are the contract. A change here is a change to what the peer
    /// is sent, and deserves the same scrutiny as a schema change.
    #[test]
    fn a_data_frame_encodes_to_the_documented_bytes() {
        insta::assert_snapshot!("frame-event", hex(&encode(&event())));
    }

    #[test]
    fn every_frame_round_trips_through_the_wire() {
        for frame in [
            event(),
            Frame::default(),
            Frame {
                method: Method::Control,
                headers: vec![("type".into(), "ping".into())],
                ..Frame::default()
            },
            Frame {
                method: Method::Other(9),
                payload: vec![0, 1, 2, 0xff],
                ..Frame::default()
            },
        ] {
            let bytes = encode(&frame);
            assert_eq!(decode(&bytes).expect("a frame"), frame, "{}", hex(&bytes));
        }
    }

    #[test]
    fn a_field_this_build_does_not_know_is_stepped_over() {
        let mut bytes = encode(&event());
        // Field 15, length-delimited, three bytes the decoder has no arm for.
        put_bytes(&mut bytes, 15, b"new");
        assert_eq!(decode(&bytes).expect("a frame"), event());
    }

    #[test]
    fn a_truncated_frame_is_an_error_rather_than_a_guess() {
        let bytes = encode(&event());
        for cut in 1..bytes.len() {
            if let Ok(frame) = decode(&bytes[..cut]) {
                assert_ne!(frame, event(), "a short read must not look complete");
            }
        }
        assert_eq!(decode(&[0x08]), Err(CodecError::Truncated));
    }

    #[test]
    fn an_ack_echoes_the_frame_with_the_latency_and_the_payload_the_peer_wants() {
        let ack = ack(&event(), Duration::from_millis(12));
        assert_eq!(ack.seq_id, 7, "the peer matches the ack to its frame");
        assert_eq!(ack.header(header::BIZ_RT), Some("12"));
        assert_eq!(ack.header(header::TYPE), Some("event"));
        assert_eq!(
            String::from_utf8_lossy(&ack.payload),
            r#"{"code":200,"headers":{},"data":"e30="}"#,
            "`data` is base64 of the JSON bytes, which is how Go encodes []byte"
        );
        // And the whole thing survives the wire it will be written to.
        assert_eq!(decode(&encode(&ack)).expect("a frame"), ack);
    }

    #[test]
    fn a_header_is_read_by_name_and_as_a_number_when_it_is_one() {
        let frame = event();
        assert_eq!(frame.kind(), Some(kind::EVENT));
        assert_eq!(frame.number(header::SUM), Some(1));
        assert_eq!(frame.number(header::TYPE), None);
        assert_eq!(frame.header("nothing"), None);
    }

    #[test]
    fn setting_a_header_twice_leaves_one_of_it() {
        let mut frame = event();
        frame.set_header(header::BIZ_RT, "1");
        frame.set_header(header::BIZ_RT, "2");
        assert_eq!(frame.header(header::BIZ_RT), Some("2"));
        assert_eq!(
            frame
                .headers
                .iter()
                .filter(|(key, _)| key == header::BIZ_RT)
                .count(),
            1
        );
    }
}
