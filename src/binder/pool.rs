//! The one Binder thread pool of this process, shared by every feature that
//! receives incoming transactions (automation callbacks, the encoder's graphic
//! producer during display mirroring).
//!
//! `ABinderProcess_*` are libbinder_ndk platform exports declared in
//! frameworks/native `libs/binder/ndk/include_platform/android/binder_process.h`
//! (not in the public NDK sysroot), resolved at runtime. The executable owns the
//! pool: a pool somebody else already started is refused, the maximum is set once
//! before the start and the library handle is kept for the process lifetime
//! because pool threads run its code until exit. Pool threads are never joined.

use super::native::Library;
use std::{ffi::c_void, sync::OnceLock};

/// Kernel-spawned workers every feature of this process requests (plus the one
/// `ABinderProcess_startThreadPool` creates). The maximum can only be set before
/// the start, so the first caller decides for the process: all callers ask for
/// this one value and either order works. Sized by the display mirror, whose
/// encoder producer is called by the compositor for every frame while the
/// display service calls back; an automation session alone needs two. Workers
/// are only spawned on demand.
pub(crate) const THREADS: u32 = 4;

/// Why the process pool cannot serve a caller. Carries no platform text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoolError {
    LibraryUnavailable,
    MissingSymbol(&'static str),
    /// A pool not started by this helper exists, or the driver refused the maximum.
    Foreign,
    /// The pool was already started with fewer kernel-spawned threads than requested.
    Insufficient,
}

struct Pool {
    _library: Library,
    max_threads: u32,
}

// SAFETY: the dlopen handle is only kept alive, never used after `start`; the
// static owner is never dropped.
unsafe impl Send for Pool {}
// SAFETY: same invariant; the value is immutable after initialization.
unsafe impl Sync for Pool {}

/// Start the pool with `max_threads` kernel-spawned workers (plus the one worker
/// `ABinderProcess_startThreadPool` creates), or confirm that the pool this
/// helper already started is at least that large. The first caller decides, so
/// callers pass [`THREADS`].
pub(crate) fn start(max_threads: u32) -> Result<(), PoolError> {
    static POOL: OnceLock<Result<Pool, PoolError>> = OnceLock::new();
    match POOL.get_or_init(|| launch(max_threads)) {
        Ok(pool) if pool.max_threads >= max_threads => Ok(()),
        Ok(_) => Err(PoolError::Insufficient),
        Err(error) => Err(*error),
    }
}

fn launch(max_threads: u32) -> Result<Pool, PoolError> {
    let library = Library::open(c"libbinder_ndk.so").ok_or(PoolError::LibraryUnavailable)?;
    macro_rules! symbol {
        ($name:literal, $type:ty) => {{
            let address = library.symbol($name).ok_or(PoolError::MissingSymbol(
                $name.to_str().expect("static ASCII symbol"),
            ))?;
            // SAFETY: each invocation names the exact C signature of the export in
            // the pinned binder_process.h (uint32_t is u32, bool is one byte). The
            // library is retained by the returned static owner.
            unsafe { std::mem::transmute::<*mut c_void, $type>(address.as_ptr()) }
        }};
    }
    let set_max = symbol!(
        c"ABinderProcess_setThreadPoolMaxThreadCount",
        unsafe extern "C" fn(u32) -> bool
    );
    let is_started = symbol!(
        c"ABinderProcess_isThreadPoolStarted",
        unsafe extern "C" fn() -> bool
    );
    let start = symbol!(c"ABinderProcess_startThreadPool", unsafe extern "C" fn());
    // SAFETY: no arguments or a plain count; called once, before any callback
    // object of this process is handed to another process.
    if unsafe { is_started() || !set_max(max_threads) } {
        return Err(PoolError::Foreign);
    }
    // SAFETY: one-shot start after a finite maximum was set; the library and all
    // class metadata used by callbacks stay loaded for the process lifetime.
    unsafe { start() };
    Ok(Pool {
        _library: library,
        max_threads,
    })
}
