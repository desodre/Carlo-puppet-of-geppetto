//! Display mirror proof (P2.2-P2.5): after `IDisplayManager.getDisplayIds` showed
//! the target display, one private `IDisplayManager.createVirtualDisplay`
//! transaction hands the H.264 encoder's input surface to the display service with
//! `VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR`, the encoder output is drained, and the
//! display is released again. No Java, no EGL and no pixel access in this path.
//!
//! Three failures are classified apart from every other error ([`FailureClass`]):
//! the encoder rejecting the format (before the display service is contacted), a
//! target display the service does not list (before a display is asked for; the
//! service itself would silently mirror the default display instead), and the
//! service's `SecurityException`. `display-fault-probe` provokes each one.
//!
//! The contract is pinned in `docs/compat/samsung-a16x-api36-display.json` and is
//! valid for one exact build only: this vendor shifts the AOSP transaction codes,
//! so any other fingerprint, API level, architecture or identity is reported as an
//! unsupported profile before a single Binder call is made.
//!
//! Request construction, String8 packing, reply interpretation, callback
//! accounting and the success criterion are safe Rust exercised on the host. The
//! Android-only `ffi` module merely emits the ordered fields.
//!
//! Binder and codec calls are synchronous and are not assumed to be cancellable:
//! the waits this module controls are bounded by monotonic deadlines, and the
//! bounded host session is the backstop. If the process dies, the display service
//! destroys the display through the death link on the callback Binder.

use super::{Collector, Config, Error, PRESET, Stream};
use crate::{diagnostics::Diagnostics, error::CarloError};
use serde::Serialize;
use std::{
    ffi::CStr,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

/// The only build this contract was read from (`ro.build.fingerprint`).
const FINGERPRINT: &str = "samsung/a16xub/a16x:16/BP4A.251205.006/A166MUBSADZJA:user/release-keys";
const API: u32 = 36;
const ARCHITECTURE: &str = "aarch64";
const SHELL_UID: u32 = 2000;
const SYSTEM_UID: u32 = 1000;

/// `IDisplayManager.getDisplayIds` on the pinned build.
const LIST: u32 = 8;
/// `IDisplayManager.createVirtualDisplay` on the pinned build (AOSP 21 + 6).
const CREATE: u32 = 27;
/// `IDisplayManager.releaseVirtualDisplay` on the pinned build (AOSP 24 + 6).
const RELEASE: u32 = 30;
const ON_PAUSED: u32 = 1;
const ON_RESUMED: u32 = 2;
const ON_STOPPED: u32 = 3;

/// `DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR`; the only bit a mirror sends.
const FLAG_AUTO_MIRROR: i32 = 16;
/// `DisplayManager.VIRTUAL_DISPLAY_FLAG_SECURE`. Sent by the permission fault
/// session only: it needs `CAPTURE_SECURE_VIDEO_OUTPUT`, which shell does not hold.
const FLAG_SECURE: i32 = 4;
/// Bound on the `int[]` of `getDisplayIds`, checked before any element is read.
const MAX_DISPLAYS: i32 = 64;
/// Distance of the absent-display fault target above the highest listed id.
const ABSENT_OFFSET: i32 = 1000;
const DENSITY_DPI: i32 = 160;
const DEFAULT_DISPLAY: i32 = 0;
const DISPLAY_NAME: &CStr = c"carlo-mirror";
const PACKAGE: &CStr = c"com.android.shell";

/// Java exception codes of a reply header (`android.os.Parcel.EX_*`).
const EX_SECURITY: i32 = -1;
const EX_ILLEGAL_ARGUMENT: i32 = -3;
/// `binder_status_t` values returned to the display service.
const STATUS_OK: i32 = 0;
const STATUS_PERMISSION_DENIED: i32 = -1;
const STATUS_UNKNOWN_TRANSACTION: i32 = -74;

/// Capture ends at `PRESET.frames` coded frames or after this long.
const CAPTURE: Duration = Duration::from_secs(5);
/// Bound on waiting for the encoder's end of stream after the release.
const END_OF_STREAM: Duration = Duration::from_secs(3);

/// Exact build, API, architecture and shell identity; `shell_api` is only `Some`
/// when the foundation checks passed for the shell UID.
fn supported(
    shell_api: Option<u32>,
    effective_uid: Option<u32>,
    architecture: &str,
    fingerprint: Option<&str>,
) -> bool {
    shell_api == Some(API)
        && effective_uid == Some(SHELL_UID)
        && architecture == ARCHITECTURE
        && fingerprint == Some(FINGERPRINT)
}

/// One value of a request parcel, after the interface token. The FFI layer emits
/// these in slice order and adds nothing of its own.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    I32(i32),
    F32(f32),
    /// Java `writeString8`, emitted as the words of [`string8_words`].
    Str8(&'static CStr),
    /// A null `String8`: int32 -1.
    NullStr8,
    /// Java `writeString` (UTF-16), written by `AParcel_writeString`.
    Str16(&'static CStr),
    NullBinder,
    /// The local `IVirtualDisplayCallback` object of this session.
    CallbackBinder,
    /// `ANativeWindow_writeToParcel` of the encoder input window.
    Surface,
}

/// Which display a session mirrors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// This id, which must be in the service's list.
    Display(i32),
    /// [`ABSENT_OFFSET`] above the highest listed id: absent by construction
    /// (fault provocation only).
    BeyondListed,
}

/// What one mirror session asks for. Internal: no command line option reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    target: Target,
    /// Bits sent besides `AUTO_MIRROR`; none outside fault provocation.
    extra_flags: i32,
    config: Config,
}

impl Request {
    /// The only request the mirror commands send.
    const DEFAULT: Self = Self {
        target: Target::Display(DEFAULT_DISPLAY),
        extra_flags: 0,
        config: PRESET,
    };

    fn flags(&self) -> i32 {
        FLAG_AUTO_MIRROR | self.extra_flags
    }
}

/// `getDisplayIds(boolean includeDisabled)`: false.
const LIST_REQUEST: [Field; 1] = [Field::I32(0)];

/// `createVirtualDisplay(VirtualDisplayConfig, IVirtualDisplayCallback,
/// IMediaProjection, String packageName)` in the pinned field order.
/// `display_id` is the listed display [`located`] returned for the request.
fn create_request(request: &Request, display_id: i32) -> [Field; 22] {
    [
        Field::I32(1), // non-null VirtualDisplayConfig
        Field::Str8(DISPLAY_NAME),
        Field::I32(request.config.width),
        Field::I32(request.config.height),
        Field::I32(DENSITY_DPI),
        Field::I32(request.flags()),
        Field::I32(1), // non-null Surface
        Field::Surface,
        Field::NullStr8,        // uniqueId
        Field::I32(display_id), // displayIdToMirror
        Field::I32(0),          // windowManagerMirroringEnabled
        Field::I32(0),          // displayCategories: empty ArraySet
        Field::F32(0.0),        // requestedRefreshRate
        Field::I32(0),          // isHomeSupported
        Field::I32(-1),         // null DisplayCutout
        Field::I32(0),          // ignoreActivitySizeRestrictions
        Field::F32(0.0),        // defaultBrightness
        Field::F32(-1.0),       // dimBrightness
        Field::NullBinder,      // brightnessListener
        Field::CallbackBinder,  // IVirtualDisplayCallback
        Field::NullBinder,      // IMediaProjection
        Field::Str16(PACKAGE),  // packageName
    ]
}

/// `releaseVirtualDisplay(IVirtualDisplayCallback)`: the creating callback only.
const RELEASE_REQUEST: [Field; 1] = [Field::CallbackBinder];

