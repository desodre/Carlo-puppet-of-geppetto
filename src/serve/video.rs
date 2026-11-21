//! The video channel of a session and the stream it carries.
//!
//! Threads, and why control never waits for the video peer:
//!
//! * The **control thread** (the one `serve` runs on) owns [`Video`]. It only
//!   reads atomics, spawns threads and waits on in-process channels with a
//!   deadline. It never reads from or writes to the video socket, except for
//!   holding the writer lock described below.
//! * The **attendant thread** is spawned after the control handshake and owns the
//!   session's listener. It admits the video connection (peer credentials, then
//!   one `attach` frame carrying the session identifier, under one absolute
//!   deadline), drops the listener, and afterwards only watches the read side:
//!   the channel is agent-to-client, so any byte or an end of file closes it.
//! * A **stream thread** exists from a `video.start` until its stream ended. It
//!   runs the [`Source`] and is the only writer of video packets, each under a
//!   write deadline. Every platform object of the source is created and dropped
//!   on it.
//!
//! The attendant writes the `attach` response while holding the writer lock, so
//! that no packet can interleave with it; that write is bounded by one slice and
//! goes into an empty socket buffer. Everything else shared between threads is an
//! atomic or a channel.
//!
//! A write that misses its deadline may have left half a frame on the wire, so
//! it closes the video channel for good; control goes on.

use std::io::Read;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::control::{constant_time_eq, failure};
use super::socket::{self, BoundedStream, ControlSocket, Ended, SLICE};
use crate::protocol::{self, Frame, Kind, VideoPacket, VideoPacketKind};
use crate::video::mirror::Consumer;
use crate::video::stream::{KEY_FRAME_INTERVAL_SECONDS, Params, Source};
use crate::video::{
    Error as SourceError, FLAG_CODEC_CONFIG, FLAG_END_OF_STREAM, FLAG_KEY_FRAME, Sink,
};

/// Failed attachment attempts after which the listener is closed.
const MAX_ATTACH_ATTEMPTS: u32 = 3;
/// The only display a stream can mirror for now (#51).
const MAIN_DISPLAY: u64 = 0;

/// Every wait of the video channel. Constants in production; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timings {
    /// From the control handshake until the video channel must be attached; the
    /// listener is closed when this passes.
    pub attach: Duration,
    /// From accepting a video connection to its complete `attach` frame.
    pub attach_handshake: Duration,
    /// From `video.start` to a started display and encoder.
    pub start: Duration,
    /// From `video.stop` (or teardown) to the end of the stream thread.
    pub stop: Duration,
    /// For handing one video packet to the client.
    pub packet_write: Duration,
}

impl Timings {
    pub(super) const DEFAULT: Self = Self {
        attach: Duration::from_secs(10),
        attach_handshake: Duration::from_secs(5),
        start: Duration::from_secs(5),
        stop: Duration::from_secs(8),
        packet_write: Duration::from_secs(2),
    };
}

const PENDING: u8 = 0;
const ATTACHED: u8 = 1;
const ABSENT: u8 = 2;
const CLOSED: u8 = 3;

/// State of the video channel, shared by the three threads.
struct Channel {
    state: AtomicU8,
    /// Raised at session teardown: ends every wait on the video side.
    closing: AtomicBool,
    /// Write handle of the attached connection; stream threads clone it.
    writer: Mutex<Option<UnixStream>>,
}

impl Channel {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(ABSENT),
            closing: AtomicBool::new(false),
            writer: Mutex::new(None),
        }
    }

    fn name(&self) -> &'static str {
        match self.state.load(Ordering::Acquire) {
            PENDING => "pending",
            ATTACHED => "attached",
            CLOSED => "closed",
            _ => "absent",
        }
    }

    fn attached(&self) -> bool {
        self.state.load(Ordering::Acquire) == ATTACHED
    }

    fn settle(&self, from: u8, to: u8) {
        let _ = self
            .state
            .compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire);
    }

    fn lock(&self) -> MutexGuard<'_, Option<UnixStream>> {
        // The guarded value is a plain handle: a panic elsewhere cannot corrupt it.
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Close the connection in both directions, for every handle to it, and make
    /// an attached channel `closed`. Idempotent.
    fn close(&self) {
        self.settle(ATTACHED, CLOSED);
        if let Some(stream) = self.lock().take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn writer(&self) -> Option<UnixStream> {
        self.lock()
            .as_ref()
            .and_then(|stream| stream.try_clone().ok())
    }
}

/// What a valid first frame on a video connection must say.
fn attach_request(frame: &Frame, session: &str) -> Result<(), &'static str> {
    if frame.kind != Kind::Control {
        return Err("unexpected_kind");
    }
    let request = serde_json::from_slice::<Value>(&frame.payload).ok();
    let Some(command) = request
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
    else {
        return Err("invalid_request");
    };
    let request = request.as_ref().unwrap_or(&Value::Null);
    if command != "attach" {
        return Err("attach_required");
    }
    if request["channel"].as_str() != Some("video") {
        return Err("unsupported_channel");
    }
    let presented = request["session"].as_str().unwrap_or("");
    if constant_time_eq(presented.as_bytes(), session.as_bytes()) {
        Ok(())
    } else {
        Err("invalid_session")
    }
}

/// Validate one accepted connection: kernel credentials before any read, then
/// exactly one frame before `deadline`. A refused connection gets a best-effort
/// answer within the same deadline and is closed; nothing else is affected.
fn candidate(
    stream: UnixStream,
    session: &str,
    own_uid: u32,
    channel: &Channel,
    deadline: Instant,
) -> Option<(UnixStream, u32)> {
    let peer = socket::peer_credentials(&stream).ok()?;
    if !socket::peer_allowed(peer.uid, own_uid, cfg!(target_os = "android")) {
        return None;
    }
    let mut stream = BoundedStream::new(stream, &channel.closing).ok()?;
    stream.set_deadline(deadline);
    let refusal = match protocol::read_frame(&mut stream) {
        Ok(Some(frame)) => match attach_request(&frame, session) {
            Ok(()) => return Some((stream.into_inner(), frame.request_id)),
            Err(code) => Some((frame.request_id, failure(code).to_string().into_bytes())),
        },
        Ok(None) | Err(protocol::Error::Io(_)) => None,
        Err(error) => error
            .rejection()
            .map(|frame| (frame.request_id, frame.payload)),
    };
    if let Some((request_id, payload)) = refusal {
        let _ = protocol::write_frame(&mut stream, Kind::Control, request_id, &payload);
    }
    None
}

