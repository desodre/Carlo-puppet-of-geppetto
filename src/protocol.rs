//! Wire framing for protocol v1: a fixed 16-byte header followed by a bounded payload.
//!
//! All multi-byte integers are big-endian (network order):
//!
//! | Offset | Size | Field                                   |
//! |-------:|-----:|-----------------------------------------|
//! |      0 |    4 | magic `CRLO`                            |
//! |      4 |    1 | protocol version ([`VERSION`])          |
//! |      5 |    1 | payload kind ([`Kind`])                 |
//! |      6 |    2 | reserved, must be zero                  |
//! |      8 |    4 | request ID                              |
//! |     12 |    4 | payload length in bytes                 |
//!
//! Control payloads are JSON; hierarchy XML and video are raw bytes, never Base64.
//! The header is validated in full before any payload byte is read or allocated.
//! This module is transport-independent: sockets, sessions and commands are not here.

use std::fmt;
use std::io::{self, Read, Write};

/// First four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"CRLO";
/// Protocol version written and accepted by this build.
pub const VERSION: u8 = 1;
/// Fixed header size in bytes.
pub const HEADER_LEN: usize = 16;

const CONTROL_LIMIT: u32 = 64 * 1024;
const BULK_LIMIT: u32 = 8 * 1024 * 1024;

/// Payload kinds defined by protocol v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// UTF-8 JSON command, response or event.
    Control = 1,
    /// Raw hierarchy XML bytes.
    Hierarchy = 2,
    /// Raw encoded video bytes.
    Video = 3,
}

impl Kind {
    fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Control),
            2 => Some(Self::Hierarchy),
            3 => Some(Self::Video),
            _ => None,
        }
    }

    /// Largest payload accepted or written for this kind.
    pub const fn max_payload(self) -> u32 {
        match self {
            Self::Control => CONTROL_LIMIT,
            Self::Hierarchy | Self::Video => BULK_LIMIT,
        }
    }
}

/// One decoded frame. `Debug` reports only the payload length, never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    /// Payload interpretation.
    pub kind: Kind,
    /// Correlates a response with its request; zero when there is no request.
    pub request_id: u32,
    /// Payload bytes, at most [`Kind::max_payload`].
    pub payload: Vec<u8>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &self.kind)
            .field("request_id", &self.request_id)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

/// A framing failure. See [`Error::recoverable`] and [`Error::rejection`] for the
/// response each one requires.
#[derive(Debug)]
pub enum Error {
    /// The transport failed; a deadline expiring mid-frame also lands here.
    Io(io::Error),
    /// The peer closed the stream inside a header or payload.
    Truncated {
        /// Bytes the current header or payload required.
        expected: u32,
        /// Bytes received before EOF.
        received: u32,
    },
    /// The stream does not start with [`MAGIC`].
    BadMagic,
    /// The peer speaks another protocol version.
    UnsupportedVersion {
        /// Version byte received.
        received: u8,
    },
    /// Reserved header bytes were not zero.
    ReservedBits,
    /// Unknown payload kind within the size bound; its payload was discarded.
    UnknownKind {
        /// Kind byte received.
        kind: u8,
        /// Request ID of the discarded frame.
        request_id: u32,
    },
    /// Declared length exceeds the limit for its kind; nothing was allocated.
    TooLarge {
        /// Kind byte of the frame.
        kind: u8,
        /// Declared payload length.
        length: u32,
        /// Limit that applies to this kind.
        limit: u32,
    },
    /// The bounded payload buffer could not be reserved.
    OutOfMemory {
        /// Payload length that could not be reserved.
        length: u32,
    },
}