/// Java `Parcel.writeString8` as int32 words: the UTF-8 byte length, then the bytes
/// plus one NUL, zero-padded to a 4-byte boundary and packed little-endian (the
/// byte order of every supported ABI, which `AParcel_writeInt32` writes natively).
/// libbinder_ndk has no String8 writer.
fn string8_words(text: &[u8]) -> Result<Vec<i32>, Error> {
    let length = i32::try_from(text.len()).map_err(|_| Error::LimitExceeded("string8_bytes"))?;
    let mut words = Vec::with_capacity(2 + text.len() / 4);
    words.push(length);
    let mut chunks = text.chunks_exact(4);
    for chunk in &mut chunks {
        words.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    // The remainder is 0..=3 bytes, so the terminator always fits this last word.
    let mut last = [0_u8; 4];
    let rest = chunks.remainder();
    last[..rest.len()].copy_from_slice(rest);
    words.push(i32::from_le_bytes(last));
    Ok(words)
}

/// Sequential reads over one reply. Implemented by the reply parcel on Android and
/// by a fake in host tests, so interpretation stays in safe code.
trait Reply {
    /// Exception code of the status header; the message is never surfaced.
    fn exception(&mut self, operation: &'static str) -> Result<i32, Error>;
    fn int32(&mut self, operation: &'static str) -> Result<i32, Error>;
}

fn check_exception(operation: &'static str, code: i32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        EX_SECURITY => Err(Error::PermissionDenied(operation)),
        EX_ILLEGAL_ARGUMENT => Err(Error::IllegalArgument(operation)),
        code => Err(Error::RemoteException { operation, code }),
    }
}

/// Status header, then the display id. The service answers -1 without an
/// exception when it refuses; 0 would be the default display, which this request
/// can never create, so it means the parcel was not read as pinned.
fn created_display(reply: &mut impl Reply) -> Result<i32, Error> {
    let exception = reply.exception("create_display")?;
    check_exception("create_display", exception)?;
    match reply.int32("read_display_id")? {
        id if id < 0 => Err(Error::DisplayRefused),
        DEFAULT_DISPLAY => Err(Error::DefaultDisplayReturned),
        id => Ok(id),
    }
}

fn released_display(reply: &mut impl Reply) -> Result<(), Error> {
    let exception = reply.exception("release_display")?;
    check_exception("release_display", exception)
}

/// Status header, then a Java `int[]` as `Parcel.writeIntArray` wrote it: the
/// count (-1 for a null array) and that many ids. The count is bounded before an
/// element is read; a null array is not something this service returns.
fn listed_displays(reply: &mut impl Reply) -> Result<Vec<i32>, Error> {
    let exception = reply.exception("list_displays")?;
    check_exception("list_displays", exception)?;
    let count = reply.int32("read_display_count")?;
    if !(0..=MAX_DISPLAYS).contains(&count) {
        return Err(Error::UnexpectedParcel("display_id_count"));
    }
    (0..count)
        .map(|_| reply.int32("read_listed_display_id"))
        .collect()
}

/// The display id `target` names, given the listed ids.
fn wanted(target: Target, listed: &[i32]) -> Result<i32, Error> {
    match target {
        Target::Display(id) => Ok(id),
        Target::BeyondListed => listed
            .iter()
            .copied()
            .max()
            .unwrap_or(DEFAULT_DISPLAY)
            .checked_add(ABSENT_OFFSET)
            .ok_or(Error::LimitExceeded("display_id")),
    }
}

/// `id` if the service lists it. The service does not reject an unknown
/// `displayIdToMirror` (it mirrors the default display instead), so absence has
/// to be decided here, before a display is asked for.
fn located(id: i32, listed: &[i32]) -> Result<i32, Error> {
    if listed.contains(&id) {
        Ok(id)
    } else {
        Err(Error::DisplayAbsent)
    }
}

/// An encoder error from the calls that validate the requested configuration
/// becomes [`Error::UnsupportedFormat`]; anything else is returned unchanged.
fn rejected_format(error: Error) -> Error {
    match error {
        Error::Codec {
            operation: operation @ ("configure" | "create_input_surface" | "start"),
            status,
        } if status != 0 => Error::UnsupportedFormat { operation, status },
        other => other,
    }
}

/// The three failures a caller must tell apart, derived from the typed error
/// alone. Every other error has no class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureClass {
    /// The display service threw `SecurityException`.
    PermissionDenied,
    /// The target display is not in the service's list.
    DisplayAbsent,
    /// The encoder rejected the requested configuration.
    UnsupportedFormat,
}

impl FailureClass {
    fn of(error: &Error) -> Option<Self> {
        match error {
            Error::PermissionDenied(_) => Some(Self::PermissionDenied),
            Error::DisplayAbsent => Some(Self::DisplayAbsent),
            Error::UnsupportedFormat { .. } => Some(Self::UnsupportedFormat),
            _ => None,
        }
    }
}

/// Transactions a session handed to the display service (attempts: counted just
/// before each send). Callback traffic and service lookup are not included.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
struct Transactions {
    list: u32,
    create: u32,
    release: u32,
}

/// State of the local `IVirtualDisplayCallback`. It is shared with Binder pool
/// threads, so it is `Sync` by construction: atomics only, no locks, no payload.
#[derive(Debug, Default)]
struct Callbacks {
    paused: AtomicU32,
    resumed: AtomicU32,
    stopped: AtomicU32,
    rejected: AtomicU32,
}

impl Callbacks {
    /// Account one incoming transaction and choose its `binder_status_t`. The
    /// methods are oneway and carry no arguments; nothing is read from the parcel.
    fn dispatch(&self, caller_uid: u32, code: u32) -> i32 {
        if caller_uid != SYSTEM_UID {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return STATUS_PERMISSION_DENIED;
        }
        let counter = match code {
            ON_PAUSED => &self.paused,
            ON_RESUMED => &self.resumed,
            ON_STOPPED => &self.stopped,
            _ => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                return STATUS_UNKNOWN_TRANSACTION;
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
        STATUS_OK
    }

    fn counts(&self) -> CallbackCounts {
        CallbackCounts {
            paused: self.paused.load(Ordering::Relaxed),
            resumed: self.resumed.load(Ordering::Relaxed),
            stopped: self.stopped.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

/// Callback transactions seen until the report was built. `rejected` counts wrong
/// callers and unknown codes. The oneway `onStopped` may arrive after the report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
struct CallbackCounts {
    paused: u32,
    resumed: u32,
    stopped: u32,
    rejected: u32,
}

/// What one mirror session did. Holds counters and numeric codes only: never an
/// exception message, a surface name or frame content.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Outcome {
    /// The service returned a display id above the default display.
    pub(super) display_created: bool,
    display_id: Option<i32>,
    /// A display was created and its release transaction was acknowledged.
    pub(super) display_released: bool,
    /// How many ids the service listed; the ids themselves are not reported.
    displays_listed: Option<usize>,
    /// The id that was looked up in that list.
    target_display_id: Option<i32>,
    transactions: Transactions,
    callbacks: CallbackCounts,
    codec_name: Option<String>,
    output_width: Option<i32>,
    output_height: Option<i32>,
    stream: Stream,
    elapsed_ms: u64,
    pub(super) error: Option<Error>,
    /// Class of `error`, when it is one of the three classified failures.
    failure_class: Option<FailureClass>,
    release_error: Option<Error>,
}

impl Outcome {
    /// Record how the session ended; the class always follows the error.
    fn conclude(&mut self, result: Result<(), Error>) {
        self.error = result.err();
        self.failure_class = self.error.as_ref().and_then(FailureClass::of);
    }

    /// An idle screen legitimately yields few frames: one coded frame that is
    /// decodable from its own parameter sets and IDR is enough, provided the
    /// display was created and released and nothing failed.
    fn passed(&self) -> bool {
        self.error.is_none()
            && self.release_error.is_none()
            && self.display_created
            && self.display_released
            && self.stream.coded_frames() >= 1
            && self.stream.nal_units.sps >= 1
            && self.stream.nal_units.pps >= 1
            && self.stream.nal_units.idr >= 1
    }
}

/// The display parameters that were (or would have been) sent.
#[derive(Debug, Serialize)]
struct DisplayRequest {
    flags: i32,
    density_dpi: i32,
    /// `None` when the id is derived from the listed ids (absent-display fault).
    display_id_to_mirror: Option<i32>,
}

impl DisplayRequest {
    fn of(request: &Request) -> Self {
        Self {
            flags: request.flags(),
            density_dpi: DENSITY_DPI,
            display_id_to_mirror: match request.target {
                Target::Display(id) => Some(id),
                Target::BeyondListed => None,
            },
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Report {
    schema_version: u32,
    environment: Diagnostics,
    runtime_fingerprint: Option<String>,
    /// Frames are the mirrored default display, composed by the platform.
    source: &'static str,
    /// True only when the success criterion held for this run.
    screen_capture: bool,
    /// No streaming protocol exists yet.
    video_available: bool,
    requested: Config,
    display: DisplayRequest,
    outcome: Outcome,
    #[serde(skip)]
    data: Option<Vec<u8>>,
}

impl Report {
    pub(crate) fn passed(&self) -> bool {
        self.outcome.passed()
    }

    #[cfg(test)]
    pub(super) fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// Annex B bytes when requested and the proof passed; consumed once.
    pub(crate) fn take_data(&mut self) -> Option<Vec<u8>> {
        if self.passed() {
            self.data.take()
        } else {
            None
        }
    }
}

/// Mirror the default display into the fixed encoder preset on the pinned profile.
///
/// The Binder and codec calls are synchronous and not cancellable: run under the
/// bounded host session helper, which terminates the process on timeout (the
/// display service then destroys the display through its death link).
pub(crate) fn collect(keep_data: bool) -> Result<Report, CarloError> {
    let environment = Diagnostics::collect()?;
    let fingerprint = crate::accessibility::fingerprint()?;
    Ok(run(environment, fingerprint, keep_data, probe))
}

/// One session of `request`, or none: when `permitted` is false `session` is never
/// invoked, so no library is opened and no Binder call is made.
fn attempt(
    permitted: bool,
    request: &Request,
    keep_data: bool,
    session: impl FnOnce(&Request, &mut Collector, &mut Outcome) -> Result<(), Error>,
) -> (Outcome, Option<Vec<u8>>) {
    let started = Instant::now();
    let mut collector = Collector::new(&request.config, keep_data);
    let mut outcome = Outcome::default();
    let result = if permitted {
        session(request, &mut collector, &mut outcome)
    } else {
        Err(Error::UnsupportedProfile)
    };
    outcome.conclude(result);
    outcome.stream = collector.stream;
    outcome.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    (outcome, collector.data)
}

/// Gate, then run `session` at most once with the default request.
fn run(
    environment: Diagnostics,
    fingerprint: Option<String>,
    keep_data: bool,
    session: impl FnOnce(&Request, &mut Collector, &mut Outcome) -> Result<(), Error>,
) -> Report {
    let permitted = supported(
        environment.shell_api(),
        environment.effective_uid(),
        std::env::consts::ARCH,
        fingerprint.as_deref(),
    );
    let (outcome, data) = attempt(permitted, &Request::DEFAULT, keep_data, session);
    Report {
        schema_version: 1,
        environment,
        runtime_fingerprint: fingerprint,
        source: "display_mirror",
        screen_capture: outcome.passed(),
        video_available: false,
        requested: Request::DEFAULT.config,
        display: DisplayRequest::of(&Request::DEFAULT),
        outcome,
        data,
    }
}

#[cfg(target_os = "android")]
fn probe(request: &Request, collector: &mut Collector, outcome: &mut Outcome) -> Result<(), Error> {
    ffi::mirror(request, collector, outcome)
}

#[cfg(not(target_os = "android"))]
fn probe(_: &Request, _: &mut Collector, _: &mut Outcome) -> Result<(), Error> {
    unreachable!("profile requires Android")
}

/// One provoked failure: the request that causes it and what must be observed.
struct Fault {
    name: &'static str,
    request: Request,
    expected: FailureClass,
    /// Exactly these transactions may have been handed to the display service.
    transactions: Transactions,
}

/// Encoder configuration of the fault sessions that reach the codec: the preset
/// with no frames, so a display the service unexpectedly created is released at
/// once instead of being captured.
const FRAMELESS: Config = Config {
    frames: 0,
    ..PRESET
};

/// The fault sessions, in execution order. The permission session runs first so
/// that its Binder pool start precedes every codec use of the process.
const FAULTS: [Fault; 3] = [
    // SECURE needs CAPTURE_SECURE_VIDEO_OUTPUT: the service throws at its
    // permission gate, before a display exists. The release owed by the guard is
    // still sent and is a no-op for a callback the service does not know.
    Fault {
        name: "permission",
        request: Request {
            target: Target::Display(DEFAULT_DISPLAY),
            extra_flags: FLAG_SECURE,
            config: FRAMELESS,
        },
        expected: FailureClass::PermissionDenied,
        transactions: Transactions {
            list: 1,
            create: 1,
            release: 1,
        },
    },
    Fault {
        name: "display_absent",
        request: Request {
            target: Target::BeyondListed,
            extra_flags: 0,
            config: FRAMELESS,
        },
        expected: FailureClass::DisplayAbsent,
        transactions: Transactions {
            list: 1,
            create: 0,
            release: 0,
        },
    },
    // The configuration the native encoder test already shows to be rejected.
    Fault {
        name: "unsupported_format",
        request: Request {
            target: Target::Display(DEFAULT_DISPLAY),
            extra_flags: 0,
            config: Config {
                width: 0,
                height: 0,
                ..FRAMELESS
            },
        },
        expected: FailureClass::UnsupportedFormat,
        transactions: Transactions {
            list: 0,
            create: 0,
            release: 0,
        },
    },
];

/// What one fault session was asked and did.
#[derive(Debug, Serialize)]
struct FaultSession {
    name: &'static str,
    expected: FailureClass,
    expected_transactions: Transactions,
    requested: Config,
    display: DisplayRequest,
    /// The expected class and transactions, and no display created or left.
    passed: bool,
    outcome: Outcome,
}

impl FaultSession {
    fn new(fault: &Fault, outcome: Outcome) -> Self {
        Self {
            name: fault.name,
            expected: fault.expected,
            expected_transactions: fault.transactions,
            requested: fault.request.config,
            display: DisplayRequest::of(&fault.request),
            passed: outcome.failure_class == Some(fault.expected)
                && outcome.transactions == fault.transactions
                && !outcome.display_created
                && outcome.release_error.is_none(),
            outcome,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct FaultReport {
    schema_version: u32,
    environment: Diagnostics,
    runtime_fingerprint: Option<String>,
    /// Failures are provoked on purpose; nothing is captured.
    source: &'static str,
    screen_capture: bool,
    video_available: bool,
    sessions: Vec<FaultSession>,
}

impl FaultReport {
    /// Every fault produced exactly its class and left no display behind.
    pub(crate) fn passed(&self) -> bool {
        self.sessions.len() == FAULTS.len() && self.sessions.iter().all(|session| session.passed)
    }
}

/// Provoke the three classified failures on the pinned profile, each in its own
/// session. Same bounds and gate as [`collect`]; no stream data is kept.
pub(crate) fn collect_faults() -> Result<FaultReport, CarloError> {
    let environment = Diagnostics::collect()?;
    let fingerprint = crate::accessibility::fingerprint()?;
    Ok(run_faults(environment, fingerprint, probe))
}

/// Gate, then run `session` once per fault. Outside the pinned profile every
/// session reports the unsupported profile and `session` is never invoked.
fn run_faults(
    environment: Diagnostics,
    fingerprint: Option<String>,
    session: impl FnMut(&Request, &mut Collector, &mut Outcome) -> Result<(), Error>,
) -> FaultReport {
    let permitted = supported(
        environment.shell_api(),
        environment.effective_uid(),
        std::env::consts::ARCH,
        fingerprint.as_deref(),
    );
    fault_report(permitted, environment, fingerprint, session)
}

fn fault_report(
    permitted: bool,
    environment: Diagnostics,
    runtime_fingerprint: Option<String>,
    mut session: impl FnMut(&Request, &mut Collector, &mut Outcome) -> Result<(), Error>,
) -> FaultReport {
    FaultReport {
        schema_version: 1,
        environment,
        runtime_fingerprint,
        source: "display_fault_provocation",
        screen_capture: false,
        video_available: false,
        sessions: FAULTS
            .iter()
            .map(|fault| {
                let (outcome, _) = attempt(permitted, &fault.request, false, &mut session);
                FaultSession::new(fault, outcome)
            })
            .collect(),
    }
}

/// libbinder_ndk and libnativewindow C ABI for the mirror session.
///
/// Public exports are pinned to the NDK 28.2.13676358 headers
/// `android/binder_ibinder.h`, `android/binder_parcel.h`, `android/binder_status.h`
/// and `android/native_window_aidl.h`. `AServiceManager_checkService` is a
/// libbinder_ndk platform export (frameworks/native
/// `libs/binder/ndk/include_platform/android/binder_manager.h`), as in
/// `crate::binder::native`. No C++ symbol is used and opaque handles are never
/// cast to another type.
///
/// Lifetime: the function table, both Binder classes and the thread pool live for
/// the process, because the display service and SurfaceFlinger keep calling into
/// this process (callback object, graphic buffer producer) until they drop their
/// references, possibly after the session ended.
///
/// Threads: every parcel and strong reference has one RAII owner that is created,
/// used and released on the session thread (`NonNull` keeps the owners `!Send`).
/// `on_transact`/`on_destroy` run on Binder pool threads; the only state they
/// touch is the `Sync` [`Callbacks`] behind an `Arc`, and no panic crosses the
/// FFI boundary (`catch_unwind`). `AIBinder_transact` blocks until the service
/// replies and cannot be cancelled; nested calls from the service are served on
/// the blocked thread and unrelated incoming calls on the pool.
#[cfg(target_os = "android")]
mod ffi {
    use super::{
        CAPTURE, CREATE, Callbacks, END_OF_STREAM, Field, LIST, LIST_REQUEST, Outcome, RELEASE,
        RELEASE_REQUEST, Reply, Request, create_request, created_display, listed_displays, located,
        rejected_format, released_display, string8_words, wanted,
    };
    use crate::{
        binder::{native::Library, pool},
        video::{
            Collector, Error, Outcome as EncoderOutcome,
            native::{ConfiguredInput, Window},
        },
    };
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr::{self, NonNull},
        sync::{Arc, OnceLock},
        time::Instant,
    };

    #[repr(C)]
    struct RawBinder {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct RawClass {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct RawParcel {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct RawStatus {
        _opaque: [u8; 0],
    }
    type OnCreate = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
    type OnDestroy = unsafe extern "C" fn(*mut c_void);
    type OnTransact =
        unsafe extern "C" fn(*mut RawBinder, u32, *const RawParcel, *mut RawParcel) -> i32;

    const STATUS_BAD_VALUE: i32 = -22;
    /// A status header plus one int or a bounded id list; an exception adds a
    /// bounded message and trace.
    const MAX_REPLY_BYTES: i32 = 64 * 1024;
    const DRAIN_SLICE_US: i64 = 10_000;
    /// Kernel-spawned pool workers besides the one `startThreadPool` creates.
    ///
    /// The automation session needs one. Mirroring needs more: the graphic buffer
    /// producer of the encoder is a local object, so SurfaceFlinger's compositor
    /// thread calls into this process for every frame and can sit in a blocking
    /// `dequeueBuffer` while the display service concurrently queries the surface
    /// or delivers callbacks. With no free worker those callers, the compositor
    /// included, would stall. Four leaves headroom and stays a small fixed bound;
    /// workers are only spawned on demand.
    const POOL_THREADS: u32 = 4;

    struct Api {
        _libraries: [Library; 2],
        check_service: unsafe extern "C" fn(*const c_char) -> *mut RawBinder,
        binder_release: unsafe extern "C" fn(*mut RawBinder),
        class_define:
            unsafe extern "C" fn(*const c_char, OnCreate, OnDestroy, OnTransact) -> *mut RawClass,
        binder_associate: unsafe extern "C" fn(*mut RawBinder, *const RawClass) -> bool,
        binder_new: unsafe extern "C" fn(*const RawClass, *mut c_void) -> *mut RawBinder,
        binder_user_data: unsafe extern "C" fn(*mut RawBinder) -> *mut c_void,
        calling_uid: unsafe extern "C" fn() -> u32,
        prepare: unsafe extern "C" fn(*mut RawBinder, *mut *mut RawParcel) -> i32,
        transact: unsafe extern "C" fn(
            *mut RawBinder,
            u32,
            *mut *mut RawParcel,
            *mut *mut RawParcel,
            u32,
        ) -> i32,
        parcel_delete: unsafe extern "C" fn(*mut RawParcel),
        parcel_size: unsafe extern "C" fn(*const RawParcel) -> i32,
        write_i32: unsafe extern "C" fn(*mut RawParcel, i32) -> i32,
        write_f32: unsafe extern "C" fn(*mut RawParcel, f32) -> i32,
        write_string: unsafe extern "C" fn(*mut RawParcel, *const c_char, i32) -> i32,
        write_binder: unsafe extern "C" fn(*mut RawParcel, *mut RawBinder) -> i32,
        read_i32: unsafe extern "C" fn(*const RawParcel, *mut i32) -> i32,
        read_status: unsafe extern "C" fn(*const RawParcel, *mut *mut RawStatus) -> i32,
        status_exception: unsafe extern "C" fn(*const RawStatus) -> i32,
        status_delete: unsafe extern "C" fn(*mut RawStatus),
        window_write: unsafe extern "C" fn(*mut Window, *mut RawParcel) -> i32,
    }

    // SAFETY: immutable C function pointers plus owned dlopen handles that are never
    // dropped (static table). Bionic permits calling resolved symbols from any thread;
    // thread affinity of parcels and Binder references is enforced by !Send owners.
    unsafe impl Send for Api {}
    // SAFETY: same invariant as Send; the table has no mutable state.
    unsafe impl Sync for Api {}

    impl Api {
        fn global() -> Result<&'static Self, Error> {
            static API: OnceLock<Result<Api, Error>> = OnceLock::new();
            API.get_or_init(Self::load).as_ref().map_err(Clone::clone)
        }

        fn load() -> Result<Self, Error> {
            fn open(name: &'static CStr, label: &'static str) -> Result<Library, Error> {
                Library::open(name).ok_or(Error::LibraryUnavailable(label))
            }
            let binder = open(c"libbinder_ndk.so", "libbinder_ndk.so")?;
            let window = open(c"libnativewindow.so", "libnativewindow.so")?;
            macro_rules! symbol {
                ($library:ident, $name:literal, $type:ty) => {{
                    let address = $library.symbol($name).ok_or(Error::MissingSymbol(
                        $name.to_str().expect("static ASCII symbol"),
                    ))?;
                    // SAFETY: every invocation names the exact C signature of this
                    // export in the pinned headers (binder_status_t, binder_exception_t
                    // and int32_t are i32, transaction_code_t, binder_flags_t and
                    // uid_t are u32, float is f32, bool is one byte, handles are
                    // opaque pointers). The owning library stays loaded for the process.
                    unsafe { std::mem::transmute::<*mut c_void, $type>(address.as_ptr()) }
                }};
            }
            Ok(Self {
                check_service: symbol!(
                    binder,
                    c"AServiceManager_checkService",
                    unsafe extern "C" fn(*const c_char) -> *mut RawBinder
                ),
                binder_release: symbol!(
                    binder,
                    c"AIBinder_decStrong",
                    unsafe extern "C" fn(*mut RawBinder)
                ),
                class_define: symbol!(
                    binder,
                    c"AIBinder_Class_define",
                    unsafe extern "C" fn(
                        *const c_char,
                        OnCreate,
                        OnDestroy,
                        OnTransact,
                    ) -> *mut RawClass
                ),
                binder_associate: symbol!(
                    binder,
                    c"AIBinder_associateClass",
                    unsafe extern "C" fn(*mut RawBinder, *const RawClass) -> bool
                ),
                binder_new: symbol!(
                    binder,
                    c"AIBinder_new",
                    unsafe extern "C" fn(*const RawClass, *mut c_void) -> *mut RawBinder
                ),
                binder_user_data: symbol!(
                    binder,
                    c"AIBinder_getUserData",
                    unsafe extern "C" fn(*mut RawBinder) -> *mut c_void
                ),
                calling_uid: symbol!(
                    binder,
                    c"AIBinder_getCallingUid",
                    unsafe extern "C" fn() -> u32
                ),
                prepare: symbol!(
                    binder,
                    c"AIBinder_prepareTransaction",
                    unsafe extern "C" fn(*mut RawBinder, *mut *mut RawParcel) -> i32
                ),
                transact: symbol!(
                    binder,
                    c"AIBinder_transact",
                    unsafe extern "C" fn(
                        *mut RawBinder,
                        u32,
                        *mut *mut RawParcel,
                        *mut *mut RawParcel,
                        u32,
                    ) -> i32
                ),
                parcel_delete: symbol!(
                    binder,
                    c"AParcel_delete",
                    unsafe extern "C" fn(*mut RawParcel)
                ),
                parcel_size: symbol!(
                    binder,
                    c"AParcel_getDataSize",
                    unsafe extern "C" fn(*const RawParcel) -> i32
                ),
                write_i32: symbol!(
                    binder,
                    c"AParcel_writeInt32",
                    unsafe extern "C" fn(*mut RawParcel, i32) -> i32
                ),
                write_f32: symbol!(
                    binder,
                    c"AParcel_writeFloat",
                    unsafe extern "C" fn(*mut RawParcel, f32) -> i32
                ),
                write_string: symbol!(
                    binder,
                    c"AParcel_writeString",
                    unsafe extern "C" fn(*mut RawParcel, *const c_char, i32) -> i32
                ),
                write_binder: symbol!(
                    binder,
                    c"AParcel_writeStrongBinder",
                    unsafe extern "C" fn(*mut RawParcel, *mut RawBinder) -> i32
                ),
                read_i32: symbol!(
                    binder,
                    c"AParcel_readInt32",
                    unsafe extern "C" fn(*const RawParcel, *mut i32) -> i32
                ),
                read_status: symbol!(
                    binder,
                    c"AParcel_readStatusHeader",
                    unsafe extern "C" fn(*const RawParcel, *mut *mut RawStatus) -> i32
                ),
                status_exception: symbol!(
                    binder,
                    c"AStatus_getExceptionCode",
                    unsafe extern "C" fn(*const RawStatus) -> i32
                ),
                status_delete: symbol!(
                    binder,
                    c"AStatus_delete",
                    unsafe extern "C" fn(*mut RawStatus)
                ),
                window_write: symbol!(
                    window,
                    c"ANativeWindow_writeToParcel",
                    unsafe extern "C" fn(*mut Window, *mut RawParcel) -> i32
                ),
                _libraries: [binder, window],
            })
        }
    }

    fn written(operation: &'static str, status: i32) -> Result<(), Error> {
        if status == 0 {
            Ok(())
        } else {
            Err(Error::Parcel { operation, status })
        }
    }

    fn transported(operation: &'static str, status: i32) -> Result<(), Error> {
        if status == 0 {
            Ok(())
        } else {
            Err(Error::Binder { operation, status })
        }
    }

    unsafe extern "C" fn on_create(data: *mut c_void) -> *mut c_void {
        data
    }

    /// # Safety
    /// Called once by libbinder_ndk with the user data of a destroyed local Binder.
    unsafe extern "C" fn on_destroy(data: *mut c_void) {
        if data.is_null() {
            return;
        }
        // SAFETY: `Strong::local` transferred exactly one Box<Arc<Callbacks>> to the
        // object; onDestroy runs once, after every reference and dispatch ended.
        let _ = std::panic::catch_unwind(|| unsafe {
            drop(Box::from_raw(data.cast::<Arc<Callbacks>>()));
        });
    }

    /// The remote service class only names the descriptor; nothing is served.
    unsafe extern "C" fn refuse(
        _: *mut RawBinder,
        _: u32,
        _: *const RawParcel,
        _: *mut RawParcel,
    ) -> i32 {
        super::STATUS_UNKNOWN_TRANSACTION
    }

    /// `IVirtualDisplayCallback` dispatch, on a Binder pool thread. The methods
    /// take no arguments, so the parcels are never read.
    ///
    /// # Safety
    /// Called by libbinder_ndk for a live local Binder created by `Strong::local`.
    unsafe extern "C" fn on_transact(
        binder: *mut RawBinder,
        code: u32,
        _: *const RawParcel,
        _: *mut RawParcel,
    ) -> i32 {
        std::panic::catch_unwind(|| {
            let Ok(api) = Api::global() else {
                return STATUS_BAD_VALUE;
            };
            if binder.is_null() {
                return STATUS_BAD_VALUE;
            }
            // SAFETY: the Binder is valid for the duration of onTransact; the calling
            // identity is that of the transaction being dispatched on this thread.
            let (uid, data) = unsafe { ((api.calling_uid)(), (api.binder_user_data)(binder)) };
            if data.is_null() {
                return STATUS_BAD_VALUE;
            }
            // SAFETY: user data is the Box<Arc<Callbacks>> from `Strong::local`;
            // onDestroy cannot run while this dispatch holds the object alive. The
            // target is Sync and only shared references are formed.
            let callbacks = unsafe { &*data.cast::<Arc<Callbacks>>() };
            callbacks.dispatch(uid, code)
        })
        .unwrap_or(STATUS_BAD_VALUE)
    }

    struct Classes {
        manager: NonNull<RawClass>,
        callback: NonNull<RawClass>,
    }

    // SAFETY: class descriptors and callbacks are immutable metadata that
    // libbinder_ndk keeps until process exit (there is no delete API).
    unsafe impl Send for Classes {}
    // SAFETY: same invariant; concurrent new/associate only read the metadata.
    unsafe impl Sync for Classes {}

    impl Classes {
        fn global(api: &'static Api) -> Result<&'static Self, Error> {
            static CLASSES: OnceLock<Result<Classes, Error>> = OnceLock::new();
            CLASSES
                .get_or_init(|| {
                    let define = |descriptor: &'static CStr, handler: OnTransact| {
                        // SAFETY: static terminated descriptor and static ABI-matching
                        // callbacks; the class is owned by libbinder_ndk for the process.
                        NonNull::new(unsafe {
                            (api.class_define)(descriptor.as_ptr(), on_create, on_destroy, handler)
                        })
                        .ok_or(Error::Runtime("define_class_returned_null"))
                    };
                    Ok(Self {
                        manager: define(c"android.hardware.display.IDisplayManager", refuse)?,
                        callback: define(
                            c"android.hardware.display.IVirtualDisplayCallback",
                            on_transact,
                        )?,
                    })
                })
                .as_ref()
                .map_err(Clone::clone)
        }
    }

    /// Owns one strong Binder reference.
    struct Strong {
        api: &'static Api,
        raw: NonNull<RawBinder>,
    }

    impl Strong {
        /// The display service proxy, associated with its descriptor so that
        /// `AIBinder_prepareTransaction` writes the interface token.
        fn service(api: &'static Api, classes: &'static Classes) -> Result<Self, Error> {
            // SAFETY: static terminated service name; null or one owned strong
            // reference is returned. checkService does not wait for the service.
            let raw = NonNull::new(unsafe { (api.check_service)(c"display".as_ptr()) })
                .ok_or(Error::ServiceUnavailable("display"))?;
            let service = Self { api, raw };
            // SAFETY: live remote object and process-lived class metadata; the NDK
            // compares the remote descriptor and refuses a mismatch.
            if unsafe { (api.binder_associate)(raw.as_ptr(), classes.manager.as_ptr()) } {
                Ok(service)
            } else {
                Err(Error::ServiceUnavailable("display_descriptor"))
            }
        }

        /// A local callback object whose user data shares `callbacks`.
        fn local(
            api: &'static Api,
            classes: &'static Classes,
            callbacks: &Arc<Callbacks>,
        ) -> Result<Self, Error> {
            let data = Box::into_raw(Box::new(Arc::clone(callbacks)));
            // SAFETY: process-lived class; the boxed Arc stays valid until onDestroy,
            // which gives callbacks a lifetime independent of this session.
            let raw = unsafe { (api.binder_new)(classes.callback.as_ptr(), data.cast()) };
            if let Some(raw) = NonNull::new(raw) {
                Ok(Self { api, raw })
            } else {
                // SAFETY: a null result transferred no ownership (onCreate only
                // returns its argument), so the box is still exclusively ours.
                drop(unsafe { Box::from_raw(data) });
                Err(Error::Runtime("new_binder_returned_null"))
            }
        }
    }

    impl Drop for Strong {
        fn drop(&mut self) {
            // SAFETY: releases exactly the one reference this owner received; the
            // function table and its library live for the process.
            unsafe { (self.api.binder_release)(self.raw.as_ptr()) };
        }
    }

    /// Owns one request parcel from `AIBinder_prepareTransaction` or one reply
    /// parcel from `AIBinder_transact`, including the references stored in it.
    struct Parcel {
        api: &'static Api,
        raw: NonNull<RawParcel>,
    }

    impl Parcel {
        /// A request for `target`, already holding its interface token.
        fn prepare(target: &Strong, operation: &'static str) -> Result<Self, Error> {
            let api = target.api;
            let mut raw = ptr::null_mut();
            // SAFETY: live associated remote Binder and a writable out pointer; a
            // non-null result is one parcel owned by the caller even on error.
            let status = unsafe { (api.prepare)(target.raw.as_ptr(), &raw mut raw) };
            let parcel = NonNull::new(raw).map(|raw| Self { api, raw });
            transported(operation, status)?;
            parcel.ok_or(Error::Runtime("prepare_returned_null"))
        }

        fn int(&mut self, value: i32) -> Result<(), Error> {
            // SAFETY: live parcel exclusively owned by this thread.
            written("write_int", unsafe {
                (self.api.write_i32)(self.raw.as_ptr(), value)
            })
        }

        /// Append `fields` in order; nothing else is written. `input` is the
        /// encoder whose window a `Surface` field flattens.
        fn emit(
            &mut self,
            fields: &[Field],
            callback: &Strong,
            input: Option<&ConfiguredInput>,
        ) -> Result<(), Error> {
            let (api, raw) = (self.api, self.raw.as_ptr());
            for field in fields {
                match *field {
                    Field::I32(value) => self.int(value)?,
                    Field::NullStr8 => self.int(-1)?,
                    Field::Str8(text) => {
                        for word in string8_words(text.to_bytes())? {
                            self.int(word)?;
                        }
                    }
                    Field::F32(value) => {
                        // SAFETY: live parcel exclusively owned by this thread.
                        written("write_float", unsafe { (api.write_f32)(raw, value) })?;
                    }
                    Field::Str16(text) => {
                        let length = i32::try_from(text.to_bytes().len())
                            .map_err(|_| Error::LimitExceeded("string16_bytes"))?;
                        // SAFETY: live parcel; `text` is a terminated static string of
                        // `length` UTF-8 bytes that the call copies as UTF-16.
                        written("write_string", unsafe {
                            (api.write_string)(raw, text.as_ptr(), length)
                        })?;
                    }
                    Field::NullBinder => {
                        // SAFETY: live parcel; a null Binder is a documented value.
                        written("write_null_binder", unsafe {
                            (api.write_binder)(raw, ptr::null_mut())
                        })?;
                    }
                    Field::CallbackBinder => {
                        // SAFETY: both objects are live for the call; the parcel
                        // takes its own reference to the Binder.
                        written("write_callback", unsafe {
                            (api.write_binder)(raw, callback.raw.as_ptr())
                        })?;
                    }
                    Field::Surface => {
                        let input = input.ok_or(Error::Runtime("surface_without_input"))?;
                        // SAFETY: the borrowed input keeps its window alive for the
                        // call, which does not take ownership of it; the parcel is
                        // live and takes its own references to what it stores.
                        written("write_window", unsafe {
                            (api.window_write)(input.window().as_ptr(), raw)
                        })?;
                    }
                }
            }
            Ok(())
        }

        /// Send this request two-way and return the bounded reply. The call blocks
        /// until the service answers and cannot be cancelled.
        fn transact(
            self,
            target: &Strong,
            code: u32,
            operation: &'static str,
        ) -> Result<Self, Error> {
            let api = self.api;
            let mut input = self.raw.as_ptr();
            // AIBinder_transact takes ownership of the request and nulls the slot,
            // also when it returns an error.
            std::mem::forget(self);
            let mut output = ptr::null_mut();
            // SAFETY: the request was prepared for this same live target; both slots
            // are valid and exclusive. A non-null output is one reply owned here.
            let status = unsafe {
                (api.transact)(
                    target.raw.as_ptr(),
                    code,
                    &raw mut input,
                    &raw mut output,
                    0,
                )
            };
            let reply = NonNull::new(output).map(|raw| Self { api, raw });
            transported(operation, status)?;
            let reply = reply.ok_or(Error::Runtime("transaction_returned_null"))?;
            // SAFETY: live reply parcel; the call only reads its size.
            let size = unsafe { (api.parcel_size)(reply.raw.as_ptr()) };
            if !(0..=MAX_REPLY_BYTES).contains(&size) {
                return Err(Error::LimitExceeded("reply_bytes"));
            }
            Ok(reply)
        }
    }

    impl Drop for Parcel {
        fn drop(&mut self) {
            // SAFETY: sole owner of the parcel, deleted once; deletion releases the
            // object references the parcel itself holds.
            unsafe { (self.api.parcel_delete)(self.raw.as_ptr()) };
        }
    }

    impl Reply for Parcel {
        fn exception(&mut self, operation: &'static str) -> Result<i32, Error> {
            struct Status(&'static Api, NonNull<RawStatus>);
            impl Drop for Status {
                fn drop(&mut self) {
                    // SAFETY: the one status allocation readStatusHeader returned.
                    unsafe { (self.0.status_delete)(self.1.as_ptr()) };
                }
            }
            let mut raw = ptr::null_mut();
            // SAFETY: live reply positioned at its status header and a writable out
            // pointer; a non-null result is owned below even if the call failed.
            let result = unsafe { (self.api.read_status)(self.raw.as_ptr(), &raw mut raw) };
            let status = NonNull::new(raw).map(|raw| Status(self.api, raw));
            transported(operation, result)?;
            let status = status.ok_or(Error::Runtime("status_returned_null"))?;
            // SAFETY: live owned status; only its code is read, never its message.
            Ok(unsafe { (self.api.status_exception)(status.1.as_ptr()) })
        }

        fn int32(&mut self, operation: &'static str) -> Result<i32, Error> {
            let mut value = 0;
            // SAFETY: live parcel and a valid out pointer for the call.
            let result = unsafe { (self.api.read_i32)(self.raw.as_ptr(), &raw mut value) };
            written(operation, result).map(|()| value)
        }
    }

    /// From just before the create transaction is sent until it is dropped, this
    /// guard owes the service one release for the callback Binder. It is armed
    /// before the call because a transport failure does not prove that nothing was
    /// created; releasing a callback the service does not know is a no-op there.
    struct Display<'a> {
        service: &'a Strong,
        callback: &'a Strong,
        armed: bool,
    }

    impl Display<'_> {
        /// Send the release once; later calls and the drop do nothing.
        fn release(&mut self) -> Result<(), Error> {
            if !std::mem::replace(&mut self.armed, false) {
                return Ok(());
            }
            let mut request = Parcel::prepare(self.service, "prepare_release_display")?;
            request.emit(&RELEASE_REQUEST, self.callback, None)?;
            let mut reply = request.transact(self.service, RELEASE, "release_display")?;
            released_display(&mut reply)
        }
    }

    impl Drop for Display<'_> {
        fn drop(&mut self) {
            // Reached armed only on an early return or while unwinding.
            let _ = self.release();
        }
    }

    fn pool_error(error: pool::PoolError) -> Error {
        match error {
            pool::PoolError::LibraryUnavailable => Error::LibraryUnavailable("libbinder_ndk.so"),
            pool::PoolError::MissingSymbol(name) => Error::MissingSymbol(name),
            pool::PoolError::Foreign => Error::BinderPool("unavailable_or_already_started"),
            pool::PoolError::Insufficient => Error::BinderPool("started_with_fewer_threads"),
        }
    }

    /// Ask the service for its display ids and return the one the request names,
    /// or the typed absence. Read-only: nothing is created or changed.
    fn locate(
        service: &Strong,
        callback: &Strong,
        request: &Request,
        outcome: &mut Outcome,
    ) -> Result<i32, Error> {
        let mut parcel = Parcel::prepare(service, "prepare_list_displays")?;
        // The list request has no Binder field; `callback` is not written.
        parcel.emit(&LIST_REQUEST, callback, None)?;
        outcome.transactions.list += 1;
        let mut reply = parcel.transact(service, LIST, "list_displays")?;
        let listed = listed_displays(&mut reply)?;
        outcome.displays_listed = Some(listed.len());
        let id = wanted(request.target, &listed)?;
        outcome.target_display_id = Some(id);
        located(id, &listed)
    }

    /// Send the create request and drain mirrored frames until `frames` coded
    /// frames arrived or the capture deadline passed.
    fn capture(
        request: Parcel,
        service: &Strong,
        input: &ConfiguredInput,
        frames: u32,
        collector: &mut Collector,
        outcome: &mut Outcome,
        encoder: &mut EncoderOutcome,
    ) -> Result<(), Error> {
        outcome.transactions.create += 1;
        let mut reply = request.transact(service, CREATE, "create_display")?;
        let id = created_display(&mut reply)?;
        drop(reply);
        outcome.display_id = Some(id);
        outcome.display_created = true;
        let deadline = Instant::now() + CAPTURE;
        while collector.stream.coded_frames() < frames && Instant::now() < deadline {
            if collector.ended() {
                return Err(Error::InvalidStream("unexpected_end_of_stream"));
            }
            input.drain(DRAIN_SLICE_US, collector, encoder)?;
        }
        Ok(())
    }

    /// Ask the encoder to finish and drain until its end of stream, bounded.
    fn finish(
        input: &ConfiguredInput,
        collector: &mut Collector,
        encoder: &mut EncoderOutcome,
    ) -> Result<(), Error> {
        input.end_input()?;
        let deadline = Instant::now() + END_OF_STREAM;
        while !collector.ended() {
            if Instant::now() >= deadline {
                return Err(Error::Timeout("end_of_stream"));
            }
            input.drain(DRAIN_SLICE_US, collector, encoder)?;
        }
        Ok(())
    }

    /// One mirror session. Order: function tables and classes, the process Binder
    /// pool (before any object of this process is handed out), a started encoder
    /// fed no frames (a rejected format ends the session here, before the display
    /// service is looked up), the display service, the callback object, the
    /// display list (an absent target ends the session here, before a display is
    /// asked for), the request, then the create transaction under the release guard.
    ///
    /// Locals drop in reverse order: the guard (which releases the display if that
    /// has not happened yet, including while unwinding), the callback reference,
    /// the service proxy, and last the encoder input, whose fields drop window
    /// first and codec second. Request and reply parcels are consumed or dropped
    /// before the capture loop starts; the libraries are never unloaded.
    pub(super) fn mirror(
        request: &Request,
        collector: &mut Collector,
        outcome: &mut Outcome,
    ) -> Result<(), Error> {
        let api = Api::global()?;
        let classes = Classes::global(api)?;
        pool::start(POOL_THREADS).map_err(pool_error)?;
        let input = ConfiguredInput::new(&request.config).map_err(rejected_format)?;
        outcome.codec_name = input.codec_name();
        let service = Strong::service(api, classes)?;
        let callbacks = Arc::new(Callbacks::default());
        let callback = Strong::local(api, classes, &callbacks)?;
        let display_id = locate(&service, &callback, request, outcome)?;
        let mut create = Parcel::prepare(&service, "prepare_create_display")?;
        create.emit(
            &create_request(request, display_id),
            &callback,
            Some(&input),
        )?;
        let mut encoder = EncoderOutcome::default();
        let mut display = Display {
            service: &service,
            callback: &callback,
            armed: true,
        };
        let captured = capture(
            create,
            &service,
            &input,
            request.config.frames,
            collector,
            outcome,
            &mut encoder,
        );
        // The display is released before the encoder is told to finish, so the
        // compositor stops queueing into a surface that is about to end.
        outcome.transactions.release += 1;
        let released = display.release();
        outcome.display_released = outcome.display_created && released.is_ok();
        outcome.release_error = released.err();
        let finished = captured.and_then(|()| finish(&input, collector, &mut encoder));
        outcome.output_width = encoder.output_width;
        outcome.output_height = encoder.output_height;
        outcome.callbacks = callbacks.counts();
        finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84];

    #[test]
    fn create_request_is_exactly_the_pinned_field_sequence() {
        assert_eq!(
            create_request(&Request::DEFAULT, DEFAULT_DISPLAY),
            [
                Field::I32(1),
                Field::Str8(c"carlo-mirror"),
                Field::I32(360),
                Field::I32(640),
                Field::I32(160),
                Field::I32(16),
                Field::I32(1),
                Field::Surface,
                Field::NullStr8,
                Field::I32(0),
                Field::I32(0),
                Field::I32(0),
                Field::F32(0.0),
                Field::I32(0),
                Field::I32(-1),
                Field::I32(0),
                Field::F32(0.0),
                Field::F32(-1.0),
                Field::NullBinder,
                Field::CallbackBinder,
                Field::NullBinder,
                Field::Str16(c"com.android.shell"),
            ]
        );
        assert_eq!(RELEASE_REQUEST, [Field::CallbackBinder]);
        assert_eq!((CREATE, RELEASE), (27, 30));
        // Only AUTO_MIRROR, whatever the encoder size.
        let other = create_request(
            &Request {
                config: Config {
                    width: 720,
                    height: 1280,
                    ..PRESET
                },
                ..Request::DEFAULT
            },
            DEFAULT_DISPLAY,
        );
        assert_eq!(
            other[2..6],
            [
                Field::I32(720),
                Field::I32(1280),
                Field::I32(160),
                Field::I32(16)
            ]
        );
        // The request only parameterises flags and the mirrored display, in place.
        let secure = Request {
            extra_flags: FLAG_SECURE,
            ..Request::DEFAULT
        };
        let default = create_request(&Request::DEFAULT, DEFAULT_DISPLAY);
        let varied = create_request(&secure, 6);
        assert_eq!((varied[5], varied[9]), (Field::I32(20), Field::I32(6)));
        for (index, (left, right)) in default.iter().zip(&varied).enumerate() {
            assert_eq!(left == right, index != 5 && index != 9, "{index}");
        }
        assert_eq!(Request::DEFAULT.flags(), 16);
        assert_eq!(Request::DEFAULT.config, PRESET);
    }

    #[test]
    fn display_list_request_and_reply_follow_the_pinned_int_array_layout() {
        assert_eq!(LIST_REQUEST, [Field::I32(0)]);
        assert_eq!(LIST, 8);
        let list = |values: &[i32]| listed_displays(&mut Fake::reply(0, values));
        assert_eq!(list(&[0]), Ok(vec![]));
        assert_eq!(list(&[1, 0]), Ok(vec![0]));
        assert_eq!(list(&[2, 0, 6]), Ok(vec![0, 6]));
        let mut full = vec![64];
        full.extend(0..64);
        assert_eq!(list(&full).map(|ids| ids.len()), Ok(64));
        // A null array, a negative or an oversized count: no element is read.
        for count in [-1, -2, i32::MIN, 65, i32::MAX] {
            let mut reply = Fake::reply(0, &[count, 0, 6]);
            assert_eq!(
                listed_displays(&mut reply),
                Err(Error::UnexpectedParcel("display_id_count"))
            );
            assert_eq!(reply.reads, ["list_displays", "read_display_count"]);
        }
        // Truncated: the count promises more ids than the reply holds.
        assert_eq!(
            list(&[2, 0]),
            Err(Error::Parcel {
                operation: "read_listed_display_id",
                status: Fake::NOT_ENOUGH_DATA
            })
        );
        assert_eq!(
            list(&[]),
            Err(Error::Parcel {
                operation: "read_display_count",
                status: Fake::NOT_ENOUGH_DATA
            })
        );
        // Nothing behind an exception is read.
        let mut denied = Fake::reply(-1, &[1, 0]);
        assert_eq!(
            listed_displays(&mut denied),
            Err(Error::PermissionDenied("list_displays"))
        );
        assert_eq!(denied.reads, ["list_displays"]);
    }

    #[test]
    fn absent_display_is_decided_from_the_listed_ids() {
        assert_eq!(wanted(Target::Display(0), &[]), Ok(0));
        assert_eq!(wanted(Target::Display(6), &[0]), Ok(6));
        assert_eq!(located(0, &[0]), Ok(0));
        assert_eq!(located(6, &[0, 6]), Ok(6));
        for (id, listed) in [(0, &[][..]), (0, &[6][..]), (6, &[0][..]), (-1, &[0][..])] {
            assert_eq!(located(id, listed), Err(Error::DisplayAbsent));
        }
        // The fault target is never one of the listed ids.
        for listed in [&[][..], &[0][..], &[0, 6][..], &[6, 0, 1000][..], &[-5][..]] {
            let id = wanted(Target::BeyondListed, listed).unwrap();
            assert_eq!(located(id, listed), Err(Error::DisplayAbsent), "{id}");
        }
        assert_eq!(wanted(Target::BeyondListed, &[0, 6]), Ok(1006));
        assert_eq!(wanted(Target::BeyondListed, &[]), Ok(1000));
        assert_eq!(
            wanted(Target::BeyondListed, &[0, i32::MAX - 999]),
            Err(Error::LimitExceeded("display_id"))
        );
    }

    /// Every error variant that is not one of the three classified sources.
    fn unclassified() -> Vec<Error> {
        vec![
            Error::UnsupportedProfile,
            Error::LibraryUnavailable("libbinder_ndk.so"),
            Error::MissingSymbol("AIBinder_transact"),
            Error::EncoderUnavailable,
            Error::Codec {
                operation: "configure",
                status: -1010,
            },
            Error::Codec {
                operation: "dequeue_output",
                status: -1,
            },
            Error::Egl {
                operation: "initialize",
                code: 0x3001,
            },
            Error::Gl { code: 0x0501 },
            Error::Timeout("end_of_stream"),
            Error::InvalidStream("unexpected_end_of_stream"),
            Error::LimitExceeded("reply_bytes"),
            Error::Parcel {
                operation: "read_display_id",
                status: -61,
            },
            Error::UnexpectedParcel("display_id_count"),
            Error::ServiceUnavailable("display"),
            Error::BinderPool("unavailable_or_already_started"),
            Error::Binder {
                operation: "create_display",
                status: -1,
            },
            Error::IllegalArgument("create_display"),
            Error::RemoteException {
                operation: "create_display",
                code: -5,
            },
            Error::DisplayRefused,
            Error::DefaultDisplayReturned,
            Error::Runtime("status_returned_null"),
        ]
    }

    #[test]
    fn failure_classes_come_only_from_their_own_sources() {
        // Permission: the SecurityException of a service reply.
        let denied = created_display(&mut Fake::new(-1, 7)).unwrap_err();
        assert_eq!(
            FailureClass::of(&denied),
            Some(FailureClass::PermissionDenied)
        );
        // Absent display: the lookup in the listed ids.
        let absent = located(1006, &[0, 6]).unwrap_err();
        assert_eq!(FailureClass::of(&absent), Some(FailureClass::DisplayAbsent));
        // Format: the encoder calls that validate the configuration.
        for operation in ["configure", "create_input_surface", "start"] {
            let rejected = rejected_format(Error::Codec {
                operation,
                status: -1010,
            });
            assert_eq!(
                rejected,
                Error::UnsupportedFormat {
                    operation,
                    status: -1010
                }
            );
            assert_eq!(
                FailureClass::of(&rejected),
                Some(FailureClass::UnsupportedFormat)
            );
        }
        // No other error has a class, before or after the encoder mapping, and
        // the mapping leaves every other error untouched.
        for error in unclassified() {
            assert_eq!(FailureClass::of(&error), None, "{error:?}");
            let mapped = rejected_format(error.clone());
            if !matches!(
                error,
                Error::Codec {
                    operation: "configure",
                    ..
                }
            ) {
                assert_eq!(mapped, error);
                assert_eq!(FailureClass::of(&mapped), None, "{error:?}");
            }
        }
        // A null handle without a status is not a format rejection.
        let null_window = Error::Codec {
            operation: "create_input_surface",
            status: 0,
        };
        assert_eq!(rejected_format(null_window.clone()), null_window);
        // A silent -1 id stays a refusal, not a permission failure.
        let refused = created_display(&mut Fake::new(0, -1)).unwrap_err();
        assert_eq!(FailureClass::of(&refused), None);
        let mut outcome = Outcome::default();
        outcome.conclude(Err(Error::DisplayAbsent));
        assert_eq!(outcome.failure_class, Some(FailureClass::DisplayAbsent));
        outcome.conclude(Err(Error::DisplayRefused));
        assert_eq!(outcome.failure_class, None);
        outcome.conclude(Err(Error::PermissionDenied("create_display")));
        outcome.conclude(Ok(()));
        assert_eq!((&outcome.error, outcome.failure_class), (&None, None));
        for (class, code) in [
            (FailureClass::PermissionDenied, "permission_denied"),
            (FailureClass::DisplayAbsent, "display_absent"),
            (FailureClass::UnsupportedFormat, "unsupported_format"),
        ] {
            assert_eq!(serde_json::to_value(class).unwrap(), code);
        }
        assert_eq!(
            serde_json::to_value(Error::DisplayAbsent).unwrap(),
            serde_json::json!({"kind": "display_absent"})
        );
        assert_eq!(
            serde_json::to_value(rejected_format(Error::Codec {
                operation: "configure",
                status: -1010
            }))
            .unwrap(),
            serde_json::json!({
                "kind": "unsupported_format",
                "detail": {"operation": "configure", "status": -1010}
            })
        );
    }

    #[test]
    fn string8_is_length_then_nul_terminated_zero_padded_little_endian_words() {
        fn bytes(text: &[u8]) -> Vec<u8> {
            string8_words(text)
                .unwrap()
                .into_iter()
                .flat_map(i32::to_le_bytes)
                .collect()
        }
        assert_eq!(bytes(b""), [0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bytes(b"abc"), [3, 0, 0, 0, b'a', b'b', b'c', 0]);
        assert_eq!(
            bytes(b"abcd"),
            [4, 0, 0, 0, b'a', b'b', b'c', b'd', 0, 0, 0, 0]
        );
        let name = bytes(DISPLAY_NAME.to_bytes());
        assert_eq!(name.len(), 4 + 16);
        assert_eq!(name[..4], [12, 0, 0, 0]);
        assert_eq!(&name[4..16], b"carlo-mirror");
        assert_eq!(name[16..], [0, 0, 0, 0]);
        for length in 0..=17 {
            let text = vec![b'x'; length];
            let packed = bytes(&text);
            // Length word, then the smallest multiple of four holding bytes + NUL.
            assert_eq!(packed.len(), 4 + (length + 1).div_ceil(4) * 4);
            assert_eq!(packed[4..4 + length], text[..]);
            assert!(packed[4 + length..].iter().all(|byte| *byte == 0));
        }
        assert_eq!(string8_words(b"ab").unwrap(), [2, 0x0000_6261]);
    }

    struct Fake {
        exception: Result<i32, Error>,
        values: std::collections::VecDeque<i32>,
        reads: Vec<&'static str>,
    }

    impl Fake {
        /// `STATUS_NOT_ENOUGH_DATA`, as a real parcel answers past its end.
        const NOT_ENOUGH_DATA: i32 = -61;

        fn new(exception: i32, id: i32) -> Self {
            Self::reply(exception, &[id])
        }

        fn reply(exception: i32, values: &[i32]) -> Self {
            Self {
                exception: Ok(exception),
                values: values.iter().copied().collect(),
                reads: Vec::new(),
            }
        }
    }

    impl Reply for Fake {
        fn exception(&mut self, operation: &'static str) -> Result<i32, Error> {
            self.reads.push(operation);
            self.exception.clone()
        }

        fn int32(&mut self, operation: &'static str) -> Result<i32, Error> {
            self.reads.push(operation);
            self.values.pop_front().ok_or(Error::Parcel {
                operation,
                status: Self::NOT_ENOUGH_DATA,
            })
        }
    }

    #[test]
    fn create_reply_maps_exceptions_and_refuses_negative_and_default_ids() {
        let mut accepted = Fake::new(0, 7);
        assert_eq!(created_display(&mut accepted), Ok(7));
        assert_eq!(accepted.reads, ["create_display", "read_display_id"]);
        assert_eq!(
            created_display(&mut Fake::new(0, -1)),
            Err(Error::DisplayRefused)
        );
        assert_eq!(
            created_display(&mut Fake::new(0, i32::MIN)),
            Err(Error::DisplayRefused)
        );
        assert_eq!(
            created_display(&mut Fake::new(0, 0)),
            Err(Error::DefaultDisplayReturned)
        );
        for (code, expected) in [
            (-1, Error::PermissionDenied("create_display")),
            (-3, Error::IllegalArgument("create_display")),
            (
                -5,
                Error::RemoteException {
                    operation: "create_display",
                    code: -5,
                },
            ),
            (
                1,
                Error::RemoteException {
                    operation: "create_display",
                    code: 1,
                },
            ),
        ] {
            // The id behind an exception is never read, let alone trusted.
            let mut reply = Fake::new(code, 7);
            assert_eq!(created_display(&mut reply), Err(expected));
            assert_eq!(reply.reads, ["create_display"]);
        }
        let transport = Error::Binder {
            operation: "create_display",
            status: -32,
        };
        let mut broken = Fake {
            exception: Err(transport.clone()),
            ..Fake::new(0, 7)
        };
        assert_eq!(created_display(&mut broken), Err(transport));

        let mut released = Fake::new(0, 99);
        assert_eq!(released_display(&mut released), Ok(()));
        assert_eq!(released.reads, ["release_display"]);
        assert_eq!(
            released_display(&mut Fake::new(-1, 0)),
            Err(Error::PermissionDenied("release_display"))
        );
    }

    #[test]
    fn callbacks_count_only_system_callers_and_known_codes() {
        fn sync<T: Sync + Send>() {}
        sync::<Callbacks>();
        let callbacks = Callbacks::default();
        assert_eq!(callbacks.dispatch(1000, ON_PAUSED), 0);
        assert_eq!(callbacks.dispatch(1000, ON_RESUMED), 0);
        assert_eq!(callbacks.dispatch(1000, ON_RESUMED), 0);
        assert_eq!(callbacks.dispatch(1000, ON_STOPPED), 0);
        assert_eq!(callbacks.dispatch(1000, 4), -74);
        assert_eq!(callbacks.dispatch(1000, 0), -74);
        for uid in [0, 2000, 10_123] {
            assert_eq!(callbacks.dispatch(uid, ON_STOPPED), -1);
        }
        assert_eq!(
            callbacks.counts(),
            CallbackCounts {
                paused: 1,
                resumed: 2,
                stopped: 1,
                rejected: 5
            }
        );
        assert_eq!((ON_PAUSED, ON_RESUMED, ON_STOPPED), (1, 2, 3));
    }

    fn mirrored(collector: &mut Collector, outcome: &mut Outcome) -> Result<(), Error> {
        collector.push(super::super::FLAG_CODEC_CONFIG, 0, SPS_PPS)?;
        collector.push(super::super::FLAG_KEY_FRAME, 40, IDR)?;
        collector.push(super::super::FLAG_END_OF_STREAM, 40, &[])?;
        outcome.display_created = true;
        outcome.display_id = Some(7);
        outcome.display_released = true;
        Ok(())
    }

    #[test]
    fn one_decodable_frame_with_created_and_released_display_passes() {
        let mut outcome = Outcome::default();
        let mut collector = Collector::new(&PRESET, false);
        mirrored(&mut collector, &mut outcome).unwrap();
        outcome.stream = collector.stream;
        assert!(outcome.passed());

        let broken: [fn(&mut Outcome); 8] = [
            |outcome| outcome.display_created = false,
            |outcome| outcome.display_released = false,
            |outcome| outcome.error = Some(Error::Timeout("end_of_stream")),
            |outcome| outcome.release_error = Some(Error::PermissionDenied("release_display")),
            |outcome| outcome.stream.nal_units.sps = 0,
            |outcome| outcome.stream.nal_units.pps = 0,
            |outcome| outcome.stream.nal_units.idr = 0,
            |outcome| {
                // Parameter sets alone are not a frame.
                outcome.stream.nal_units.idr = 0;
                outcome.stream.nal_units.non_idr = 0;
            },
        ];
        for breakage in broken {
            let mut outcome = Outcome::default();
            let mut collector = Collector::new(&PRESET, false);
            mirrored(&mut collector, &mut outcome).unwrap();
            outcome.stream = collector.stream;
            breakage(&mut outcome);
            assert!(!outcome.passed(), "{outcome:?}");
        }
        assert!(!Outcome::default().passed());
    }

    #[test]
    fn only_the_exact_pinned_profile_is_supported() {
        assert!(supported(
            Some(36),
            Some(2000),
            "aarch64",
            Some(FINGERPRINT)
        ));
        let userdebug = FINGERPRINT.replace("user/release-keys", "userdebug/test-keys");
        for (api, uid, architecture, fingerprint) in [
            (Some(35), Some(2000), "aarch64", Some(FINGERPRINT)),
            (Some(37), Some(2000), "aarch64", Some(FINGERPRINT)),
            (None, Some(2000), "aarch64", Some(FINGERPRINT)),
            (Some(36), Some(0), "aarch64", Some(FINGERPRINT)),
            (Some(36), Some(1000), "aarch64", Some(FINGERPRINT)),
            (Some(36), None, "aarch64", Some(FINGERPRINT)),
            (Some(36), Some(2000), "x86_64", Some(FINGERPRINT)),
            (Some(36), Some(2000), "aarch64", Some(userdebug.as_str())),
            (
                Some(36),
                Some(2000),
                "aarch64",
                Some("google/sdk_gphone64_arm64/emu64a:16/BP4A.251205.006/1:user/release-keys"),
            ),
            (Some(36), Some(2000), "aarch64", Some("")),
            (Some(36), Some(2000), "aarch64", None),
        ] {
            assert!(!supported(api, uid, architecture, fingerprint));
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn unsupported_profile_makes_no_call_and_reports_no_capture() {
        let mut called = false;
        let mut report = run(
            Diagnostics::collect().unwrap(),
            Some(FINGERPRINT.to_owned()),
            true,
            |_, _, _| {
                called = true;
                Ok(())
            },
        );
        assert!(!called);
        assert_eq!(report.outcome.error, Some(Error::UnsupportedProfile));
        assert!(!report.passed() && !report.screen_capture);
        assert!(report.take_data().is_none());
        assert_eq!(report.outcome.stream.packets, 0);

        let mut report = collect(true).unwrap();
        assert_eq!(report.outcome.error, Some(Error::UnsupportedProfile));
        assert!(!report.outcome.display_created && !report.outcome.display_released);
        assert!(report.take_data().is_none());

        let mut calls = 0;
        let faults = run_faults(
            Diagnostics::collect().unwrap(),
            Some(FINGERPRINT.to_owned()),
            |_, _, _| {
                calls += 1;
                Ok(())
            },
        );
        assert_eq!(calls, 0);
        assert!(!faults.passed());
        assert_eq!(faults.sessions.len(), 3);
        for session in &faults.sessions {
            assert_eq!(session.outcome.error, Some(Error::UnsupportedProfile));
            assert_eq!(session.outcome.failure_class, None);
            assert_eq!(session.outcome.transactions, Transactions::default());
            assert!(!session.passed);
        }
        assert!(!collect_faults().unwrap().passed());
    }

    /// A report as the pinned profile would build it, without any platform call.
    fn report(
        keep_data: bool,
        session: impl FnOnce(&mut Collector, &mut Outcome) -> Result<(), Error>,
    ) -> Report {
        let (outcome, data) = attempt(
            true,
            &Request::DEFAULT,
            keep_data,
            |_, collector, outcome| session(collector, outcome),
        );
        Report {
            schema_version: 1,
            environment: Diagnostics::collect().unwrap(),
            runtime_fingerprint: Some(FINGERPRINT.to_owned()),
            source: "display_mirror",
            screen_capture: outcome.passed(),
            video_available: false,
            requested: PRESET,
            display: DisplayRequest::of(&Request::DEFAULT),
            outcome,
            data,
        }
    }

    /// What the pinned service and encoder are expected to answer to `request`,
    /// through the same safe helpers the session uses.
    fn provoked(request: &Request, outcome: &mut Outcome) -> Result<(), Error> {
        if request.config.width == 0 {
            return Err(rejected_format(Error::Codec {
                operation: "configure",
                status: -1010,
            }));
        }
        outcome.transactions.list += 1;
        let listed = listed_displays(&mut Fake::reply(0, &[2, 0, 6]))?;
        outcome.displays_listed = Some(listed.len());
        let id = wanted(request.target, &listed)?;
        outcome.target_display_id = Some(id);
        located(id, &listed)?;
        outcome.transactions.create += 1;
        let exception = if request.flags() & FLAG_SECURE != 0 {
            -1
        } else {
            0
        };
        let created = created_display(&mut Fake::new(exception, 7));
        outcome.transactions.release += 1;
        outcome.display_id = Some(created?);
        outcome.display_created = true;
        outcome.display_released = true;
        Ok(())
    }

    fn faults(
        session: impl FnMut(&Request, &mut Collector, &mut Outcome) -> Result<(), Error>,
    ) -> FaultReport {
        fault_report(
            true,
            Diagnostics::collect().unwrap(),
            Some(FINGERPRINT.to_owned()),
            session,
        )
    }

    #[test]
    fn fault_probe_passes_only_on_the_three_expected_classes_and_no_display() {
        // The requests: one extra flag, one derived target, one rejected size;
        // none of them can capture frames.
        assert_eq!(
            FAULTS.map(|fault| (fault.name, fault.request.flags(), fault.request.target)),
            [
                ("permission", 16 | 4, Target::Display(0)),
                ("display_absent", 16, Target::BeyondListed),
                ("unsupported_format", 16, Target::Display(0)),
            ]
        );
        assert!(FAULTS.iter().all(|fault| fault.request.config.frames == 0));
        assert_eq!(FAULTS[0].request.config, FRAMELESS);
        assert_eq!(
            (
                FAULTS[2].request.config.width,
                FAULTS[2].request.config.height
            ),
            (0, 0)
        );

        let report = faults(|request, _, outcome| provoked(request, outcome));
        assert!(report.passed());
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["source"], "display_fault_provocation");
        assert_eq!(json["screen_capture"], false);
        assert_eq!(json["video_available"], false);
        let expected = [
            ("permission", "permission_denied", [1, 1, 1], Some(2)),
            ("display_absent", "display_absent", [1, 0, 0], Some(2)),
            ("unsupported_format", "unsupported_format", [0, 0, 0], None),
        ];
        for (session, (name, class, sent, listed)) in
            json["sessions"].as_array().unwrap().iter().zip(expected)
        {
            assert_eq!(session["name"], name);
            assert_eq!(session["expected"], class);
            assert_eq!(session["passed"], true);
            assert_eq!(session["outcome"]["failure_class"], class);
            assert_eq!(session["outcome"]["error"]["kind"], class);
            assert_eq!(session["outcome"]["display_created"], false);
            assert_eq!(session["outcome"]["display_id"], serde_json::Value::Null);
            assert_eq!(
                session["outcome"]["transactions"],
                serde_json::json!({"list": sent[0], "create": sent[1], "release": sent[2]})
            );
            assert_eq!(
                session["outcome"]["displays_listed"],
                serde_json::json!(listed)
            );
        }
        assert_eq!(json["sessions"][0]["display"]["flags"], 20);
        assert_eq!(json["sessions"][1]["outcome"]["target_display_id"], 1006);
        assert_eq!(
            json["sessions"][1]["display"]["display_id_to_mirror"],
            serde_json::Value::Null
        );
        assert_eq!(json["sessions"][2]["requested"]["width"], 0);

        // The service unexpectedly accepting the secure display fails the probe,
        // even though the session released it.
        let accepted = faults(|request, _, outcome| {
            let relaxed = Request {
                extra_flags: 0,
                ..*request
            };
            provoked(&relaxed, outcome)
        });
        assert!(!accepted.passed());
        assert!(!accepted.sessions[0].passed);
        assert!(accepted.sessions[0].outcome.display_created);
        assert_eq!(accepted.sessions[0].outcome.error, None);
        assert!(accepted.sessions[1].passed && accepted.sessions[2].passed);

        // The right class with the wrong transactions, a different class, an
        // unclassified error, no error or a failed release does not pass either.
        type Breakage = fn(&Request, &mut Outcome) -> Result<(), Error>;
        let breakages: [Breakage; 5] = [
            |_, _| Ok(()),
            |request, outcome| {
                let result = provoked(request, outcome);
                outcome.transactions.create += 1;
                result
            },
            |_, _| Err(Error::PermissionDenied("list_displays")),
            |request, outcome| {
                let _ = provoked(request, outcome);
                Err(Error::DisplayRefused)
            },
            |request, outcome| {
                outcome.release_error = Some(Error::Binder {
                    operation: "release_display",
                    status: -32,
                });
                provoked(request, outcome)
            },
        ];
        for (index, breakage) in breakages.into_iter().enumerate() {
            let broken = faults(|request, _, outcome| breakage(request, outcome));
            assert!(!broken.passed(), "{index} {:?}", broken.sessions);
        }
    }

    #[test]
    fn report_shape_is_stable_and_carries_no_names_or_frame_bytes() {
        let mut passed = report(true, mirrored);
        assert!(passed.passed());
        let json = serde_json::to_value(&passed).unwrap();
        assert_eq!(json["source"], "display_mirror");
        assert_eq!(json["screen_capture"], true);
        assert_eq!(json["video_available"], false);
        assert_eq!(json["requested"]["width"], 360);
        assert_eq!(
            json["display"],
            serde_json::json!({"flags": 16, "density_dpi": 160, "display_id_to_mirror": 0})
        );
        let mut keys: Vec<_> = json.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "display",
                "environment",
                "outcome",
                "requested",
                "runtime_fingerprint",
                "schema_version",
                "screen_capture",
                "source",
                "video_available"
            ]
        );
        let mut keys: Vec<_> = json["outcome"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "callbacks",
                "codec_name",
                "display_created",
                "display_id",
                "display_released",
                "displays_listed",
                "elapsed_ms",
                "error",
                "failure_class",
                "output_height",
                "output_width",
                "release_error",
                "stream",
                "target_display_id",
                "transactions"
            ]
        );
        assert_eq!(json["outcome"]["failure_class"], serde_json::Value::Null);
        assert_eq!(json["outcome"]["display_id"], 7);
        assert_eq!(
            json["outcome"]["callbacks"],
            serde_json::json!({"paused": 0, "resumed": 0, "stopped": 0, "rejected": 0})
        );
        assert_eq!(json["outcome"]["stream"]["nal_units"]["idr"], 1);
        assert_eq!(json["outcome"]["stream"]["first_pts_us"], 40);
        let text = serde_json::to_string(&passed).unwrap();
        for hidden in ["carlo-mirror", "com.android.shell", "IDisplayManager"] {
            assert!(!text.contains(hidden), "{hidden}");
        }
        let data = passed.take_data().unwrap();
        assert_eq!(data, [SPS_PPS, IDR].concat());
        assert!(passed.take_data().is_none());

        // A failed run reports a typed error, no capture, and releases no bytes.
        let mut failed = report(true, |collector, outcome| {
            mirrored(collector, outcome)?;
            outcome.display_released = false;
            outcome.release_error = Some(Error::Binder {
                operation: "release_display",
                status: -32,
            });
            Err(Error::PermissionDenied("create_display"))
        });
        assert!(!failed.passed());
        assert!(failed.take_data().is_none());
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json["screen_capture"], false);
        assert_eq!(
            json["outcome"]["error"],
            serde_json::json!({"kind": "permission_denied", "detail": "create_display"})
        );
        assert_eq!(json["outcome"]["failure_class"], "permission_denied");
        assert_eq!(
            json["outcome"]["release_error"],
            serde_json::json!({
                "kind": "binder",
                "detail": {"operation": "release_display", "status": -32}
            })
        );
        for (error, kind) in [
            (Error::DisplayRefused, "display_refused"),
            (Error::DefaultDisplayReturned, "default_display_returned"),
        ] {
            let json = serde_json::to_value(report(false, |_, _| Err(error))).unwrap();
            assert_eq!(json["outcome"]["error"]["kind"], kind);
            assert_eq!(json["outcome"]["failure_class"], serde_json::Value::Null);
            assert_eq!(json["screen_capture"], false);
        }
    }
}