/// Accept connections until one attaches, the deadline passes, the session
/// closes or [`MAX_ATTACH_ATTEMPTS`] were refused.
fn admit(
    listener: &ControlSocket,
    session: &str,
    own_uid: u32,
    channel: &Channel,
    timings: &Timings,
) -> Option<(UnixStream, u32)> {
    let deadline = Instant::now() + timings.attach;
    for _ in 0..MAX_ATTACH_ATTEMPTS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let stream = listener.accept(remaining, &channel.closing).ok()?;
        // Never past the attach deadline, so the listener's lifetime is bounded.
        let handshake = deadline.min(Instant::now() + timings.attach_handshake);
        let admitted = candidate(stream, session, own_uid, channel, handshake);
        if admitted.is_some() {
            return admitted;
        }
    }
    None
}

/// Body of the attendant thread. The listener is dropped (closed, socket file
/// removed) before the outcome of the attachment becomes visible to anyone.
fn attend(
    listener: ControlSocket,
    session: String,
    own_uid: u32,
    channel: &Channel,
    timings: &Timings,
) {
    let admitted = admit(&listener, &session, own_uid, channel, timings);
    drop(listener);
    drop(session);
    let Some((stream, request_id)) = admitted else {
        channel.settle(PENDING, ABSENT);
        return;
    };
    let acknowledged = (|| {
        let writer = stream.try_clone().ok()?;
        let mut stream = BoundedStream::new(stream, &channel.closing).ok()?;
        // Held across the state change and the response: a stream thread gets
        // its writer only after the response left, so packets cannot interleave.
        let mut slot = channel.lock();
        *slot = Some(writer);
        channel.settle(PENDING, ATTACHED);
        stream.set_deadline(Instant::now() + SLICE);
        let body = json!({ "ok": true, "channel": "video" }).to_string();
        let sent = protocol::write_frame(&mut stream, Kind::Control, request_id, body.as_bytes());
        drop(slot);
        sent.ok().map(|()| stream.into_inner())
    })();
    if let Some(stream) = acknowledged {
        watch(&stream, channel);
    }
    // An authenticated peer that vanished, sent something or closed: the slot is
    // spent either way, because the listener is already gone.
    channel.settle(PENDING, ABSENT);
    channel.close();
}

/// Block, in slices, until the client sends anything, closes, the connection
/// fails, the channel is closed by a writer or the session ends.
fn watch(mut stream: &UnixStream, channel: &Channel) {
    let mut byte = [0_u8; 1];
    while !channel.closing.load(Ordering::Relaxed) && channel.attached() {
        match stream.read(&mut byte) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            _ => return,
        }
    }
}

/// Parse and validate a `video.start` request. Every field is optional. An
/// ill-typed or out-of-range field is `invalid_request` and wins over the two
/// `unsupported_*` codes. Nothing of the request is echoed.
fn parse(request: &Value) -> Result<Params, &'static str> {
    let integer = |key: &str, default: u64, minimum: u64, maximum: u64| match request.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|number| (minimum..=maximum).contains(number))
            .ok_or("invalid_request"),
    };
    let display = match request.get("display") {
        None => MAIN_DISPLAY,
        Some(value) if value.is_u64() => value.as_u64().unwrap_or(u64::MAX),
        // A negative integer names no display, but it is still an integer.
        Some(value) if value.is_i64() => u64::MAX,
        Some(_) => return Err("invalid_request"),
    };
    let width = integer("max_width", 360, 16, 1920)?;
    let height = integer("max_height", 640, 16, 1920)?;
    let fps = integer("fps", 15, 1, 60)?;
    let bitrate = integer("bitrate", 1_500_000, 100_000, 20_000_000)?;
    let default_encoder = match request.get("encoder") {
        None => true,
        Some(Value::String(name)) => name == "default",
        Some(_) => return Err("invalid_request"),
    };
    if display != MAIN_DISPLAY {
        return Err("unsupported_display");
    }
    if !default_encoder {
        return Err("unsupported_encoder");
    }
    // Encoders take even dimensions: round down. The ranges fit every target type.
    let even = |value: u64| u16::try_from(value & !1).map_err(|_| "invalid_request");
    Ok(Params {
        width: even(width)?,
        height: even(height)?,
        fps: u32::try_from(fps).map_err(|_| "invalid_request")?,
        bitrate: u32::try_from(bitrate).map_err(|_| "invalid_request")?,
    })
}

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const ABORTED: u8 = 2;

/// What the control thread tells one stream thread.
struct Signal {
    stop: AtomicBool,
    /// `STARTING` until either the source started (`RUNNING`, set by the stream
    /// thread) or the start deadline passed (`ABORTED`, set by control). Whoever
    /// wins the exchange decides whether the stream exists.
    phase: AtomicU8,
}

/// Why a stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Stopped,
    ConsumerTimeout,
    ChannelClosed,
    Error(&'static str),
}

/// Counters of one ended stream; no content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Finished {
    reason: Reason,
    packets: u64,
    bytes: u64,
    key_frames: u64,
}

/// Why the packet writer stopped forwarding before the source ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    /// The start deadline passed first; nothing was or will be written.
    Aborted,
    ConsumerTimeout,
    ChannelClosed,
    /// The encoder output broke the packet contract; holds the stable code.
    Invalid(&'static str),
}

type Started = Result<Option<String>, &'static str>;

/// The [`Consumer`] of a stream: turns encoder output into video packets on the
/// video channel. Lives on the stream thread only. It never fails the source: a
/// consumer problem is recorded, `wanted` turns false and the source winds down
/// in its normal order.
struct Packets<'a> {
    writer: BoundedStream<'a>,
    channel: &'a Channel,
    signal: &'a Signal,
    started: Option<Sender<Started>>,
    write: Duration,
    generation: u32,
    request_id: u32,
    width: u16,
    height: u16,
    configured: bool,
    first_media: bool,
    ended: bool,
    halted: Option<Halt>,
    frame: Vec<u8>,
    packets: u64,
    bytes: u64,
    key_frames: u64,
}

