//! Narrow C ABI boundary pinned to frameworks/native bfcf75076e562945bae131a4929f29a90d0d2481.
//!
//! Handles and function pointers never leave this module. NonNull handles prevent
//! Send/Sync: all operations and releases happen on the invoking thread. A strong
//! Binder borrows its function table, which owns the dlopen reference; decStrong
//! runs before dlclose. No Binder thread pool or callbacks are created by this probe.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::NonNull;

#[cfg(target_os = "android")]
use super::Outcome;
use super::{Lookup, ProbeStatus, Symbol, classify};

const NAMES: [&CStr; 4] = [
    c"AServiceManager_checkService",
    c"AIBinder_decStrong",
    c"AIBinder_isRemote",
    c"AIBinder_ping",
];
const LABELS: [&str; 4] = [
    "AServiceManager_checkService",
    "AIBinder_decStrong",
    "AIBinder_isRemote",
    "AIBinder_ping",
];

#[repr(C)]
struct RawBinder {
    _opaque: [u8; 0],
}

type CheckService = unsafe extern "C" fn(*const c_char) -> *mut RawBinder;
type DecStrong = unsafe extern "C" fn(*mut RawBinder);
type IsRemote = unsafe extern "C" fn(*const RawBinder) -> bool;
type Ping = unsafe extern "C" fn(*mut RawBinder) -> i32;

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

pub(crate) struct Library(NonNull<c_void>);

impl Library {
    pub(crate) fn open(name: &CStr) -> Option<Self> {
        const RTLD_NOW: c_int = 2;
        // SAFETY: name is terminated and lives through dlopen; RTLD_NOW is Bionic/POSIX's 2.
        NonNull::new(unsafe { dlopen(name.as_ptr(), RTLD_NOW) }).map(Self)
    }

    pub(crate) fn symbol(&self, name: &CStr) -> Option<NonNull<c_void>> {
        // SAFETY: self owns an open handle, and name is terminated for this call.
        // The address is never dereferenced until its specific ABI is established below.
        NonNull::new(unsafe { dlsym(self.0.as_ptr(), name.as_ptr()) })
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this handle came from dlopen and is closed exactly once. All
        // callers/function-table borrowers are already gone when the owner drops.
        unsafe { dlclose(self.0.as_ptr()) };
    }
}

struct Functions {
    _library: Library,
    check: CheckService,
    release: DecStrong,
    remote: IsRemote,
    ping: Ping,
}

impl Functions {
    fn load(library: Library) -> Result<(Self, Vec<Symbol>), Vec<Symbol>> {
        let addresses = NAMES.map(|name| library.symbol(name));
        let symbols = LABELS
            .iter()
            .zip(&addresses)
            .map(|(name, address)| Symbol {
                name,
                available: address.is_some(),
            })
            .collect();
        let [Some(check), Some(release), Some(remote), Some(ping)] = addresses else {
            // library closes on this error; no service calls or Binder refs yet.
            return Err(symbols);
        };
        // SAFETY: these four non-null exports are C functions with the exact
        // signatures declared in the pinned binder_manager.h/binder_ibinder.h.
        // The API 34 shell guard limits deployment; the owning library remains
        // alive for all calls. No Rust layout is substituted for AIBinder.
        let (check, release, remote, ping) = unsafe {
            (
                std::mem::transmute::<*mut c_void, CheckService>(check.as_ptr()),
                std::mem::transmute::<*mut c_void, DecStrong>(release.as_ptr()),
                std::mem::transmute::<*mut c_void, IsRemote>(remote.as_ptr()),
                std::mem::transmute::<*mut c_void, Ping>(ping.as_ptr()),
            )
        };
        Ok((
            Self {
                _library: library,
                check,
                release,
                remote,
                ping,
            },
            symbols,
        ))
    }

    fn lookup(&self, name: &'static CStr, label: &'static str) -> Lookup {
        // SAFETY: name is terminated. checkService returns null or transfers one
        // strong reference, documented by the pinned platform header and source.
        let pointer = unsafe { (self.check)(name.as_ptr()) };
        let Some(pointer) = NonNull::new(pointer) else {
            // Null does not distinguish absent/denied/communication failure.
            return Lookup {
                name: label,
                found: false,
                remote: None,
                ping_status: None,
            };
        };
        let binder = StrongBinder { pointer, api: self };
        // SAFETY: binder owns a live strong reference from this exact library;
        // neither function takes ownership, and calls run on the same thread.
        let (remote, ping_status) = unsafe {
            (
                (self.remote)(binder.pointer.as_ptr()),
                (self.ping)(binder.pointer.as_ptr()),
            )
        };
        Lookup {
            name: label,
            found: true,
            remote: Some(remote),
            ping_status: Some(ping_status),
        }
        // binder drops here on success or nonzero ping status.
    }
}

