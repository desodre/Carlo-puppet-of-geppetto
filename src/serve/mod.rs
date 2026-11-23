//! `carlo serve`: one client on two channels, control and video, over a private
//! Unix socket. No HTTP, no WebSocket.
//!
//! Lifecycle: validate the session directory, bind `control.sock` (0600) or, only
//! when creating that file is denied, a random abstract-namespace name, print
//! one ready line with a fresh session identifier, accept the control client
//! within a deadline, check the peer's kernel credentials, then answer control
//! frames until `shutdown`, EOF or a failure. After the control handshake the
//! same listener admits exactly one more connection, the video channel, bound to
//! the session by its identifier; the listener is closed (and the socket path
//! removed) as soon as that channel is attached or its deadline passed.
//! Everything Carlo created is removed on every exit path; the directory belongs
//! to the host and is never touched. The contract is `docs/protocol.md`.
//!
//! The control loop runs on the thread that called [`run`] and never touches
//! the video connection; see `video.rs` for the other threads. No reconnection
//! and no second client exist here.

mod control;
mod hierarchy;
mod socket;
mod video;

use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::diagnostics::Diagnostics;
use crate::error::CarloError;
use crate::protocol::{self, Kind, VERSION};
use crate::video::stream::Mirror;
use control::{Control, Next};
use hierarchy::Device;
use socket::{BoundedStream, ControlSocket, Ended, STOP, Signals};
use video::{Timings, Video};

/// How long the socket waits for its control client.
const ACCEPT_DEADLINE: Duration = Duration::from_secs(30);
/// Time from accept to a successful `hello`, however the bytes are fragmented.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);
/// Time allowed for each complete request frame after the handshake.
const IDLE_DEADLINE: Duration = Duration::from_secs(30);
/// Time allowed to hand one response to a peer that is not reading.
const WRITE_DEADLINE: Duration = Duration::from_secs(10);
/// Session identifier entropy in bytes (128 bits, 32 hex digits).
const SESSION_BYTES: usize = 16;

