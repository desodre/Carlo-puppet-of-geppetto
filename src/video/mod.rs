//! Synthetic H.264 encoder proof (P2.1). Frames come from EGL/GLES clears, never
//! from the screen: success here is not display capture and not a streaming API.
//! The only path that reads a display is the profile-gated `mirror` module.
//!
//! Stream accounting and Annex B inspection are safe Rust, exercised on the host.

#![cfg_attr(
    not(target_os = "android"),
    allow(
        dead_code,
        reason = "Android FFI constructors; host exercises pure stream accounting"
    )
)]

use crate::{diagnostics::Diagnostics, error::CarloError};
use serde::Serialize;

pub(crate) mod mirror;
#[cfg(target_os = "android")]
mod native;
pub(crate) mod stream;
pub(crate) mod surface;

/// Per-packet bound from the plan's initial limits.
const MAX_PACKET_BYTES: usize = 8 * 1024 * 1024;
/// Whole-probe bound; the fixed preset produces a few hundred KiB.
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
/// `AMEDIACODEC_BUFFER_FLAG_*` bits of one encoder output buffer.
pub(crate) const FLAG_KEY_FRAME: u32 = 1;
pub(crate) const FLAG_CODEC_CONFIG: u32 = 2;
pub(crate) const FLAG_END_OF_STREAM: u32 = 4;

/// Shell profiles on which this probe has actually run (API level, architecture).
/// Only public NDK exports are used, but other profiles stay unavailable until executed.
const EXECUTED_PROFILES: [(u32, &str); 1] = [(36, "aarch64")];

fn executed(shell_api: Option<u32>, architecture: &str) -> bool {
    shell_api.is_some_and(|api| EXECUTED_PROFILES.contains(&(api, architecture)))
}

/// Fixed spike preset (first comparison preset of the plan); not a public option yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Config {
    width: i32,
    height: i32,
    frames_per_second: i32,
    bits_per_second: i32,
    key_frame_interval_seconds: i32,
    frames: u32,
}

pub(crate) const PRESET: Config = Config {
    width: 360,
    height: 640,
    frames_per_second: 15,
    bits_per_second: 1_500_000,
    key_frame_interval_seconds: 1,
    frames: 30,
};

impl Config {
    /// Upper bound on output buffers: one per frame plus config/EOS, with slack
    /// for encoders that split parameter sets.
    fn max_packets(&self) -> u32 {
        self.frames.saturating_mul(2).saturating_add(16)
    }
}