impl Packets<'_> {
    /// Write one packet within the write deadline. A failure closes the video
    /// channel: part of a frame may already be on the wire.
    fn write(&mut self, kind: VideoPacketKind, key_frame: bool, pts_us: i64, data: &[u8]) -> bool {
        if data.len() > VideoPacket::MAX_DATA {
            self.halted = Some(Halt::Invalid("packet_too_large"));
            return false;
        }
        let header = VideoPacket {
            kind,
            key_frame,
            generation: self.generation,
            pts_us,
            width: self.width,
            height: self.height,
        };
        self.frame.clear();
        if self
            .frame
            .try_reserve(protocol::VIDEO_HEADER_LEN + data.len())
            .is_err()
        {
            self.halted = Some(Halt::Invalid("out_of_memory"));
            return false;
        }
        self.frame.extend_from_slice(&header.encode());
        self.frame.extend_from_slice(data);
        self.writer.set_deadline(Instant::now() + self.write);
        let sent =
            protocol::write_frame(&mut self.writer, Kind::Video, self.request_id, &self.frame);
        if sent.is_err() {
            self.halted = Some(match self.writer.ended() {
                Some(Ended::Deadline) => Halt::ConsumerTimeout,
                _ => Halt::ChannelClosed,
            });
            self.channel.close();
            return false;
        }
        true
    }

    fn count(&mut self, data: &[u8], key_frame: bool) {
        self.packets += 1;
        self.bytes += data.len() as u64;
        self.key_frames += u64::from(key_frame);
    }

    /// After the source returned: decide the reason, send the `end` packet if
    /// the channel is still in step, and report. `None` when the stream never
    /// started; the start response then carries the failure.
    fn conclude(mut self, result: Result<(), &'static str>) -> Option<Finished> {
        if let Some(started) = self.started.take() {
            let _ = started.send(Err(result.err().unwrap_or("internal")));
            return None;
        }
        let mut reason = match (self.halted, result) {
            (Some(Halt::ConsumerTimeout), _) => Reason::ConsumerTimeout,
            (Some(Halt::ChannelClosed), _) => Reason::ChannelClosed,
            (Some(Halt::Invalid(code)), _) | (None, Err(code)) => Reason::Error(code),
            (Some(Halt::Aborted), _) | (None, Ok(())) => Reason::Stopped,
        };
        let in_step = matches!(self.halted, None | Some(Halt::Invalid(_)));
        if in_step && !self.write(VideoPacketKind::End, false, 0, &[]) && reason == Reason::Stopped
        {
            reason = match self.halted {
                Some(Halt::ConsumerTimeout) => Reason::ConsumerTimeout,
                _ => Reason::ChannelClosed,
            };
        }
        Some(Finished {
            reason,
            packets: self.packets,
            bytes: self.bytes,
            key_frames: self.key_frames,
        })
    }
}

impl Sink for Packets<'_> {
    fn push(&mut self, flags: u32, pts_us: i64, data: &[u8]) -> Result<(), SourceError> {
        if flags & FLAG_END_OF_STREAM != 0 {
            self.ended = true;
        }
        if self.halted.is_some() || data.is_empty() {
            return Ok(());
        }
        if flags & FLAG_CODEC_CONFIG != 0 {
            if self.write(VideoPacketKind::Config, false, 0, data) {
                self.configured = true;
                self.count(data, false);
            }
            return Ok(());
        }
        let key_frame = flags & FLAG_KEY_FRAME != 0;
        if !self.configured {
            self.halted = Some(Halt::Invalid("media_before_config"));
        } else if !self.first_media && !key_frame {
            self.halted = Some(Halt::Invalid("first_frame_not_key"));
        } else if self.write(VideoPacketKind::Media, key_frame, pts_us, data) {
            self.first_media = true;
            self.count(data, key_frame);
        }
        Ok(())
    }

    fn ended(&self) -> bool {
        self.ended
    }

    fn format(&mut self, width: Option<i32>, height: Option<i32>) {
        let dimension = |value: Option<i32>| value.and_then(|value| u16::try_from(value).ok());
        if let (Some(width), Some(height)) = (dimension(width), dimension(height)) {
            (self.width, self.height) = (width, height);
        }
    }
}

impl Consumer for Packets<'_> {
    fn started(&mut self, codec_name: Option<&str>) {
        let Some(started) = self.started.take() else {
            return;
        };
        let running = self
            .signal
            .phase
            .compare_exchange(STARTING, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !running || started.send(Ok(codec_name.map(str::to_owned))).is_err() {
            self.halted = Some(Halt::Aborted);
        }
    }

    fn wanted(&mut self) -> bool {
        if self.halted.is_some()
            || self.signal.stop.load(Ordering::Relaxed)
            || self.channel.closing.load(Ordering::Relaxed)
        {
            return false;
        }
        if !self.channel.attached() {
            self.halted = Some(Halt::ChannelClosed);
            return false;
        }
        true
    }
}

/// Everything one stream thread is given.
struct Job {
    source: Arc<dyn Source>,
    channel: Arc<Channel>,
    signal: Arc<Signal>,
    writer: UnixStream,
    params: Params,
    generation: u32,
    request_id: u32,
    write: Duration,
}

/// Body of a stream thread: the source and all its platform objects live and
/// die inside `Source::run`, on this thread.
fn work(job: Job, started: Sender<Started>, finished: Sender<Finished>) {
    let Ok(writer) = BoundedStream::new(job.writer, &job.channel.closing) else {
        let _ = started.send(Err("internal"));
        return;
    };
    let mut packets = Packets {
        writer,
        channel: &job.channel,
        signal: &job.signal,
        started: Some(started),
        write: job.write,
        generation: job.generation,
        request_id: job.request_id,
        width: job.params.width,
        height: job.params.height,
        configured: false,
        first_media: false,
        ended: false,
        halted: None,
        frame: Vec::new(),
        packets: 0,
        bytes: 0,
        key_frames: 0,
    };
    let result = job.source.run(&job.params, &mut packets);
    if let Some(report) = packets.conclude(result) {
        let _ = finished.send(report);
    }
}

/// A stream thread the control thread has not reaped yet.
struct Active {
    signal: Arc<Signal>,
    finished: Receiver<Finished>,
    worker: JoinHandle<()>,
    generation: u32,
    /// False for a start that timed out: it has no generation to report.
    reportable: bool,
}

/// The video side of a session, owned by the control thread.
pub(super) struct Video {
    source: Arc<dyn Source>,
    timings: Timings,
    channel: Arc<Channel>,
    attendant: Option<JoinHandle<()>>,
    /// Generation of the last stream that started; the next one is this plus one.
    generation: u32,
    active: Option<Active>,
}

impl Video {
    pub(super) fn new(source: Arc<dyn Source>, timings: Timings) -> Self {
        Self {
            source,
            timings,
            channel: Arc::new(Channel::new()),
            attendant: None,
            generation: 0,
            active: None,
        }
    }

