//! Surface parcel proof: the encoder input `ANativeWindow` is flattened with the
//! public `ANativeWindow_writeToParcel` into a process-local scratch parcel and
//! read back to show that it carries the Binder of its graphic buffer producer.
//!
//! Nothing is transacted: the parcel never leaves this process, no display is
//! touched and no frame is encoded. The field order is pinned in
//! `docs/compat/surface-parcel.json`. Interpretation and limits are safe Rust
//! exercised on the host; the FFI lives in the Android-only `ffi` module below.

use super::Error;
use crate::{diagnostics::Diagnostics, error::CarloError};
use serde::Serialize;

/// `'bque'`, written by `Surface::writeToParcel` before the producer Binder.
const MAGIC: u32 = 0x6271_7565;
/// A flattened surface is a short name, two words and two Binder objects.
const MAX_PARCEL_BYTES: usize = 4096;
const MAX_NAME_UNITS: usize = 256;
/// UTF-8 form of [`MAX_NAME_UNITS`] UTF-16 units plus the terminator the NDK writes.
const NAME_BYTES: usize = MAX_NAME_UNITS * 3 + 1;
/// `ANativeWindow_writeToParcel` was introduced in API 34.
const MIN_API: u32 = 34;

fn supported(shell_api: Option<u32>, architecture: &str) -> bool {
    super::executed(shell_api, architecture) && shell_api.is_some_and(|api| api >= MIN_API)
}

/// What the string allocator observed for the surface name.
enum Name {
    Null,
    /// UTF-8 bytes written to the caller's buffer, terminator excluded.
    Utf8(usize),
    /// Longer than the buffer; nothing was written.
    TooLong,
}