/// Failures keep fixed operation names and numeric platform codes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub(crate) enum Error {
    UnsupportedProfile,
    LibraryUnavailable(&'static str),
    MissingSymbol(&'static str),
    /// No H.264 encoder could be instantiated (absent codec or denied access).
    EncoderUnavailable,
    /// `media_status_t` from a named NDK media call; configure covers format rejection.
    Codec {
        operation: &'static str,
        status: i32,
    },
    /// `eglGetError` after a named EGL call.
    Egl {
        operation: &'static str,
        code: i32,
    },
    /// `glGetError` after drawing a synthetic frame.
    Gl {
        code: u32,
    },
    Timeout(&'static str),
    InvalidStream(&'static str),
    LimitExceeded(&'static str),
    /// `binder_status_t` from a named NDK parcel or window-flattening call.
    Parcel {
        operation: &'static str,
        status: i32,
    },
    /// A flattened surface field outside the pinned layout.
    UnexpectedParcel(&'static str),
    /// A named platform service was absent or its descriptor did not match.
    ServiceUnavailable(&'static str),
    /// The process Binder thread pool could not be owned by this executable.
    BinderPool(&'static str),
    /// `binder_status_t` from preparing or sending a named transaction.
    Binder {
        operation: &'static str,
        status: i32,
    },
    /// The service threw `SecurityException` for the named transaction.
    PermissionDenied(&'static str),
    /// The service threw `IllegalArgumentException` for the named transaction.
    IllegalArgument(&'static str),
    /// Any other exception code in a reply header; its message is never read out.
    RemoteException {
        operation: &'static str,
        code: i32,
    },
    /// The encoder refused the requested configuration in the named codec call,
    /// before anything was asked of the display service.
    UnsupportedFormat {
        operation: &'static str,
        status: i32,
    },
    /// The requested display id is not in the display service's own list; no
    /// display was asked for.
    DisplayAbsent,
    /// The display service answered with a negative display id (limit or refusal).
    DisplayRefused,
    /// The reply named the default display: the request was not read as pinned.
    DefaultDisplayReturned,
    /// A fixed internal invariant failed (null handle, poisoned state).
    Runtime(&'static str),
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
struct NalUnits {
    sps: u32,
    pps: u32,
    idr: u32,
    non_idr: u32,
    other: u32,
}

/// Counters over drained encoder output. Never holds or logs pixel content.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Stream {
    packets: u32,
    bytes: usize,
    config_packets: u32,
    key_frames: u32,
    end_of_stream: bool,
    first_pts_us: Option<i64>,
    last_pts_us: Option<i64>,
    pts_non_decreasing: bool,
    nal_units: NalUnits,
}

impl Stream {
    fn coded_frames(&self) -> u32 {
        self.nal_units.idr + self.nal_units.non_idr
    }
}

/// Bounded sink for drained output; keeps bytes only when the caller asked for data.
pub(crate) struct Collector {
    stream: Stream,
    max_packets: u32,
    data: Option<Vec<u8>>,
}

impl Collector {
    pub(crate) fn new(config: &Config, keep_data: bool) -> Self {
        Self {
            stream: Stream {
                pts_non_decreasing: true,
                ..Stream::default()
            },
            max_packets: config.max_packets(),
            data: keep_data.then(Vec::new),
        }
    }

    /// Account one encoder output buffer. Limits are checked before any copy.
    pub(crate) fn push(&mut self, flags: u32, pts_us: i64, data: &[u8]) -> Result<(), Error> {
        if self.stream.end_of_stream {
            return Err(Error::InvalidStream("output_after_end_of_stream"));
        }
        if self.stream.packets >= self.max_packets {
            return Err(Error::LimitExceeded("packet_count"));
        }
        if data.len() > MAX_PACKET_BYTES {
            return Err(Error::LimitExceeded("packet_bytes"));
        }
        if data.len() > MAX_STREAM_BYTES - self.stream.bytes {
            return Err(Error::LimitExceeded("stream_bytes"));
        }
        let units = scan(data)?;
        self.stream.packets += 1;
        self.stream.bytes += data.len();
        self.stream.nal_units.sps += units.sps;
        self.stream.nal_units.pps += units.pps;
        self.stream.nal_units.idr += units.idr;
        self.stream.nal_units.non_idr += units.non_idr;
        self.stream.nal_units.other += units.other;
        if flags & FLAG_CODEC_CONFIG != 0 {
            self.stream.config_packets += 1;
        } else if !data.is_empty() {
            if flags & FLAG_KEY_FRAME != 0 {
                self.stream.key_frames += 1;
            }
            if self.stream.last_pts_us.is_some_and(|last| pts_us < last) {
                self.stream.pts_non_decreasing = false;
            }
            self.stream.first_pts_us.get_or_insert(pts_us);
            self.stream.last_pts_us = Some(pts_us);
        }
        if flags & FLAG_END_OF_STREAM != 0 {
            self.stream.end_of_stream = true;
        }
        if let Some(buffer) = &mut self.data {
            buffer.extend_from_slice(data);
        }
        Ok(())
    }

    pub(crate) fn ended(&self) -> bool {
        self.stream.end_of_stream
    }
}

/// Destination of drained encoder output. Called only on the thread that owns
/// the encoder, while the dequeued buffer is still held: an implementation must
/// bound whatever it does with `data` and must not keep the slice.
pub(crate) trait Sink {
    /// Take one output buffer with its `FLAG_*` bits.
    fn push(&mut self, flags: u32, pts_us: i64, data: &[u8]) -> Result<(), Error>;
    /// Whether the end-of-stream buffer was pushed.
    fn ended(&self) -> bool;
    /// Output dimensions announced by the encoder before its first buffer.
    fn format(&mut self, _width: Option<i32>, _height: Option<i32>) {}
}

impl Sink for Collector {
    fn push(&mut self, flags: u32, pts_us: i64, data: &[u8]) -> Result<(), Error> {
        Collector::push(self, flags, pts_us, data)
    }

    fn ended(&self) -> bool {
        Collector::ended(self)
    }
}

/// Count NAL unit types of one Annex B packet. A nonempty packet must start with a
/// start code; emulation prevention guarantees `00 00 01` never occurs in a payload.
fn scan(data: &[u8]) -> Result<NalUnits, Error> {
    let mut units = NalUnits::default();
    if data.is_empty() {
        return Ok(units);
    }
    if !(data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])) {
        return Err(Error::InvalidStream("missing_start_code"));
    }
    let mut position = 0;
    while let Some(found) = data[position..].windows(3).position(|w| w == [0, 0, 1]) {
        let header = position + found + 3;
        let Some(byte) = data.get(header) else {
            return Err(Error::InvalidStream("truncated_nal_header"));
        };
        if byte & 0x80 != 0 {
            return Err(Error::InvalidStream("forbidden_zero_bit"));
        }
        match byte & 0x1f {
            1 => units.non_idr += 1,
            5 => units.idr += 1,
            7 => units.sps += 1,
            8 => units.pps += 1,
            _ => units.other += 1,
        }
        position = header;
    }
    Ok(units)
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Outcome {
    codec_name: Option<String>,
    output_width: Option<i32>,
    output_height: Option<i32>,
    frames_submitted: u32,
    stream: Stream,
    error: Option<Error>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    schema_version: u32,
    environment: Diagnostics,
    /// Frames are GLES clears; nothing is read from any display.
    source: &'static str,
    screen_capture: bool,
    video_available: bool,
    requested: Config,
    outcome: Outcome,
    #[serde(skip)]
    data: Option<Vec<u8>>,
}

impl Report {
    /// Every submitted frame came back coded, decodable from its own parameter
    /// sets and first IDR, and the encoder acknowledged end of stream.
    pub(crate) fn passed(&self) -> bool {
        let stream = &self.outcome.stream;
        self.outcome.error.is_none()
            && stream.end_of_stream
            && stream.nal_units.sps >= 1
            && stream.nal_units.pps >= 1
            && stream.nal_units.idr >= 1
            && self.outcome.frames_submitted == self.requested.frames
            && stream.coded_frames() == self.requested.frames
    }

    /// Annex B bytes when requested and the probe passed; consumed once.
    pub(crate) fn take_data(&mut self) -> Option<Vec<u8>> {
        if self.passed() {
            self.data.take()
        } else {
            None
        }
    }
}

/// Encode the fixed synthetic preset on the executed reference profile.
///
/// MediaCodec/EGL calls are synchronous and not cancellable: run under the bounded
/// host session helper, which terminates the process on timeout.
pub(crate) fn collect(keep_data: bool) -> Result<Report, CarloError> {
    let environment = Diagnostics::collect()?;
    let supported = executed(environment.shell_api(), std::env::consts::ARCH);
    let mut collector = Collector::new(&PRESET, keep_data);
    let mut outcome = Outcome::default();
    outcome.error = if supported {
        encode(&mut collector, &mut outcome).err()
    } else {
        Some(Error::UnsupportedProfile)
    };
    outcome.stream = collector.stream;
    Ok(Report {
        schema_version: 1,
        environment,
        source: "synthetic_egl_gles",
        screen_capture: false,
        video_available: false,
        requested: PRESET,
        outcome,
        data: collector.data,
    })
}

#[cfg(target_os = "android")]
fn encode(collector: &mut Collector, outcome: &mut Outcome) -> Result<(), Error> {
    native::encode(&PRESET, collector, outcome)
}

#[cfg(not(target_os = "android"))]
fn encode(_: &mut Collector, _: &mut Outcome) -> Result<(), Error> {
    unreachable!("profile requires Android")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84];
    const P: &[u8] = &[0, 0, 1, 0x41, 0x9a];

    fn report(collector: Collector, frames_submitted: u32, error: Option<Error>) -> Report {
        Report {
            schema_version: 1,
            environment: Diagnostics::collect().unwrap(),
            source: "synthetic_egl_gles",
            screen_capture: false,
            video_available: false,
            requested: PRESET,
            outcome: Outcome {
                frames_submitted,
                stream: collector.stream,
                error,
                ..Outcome::default()
            },
            data: collector.data,
        }
    }

    fn complete(keep_data: bool) -> Collector {
        let mut collector = Collector::new(&PRESET, keep_data);
        collector.push(FLAG_CODEC_CONFIG, 0, SPS_PPS).unwrap();
        collector.push(FLAG_KEY_FRAME, 0, IDR).unwrap();
        for frame in 1..PRESET.frames {
            collector.push(0, i64::from(frame) * 66_666, P).unwrap();
        }
        collector.push(FLAG_END_OF_STREAM, 0, &[]).unwrap();
        collector
    }

    #[test]
    fn complete_stream_passes_and_data_is_exactly_the_drained_bytes() {
        let collector = complete(true);
        assert_eq!(collector.stream.packets, PRESET.frames + 2);
        assert_eq!(collector.stream.config_packets, 1);
        assert_eq!(collector.stream.key_frames, 1);
        assert_eq!(collector.stream.first_pts_us, Some(0));
        assert!(collector.stream.pts_non_decreasing);
        assert_eq!(
            collector.stream.nal_units,
            NalUnits {
                sps: 1,
                pps: 1,
                idr: 1,
                non_idr: PRESET.frames - 1,
                other: 0
            }
        );
        let mut report = report(collector, PRESET.frames, None);
        assert!(report.passed());
        let data = report.take_data().unwrap();
        assert_eq!(data.len(), report.outcome.stream.bytes);
        assert!(data.starts_with(SPS_PPS));
        assert!(report.take_data().is_none());
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["screen_capture"], false);
        assert_eq!(json["video_available"], false);
        assert!(json.get("data").is_none());
    }

    #[test]
    fn incomplete_or_failed_streams_never_pass_or_release_data() {
        let mut missing_eos = Collector::new(&PRESET, true);
        missing_eos.push(FLAG_CODEC_CONFIG, 0, SPS_PPS).unwrap();
        missing_eos.push(FLAG_KEY_FRAME, 0, IDR).unwrap();
        let mut dropped = report(missing_eos, PRESET.frames, None);
        assert!(!dropped.passed());
        assert!(dropped.take_data().is_none());

        let failed = report(complete(true), PRESET.frames, Some(Error::Timeout("drain")));
        assert!(!failed.passed());
        assert!(!report(complete(false), PRESET.frames - 1, None).passed());

        let mut no_parameter_sets = Collector::new(&PRESET, false);
        no_parameter_sets.push(FLAG_KEY_FRAME, 0, IDR).unwrap();
        for _ in 1..PRESET.frames {
            no_parameter_sets.push(0, 1, P).unwrap();
        }
        no_parameter_sets.push(FLAG_END_OF_STREAM, 1, &[]).unwrap();
        assert!(!report(no_parameter_sets, PRESET.frames, None).passed());
    }

    #[test]
    fn malformed_packets_are_rejected_without_being_counted() {
        let mut collector = Collector::new(&PRESET, true);
        for (packet, reason) in [
            (&[0x65, 0x88][..], "missing_start_code"),
            (&[0, 0, 1][..], "truncated_nal_header"),
            (&[0, 0, 1, 0x65, 0, 0, 1][..], "truncated_nal_header"),
            (&[0, 0, 1, 0xe5][..], "forbidden_zero_bit"),
        ] {
            assert_eq!(
                collector.push(0, 0, packet),
                Err(Error::InvalidStream(reason))
            );
        }
        assert_eq!(collector.stream.packets, 0);
        assert_eq!(collector.stream.bytes, 0);
        assert!(collector.data.as_ref().unwrap().is_empty());
        assert_eq!(scan(&[0, 0, 1, 0x06, 0xff]).unwrap().other, 1);
    }

    #[test]
    fn limits_and_ordering_are_enforced_before_copying() {
        let mut collector = Collector::new(&PRESET, true);
        let oversized = vec![0_u8; MAX_PACKET_BYTES + 1];
        assert_eq!(
            collector.push(0, 0, &oversized),
            Err(Error::LimitExceeded("packet_bytes"))
        );
        collector.stream.bytes = MAX_STREAM_BYTES - 1;
        assert_eq!(
            collector.push(0, 0, P),
            Err(Error::LimitExceeded("stream_bytes"))
        );
        collector.stream.bytes = 0;
        for _ in 0..PRESET.max_packets() {
            collector.push(0, 5, P).unwrap();
        }
        assert_eq!(
            collector.push(0, 5, P),
            Err(Error::LimitExceeded("packet_count"))
        );

        let mut ordered = Collector::new(&PRESET, false);
        ordered.push(0, 10, P).unwrap();
        ordered.push(0, 9, P).unwrap();
        assert!(!ordered.stream.pts_non_decreasing);
        ordered.push(FLAG_END_OF_STREAM, 9, &[]).unwrap();
        assert!(ordered.ended());
        assert_eq!(
            ordered.push(0, 11, P),
            Err(Error::InvalidStream("output_after_end_of_stream"))
        );
    }

    #[test]
    fn only_executed_shell_profiles_are_supported() {
        assert!(executed(Some(36), "aarch64"));
        for (api, architecture) in [
            (Some(36), "x86_64"),
            (Some(34), "aarch64"),
            (Some(35), "aarch64"),
            (None, "aarch64"),
        ] {
            assert!(!executed(api, architecture));
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn unsupported_host_reports_unavailable_without_encoding() {
        let mut report = collect(true).unwrap();
        assert_eq!(report.outcome.error, Some(Error::UnsupportedProfile));
        assert!(!report.passed());
        assert!(report.take_data().is_none());
        assert_eq!(report.outcome.stream.packets, 0);
    }
}