    /// A session without a listener and without a usable source.
    #[cfg(test)]
    pub(super) fn unattached() -> Self {
        struct Never;
        impl Source for Never {
            fn available(&self) -> bool {
                false
            }
            fn run(&self, _: &Params, _: &mut dyn Consumer) -> Result<(), &'static str> {
                Err("internal")
            }
        }
        Self::new(Arc::new(Never), Timings::DEFAULT)
    }

    /// Start the attendant thread, which owns `listener` from here on. If the
    /// thread cannot be created the listener is dropped and video stays absent.
    pub(super) fn listen(&mut self, listener: ControlSocket, session: String) {
        if self.attendant.is_some() {
            return;
        }
        let (channel, timings) = (Arc::clone(&self.channel), self.timings);
        let own_uid = socket::effective_uid();
        channel.settle(ABSENT, PENDING);
        let spawned = std::thread::Builder::new()
            .name("carlo-video-channel".into())
            .spawn(move || attend(listener, session, own_uid, &channel, &timings));
        match spawned {
            Ok(handle) => self.attendant = Some(handle),
            Err(_) => self.channel.settle(PENDING, ABSENT),
        }
    }

    /// `"pending"`, `"attached"`, `"absent"` or `"closed"`.
    pub(super) fn channel(&self) -> &'static str {
        self.channel.name()
    }

    /// `video.start`. Blocks the control thread for at most the start deadline,
    /// on the source only, never on the video peer.
    pub(super) fn start(&mut self, request_id: u32, request: &Value) -> Value {
        let params = match parse(request) {
            Ok(params) => params,
            Err(code) => return failure(code),
        };
        if !self.channel.attached() {
            return failure("video_channel_absent");
        }
        if let Some(active) = self.active.take() {
            if matches!(active.finished.try_recv(), Err(TryRecvError::Empty)) {
                self.active = Some(active);
                return failure("already_streaming");
            }
            // It ended on its own and nobody asked; its report is superseded.
            let _ = active.worker.join();
        }
        if !self.source.available() {
            return json!({
                "ok": false,
                "error": { "code": "unavailable", "reason": "unsupported_profile" },
            });
        }
        let Some(writer) = self.channel.writer() else {
            return failure("video_channel_absent");
        };
        let Some(generation) = self.generation.checked_add(1) else {
            return failure("internal");
        };
        let signal = Arc::new(Signal {
            stop: AtomicBool::new(false),
            phase: AtomicU8::new(STARTING),
        });
        let job = Job {
            source: Arc::clone(&self.source),
            channel: Arc::clone(&self.channel),
            signal: Arc::clone(&signal),
            writer,
            params,
            generation,
            request_id,
            write: self.timings.packet_write,
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let Ok(worker) = std::thread::Builder::new()
            .name("carlo-video-stream".into())
            .spawn(move || work(job, started_tx, finished_tx))
        else {
            return failure("internal");
        };
        let mut outcome = started_rx.recv_timeout(self.timings.start);
        if matches!(outcome, Err(RecvTimeoutError::Timeout))
            && signal
                .phase
                .compare_exchange(STARTING, ABORTED, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            // The source started in the same instant; its message follows at once.
            outcome = started_rx.recv_timeout(SLICE);
        }
        match outcome {
            Ok(Ok(codec)) => {
                self.generation = generation;
                self.active = Some(Active {
                    signal,
                    finished: finished_rx,
                    worker,
                    generation,
                    reportable: true,
                });
                json!({
                    "ok": true,
                    "generation": generation,
                    "display": MAIN_DISPLAY,
                    "codec": codec,
                    "accepted": {
                        "width": params.width,
                        "height": params.height,
                        "fps": params.fps,
                        "bitrate": params.bitrate,
                        "key_frame_interval_seconds": KEY_FRAME_INTERVAL_SECONDS,
                    },
                })
            }
            // The thread reported after its source returned: the join is prompt.
            Ok(Err(code)) => {
                let _ = worker.join();
                failure(code)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                failure("internal")
            }
            // Still inside a platform call that cannot be cancelled. It is told
            // to stop and stays tracked, so no second session runs beside it.
            Err(RecvTimeoutError::Timeout) => {
                signal.stop.store(true, Ordering::Relaxed);
                self.active = Some(Active {
                    signal,
                    finished: finished_rx,
                    worker,
                    generation,
                    reportable: false,
                });
                failure("timeout")
            }
        }
    }

    /// `video.stop`: end the stream, or report one that already ended. Blocks
    /// for at most the stop deadline: on the source winding down and, if a
    /// packet write to a stalled peer is in flight, on that one write deadline.
    pub(super) fn stop(&mut self) -> Value {
        let Some(active) = self.active.take() else {
            return failure("not_streaming");
        };
        active.signal.stop.store(true, Ordering::Relaxed);
        let finished = match active.finished.recv_timeout(self.timings.stop) {
            Ok(finished) => finished,
            Err(RecvTimeoutError::Timeout) => {
                self.active = Some(active);
                return failure("timeout");
            }
            Err(RecvTimeoutError::Disconnected) => Finished {
                reason: Reason::Error("internal"),
                packets: 0,
                bytes: 0,
                key_frames: 0,
            },
        };
        let _ = active.worker.join();
        if !active.reportable {
            return failure("not_streaming");
        }
        let (reason, error) = match finished.reason {
            Reason::Stopped => ("stopped", None),
            Reason::ConsumerTimeout => ("consumer_timeout", None),
            Reason::ChannelClosed => ("channel_closed", None),
            Reason::Error(code) => ("error", Some(code)),
        };
        let mut body = json!({
            "ok": true,
            "generation": active.generation,
            "reason": reason,
            "packets": finished.packets,
            "bytes": finished.bytes,
            "key_frames": finished.key_frames,
        });
        if let Some(code) = error {
            body["error_code"] = code.into();
        }
        body
    }

    /// Teardown: stop the stream (display released, encoder drained, `end` sent
    /// if still possible), then close the video connection and the listener.
    /// Bounded: a stream thread that does not finish within the stop deadline is
    /// left behind for process exit, where the display service's death link
    /// destroys the display. Idempotent.
    pub(super) fn shutdown(&mut self) {
        if let Some(active) = self.active.take() {
            active.signal.stop.store(true, Ordering::Relaxed);
            let waited = active.finished.recv_timeout(self.timings.stop);
            if !matches!(waited, Err(RecvTimeoutError::Timeout)) {
                let _ = active.worker.join();
            }
        }
        self.channel.closing.store(true, Ordering::Relaxed);
        self.channel.close();
        // Every wait of the attendant checks `closing` at least once per slice.
        if let Some(attendant) = self.attendant.take() {
            let _ = attendant.join();
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::io::Write;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;
    use std::sync::atomic::AtomicU32;

    use super::super::control::Control;
    use super::super::{Error, converse};
    use super::*;
    use crate::diagnostics::Diagnostics;

    const SESSION: &str = "0123456789abcdef0123456789abcdef";
    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
    const PROMPT: Duration = Duration::from_secs(5);
    const FAST: Timings = Timings {
        attach: Duration::from_secs(3),
        attach_handshake: Duration::from_millis(400),
        start: Duration::from_millis(400),
        stop: Duration::from_secs(3),
        packet_write: Duration::from_millis(250),
    };

    /// A scripted source: test-only stand-in for the display mirror.
    struct Synthetic {
        available: bool,
        /// Fail the next runs with this code before starting.
        failure: Mutex<Option<&'static str>>,
        start_delay: Mutex<Duration>,
        interval: Duration,
        frame_bytes: usize,
        config: bool,
        key_first: bool,
        runs: AtomicU32,
        live: AtomicBool,
    }

    impl Synthetic {
        fn new() -> Self {
            Self {
                available: true,
                failure: Mutex::new(None),
                start_delay: Mutex::new(Duration::ZERO),
                interval: Duration::from_millis(3),
                frame_bytes: 64,
                config: true,
                key_first: true,
                runs: AtomicU32::new(0),
                live: AtomicBool::new(false),
            }
        }

        fn frame(&self, key: bool) -> Vec<u8> {
            let mut frame = vec![0, 0, 0, 1, if key { 0x65 } else { 0x41 }];
            frame.resize(self.frame_bytes.max(5), 0x80);
            frame
        }
    }

    impl Source for Synthetic {
        fn available(&self) -> bool {
            self.available
        }

        fn run(&self, params: &Params, consumer: &mut dyn Consumer) -> Result<(), &'static str> {
            assert!(
                !self.live.swap(true, Ordering::SeqCst),
                "two sources at once"
            );
            self.runs.fetch_add(1, Ordering::SeqCst);
            let delay = *self.start_delay.lock().unwrap();
            std::thread::sleep(delay);
            let result = (|| {
                if let Some(code) = *self.failure.lock().unwrap() {
                    return Err(code);
                }
                consumer.started(Some("synthetic.avc"));
                consumer.format(
                    Some(i32::from(params.width)),
                    Some(i32::from(params.height)),
                );
                let push = |consumer: &mut dyn Consumer, flags, pts, data: &[u8]| {
                    consumer.push(flags, pts, data).map_err(|_| "internal")
                };
                if self.config {
                    push(consumer, FLAG_CODEC_CONFIG, 0, SPS_PPS)?;
                }
                let mut index = 0_i64;
                while consumer.wanted() {
                    let key = index == 0 && self.key_first;
                    let flags = if key { FLAG_KEY_FRAME } else { 0 };
                    push(consumer, flags, index * 1000, &self.frame(key))?;
                    index += 1;
                    std::thread::sleep(self.interval);
                }
                push(consumer, FLAG_END_OF_STREAM, index * 1000, &[])
            })();
            self.live.store(false, Ordering::SeqCst);
            result
        }
    }

    /// One served session over a socket pair (control) and a real abstract
    /// listener (video), with the production loop on its own thread.
    struct Session {
        control: UnixStream,
        address: SocketAddr,
        server: Option<JoinHandle<Result<(), Error>>>,
        next_id: u32,
    }

    impl Session {
        fn start(source: &Arc<Synthetic>, timings: Timings) -> Self {
            let listener = ControlSocket::bind_abstract().unwrap();
            let address = SocketAddr::from_abstract_name(listener.address().as_bytes()).unwrap();
            let (server, control) = UnixStream::pair().unwrap();
            control.set_read_timeout(Some(PROMPT)).unwrap();
            let source = Arc::clone(source);
            let server = std::thread::spawn(move || {
                let stop = AtomicBool::new(false);
                let diagnostics =
                    Diagnostics::from_environment("linux", "x86_64", None, None, None, None);
                let video = Video::new(source, timings);
                let mut control = Control::new(SESSION.into(), &diagnostics, video);
                let mut stream = BoundedStream::new(server, &stop).unwrap();
                let long = Duration::from_secs(30);
                let result = converse(&mut stream, &mut control, long, long, Some(listener));
                control.teardown();
                result
            });
            let mut session = Self {
                control,
                address,
                server: Some(server),
                next_id: 100,
            };
            let hello = format!(r#"{{"command":"hello","protocol":1,"session":"{SESSION}"}}"#);
            assert_eq!(session.call(&hello)["ok"], true);
            session
        }

        /// One control exchange; returns the response and how long it took.
        fn timed(&mut self, body: &str) -> (Value, Duration) {
            let started = Instant::now();
            self.next_id += 1;
            protocol::write_frame(
                &mut self.control,
                Kind::Control,
                self.next_id,
                body.as_bytes(),
            )
            .unwrap();
            let frame = protocol::read_frame(&mut self.control).unwrap().unwrap();
            assert_eq!(
                (frame.kind, frame.request_id),
                (Kind::Control, self.next_id)
            );
            let text = String::from_utf8(frame.payload).unwrap();
            assert!(!text.contains(SESSION));
            (serde_json::from_str(&text).unwrap(), started.elapsed())
        }

        fn call(&mut self, body: &str) -> Value {
            self.timed(body).0
        }

        fn command(&mut self, name: &str) -> Value {
            self.call(&json!({ "command": name }).to_string())
        }

        fn channel(&mut self) -> String {
            let body = self.command("channels");
            assert_eq!(body["ok"], true);
            body["video"].as_str().unwrap().to_owned()
        }

        fn ping_is_prompt(&mut self) {
            let (body, elapsed) = self.timed(r#"{"command":"ping"}"#);
            assert_eq!(body, json!({"ok": true}));
            assert!(elapsed < Duration::from_millis(200), "{elapsed:?}");
        }

        fn wait_for_channel(&mut self, expected: &str) {
            let deadline = Instant::now() + PROMPT;
            while self.channel() != expected {
                assert!(Instant::now() < deadline, "channel never became {expected}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn connect(&self) -> std::io::Result<UnixStream> {
            let stream = UnixStream::connect_addr(&self.address)?;
            stream.set_read_timeout(Some(PROMPT))?;
            Ok(stream)
        }

        fn attach_with(&self, body: &str) -> (UnixStream, Option<Value>) {
            let mut stream = self.connect().unwrap();
            protocol::write_frame(&mut stream, Kind::Control, 7, body.as_bytes()).unwrap();
            let reply = protocol::read_frame(&mut stream)
                .ok()
                .flatten()
                .map(|frame| {
                    assert_eq!((frame.kind, frame.request_id), (Kind::Control, 7));
                    serde_json::from_slice(&frame.payload).unwrap()
                });
            (stream, reply)
        }

        fn attach(&mut self) -> UnixStream {
            let body = json!({"command": "attach", "channel": "video", "session": SESSION});
            let (stream, reply) = self.attach_with(&body.to_string());
            assert_eq!(reply, Some(json!({"ok": true, "channel": "video"})));
            // Attached means the listener is already gone: nobody else can attach.
            assert_eq!(self.channel(), "attached");
            assert!(self.connect().is_err());
            stream
        }

        fn shutdown(mut self) -> Result<(), Error> {
            assert_eq!(self.command("shutdown"), json!({"ok": true}));
            self.join()
        }

        fn join(&mut self) -> Result<(), Error> {
            let result = self.server.take().unwrap().join().unwrap();
            assert!(self.connect().is_err(), "listener survived the session");
            result
        }
    }

    fn code(body: &Value) -> &str {
        assert_eq!(body["ok"], false, "{body}");
        body["error"]["code"].as_str().unwrap()
    }

    fn packet(stream: &mut UnixStream) -> Option<(u32, VideoPacket, Vec<u8>)> {
        let frame = protocol::read_frame(stream).unwrap()?;
        assert_eq!(frame.kind, Kind::Video);
        let (packet, data) = VideoPacket::decode(&frame.payload).unwrap();
        Some((frame.request_id, packet, data.to_vec()))
    }

    /// Read to the `end` packet of `generation`; returns (packets, bytes, keys)
    /// of the config and media packets seen on the way.
    fn drain_to_end(stream: &mut UnixStream, generation: u32) -> (u64, u64, u64) {
        let mut seen = (0, 0, 0);
        loop {
            let (_, header, data) = packet(stream).expect("an end packet before EOF");
            assert_eq!(header.generation, generation);
            if header.kind == VideoPacketKind::End {
                return seen;
            }
            seen.0 += 1;
            seen.1 += data.len() as u64;
            seen.2 += u64::from(header.key_frame);
        }
    }

    #[test]
    fn stream_cycles_keep_packet_order_and_generations_while_control_answers() {
        let source = Arc::new(Synthetic::new());
        let mut session = Session::start(&source, FAST);
        assert_eq!(session.channel(), "pending");
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        let mut video = session.attach();

        let started = session.command("video.start");
        let start_id = session.next_id;
        assert_eq!(
            started,
            json!({
                "ok": true, "generation": 1, "display": 0, "codec": "synthetic.avc",
                "accepted": {"width": 360, "height": 640, "fps": 15, "bitrate": 1_500_000,
                             "key_frame_interval_seconds": 1},
            })
        );
        assert_eq!(code(&session.command("video.start")), "already_streaming");

        let (id, config, data) = packet(&mut video).unwrap();
        assert_eq!(id, start_id);
        assert_eq!(
            config,
            VideoPacket {
                kind: VideoPacketKind::Config,
                key_frame: false,
                generation: 1,
                pts_us: 0,
                width: 360,
                height: 640
            }
        );
        assert_eq!(data, SPS_PPS);
        let mut seen = (1_u64, data.len() as u64, 0_u64);
        let mut last_pts = -1;
        for index in 0..40 {
            let (id, media, data) = packet(&mut video).unwrap();
            assert_eq!((id, media.kind), (start_id, VideoPacketKind::Media));
            assert_eq!((media.generation, media.width, media.height), (1, 360, 640));
            assert_eq!(
                media.key_frame,
                index == 0,
                "the first media packet is the key frame"
            );
            assert!(media.pts_us > last_pts);
            last_pts = media.pts_us;
            seen = (
                seen.0 + 1,
                seen.1 + data.len() as u64,
                seen.2 + u64::from(media.key_frame),
            );
            // Control answers at once while packets flow.
            session.ping_is_prompt();
        }

        let stopped = session.command("video.stop");
        let rest = drain_to_end(&mut video, 1);
        assert_eq!(
            stopped,
            json!({
                "ok": true, "generation": 1, "reason": "stopped",
                "packets": seen.0 + rest.0, "bytes": seen.1 + rest.1, "key_frames": 1,
            })
        );
        assert_eq!(code(&session.command("video.stop")), "not_streaming");
        assert!(!source.live.load(Ordering::SeqCst));

        // A second stream on the same channel: next generation, its own settings.
        let request = json!({"command": "video.start", "display": 0, "max_width": 721,
                             "max_height": 17, "fps": 60, "bitrate": 100_000,
                             "encoder": "default"});
        let started = session.call(&request.to_string());
        assert_eq!(started["generation"], 2);
        assert_eq!(
            started["accepted"],
            json!({"width": 720, "height": 16, "fps": 60, "bitrate": 100_000,
                   "key_frame_interval_seconds": 1})
        );
        let (id, config, _) = packet(&mut video).unwrap();
        assert_eq!(
            (id, config.kind),
            (session.next_id, VideoPacketKind::Config)
        );
        assert_eq!(
            (config.generation, config.width, config.height),
            (2, 720, 16)
        );
        let (_, media, _) = packet(&mut video).unwrap();
        assert!(media.key_frame && media.kind == VideoPacketKind::Media);
        let stopped = session.command("video.stop");
        assert_eq!(
            (&stopped["generation"], &stopped["reason"]),
            (&json!(2), &json!("stopped"))
        );
        drain_to_end(&mut video, 2);
        assert_eq!(source.runs.load(Ordering::SeqCst), 2);

        session.shutdown().unwrap();
        // Teardown closed the video channel too.
        assert!(packet(&mut video).is_none());
    }

    #[test]
    fn stalled_consumer_ends_the_stream_and_the_channel_but_not_control() {
        let source = Arc::new(Synthetic {
            interval: Duration::ZERO,
            frame_bytes: 256 * 1024,
            ..Synthetic::new()
        });
        let mut session = Session::start(&source, FAST);
        // Attached, then never read from.
        let video = session.attach();
        assert_eq!(session.command("video.start")["generation"], 1);
        // While the writer is stuck behind the full socket, control is unaffected.
        for _ in 0..20 {
            session.ping_is_prompt();
            std::thread::sleep(Duration::from_millis(10));
        }
        session.wait_for_channel("closed");
        session.ping_is_prompt();
        let stopped = session.command("video.stop");
        assert_eq!(
            (&stopped["ok"], &stopped["reason"], &stopped["generation"]),
            (&json!(true), &json!("consumer_timeout"), &json!(1))
        );
        assert!(stopped.get("error_code").is_none());
        assert!(!source.live.load(Ordering::SeqCst));
        // A half-written frame cannot be resumed: the channel is gone for good.
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        assert_eq!(code(&session.command("video.stop")), "not_streaming");
        session.ping_is_prompt();
        drop(video);
        session.shutdown().unwrap();
    }

    #[test]
    fn anything_from_the_client_on_the_video_channel_closes_only_that_channel() {
        // A byte while streaming.
        let source = Arc::new(Synthetic::new());
        let mut session = Session::start(&source, FAST);
        let mut video = session.attach();
        assert_eq!(session.command("video.start")["generation"], 1);
        assert_eq!(packet(&mut video).unwrap().1.kind, VideoPacketKind::Config);
        video.write_all(b"x").unwrap();
        session.wait_for_channel("closed");
        let stopped = session.command("video.stop");
        assert_eq!(stopped["reason"], "channel_closed");
        assert!(!source.live.load(Ordering::SeqCst));
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        session.ping_is_prompt();
        session.shutdown().unwrap();

        // A whole valid frame while idle, and a plain close while idle.
        for close in [false, true] {
            let mut session = Session::start(&source, FAST);
            let mut video = session.attach();
            if close {
                drop(video);
            } else {
                protocol::write_frame(&mut video, Kind::Control, 1, br#"{"command":"ping"}"#)
                    .unwrap();
                // Nothing is answered on this channel: it is simply closed.
                assert!(matches!(
                    protocol::read_frame(&mut video),
                    Ok(None) | Err(_)
                ));
            }
            session.wait_for_channel("closed");
            assert_eq!(
                code(&session.command("video.start")),
                "video_channel_absent"
            );
            assert_eq!(code(&session.command("video.stop")), "not_streaming");
            session.shutdown().unwrap();
        }
    }

    #[test]
    fn attach_needs_the_session_and_failed_attempts_are_bounded() {
        let source = Arc::new(Synthetic::new());
        let attach = |session: &str| {
            json!({"command": "attach", "channel": "video", "session": session}).to_string()
        };
        let mut session = Session::start(&source, FAST);
        // Two refused attempts leave control and the open slot untouched.
        let (mut refused, reply) = session.attach_with(&attach(&"0".repeat(32)));
        assert_eq!(code(&reply.unwrap()), "invalid_session");
        assert!(matches!(
            protocol::read_frame(&mut refused),
            Ok(None) | Err(_)
        ));
        session.ping_is_prompt();
        assert_eq!(session.channel(), "pending");
        let hello = format!(r#"{{"command":"hello","protocol":1,"session":"{SESSION}"}}"#);
        let (_, reply) = session.attach_with(&hello);
        assert_eq!(code(&reply.unwrap()), "attach_required");
        assert_eq!(session.channel(), "pending");
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        let mut video = session.attach();
        assert_eq!(session.command("video.start")["generation"], 1);
        assert_eq!(packet(&mut video).unwrap().1.kind, VideoPacketKind::Config);
        session.shutdown().unwrap();

        // Every kind of refusal counts; the third one closes the listener.
        let mut session = Session::start(&source, FAST);
        let wrong_channel = json!({"command": "attach", "channel": "control", "session": SESSION});
        let (_, reply) = session.attach_with(&wrong_channel.to_string());
        assert_eq!(code(&reply.unwrap()), "unsupported_channel");
        let (_, reply) = session.attach_with("not json");
        assert_eq!(code(&reply.unwrap()), "invalid_request");
        let mut garbage = session.connect().unwrap();
        garbage.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let rejection = protocol::read_frame(&mut garbage).unwrap().unwrap();
        assert_eq!(rejection.payload, br#"{"error":{"code":"bad_magic"}}"#);
        session.wait_for_channel("absent");
        assert!(session.connect().is_err());
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        session.ping_is_prompt();
        session.shutdown().unwrap();
    }

    #[test]
    fn absent_or_stalled_video_peer_is_cut_by_deadlines_without_delaying_control() {
        let source = Arc::new(Synthetic::new());
        // Nobody attaches: the listener closes at the attach deadline.
        let quick = Timings {
            attach: Duration::from_millis(300),
            ..FAST
        };
        let mut session = Session::start(&source, quick);
        let started = Instant::now();
        assert_eq!(session.channel(), "pending");
        session.wait_for_channel("absent");
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(session.connect().is_err());
        assert_eq!(
            code(&session.command("video.start")),
            "video_channel_absent"
        );
        session.shutdown().unwrap();

        // A peer that stalls inside its attach frame is dropped at the handshake
        // deadline; control keeps answering meanwhile and the slot stays open.
        let mut session = Session::start(&source, FAST);
        let mut stalled = session.connect().unwrap();
        stalled.write_all(b"CRLO\x01\x01").unwrap();
        let started = Instant::now();
        while started.elapsed() < FAST.attach_handshake + Duration::from_millis(100) {
            session.ping_is_prompt();
            assert_eq!(session.channel(), "pending");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut sink = Vec::new();
        assert!(matches!(stalled.read_to_end(&mut sink), Ok(0) | Err(_)));
        let _video = session.attach();
        session.shutdown().unwrap();

        // The attach deadline also cuts a stalled peer: never later than that.
        let mut session = Session::start(&source, quick);
        let mut stalled = session.connect().unwrap();
        stalled.write_all(b"CRLO").unwrap();
        session.wait_for_channel("absent");
        assert!(session.connect().is_err());
        session.shutdown().unwrap();
    }

    #[test]
    fn start_failures_are_typed_and_do_not_consume_a_generation() {
        let unavailable = Arc::new(Synthetic {
            available: false,
            ..Synthetic::new()
        });
        let mut session = Session::start(&unavailable, FAST);
        let _video = session.attach();
        assert_eq!(
            session.command("video.start"),
            json!({"ok": false,
                   "error": {"code": "unavailable", "reason": "unsupported_profile"}})
        );
        assert_eq!(code(&session.command("video.stop")), "not_streaming");
        assert_eq!(unavailable.runs.load(Ordering::SeqCst), 0);
        session.shutdown().unwrap();

        let source = Arc::new(Synthetic::new());
        let mut session = Session::start(&source, FAST);
        let mut video = session.attach();
        for failure in [
            "permission_denied",
            "display_absent",
            "unsupported_format",
            "display_refused",
            "timeout",
            "internal",
        ] {
            *source.failure.lock().unwrap() = Some(failure);
            let body = session.command("video.start");
            assert_eq!(code(&body), failure);
            assert_eq!(body["error"].as_object().unwrap().len(), 1);
            assert_eq!(code(&session.command("video.stop")), "not_streaming");
        }
        *source.failure.lock().unwrap() = None;

        // A source slower than the start deadline: typed timeout, the late
        // source is told to stop, writes nothing and is reaped by video.stop.
        *source.start_delay.lock().unwrap() = FAST.start + Duration::from_millis(300);
        let (body, elapsed) = session.timed(r#"{"command":"video.start"}"#);
        assert_eq!(code(&body), "timeout");
        assert!(
            elapsed < FAST.start + Duration::from_millis(250),
            "{elapsed:?}"
        );
        assert_eq!(code(&session.command("video.start")), "already_streaming");
        assert_eq!(code(&session.command("video.stop")), "not_streaming");
        assert!(!source.live.load(Ordering::SeqCst));
        *source.start_delay.lock().unwrap() = Duration::ZERO;

        // None of the failures took a generation or put a packet on the wire.
        assert_eq!(session.command("video.start")["generation"], 1);
        let (_, first, _) = packet(&mut video).unwrap();
        assert_eq!((first.kind, first.generation), (VideoPacketKind::Config, 1));
        session.shutdown().unwrap();
    }

    #[test]
    fn encoder_output_outside_the_packet_contract_ends_the_stream_with_a_code() {
        for (config, key_first, expected, config_packets) in [
            (false, true, "media_before_config", 0),
            (true, false, "first_frame_not_key", 1),
        ] {
            let source = Arc::new(Synthetic {
                config,
                key_first,
                ..Synthetic::new()
            });
            let mut session = Session::start(&source, FAST);
            let mut video = session.attach();
            assert_eq!(session.command("video.start")["generation"], 1);
            // No media packet is ever sent; the generation is still closed.
            assert_eq!(
                drain_to_end(&mut video, 1),
                (config_packets, 12 * config_packets, 0)
            );
            let stopped = session.command("video.stop");
            assert_eq!(
                stopped,
                json!({"ok": true, "generation": 1, "reason": "error", "error_code": expected,
                       "packets": config_packets, "bytes": 12 * config_packets,
                       "key_frames": 0})
            );
            // The channel was never out of step: a new stream may start.
            assert_eq!(session.channel(), "attached");
            assert_eq!(session.command("video.start")["generation"], 2);
            session.shutdown().unwrap();
        }
    }

    #[test]
    fn control_end_tears_down_stream_channel_and_listener() {
        // Control EOF while streaming.
        let source = Arc::new(Synthetic::new());
        let mut session = Session::start(&source, FAST);
        let mut video = session.attach();
        assert_eq!(session.command("video.start")["generation"], 1);
        assert_eq!(packet(&mut video).unwrap().1.kind, VideoPacketKind::Config);
        session.control.shutdown(Shutdown::Both).unwrap();
        session.join().unwrap();
        // The stream was stopped in order: its end packet, then the close.
        drain_to_end(&mut video, 1);
        assert!(packet(&mut video).is_none());
        assert!(!source.live.load(Ordering::SeqCst));

        // Control EOF while the video channel is still pending, with a peer
        // stalled in its attach: the listener is closed at once all the same.
        let mut session = Session::start(&source, Timings::DEFAULT);
        let mut stalled = session.connect().unwrap();
        stalled.write_all(b"CR").unwrap();
        let started = Instant::now();
        session.control.shutdown(Shutdown::Both).unwrap();
        session.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut sink = Vec::new();
        assert!(matches!(stalled.read_to_end(&mut sink), Ok(0) | Err(_)));
    }

    #[test]
    fn video_start_requests_are_validated_without_echo() {
        let parsed = |request: Value| parse(&request);
        let default = Params {
            width: 360,
            height: 640,
            fps: 15,
            bitrate: 1_500_000,
        };
        assert_eq!(parsed(json!({})), Ok(default));
        assert_eq!(
            parsed(
                json!({"display": 0, "max_width": 360, "max_height": 640, "fps": 15,
                          "bitrate": 1_500_000, "encoder": "default", "unknown": [1]})
            ),
            Ok(default)
        );
        assert_eq!(
            parsed(json!({"max_width": 16, "max_height": 1920, "fps": 1, "bitrate": 20_000_000})),
            Ok(Params {
                width: 16,
                height: 1920,
                fps: 1,
                bitrate: 20_000_000
            })
        );
        // Odd sizes round down to even.
        assert_eq!(
            parsed(json!({"max_width": 1919, "max_height": 17})).map(|p| (p.width, p.height)),
            Ok((1918, 16))
        );
        for display in [
            json!(1),
            json!(-1),
            json!(7),
            json!(u64::MAX),
            json!(i64::MIN),
        ] {
            assert_eq!(
                parsed(json!({ "display": display })),
                Err("unsupported_display")
            );
        }
        for encoder in ["software", "hardware", "", "Default", "CLIENT_SENTINEL"] {
            assert_eq!(
                parsed(json!({ "encoder": encoder })),
                Err("unsupported_encoder")
            );
        }
        for request in [
            json!({"display": "0"}),
            json!({"display": 0.0}),
            json!({"display": null}),
            json!({"display": [0]}),
            json!({"max_width": 15}),
            json!({"max_width": 1921}),
            json!({"max_width": "360"}),
            json!({"max_width": 360.5}),
            json!({"max_width": -360}),
            json!({"max_height": 0}),
            json!({"max_height": 4096}),
            json!({"max_height": null}),
            json!({"fps": 0}),
            json!({"fps": 61}),
            json!({"fps": true}),
            json!({"bitrate": 99_999}),
            json!({"bitrate": 20_000_001}),
            json!({"bitrate": u64::MAX}),
            json!({"encoder": 1}),
            json!({"encoder": null}),
            // An invalid field wins over an unsupported one.
            json!({"display": 5, "fps": 0}),
            json!({"encoder": "software", "bitrate": 1}),
        ] {
            assert_eq!(parsed(request.clone()), Err("invalid_request"), "{request}");
        }
    }

    #[test]
    fn attach_frame_rules() {
        let frame = |kind, body: Value| Frame {
            kind,
            request_id: 1,
            payload: body.to_string().into_bytes(),
        };
        let valid = json!({"command": "attach", "channel": "video", "session": SESSION});
        assert_eq!(
            attach_request(&frame(Kind::Control, valid.clone()), SESSION),
            Ok(())
        );
        assert_eq!(
            attach_request(&frame(Kind::Video, valid.clone()), SESSION),
            Err("unexpected_kind")
        );
        for (change, expected) in [
            (json!({"command": "hello"}), "attach_required"),
            (json!({"command": 7}), "invalid_request"),
            (json!({"channel": "control"}), "unsupported_channel"),
            (json!({"channel": null}), "unsupported_channel"),
            (
                json!({"session": SESSION.to_uppercase()}),
                "invalid_session",
            ),
            (json!({"session": &SESSION[..31]}), "invalid_session"),
            (json!({"session": [SESSION]}), "invalid_session"),
            (json!({"session": null}), "invalid_session"),
        ] {
            let mut body = valid.clone();
            for (key, value) in change.as_object().unwrap() {
                body[key] = value.clone();
            }
            assert_eq!(
                attach_request(&frame(Kind::Control, body), SESSION),
                Err(expected)
            );
        }
        let raw = Frame {
            kind: Kind::Control,
            request_id: 1,
            payload: b"\xff".to_vec(),
        };
        assert_eq!(attach_request(&raw, SESSION), Err("invalid_request"));
        assert_eq!(Timings::DEFAULT.attach, Duration::from_secs(10));
        assert!(Timings::DEFAULT.attach_handshake < Timings::DEFAULT.attach);
    }
}