impl Error {
    /// Stable identifier used in rejection payloads and logs.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Truncated { .. } => "truncated_frame",
            Self::BadMagic => "bad_magic",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::ReservedBits => "reserved_bits",
            Self::UnknownKind { .. } => "unknown_kind",
            Self::TooLarge { .. } => "frame_too_large",
            Self::OutOfMemory { .. } => "out_of_memory",
        }
    }

    /// Whether the stream is still aligned on a frame boundary. Every other error
    /// leaves it desynchronised: send [`Error::rejection`] if any, then close.
    pub const fn recoverable(&self) -> bool {
        matches!(self, Self::UnknownKind { .. })
    }

    /// The control frame to send before continuing or closing. `None` when the
    /// peer is gone or the transport failed, where the only response is to close.
    /// It is always written with this build's header so any version can read it.
    pub fn rejection(&self) -> Option<Frame> {
        let (request_id, detail) = match *self {
            Self::Io(_) | Self::Truncated { .. } => return None,
            Self::UnsupportedVersion { .. } => (0, Some(("supported_version", VERSION.into()))),
            Self::TooLarge { limit, .. } => (0, Some(("limit", limit))),
            Self::UnknownKind { request_id, .. } => (request_id, None),
            Self::BadMagic | Self::ReservedBits | Self::OutOfMemory { .. } => (0, None),
        };
        let mut error = serde_json::json!({ "code": self.code() });
        if let Some((key, value)) = detail {
            error[key] = value.into();
        }
        Some(Frame {
            kind: Kind::Control,
            request_id,
            payload: serde_json::json!({ "error": error })
                .to_string()
                .into_bytes(),
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(f, "frame transport: {source}"),
            Self::Truncated { expected, received } => {
                write!(f, "stream ended after {received} of {expected} bytes")
            }
            Self::BadMagic => f.write_str("frame magic mismatch"),
            Self::UnsupportedVersion { received } => {
                write!(f, "protocol version {received} is not supported")
            }
            Self::ReservedBits => f.write_str("reserved header bytes are not zero"),
            Self::UnknownKind { kind, .. } => write!(f, "unknown frame kind {kind}"),
            Self::TooLarge {
                kind,
                length,
                limit,
            } => {
                write!(
                    f,
                    "frame kind {kind} declares {length} bytes, limit is {limit}"
                )
            }
            Self::OutOfMemory { length } => write!(f, "cannot reserve {length} payload bytes"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

/// Read one frame, tolerating arbitrarily fragmented reads.
///
/// Returns `Ok(None)` when the peer closes cleanly on a frame boundary.
///
/// # Errors
/// Returns [`Error`] for transport failure, EOF inside a frame, or a header that
/// fails validation. Only [`Error::UnknownKind`] leaves the stream usable.
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>, Error> {
    let mut header = [0_u8; HEADER_LEN];
    let mut filled = 0;
    while filled < HEADER_LEN {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(truncated(HEADER_LEN as u64, filled as u64)),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    if header[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    if header[4] != VERSION {
        return Err(Error::UnsupportedVersion {
            received: header[4],
        });
    }
    if header[6..8] != [0, 0] {
        return Err(Error::ReservedBits);
    }
    let wire_kind = header[5];
    let request_id = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let length = u32::from_be_bytes([header[12], header[13], header[14], header[15]]);
    let kind = Kind::from_wire(wire_kind);
    let limit = kind.map_or(BULK_LIMIT, Kind::max_payload);
    if length > limit {
        return Err(Error::TooLarge {
            kind: wire_kind,
            length,
            limit,
        });
    }
    let mut body = reader.take(length.into());
    let Some(kind) = kind else {
        let discarded = io::copy(&mut body, &mut io::sink())?;
        if discarded < u64::from(length) {
            return Err(truncated(length.into(), discarded));
        }
        return Err(Error::UnknownKind {
            kind: wire_kind,
            request_id,
        });
    };
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(length as usize)
        .map_err(|_| Error::OutOfMemory { length })?;
    let received = body.read_to_end(&mut payload)?;
    if received < length as usize {
        return Err(truncated(length.into(), received as u64));
    }
    Ok(Some(Frame {
        kind,
        request_id,
        payload,
    }))
}

/// Write one frame without flushing; the caller owns buffering and deadlines.
///
/// # Errors
/// Returns [`Error::TooLarge`] before writing anything when `payload` exceeds the
/// limit for `kind`, or [`Error::Io`] when the transport fails.
pub fn write_frame(
    writer: &mut impl Write,
    kind: Kind,
    request_id: u32,
    payload: &[u8],
) -> Result<(), Error> {
    let limit = kind.max_payload();
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length <= limit)
        .ok_or(Error::TooLarge {
            kind: kind as u8,
            length: u32::try_from(payload.len()).unwrap_or(u32::MAX),
            limit,
        })?;
    let mut header = [0_u8; HEADER_LEN];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5] = kind as u8;
    header[8..12].copy_from_slice(&request_id.to_be_bytes());
    header[12..].copy_from_slice(&length.to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    Ok(())
}

fn truncated(expected: u64, received: u64) -> Error {
    // Both values are bounded by HEADER_LEN or a u32 payload length.
    Error::Truncated {
        expected: expected as u32,
        received: received as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yields one byte per read with an interruption before each, counting bytes.
    struct Trickle<'a> {
        data: &'a [u8],
        consumed: usize,
        interrupt: bool,
    }

    impl<'a> Trickle<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                consumed: 0,
                interrupt: true,
            }
        }
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.interrupt, false) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.interrupt = true;
            let Some(byte) = self.data.get(self.consumed) else {
                return Ok(0);
            };
            buffer[0] = *byte;
            self.consumed += 1;
            Ok(1)
        }
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn encoded(kind: Kind, request_id: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, kind, request_id, payload).unwrap();
        bytes
    }

    fn header(kind: u8, length: u32) -> Vec<u8> {
        let mut bytes = encoded(Kind::Control, 7, b"");
        bytes[5] = kind;
        bytes[12..].copy_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn shared_fixtures_decode_fragmented_and_reencode_identically() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/protocol_frames.json")).unwrap();
        for case in fixtures["frames"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let bytes = unhex(case["hex"].as_str().unwrap());
            let result = read_frame(&mut Trickle::new(&bytes));
            if let Some(code) = case["error"].as_str() {
                let error = result.unwrap_err();
                assert_eq!(error.code(), code, "{name}");
                assert_eq!(
                    error.recoverable(),
                    case["recoverable"].as_bool().unwrap(),
                    "{name}"
                );
                let rejection = error
                    .rejection()
                    .map(|frame| (frame.request_id, String::from_utf8(frame.payload).unwrap()));
                let expected = case["rejection"].as_object().map(|object| {
                    (
                        object["request_id"].as_u64().unwrap() as u32,
                        object["payload"].as_str().unwrap().to_owned(),
                    )
                });
                assert_eq!(rejection, expected, "{name}");
            } else {
                let frame = result.unwrap().unwrap();
                assert_eq!(frame.kind as u64, case["kind"].as_u64().unwrap(), "{name}");
                assert_eq!(
                    u64::from(frame.request_id),
                    case["request_id"].as_u64().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    frame.payload,
                    unhex(case["payload_hex"].as_str().unwrap()),
                    "{name}"
                );
                assert_eq!(
                    encoded(frame.kind, frame.request_id, &frame.payload),
                    bytes,
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn oversized_length_is_rejected_after_reading_only_the_header() {
        for (kind, limit) in [
            (1, CONTROL_LIMIT),
            (2, BULK_LIMIT),
            (3, BULK_LIMIT),
            (9, BULK_LIMIT),
        ] {
            for length in [limit + 1, u32::MAX] {
                let mut bytes = header(kind, length);
                bytes.extend_from_slice(b"unread");
                let mut reader = Trickle::new(&bytes);
                let error = read_frame(&mut reader).unwrap_err();
                assert!(
                    matches!(error, Error::TooLarge { kind: k, length: l, limit: m }
                        if k == kind && l == length && m == limit)
                );
                assert!(!error.recoverable());
                assert_eq!(reader.consumed, HEADER_LEN);
            }
        }
        let payload = vec![0; CONTROL_LIMIT as usize + 1];
        let mut written = Vec::new();
        assert!(matches!(
            write_frame(&mut written, Kind::Control, 1, &payload),
            Err(Error::TooLarge { .. })
        ));
        assert!(written.is_empty());
        let at_limit = encoded(Kind::Control, 1, &payload[1..]);
        let frame = read_frame(&mut at_limit.as_slice()).unwrap().unwrap();
        assert_eq!(frame.payload.len(), CONTROL_LIMIT as usize);
    }

    #[test]
    fn eof_is_clean_only_on_a_frame_boundary() {
        let bytes = encoded(Kind::Hierarchy, 3, b"<hierarchy/>");
        assert!(read_frame(&mut Trickle::new(&[])).unwrap().is_none());
        for cut in 1..bytes.len() {
            let error = read_frame(&mut Trickle::new(&bytes[..cut])).unwrap_err();
            let (expected, received) = if cut < HEADER_LEN {
                (HEADER_LEN, cut)
            } else {
                (bytes.len() - HEADER_LEN, cut - HEADER_LEN)
            };
            assert!(
                matches!(error, Error::Truncated { expected: e, received: r }
                    if e as usize == expected && r as usize == received),
                "cut {cut}: {error}"
            );
            assert!(error.rejection().is_none());
        }
        let mut unknown = header(9, 4);
        unknown.extend_from_slice(b"ab");
        assert!(matches!(
            read_frame(&mut unknown.as_slice()),
            Err(Error::Truncated {
                expected: 4,
                received: 2
            })
        ));
    }

    #[test]
    fn unknown_kind_is_skipped_and_the_next_frame_still_decodes() {
        let mut bytes = header(200, 5);
        bytes.extend_from_slice(b"later");
        bytes.extend(encoded(Kind::Video, 8, &[0, 0, 0, 1, 0x67]));
        let mut reader = Trickle::new(&bytes);
        let error = read_frame(&mut reader).unwrap_err();
        assert!(matches!(
            error,
            Error::UnknownKind {
                kind: 200,
                request_id: 7
            }
        ));
        assert!(error.recoverable());
        let next = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!((next.kind, next.request_id), (Kind::Video, 8));
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn transport_errors_propagate_and_debug_redacts_payload() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        let error = read_frame(&mut Broken).unwrap_err();
        assert!(matches!(&error, Error::Io(source) if source.kind() == io::ErrorKind::TimedOut));
        assert!(error.rejection().is_none() && !error.recoverable());
        let frame = Frame {
            kind: Kind::Hierarchy,
            request_id: 1,
            payload: b"SENSITIVE_TEST_SENTINEL".to_vec(),
        };
        let debug = format!("{frame:?}");
        assert!(debug.contains("payload_len: 23") && !debug.contains("SENSITIVE"));
    }
}