/// Sequential reads over one flattened surface. Implemented by the scratch parcel
/// on Android and by a fake in host tests, so ordering and limits stay in `decode`.
trait Source {
    fn size(&mut self) -> i32;
    fn rewind(&mut self) -> Result<(), Error>;
    /// Only differences between two positions are meaningful.
    fn position(&mut self) -> i32;
    fn name(&mut self, buffer: &mut [u8; NAME_BYTES]) -> Result<Name, Error>;
    fn int32(&mut self, operation: &'static str) -> Result<i32, Error>;
    fn uint32(&mut self, operation: &'static str) -> Result<u32, Error>;
    /// `None` for a null Binder, otherwise whether it is a remote proxy. The
    /// implementation releases the reference before returning.
    fn binder(&mut self, operation: &'static str) -> Result<Option<bool>, Error>;
}

/// Facts about the flattened surface. Holds the name length, never its text.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Outcome {
    parcel_bytes: Option<usize>,
    name_units: Option<usize>,
    single_buffered: Option<bool>,
    pub(super) magic_matches: bool,
    pub(super) producer_present: bool,
    producer_remote: Option<bool>,
    control_handle_present: bool,
    fully_consumed: bool,
    codec: Option<String>,
    pub(super) error: Option<Error>,
}

impl Outcome {
    pub(super) fn passed(&self) -> bool {
        self.magic_matches && self.producer_present && self.error.is_none()
    }
}

/// Read name, isSingleBuffered, magic, producer and control handle, in that order.
/// The size bound is checked before any read; an unknown magic stops before the
/// following words would be interpreted as Binder objects.
fn decode(source: &mut impl Source, outcome: &mut Outcome) -> Result<(), Error> {
    let size = source.size();
    let bytes = usize::try_from(size).map_err(|_| Error::UnexpectedParcel("size"))?;
    outcome.parcel_bytes = Some(bytes);
    if bytes > MAX_PARCEL_BYTES {
        return Err(Error::LimitExceeded("parcel_bytes"));
    }
    source.rewind()?;
    let start = source.position();
    let mut buffer = [0_u8; NAME_BYTES];
    let name = source.name(&mut buffer)?;
    let units = match name {
        Name::Null => None,
        Name::TooLong => return Err(Error::LimitExceeded("name_units")),
        Name::Utf8(length) => {
            let text = buffer
                .get(..length)
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .ok_or(Error::UnexpectedParcel("name_encoding"))?;
            Some(text.encode_utf16().count())
        }
    };
    // Only the length leaves this function.
    buffer.fill(0);
    if units.is_some_and(|units| units > MAX_NAME_UNITS) {
        return Err(Error::LimitExceeded("name_units"));
    }
    outcome.name_units = units;
    outcome.single_buffered = Some(match source.int32("read_single_buffered")? {
        0 => false,
        1 => true,
        _ => return Err(Error::UnexpectedParcel("single_buffered")),
    });
    outcome.magic_matches = source.uint32("read_magic")? == MAGIC;
    if !outcome.magic_matches {
        return Err(Error::UnexpectedParcel("magic"));
    }
    let producer = source.binder("read_producer")?;
    outcome.producer_present = producer.is_some();
    outcome.producer_remote = producer;
    outcome.control_handle_present = source.binder("read_control_handle")?.is_some();
    outcome.fully_consumed = source.position().checked_sub(start) == Some(size);
    Ok(())
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    schema_version: u32,
    environment: Diagnostics,
    /// The surface belongs to an encoder fed no frames; nothing is read from any display.
    source: &'static str,
    screen_capture: bool,
    video_available: bool,
    outcome: Outcome,
}

impl Report {
    fn new(environment: Diagnostics, outcome: Outcome) -> Self {
        Self {
            schema_version: 1,
            environment,
            source: "encoder_input_surface",
            screen_capture: false,
            video_available: false,
            outcome,
        }
    }

    /// The parcel has the pinned magic and a non-null producer Binder.
    pub(crate) fn passed(&self) -> bool {
        self.outcome.passed()
    }
}

/// Flatten the input surface of a started encoder that is fed no frames and report
/// what the parcel holds. Codec and Binder calls are synchronous and not
/// cancellable: run under the bounded host session helper.
pub(crate) fn collect() -> Result<Report, CarloError> {
    let environment = Diagnostics::collect()?;
    let outcome = if supported(environment.shell_api(), std::env::consts::ARCH) {
        probe()
    } else {
        Outcome {
            error: Some(Error::UnsupportedProfile),
            ..Outcome::default()
        }
    };
    Ok(Report::new(environment, outcome))
}

/// One probe cycle; every handle it created is released before it returns.
#[cfg(target_os = "android")]
pub(super) fn probe() -> Outcome {
    let mut outcome = Outcome::default();
    outcome.error = ffi::probe(&mut outcome).err();
    outcome
}

#[cfg(not(target_os = "android"))]
fn probe() -> Outcome {
    unreachable!("profile requires Android")
}

/// Public NDK C ABI pinned to the NDK 28.2.13676358 headers
/// `android/native_window_aidl.h`, `android/binder_parcel.h` and
/// `android/binder_ibinder.h`. No private or C++ symbols, and the window is only
/// ever passed as the opaque pointer the media API returned.
///
/// Exports are resolved at runtime and the table lives for the process:
/// libbinder_ndk owns process-wide Binder state once a proxy has been read, so
/// the libraries are never unloaded. Every parcel and strong Binder has one RAII
/// owner and is created, used and released on the calling thread (`NonNull`
/// keeps the owners `!Send`). The scratch parcel is never transacted; this module
/// starts no Binder thread pool and registers no callbacks.
#[cfg(target_os = "android")]
mod ffi {
    use super::{Error, NAME_BYTES, Name, Outcome, Source, decode};
    use crate::{
        binder::native::Library,
        video::{
            PRESET,
            native::{ConfiguredInput, Window},
        },
    };
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr::{self, NonNull},
        sync::OnceLock,
    };

    #[repr(C)]
    struct RawParcel {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    struct RawBinder {
        _opaque: [u8; 0],
    }
    /// `AParcel_stringAllocator` from the pinned `binder_parcel.h`.
    type StringAllocator = unsafe extern "C" fn(*mut c_void, i32, *mut *mut c_char) -> bool;

    struct Api {
        _libraries: [Library; 2],
        parcel_create: unsafe extern "C" fn() -> *mut RawParcel,
        parcel_delete: unsafe extern "C" fn(*mut RawParcel),
        parcel_size: unsafe extern "C" fn(*const RawParcel) -> i32,
        parcel_position: unsafe extern "C" fn(*const RawParcel) -> i32,
        parcel_set_position: unsafe extern "C" fn(*const RawParcel, i32) -> i32,
        parcel_read_i32: unsafe extern "C" fn(*const RawParcel, *mut i32) -> i32,
        parcel_read_u32: unsafe extern "C" fn(*const RawParcel, *mut u32) -> i32,
        parcel_read_string:
            unsafe extern "C" fn(*const RawParcel, *mut c_void, StringAllocator) -> i32,
        parcel_read_binder: unsafe extern "C" fn(*const RawParcel, *mut *mut RawBinder) -> i32,
        binder_release: unsafe extern "C" fn(*mut RawBinder),
        binder_remote: unsafe extern "C" fn(*const RawBinder) -> bool,
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
                    // public export in the pinned NDK headers (binder_status_t and
                    // int32_t are i32, uint32_t is u32, bool is one byte, handles are
                    // opaque pointers). The owning library stays loaded for the process.
                    unsafe { std::mem::transmute::<*mut c_void, $type>(address.as_ptr()) }
                }};
            }
            Ok(Self {
                parcel_create: symbol!(
                    binder,
                    c"AParcel_create",
                    unsafe extern "C" fn() -> *mut RawParcel
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
                parcel_position: symbol!(
                    binder,
                    c"AParcel_getDataPosition",
                    unsafe extern "C" fn(*const RawParcel) -> i32
                ),
                parcel_set_position: symbol!(
                    binder,
                    c"AParcel_setDataPosition",
                    unsafe extern "C" fn(*const RawParcel, i32) -> i32
                ),
                parcel_read_i32: symbol!(
                    binder,
                    c"AParcel_readInt32",
                    unsafe extern "C" fn(*const RawParcel, *mut i32) -> i32
                ),
                parcel_read_u32: symbol!(
                    binder,
                    c"AParcel_readUint32",
                    unsafe extern "C" fn(*const RawParcel, *mut u32) -> i32
                ),
                parcel_read_string: symbol!(
                    binder,
                    c"AParcel_readString",
                    unsafe extern "C" fn(*const RawParcel, *mut c_void, StringAllocator) -> i32
                ),
                parcel_read_binder: symbol!(
                    binder,
                    c"AParcel_readStrongBinder",
                    unsafe extern "C" fn(*const RawParcel, *mut *mut RawBinder) -> i32
                ),
                binder_release: symbol!(
                    binder,
                    c"AIBinder_decStrong",
                    unsafe extern "C" fn(*mut RawBinder)
                ),
                binder_remote: symbol!(
                    binder,
                    c"AIBinder_isRemote",
                    unsafe extern "C" fn(*const RawBinder) -> bool
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

    fn status(operation: &'static str, status: i32) -> Result<(), Error> {
        if status == 0 {
            Ok(())
        } else {
            Err(Error::Parcel { operation, status })
        }
    }

    /// Owns one strong reference transferred by `AParcel_readStrongBinder`.
    struct StrongBinder {
        api: &'static Api,
        raw: NonNull<RawBinder>,
    }

    impl StrongBinder {
        fn remote(&self) -> bool {
            // SAFETY: live strong reference owned by this thread; the call only
            // inspects the object and takes no ownership.
            unsafe { (self.api.binder_remote)(self.raw.as_ptr()) }
        }
    }

    impl Drop for StrongBinder {
        fn drop(&mut self) {
            // SAFETY: releases exactly the one reference this owner received; the
            // function table and its library live for the process.
            unsafe { (self.api.binder_release)(self.raw.as_ptr()) };
        }
    }

    /// Owns one scratch parcel from `AParcel_create`, including the object
    /// references a flattened surface stored in it.
    struct Parcel {
        api: &'static Api,
        raw: NonNull<RawParcel>,
    }

    impl Parcel {
        fn create(api: &'static Api) -> Result<Self, Error> {
            // SAFETY: no arguments; a non-null result is one parcel owned by the caller.
            NonNull::new(unsafe { (api.parcel_create)() })
                .map(|raw| Self { api, raw })
                .ok_or(Error::Parcel {
                    operation: "create",
                    status: 0,
                })
        }

        fn write(&self, input: &ConfiguredInput) -> Result<(), Error> {
            // SAFETY: the borrowed input keeps its window reference alive for the
            // call, which does not take ownership of it; the parcel is live, owned
            // by this thread and takes its own references to what it stores.
            let result =
                unsafe { (self.api.window_write)(input.window().as_ptr(), self.raw.as_ptr()) };
            status("write_window", result)
        }
    }

    impl Drop for Parcel {
        fn drop(&mut self) {
            // SAFETY: sole owner of the parcel from AParcel_create, deleted once;
            // deletion releases the object references the parcel itself holds.
            unsafe { (self.api.parcel_delete)(self.raw.as_ptr()) };
        }
    }

    /// State shared with [`allocate`] for one `AParcel_readString` call.
    struct NameSlot {
        buffer: *mut c_char,
        capacity: usize,
        name: Option<Name>,
    }

    /// `AParcel_stringAllocator`: lend the bounded caller buffer or refuse.
    ///
    /// # Safety
    /// `data` must be the exclusive `NameSlot` passed to the same `AParcel_readString`
    /// call, and `buffer` must be null or a valid out pointer, as that call provides.
    unsafe extern "C" fn allocate(
        data: *mut c_void,
        length: i32,
        buffer: *mut *mut c_char,
    ) -> bool {
        // SAFETY: `data` is the NameSlot that `Source::name` borrows exclusively for
        // the duration of the synchronous readString call invoking this callback.
        let slot = unsafe { &mut *data.cast::<NameSlot>() };
        let Ok(length) = usize::try_from(length) else {
            // A null string: nothing is written and the out pointer may be null.
            slot.name = Some(Name::Null);
            return true;
        };
        if length == 0 || buffer.is_null() {
            return false;
        }
        if length > slot.capacity {
            slot.name = Some(Name::TooLong);
            return false;
        }
        // SAFETY: non-null out pointer supplied by readString for this callback; the
        // lent buffer has at least `length` writable bytes and outlives the call.
        unsafe { buffer.write(slot.buffer) };
        slot.name = Some(Name::Utf8(length - 1));
        true
    }

    impl Source for Parcel {
        fn size(&mut self) -> i32 {
            // SAFETY: live parcel owned by this thread; the call only reads it.
            unsafe { (self.api.parcel_size)(self.raw.as_ptr()) }
        }

        fn rewind(&mut self) -> Result<(), Error> {
            // SAFETY: live parcel; position 0 is always within its data.
            let result = unsafe { (self.api.parcel_set_position)(self.raw.as_ptr(), 0) };
            status("rewind", result)
        }

        fn position(&mut self) -> i32 {
            // SAFETY: live parcel owned by this thread; the call only reads it.
            unsafe { (self.api.parcel_position)(self.raw.as_ptr()) }
        }

        fn name(&mut self, buffer: &mut [u8; NAME_BYTES]) -> Result<Name, Error> {
            let mut slot = NameSlot {
                buffer: buffer.as_mut_ptr().cast(),
                capacity: NAME_BYTES,
                name: None,
            };
            // SAFETY: live parcel; `slot` and the buffer it points to are exclusively
            // borrowed until the synchronous call returns, and `allocate` matches the
            // allocator contract (lends at most `capacity` bytes or refuses).
            let result = unsafe {
                (self.api.parcel_read_string)(self.raw.as_ptr(), (&raw mut slot).cast(), allocate)
            };
            match slot.name {
                Some(Name::TooLong) => Ok(Name::TooLong),
                Some(name) if result == 0 => Ok(name),
                _ => Err(Error::Parcel {
                    operation: "read_name",
                    status: result,
                }),
            }
        }

        fn int32(&mut self, operation: &'static str) -> Result<i32, Error> {
            let mut value = 0;
            // SAFETY: live parcel and a valid out pointer for the call.
            let result = unsafe { (self.api.parcel_read_i32)(self.raw.as_ptr(), &raw mut value) };
            status(operation, result).map(|()| value)
        }

        fn uint32(&mut self, operation: &'static str) -> Result<u32, Error> {
            let mut value = 0;
            // SAFETY: live parcel and a valid out pointer for the call.
            let result = unsafe { (self.api.parcel_read_u32)(self.raw.as_ptr(), &raw mut value) };
            status(operation, result).map(|()| value)
        }

        fn binder(&mut self, operation: &'static str) -> Result<Option<bool>, Error> {
            let mut raw = ptr::null_mut();
            // SAFETY: live parcel and a valid out pointer; a non-null result
            // transfers one strong reference, owned below even if the status is bad.
            let result = unsafe { (self.api.parcel_read_binder)(self.raw.as_ptr(), &raw mut raw) };
            let binder = NonNull::new(raw).map(|raw| StrongBinder { api: self.api, raw });
            status(operation, result)?;
            // The reference is released here, before the parcel, window and codec.
            Ok(binder.map(|binder| binder.remote()))
        }
    }

    /// Locals drop in reverse order: the parcel (and the references it stores),
    /// then the input window, then the frameless encoder.
    pub(super) fn probe(outcome: &mut Outcome) -> Result<(), Error> {
        let api = Api::global()?;
        let input = ConfiguredInput::new(&PRESET)?;
        outcome.codec = input.codec_name();
        let mut parcel = Parcel::create(api)?;
        parcel.write(&input)?;
        decode(&mut parcel, outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_NAME: &str = "carlo-secret-surface-ç";

    struct Fake {
        size: i32,
        name: Option<String>,
        single_buffered: i32,
        magic: u32,
        producer: Option<bool>,
        control: Option<bool>,
        end: i32,
        reads: Vec<&'static str>,
    }

    impl Fake {
        fn surface() -> Self {
            Self {
                size: 96,
                name: Some(SECRET_NAME.to_owned()),
                single_buffered: 0,
                magic: MAGIC,
                producer: Some(true),
                control: None,
                end: 96,
                reads: Vec::new(),
            }
        }
    }

    impl Source for Fake {
        fn size(&mut self) -> i32 {
            self.size
        }

        fn rewind(&mut self) -> Result<(), Error> {
            Ok(())
        }

        fn position(&mut self) -> i32 {
            // Offset both ends: only the difference may be relied on.
            7 + if self.reads.is_empty() { 0 } else { self.end }
        }

        fn name(&mut self, buffer: &mut [u8; NAME_BYTES]) -> Result<Name, Error> {
            self.reads.push("name");
            let Some(name) = &self.name else {
                return Ok(Name::Null);
            };
            if name.len() + 1 > NAME_BYTES {
                return Ok(Name::TooLong);
            }
            buffer[..name.len()].copy_from_slice(name.as_bytes());
            Ok(Name::Utf8(name.len()))
        }

        fn int32(&mut self, operation: &'static str) -> Result<i32, Error> {
            self.reads.push(operation);
            Ok(self.single_buffered)
        }

        fn uint32(&mut self, operation: &'static str) -> Result<u32, Error> {
            self.reads.push(operation);
            Ok(self.magic)
        }

        fn binder(&mut self, operation: &'static str) -> Result<Option<bool>, Error> {
            self.reads.push(operation);
            Ok(match operation {
                "read_producer" => self.producer,
                _ => self.control,
            })
        }
    }

    fn run(mut fake: Fake) -> (Outcome, Vec<&'static str>) {
        let mut outcome = Outcome::default();
        outcome.error = decode(&mut fake, &mut outcome).err();
        (outcome, fake.reads)
    }

    #[test]
    fn pinned_layout_is_read_in_order_and_passes() {
        let (outcome, reads) = run(Fake::surface());
        assert_eq!(
            reads,
            [
                "name",
                "read_single_buffered",
                "read_magic",
                "read_producer",
                "read_control_handle"
            ]
        );
        assert_eq!(
            outcome,
            Outcome {
                parcel_bytes: Some(96),
                name_units: Some(SECRET_NAME.encode_utf16().count()),
                single_buffered: Some(false),
                magic_matches: true,
                producer_present: true,
                producer_remote: Some(true),
                control_handle_present: false,
                fully_consumed: true,
                codec: None,
                error: None,
            }
        );
        assert!(outcome.passed());

        let (trailing, _) = run(Fake {
            end: 92,
            control: Some(false),
            name: None,
            single_buffered: 1,
            ..Fake::surface()
        });
        assert!(trailing.passed() && !trailing.fully_consumed);
        assert!(trailing.control_handle_present);
        assert_eq!(trailing.name_units, None);
        assert_eq!(trailing.single_buffered, Some(true));
    }

    #[test]
    fn wrong_magic_stops_before_binders_and_missing_producer_never_passes() {
        let (outcome, reads) = run(Fake {
            magic: MAGIC + 1,
            ..Fake::surface()
        });
        assert_eq!(outcome.error, Some(Error::UnexpectedParcel("magic")));
        assert!(!outcome.magic_matches && !outcome.passed());
        assert_eq!(reads.last(), Some(&"read_magic"));

        let (outcome, reads) = run(Fake {
            producer: None,
            control: Some(true),
            ..Fake::surface()
        });
        assert_eq!(outcome.error, None);
        assert!(outcome.magic_matches && !outcome.producer_present);
        assert_eq!(outcome.producer_remote, None);
        assert!(outcome.control_handle_present && outcome.fully_consumed);
        assert!(!outcome.passed());
        assert_eq!(reads.len(), 5);

        let (outcome, _) = run(Fake {
            single_buffered: 2,
            ..Fake::surface()
        });
        assert_eq!(
            outcome.error,
            Some(Error::UnexpectedParcel("single_buffered"))
        );
    }

    #[test]
    fn oversize_parcel_and_name_are_rejected_with_typed_limits() {
        for size in [MAX_PARCEL_BYTES as i32 + 1, i32::MAX] {
            let (outcome, reads) = run(Fake {
                size,
                ..Fake::surface()
            });
            assert_eq!(outcome.error, Some(Error::LimitExceeded("parcel_bytes")));
            assert!(reads.is_empty() && !outcome.passed());
        }
        let (outcome, reads) = run(Fake {
            size: -1,
            ..Fake::surface()
        });
        assert_eq!(outcome.error, Some(Error::UnexpectedParcel("size")));
        assert!(reads.is_empty());

        // Too long for the lent buffer, and within it but over the unit bound.
        for name in ["é".repeat(NAME_BYTES), "a".repeat(MAX_NAME_UNITS + 1)] {
            let (outcome, reads) = run(Fake {
                name: Some(name),
                ..Fake::surface()
            });
            assert_eq!(outcome.error, Some(Error::LimitExceeded("name_units")));
            assert_eq!((outcome.name_units, reads.len()), (None, 1));
        }
        let (outcome, _) = run(Fake {
            name: Some("a".repeat(MAX_NAME_UNITS)),
            ..Fake::surface()
        });
        assert_eq!(outcome.name_units, Some(MAX_NAME_UNITS));
        assert!(outcome.passed());
    }

    #[test]
    fn report_shape_is_stable_and_never_contains_the_surface_name() {
        let (mut outcome, _) = run(Fake::surface());
        outcome.codec = Some("c2.test.avc.encoder".to_owned());
        let report = Report::new(Diagnostics::collect().unwrap(), outcome);
        assert!(report.passed());
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("carlo-secret") && !text.contains(SECRET_NAME));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["source"], "encoder_input_surface");
        assert_eq!(json["screen_capture"], false);
        assert_eq!(json["video_available"], false);
        let mut keys: Vec<_> = json["outcome"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "codec",
                "control_handle_present",
                "error",
                "fully_consumed",
                "magic_matches",
                "name_units",
                "parcel_bytes",
                "producer_present",
                "producer_remote",
                "single_buffered"
            ]
        );
        assert_eq!(json["outcome"]["error"], serde_json::Value::Null);
        assert_eq!(json["outcome"]["name_units"], 22);

        let failed = Report::new(
            Diagnostics::collect().unwrap(),
            Outcome {
                error: Some(Error::Parcel {
                    operation: "write_window",
                    status: -22,
                }),
                ..Outcome::default()
            },
        );
        assert!(!failed.passed());
        assert_eq!(
            serde_json::to_value(&failed).unwrap()["outcome"]["error"],
            serde_json::json!({
                "kind": "parcel",
                "detail": {"operation": "write_window", "status": -22}
            })
        );
    }

    #[test]
    fn only_executed_profiles_at_or_above_api_34_are_supported() {
        assert!(supported(Some(36), "aarch64"));
        for (api, architecture) in [
            (Some(36), "x86_64"),
            (Some(34), "aarch64"),
            (Some(33), "aarch64"),
            (None, "aarch64"),
        ] {
            assert!(!supported(api, architecture));
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn unsupported_host_reports_unavailable_without_touching_a_codec() {
        let report = collect().unwrap();
        assert_eq!(report.outcome.error, Some(Error::UnsupportedProfile));
        assert!(!report.passed());
        assert_eq!(report.outcome.parcel_bytes, None);
    }
}