/// Why `serve` refused to start or ended a session abnormally. No variant
/// carries the session identifier or anything a client sent.
#[derive(Debug)]
pub enum Error {
    /// The session directory is unusable; nothing was created.
    Directory(&'static str),
    /// `<dir>/control.sock` does not fit `sockaddr_un` (107 bytes).
    PathTooLong,
    /// The socket path already exists; it was left untouched.
    SocketExists,
    /// The socket could not be created.
    Bind(io::Error),
    /// The created socket did not have mode 0600; it was removed.
    SocketMode,
    /// Termination signal handlers could not be installed.
    Signals,
    /// `/dev/urandom` did not yield 128 bits for an identifier or socket name.
    Entropy(io::Error),
    /// Waiting for the client failed.
    Accept(io::Error),
    /// No client connected within the accept deadline.
    AcceptTimeout,
    /// The kernel did not report the peer's credentials.
    PeerCredentials(io::Error),
    /// The peer runs as another user; nothing was read from it.
    PeerRejected {
        /// UID reported by the kernel for the peer.
        uid: u32,
    },
    /// SIGHUP, SIGINT or SIGTERM ended the session.
    Interrupted,
    /// No successful `hello` arrived within the handshake deadline.
    HandshakeTimeout,
    /// No complete request arrived within the idle deadline.
    IdleTimeout,
    /// The peer did not accept a response within the write deadline.
    WriteTimeout,
    /// The connection failed.
    Transport(io::Error),
    /// A frame violated the framing contract; holds its stable code.
    Framing(&'static str),
    /// The handshake was rejected; holds the stable code sent to the client.
    Handshake(&'static str),
    /// The client closed the connection before a successful `hello`.
    ClosedBeforeHandshake,
}

impl Error {
    /// 69 when the session was refused before becoming ready, 74 afterwards.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Directory(_) | Self::PathTooLong | Self::SocketExists | Self::SocketMode => 69,
            _ => 74,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directory(reason) => write!(f, "session directory {reason}"),
            Self::PathTooLong => f.write_str("socket path exceeds 107 bytes"),
            Self::SocketExists => f.write_str("socket path already exists; refusing to replace it"),
            Self::Bind(source) => write!(f, "create control socket: {source}"),
            Self::SocketMode => f.write_str("control socket was not created with mode 0600"),
            Self::Signals => f.write_str("cannot install termination signal handlers"),
            Self::Entropy(source) => write!(f, "read session entropy: {source}"),
            Self::Accept(source) => write!(f, "accept client: {source}"),
            Self::AcceptTimeout => f.write_str("no client connected before the accept deadline"),
            Self::PeerCredentials(source) => write!(f, "read peer credentials: {source}"),
            Self::PeerRejected { uid } => write!(f, "peer uid {uid} is not allowed"),
            Self::Interrupted => f.write_str("interrupted by a termination signal"),
            Self::HandshakeTimeout => f.write_str("no valid hello before the handshake deadline"),
            Self::IdleTimeout => f.write_str("no complete request before the idle deadline"),
            Self::WriteTimeout => f.write_str("client did not read a response in time"),
            Self::Transport(source) => write!(f, "connection: {source}"),
            Self::Framing(code) => write!(f, "framing violation: {code}"),
            Self::Handshake(code) => write!(f, "handshake rejected: {code}"),
            Self::ClosedBeforeHandshake => f.write_str("client closed before the handshake"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind(source)
            | Self::Entropy(source)
            | Self::Accept(source)
            | Self::PeerCredentials(source)
            | Self::Transport(source) => Some(source),
            _ => None,
        }
    }
}

/// The only stdout data before the session ends.
#[derive(Serialize)]
struct Ready<'a> {
    ready: bool,
    schema_version: u32,
    protocol: u8,
    transport: &'static str,
    socket: &'a str,
    session: &'a str,
}

/// Serve one client from `session_dir`, writing the ready line to `output`.
///
/// # Errors
/// Returns [`CarloError::Serve`] when the directory or socket is refused, no
/// client arrives, the peer or its handshake is rejected, a deadline passes, a
/// frame violates the contract or a termination signal arrives; other variants
/// for an invalid environment or a failed ready-line write. Returns `Ok` only
/// after `shutdown` or a clean client close following a successful handshake.
pub fn run(session_dir: &str, output: &mut impl Write) -> Result<(), CarloError> {
    let directory = Path::new(session_dir);
    let own_uid = socket::effective_uid();
    socket::validate_directory(directory, own_uid)?;
    let diagnostics = Diagnostics::collect()?;
    // The profile gates only; no library is opened and no Binder call is made.
    let source = Arc::new(Mirror::detect(&diagnostics)?);
    let hierarchy = Box::new(Device::detect(&diagnostics)?);
    // Installed before anything is created, so a signal always reaches cleanup.
    let _signals = Signals::install()?;
    let listener = ControlSocket::bind(directory)?;
    let session = random_hex()?;
    serde_json::to_writer(
        &mut *output,
        &Ready {
            ready: true,
            schema_version: 1,
            protocol: VERSION,
            transport: listener.transport(),
            socket: listener.address(),
            session: &session,
        },
    )?;
    writeln!(output).map_err(CarloError::output)?;
    output.flush().map_err(CarloError::output)?;

    // The listener outlives this accept only to admit the video channel after a
    // successful handshake; every early return below drops (closes) it.
    let stream = listener.accept(ACCEPT_DEADLINE, &STOP)?;
    let peer = socket::peer_credentials(&stream)?;
    if !socket::peer_allowed(peer.uid, own_uid, cfg!(target_os = "android")) {
        return Err(Error::PeerRejected { uid: peer.uid }.into());
    }
    let mut stream = BoundedStream::new(stream, &STOP).map_err(Error::Transport)?;
    let mut control = Control::new(
        session,
        &diagnostics,
        Video::new(source, Timings::DEFAULT),
        hierarchy,
    );
    let ended = converse(
        &mut stream,
        &mut control,
        HANDSHAKE_DEADLINE,
        IDLE_DEADLINE,
        Some(listener),
    );
    // Whatever ended control ends the session: stop the stream (display released,
    // encoder drained), close the video channel and the listener, then control.
    control.teardown();
    drop(stream);
    ended?;
    Ok(())
}

/// 32 hex digits from exactly 128 kernel-random bits; fail closed on anything
/// less. Used, independently, for the session identifier and an abstract name.
fn random_hex() -> Result<String, Error> {
    let mut bytes = [0_u8; SESSION_BYTES];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(Error::Entropy)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Answer frames until the session ends. `Ok` means `shutdown` or a clean close
/// after the handshake. `listener` is handed to the video channel once, right
/// after the response to the successful `hello`; if the handshake never
/// succeeds it is dropped here, with nobody else admitted.
fn converse(
    stream: &mut BoundedStream<'_>,
    control: &mut Control,
    handshake: Duration,
    idle: Duration,
    mut listener: Option<ControlSocket>,
) -> Result<(), Error> {
    // One absolute deadline covers everything an unauthenticated peer can send.
    let handshake_deadline = Instant::now() + handshake;
    loop {
        let initialized = control.initialized();
        stream.set_deadline(if initialized {
            Instant::now() + idle
        } else {
            handshake_deadline
        });
        let ended = |stream: &BoundedStream<'_>, source| match stream.ended() {
            Some(Ended::Interrupted) => Error::Interrupted,
            Some(Ended::Deadline) if initialized => Error::IdleTimeout,
            Some(Ended::Deadline) => Error::HandshakeTimeout,
            None => Error::Transport(source),
        };
        match protocol::read_frame(stream) {
            Ok(None) if initialized => return Ok(()),
            Ok(None) => return Err(Error::ClosedBeforeHandshake),
            Ok(Some(frame)) => {
                let reply = control.handle(&frame);
                drop(frame);
                send(stream, reply.kind, reply.request_id, &reply.payload)?;
                drop(reply.payload);
                if let Some(listener) = listener.take_if(|_| control.initialized()) {
                    control.listen(listener);
                }
                match reply.next {
                    Next::Continue => {}
                    Next::Shutdown => return Ok(()),
                    Next::Reject(code) => return Err(Error::Handshake(code)),
                }
            }
            Err(protocol::Error::Io(source)) => return Err(ended(stream, source)),
            Err(error) => {
                let delivered = match error.rejection() {
                    Some(rejection) => send(
                        stream,
                        Kind::Control,
                        rejection.request_id,
                        &rejection.payload,
                    ),
                    None => Ok(()),
                };
                if !error.recoverable() {
                    // The violation is the cause; a failed courtesy reply is not.
                    return Err(Error::Framing(error.code()));
                }
                delivered?;
            }
        }
    }
}

/// Write one response frame of `kind` (control, or a hierarchy document) within
/// [`WRITE_DEADLINE`].
fn send(
    stream: &mut BoundedStream<'_>,
    kind: Kind,
    request_id: u32,
    payload: &[u8],
) -> Result<(), Error> {
    stream.set_deadline(Instant::now() + WRITE_DEADLINE);
    protocol::write_frame(stream, kind, request_id, payload).map_err(|error| {
        match (stream.ended(), error) {
            (Some(Ended::Interrupted), _) => Error::Interrupted,
            (Some(Ended::Deadline), _) => Error::WriteTimeout,
            (None, protocol::Error::Io(source)) => Error::Transport(source),
            (None, other) => Error::Framing(other.code()),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicBool;

    use super::*;

    use super::hierarchy::fake::Fake;
    use crate::accessibility::Error as SnapshotError;

    const SESSION: &str = "0123456789abcdef0123456789abcdef";

    fn session(
        stop: &AtomicBool,
        handshake: Duration,
        idle: Duration,
        client: impl FnOnce(UnixStream) + Send + 'static,
    ) -> Result<(), Error> {
        with_source(stop, handshake, idle, Fake::default(), client)
    }

    /// The production loop and teardown order of [`run`] around `source`.
    fn with_source(
        stop: &AtomicBool,
        handshake: Duration,
        idle: Duration,
        source: Fake,
        client: impl FnOnce(UnixStream) + Send + 'static,
    ) -> Result<(), Error> {
        let (server, peer) = UnixStream::pair().unwrap();
        let client = std::thread::spawn(move || client(peer));
        let diagnostics = Diagnostics::from_environment("linux", "x86_64", None, None, None, None);
        let mut control = Control::new(
            SESSION.into(),
            &diagnostics,
            Video::unattached(),
            Box::new(source),
        );
        let mut stream = BoundedStream::new(server, stop).unwrap();
        let result = converse(&mut stream, &mut control, handshake, idle, None);
        control.teardown();
        drop(stream);
        client.join().unwrap();
        result
    }

    fn request(stream: &mut UnixStream, id: u32, body: &str) -> serde_json::Value {
        protocol::write_frame(stream, Kind::Control, id, body.as_bytes()).unwrap();
        let frame = protocol::read_frame(stream).unwrap().unwrap();
        assert_eq!((frame.kind, frame.request_id), (Kind::Control, id));
        serde_json::from_slice(&frame.payload).unwrap()
    }

    fn hello(stream: &mut UnixStream) {
        let body = format!(r#"{{"command":"hello","protocol":1,"session":"{SESSION}"}}"#);
        assert_eq!(request(stream, 1, &body)["ok"], true);
    }

    #[test]
    fn hierarchy_dump_travels_as_a_hierarchy_frame_and_teardown_runs_on_every_exit() {
        let xml = b"<?xml version='1.0' encoding='UTF-8'?><hierarchy rotation=\"0\"/>";
        // Shutdown: one document, one stable error, then a clean exit.
        let mut source = Fake::default();
        source.results.push_back(Ok(xml.to_vec()));
        source.results.push_back(Err(SnapshotError::Conflict));
        let (dumps, teardowns) = (source.dumps.clone(), source.teardowns.clone());
        let stop = AtomicBool::new(false);
        let long = Duration::from_secs(5);
        let result = with_source(&stop, long, long, source, move |mut stream| {
            hello(&mut stream);
            let dump = br#"{"command":"hierarchy.dump"}"#;
            protocol::write_frame(&mut stream, Kind::Control, 0xABCD_0001, dump).unwrap();
            let frame = protocol::read_frame(&mut stream).unwrap().unwrap();
            assert_eq!(
                (frame.kind, frame.request_id),
                (Kind::Hierarchy, 0xABCD_0001)
            );
            assert_eq!(frame.payload, xml);
            protocol::write_frame(&mut stream, Kind::Control, 7, dump).unwrap();
            let frame = protocol::read_frame(&mut stream).unwrap().unwrap();
            assert_eq!((frame.kind, frame.request_id), (Kind::Control, 7));
            let body: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
            assert_eq!(
                body,
                serde_json::json!({"ok": false, "error": {"code": "conflict"}})
            );
            let body = request(&mut stream, 8, r#"{"command":"shutdown"}"#);
            assert_eq!(body["ok"], true);
        });
        assert!(result.is_ok());
        assert_eq!((dumps.get(), teardowns.get()), (2, 1));

        // Abnormal ends: the client vanishes mid-session or a signal arrives.
        let source = Fake::default();
        let teardowns = source.teardowns.clone();
        let result = with_source(&stop, long, long, source, move |mut stream| {
            hello(&mut stream);
            stream.write_all(b"CRLO\x01").unwrap();
        });
        assert!(matches!(
            result,
            Err(Error::Transport(_) | Error::Framing(_))
        ));
        assert_eq!(teardowns.get(), 1);
        let stopped = AtomicBool::new(true);
        let source = Fake::default();
        let teardowns = source.teardowns.clone();
        let result = with_source(&stopped, long, long, source, |mut peer| {
            let mut sink = Vec::new();
            let _ = peer.read_to_end(&mut sink);
        });
        assert!(matches!(result, Err(Error::Interrupted)));
        assert_eq!(teardowns.get(), 1);
    }

    #[test]
    fn session_identifiers_are_32_hex_digits_and_differ() {
        let (first, second) = (random_hex().unwrap(), random_hex().unwrap());
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_ne!(first, second);
    }

    #[test]
    fn deadlines_are_typed_by_phase_without_waiting_for_the_real_ones() {
        let stop = AtomicBool::new(false);
        let short = Duration::from_millis(150);
        let long = Duration::from_secs(20);
        let hold = |peer: UnixStream| {
            // Keep the connection open and silent until the server gives up.
            let mut sink = Vec::new();
            let _ = (&peer).read_to_end(&mut sink);
        };
        assert!(matches!(
            session(&stop, short, long, hold),
            Err(Error::HandshakeTimeout)
        ));
        // Recoverable junk does not extend the absolute handshake deadline.
        let started = Instant::now();
        let result = session(&stop, short, long, |mut peer| {
            let mut junk = Vec::new();
            protocol::write_frame(&mut junk, Kind::Control, 9, b"").unwrap();
            junk[5] = 200;
            while peer.write_all(&junk).is_ok() {
                if protocol::read_frame(&mut peer).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        assert!(matches!(result, Err(Error::HandshakeTimeout)));
        assert!(started.elapsed() < Duration::from_secs(10));
        let result = session(&stop, long, short, |mut peer| {
            hello(&mut peer);
            let mut sink = Vec::new();
            let _ = peer.read_to_end(&mut sink);
        });
        assert!(matches!(result, Err(Error::IdleTimeout)));
        // A frame that stalls half-way is cut by the same deadline.
        let result = session(&stop, long, short, |mut peer| {
            hello(&mut peer);
            peer.write_all(b"CRLO\x01\x01").unwrap();
            let mut sink = Vec::new();
            let _ = peer.read_to_end(&mut sink);
        });
        assert!(matches!(result, Err(Error::IdleTimeout)));
    }

    #[test]
    fn stop_flag_ends_a_session_in_either_phase() {
        for after_hello in [false, true] {
            let stop = std::sync::Arc::new(AtomicBool::new(false));
            let raise = stop.clone();
            let long = Duration::from_secs(20);
            let started = Instant::now();
            let result = session(&stop, long, long, move |mut peer| {
                if after_hello {
                    hello(&mut peer);
                }
                raise.store(true, std::sync::atomic::Ordering::Relaxed);
                let mut sink = Vec::new();
                let _ = peer.read_to_end(&mut sink);
            });
            assert!(matches!(result, Err(Error::Interrupted)));
            assert!(started.elapsed() < Duration::from_secs(5));
        }
    }

    #[test]
    fn setup_refusals_exit_69_and_session_failures_exit_74() {
        for error in [
            Error::Directory("does not exist"),
            Error::PathTooLong,
            Error::SocketExists,
            Error::SocketMode,
        ] {
            assert_eq!(CarloError::from(error).exit_code(), 69);
        }
        for error in [
            Error::AcceptTimeout,
            Error::PeerRejected { uid: 1 },
            Error::Interrupted,
            Error::Handshake("invalid_session"),
            Error::Framing("bad_magic"),
            Error::ClosedBeforeHandshake,
            Error::IdleTimeout,
        ] {
            assert_eq!(CarloError::from(error).exit_code(), 74);
        }
    }
}