struct StrongBinder<'a> {
    pointer: NonNull<RawBinder>,
    api: &'a Functions,
}

impl Drop for StrongBinder<'_> {
    fn drop(&mut self) {
        // SAFETY: checkService gave this owner exactly one strong ref; no clone
        // or transfer occurs. The borrowed table/library outlives this release.
        unsafe { (self.api.release)(self.pointer.as_ptr()) };
    }
}

#[cfg(target_os = "android")]
pub(super) fn probe() -> Outcome {
    let Some(library) = Library::open(c"libbinder_ndk.so") else {
        return Outcome {
            status: ProbeStatus::LibraryUnavailable,
            symbols: Vec::new(),
            lookups: Vec::new(),
        };
    };
    let (api, symbols) = match Functions::load(library) {
        Ok(loaded) => loaded,
        Err(symbols) => {
            return Outcome {
                status: ProbeStatus::MissingSymbols,
                symbols,
                lookups: Vec::new(),
            };
        }
    };
    let accessibility = api.lookup(c"accessibility", "accessibility");
    let control = api.lookup(c"carlo.probe.absent", "carlo.probe.absent");
    Outcome {
        status: classify(&accessibility, &control),
        symbols,
        lookups: vec![accessibility, control],
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    std::thread_local! {
        static RELEASES: Cell<usize> = const { Cell::new(0) };
        static PING_STATUS: Cell<i32> = const { Cell::new(0) };
    }

    fn process_library() -> Library {
        // SAFETY: POSIX dlopen(NULL, RTLD_NOW) returns an owned handle to this
        // process; Library closes this reference after the test's borrowers drop.
        Library(NonNull::new(unsafe { dlopen(std::ptr::null(), 2) }).unwrap())
    }

    unsafe extern "C" fn check(name: *const c_char) -> *mut RawBinder {
        // SAFETY: our lookup wrapper always supplies a live NUL-terminated CStr.
        if unsafe { CStr::from_ptr(name) } == c"accessibility" {
            Box::into_raw(Box::new(0_u8)).cast()
        } else {
            std::ptr::null_mut()
        }
    }

    unsafe extern "C" fn release(pointer: *mut RawBinder) {
        // SAFETY: fake check transferred one Box<u8>, released exactly once by
        // StrongBinder; no alias dereferences or allocations remain afterward.
        drop(unsafe { Box::from_raw(pointer.cast::<u8>()) });
        RELEASES.set(RELEASES.get() + 1);
    }

    unsafe extern "C" fn remote(_: *const RawBinder) -> bool {
        true
    }

    unsafe extern "C" fn ping(_: *mut RawBinder) -> i32 {
        PING_STATUS.get()
    }

    #[test]
    fn lookup_releases_once_on_success_and_failed_ping_and_never_releases_null() {
        let api = Functions {
            _library: process_library(),
            check,
            release,
            remote,
            ping,
        };
        RELEASES.set(0);
        for (status, expected) in [(0, ProbeStatus::Passed), (-32, ProbeStatus::PingFailed)] {
            PING_STATUS.set(status);
            let found = api.lookup(c"accessibility", "accessibility");
            let absent = api.lookup(c"carlo.probe.absent", "carlo.probe.absent");
            assert!(found.found);
            assert_eq!(found.remote, Some(true));
            assert!(!absent.found);
            assert_eq!(absent.ping_status, None);
            assert_eq!(classify(&found, &absent), expected);
        }
        assert_eq!(RELEASES.get(), 2);
        let absent = api.lookup(c"carlo.probe.absent", "carlo.probe.absent");
        assert_eq!(classify(&absent, &absent), ProbeStatus::ServiceUnavailable);
        let found = api.lookup(c"accessibility", "accessibility");
        PING_STATUS.set(0);
        let good = api.lookup(c"accessibility", "accessibility");
        assert_eq!(
            classify(&good, &found),
            ProbeStatus::UnexpectedControlService
        );
        assert_eq!(RELEASES.get(), 4);
    }

    #[test]
    fn loader_refuses_absent_libraries_and_reports_every_missing_symbol_without_calls() {
        assert!(Library::open(c"libcarlo_intentionally_missing_718b03d9.so").is_none());
        let library = process_library();
        assert!(
            library
                .symbol(c"Carlo_intentionally_missing_718b03d9")
                .is_none()
        );
        #[cfg(not(target_os = "android"))]
        {
            let symbols = Functions::load(library).err().unwrap();
            assert_eq!(symbols.len(), NAMES.len());
            assert!(symbols.iter().all(|symbol| !symbol.available));
        }
    }
}
