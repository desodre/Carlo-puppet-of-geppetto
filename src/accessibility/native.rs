//! Private API 34 FFI. The process owns one immutable table/classes/pool. Keeping
//! the dlopen handle in a static prevents callbacks into unloaded code. Per-session
//! references/parcels/state still use RAII; no unbounded queues or worker creation.

mod query;

use super::{
    Error, Outcome, contract,
    state::{InitReader, Slot, decode_init},
};
use crate::binder::native::Library;
use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[repr(C)]
struct Binder {
    _opaque: [u8; 0],
}
#[repr(C)]
struct Class {
    _opaque: [u8; 0],
}
#[repr(C)]
struct Parcel {
    _opaque: [u8; 0],
}
#[repr(C)]
struct Status {
    _opaque: [u8; 0],
}
type OnCreate = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type OnDestroy = unsafe extern "C" fn(*mut c_void);
type OnTransact = unsafe extern "C" fn(*mut Binder, u32, *const Parcel, *mut Parcel) -> i32;

struct Api {
    _library: Library,
    check: unsafe extern "C" fn(*const c_char) -> *mut Binder,
    retain: unsafe extern "C" fn(*mut Binder),
    set_position: unsafe extern "C" fn(*const Parcel, i32) -> i32,
    release: unsafe extern "C" fn(*mut Binder),
    define: unsafe extern "C" fn(*const c_char, OnCreate, OnDestroy, OnTransact) -> *mut Class,
    associate: unsafe extern "C" fn(*mut Binder, *const Class) -> bool,
    new: unsafe extern "C" fn(*const Class, *mut c_void) -> *mut Binder,
    user_data: unsafe extern "C" fn(*mut Binder) -> *mut c_void,
    caller_uid: unsafe extern "C" fn() -> u32,
    prepare: unsafe extern "C" fn(*mut Binder, *mut *mut Parcel) -> i32,
    transact:
        unsafe extern "C" fn(*mut Binder, u32, *mut *mut Parcel, *mut *mut Parcel, u32) -> i32,
    delete_parcel: unsafe extern "C" fn(*mut Parcel),
    write_int: unsafe extern "C" fn(*mut Parcel, i32) -> i32,
    write_long: unsafe extern "C" fn(*mut Parcel, i64) -> i32,
    write_string: unsafe extern "C" fn(*mut Parcel, *const c_char, i32) -> i32,
    write_binder: unsafe extern "C" fn(*mut Parcel, *mut Binder) -> i32,
    read_int: unsafe extern "C" fn(*const Parcel, *mut i32) -> i32,
    read_binder: unsafe extern "C" fn(*const Parcel, *mut *mut Binder) -> i32,
    data_size: unsafe extern "C" fn(*const Parcel) -> i32,
    data_position: unsafe extern "C" fn(*const Parcel) -> i32,
    read_status: unsafe extern "C" fn(*const Parcel, *mut *mut Status) -> i32,
    exception: unsafe extern "C" fn(*const Status) -> i32,
    delete_status: unsafe extern "C" fn(*mut Status),
    pool_max: unsafe extern "C" fn(u32) -> bool,
    pool_started: unsafe extern "C" fn() -> bool,
    pool_start: unsafe extern "C" fn(),
}

// SAFETY: this table has immutable C function pointers and an owned dlopen handle.
// Bionic permits cross-thread symbol calls; the static table is never dropped.
// Binder refcounts are thread-safe. Mutable session state has its own Mutex.
unsafe impl Send for Api {}
// SAFETY: same invariant as Send; the table itself has no mutable runtime data.
unsafe impl Sync for Api {}

impl Api {
    fn global() -> Result<&'static Self, Error> {
        static API: OnceLock<Result<Api, Error>> = OnceLock::new();
        API.get_or_init(Self::load).as_ref().map_err(Clone::clone)
    }
    fn load() -> Result<Self, Error> {
        let library = Library::open(c"libbinder_ndk.so").ok_or(Error::LibraryUnavailable)?;
        macro_rules! symbol {
            ($name:literal, $type:ty) => {{
                let address = library.symbol($name).ok_or(Error::MissingSymbol(
                    $name.to_str().expect("static ASCII symbol"),
                ))?;
                // SAFETY: every invocation names the exact C signature from pinned
                // binder_ibinder/parcel/status/manager/process headers. The library
                // is owned by this table and retained for the process lifetime.
                unsafe { std::mem::transmute::<*mut c_void, $type>(address.as_ptr()) }
            }};
        }
        Ok(Self {
            check: symbol!(
                c"AServiceManager_checkService",
                unsafe extern "C" fn(*const c_char) -> *mut Binder
            ),
            retain: symbol!(c"AIBinder_incStrong", unsafe extern "C" fn(*mut Binder)),
            set_position: symbol!(
                c"AParcel_setDataPosition",
                unsafe extern "C" fn(*const Parcel, i32) -> i32
            ),
            release: symbol!(c"AIBinder_decStrong", unsafe extern "C" fn(*mut Binder)),
            define: symbol!(
                c"AIBinder_Class_define",
                unsafe extern "C" fn(*const c_char, OnCreate, OnDestroy, OnTransact) -> *mut Class
            ),
            associate: symbol!(
                c"AIBinder_associateClass",
                unsafe extern "C" fn(*mut Binder, *const Class) -> bool
            ),
            new: symbol!(
                c"AIBinder_new",
                unsafe extern "C" fn(*const Class, *mut c_void) -> *mut Binder
            ),
            user_data: symbol!(
                c"AIBinder_getUserData",
                unsafe extern "C" fn(*mut Binder) -> *mut c_void
            ),
            caller_uid: symbol!(c"AIBinder_getCallingUid", unsafe extern "C" fn() -> u32),
            prepare: symbol!(
                c"AIBinder_prepareTransaction",
                unsafe extern "C" fn(*mut Binder, *mut *mut Parcel) -> i32
            ),
            transact: symbol!(
                c"AIBinder_transact",
                unsafe extern "C" fn(
                    *mut Binder,
                    u32,
                    *mut *mut Parcel,
                    *mut *mut Parcel,
                    u32,
                ) -> i32
            ),
            delete_parcel: symbol!(c"AParcel_delete", unsafe extern "C" fn(*mut Parcel)),
            write_int: symbol!(
                c"AParcel_writeInt32",
                unsafe extern "C" fn(*mut Parcel, i32) -> i32
            ),
            write_long: symbol!(
                c"AParcel_writeInt64",
                unsafe extern "C" fn(*mut Parcel, i64) -> i32
            ),
            write_string: symbol!(
                c"AParcel_writeString",
                unsafe extern "C" fn(*mut Parcel, *const c_char, i32) -> i32
            ),
            write_binder: symbol!(
                c"AParcel_writeStrongBinder",
                unsafe extern "C" fn(*mut Parcel, *mut Binder) -> i32
            ),
            read_int: symbol!(
                c"AParcel_readInt32",
                unsafe extern "C" fn(*const Parcel, *mut i32) -> i32
            ),
            read_binder: symbol!(
                c"AParcel_readStrongBinder",
                unsafe extern "C" fn(*const Parcel, *mut *mut Binder) -> i32
            ),
            data_size: symbol!(
                c"AParcel_getDataSize",
                unsafe extern "C" fn(*const Parcel) -> i32
            ),
            data_position: symbol!(
                c"AParcel_getDataPosition",
                unsafe extern "C" fn(*const Parcel) -> i32
            ),
            read_status: symbol!(
                c"AParcel_readStatusHeader",
                unsafe extern "C" fn(*const Parcel, *mut *mut Status) -> i32
            ),
            exception: symbol!(
                c"AStatus_getExceptionCode",
                unsafe extern "C" fn(*const Status) -> i32
            ),
            delete_status: symbol!(c"AStatus_delete", unsafe extern "C" fn(*mut Status)),
            pool_max: symbol!(
                c"ABinderProcess_setThreadPoolMaxThreadCount",
                unsafe extern "C" fn(u32) -> bool
            ),
            pool_started: symbol!(
                c"ABinderProcess_isThreadPoolStarted",
                unsafe extern "C" fn() -> bool
            ),
            pool_start: symbol!(c"ABinderProcess_startThreadPool", unsafe extern "C" fn()),
            _library: library,
        })
    }
}

fn status(operation: &'static str, value: i32) -> Result<(), Error> {
    if value == 0 {
        Ok(())
    } else {
        Err(Error::Binder {
            operation,
            status: value,
        })
    }
}

struct Strong {
    pointer: NonNull<Binder>,
    api: &'static Api,
}
// SAFETY: a Strong owns one libbinder strong ref; NDK Binder refcounts and remote
// calls support thread transfer. Non-clone ownership prevents double release.
unsafe impl Send for Strong {}
impl Drop for Strong {
    fn drop(&mut self) {
        // SAFETY: one owned ref from checkService/new/readStrongBinder; table lives forever.
        unsafe { (self.api.release)(self.pointer.as_ptr()) };
    }
}
impl Strong {
    fn from_owned(api: &'static Api, pointer: *mut Binder) -> Option<Self> {
        NonNull::new(pointer).map(|pointer| Self { pointer, api })
    }
}

struct OwnedParcel {
    pointer: NonNull<Parcel>,
    api: &'static Api,
}
impl Drop for OwnedParcel {
    fn drop(&mut self) {
        // SAFETY: this owner has the only allocation returned by prepare/transact.
        unsafe { (self.api.delete_parcel)(self.pointer.as_ptr()) };
    }
}
impl OwnedParcel {
    fn from_owned(api: &'static Api, pointer: *mut Parcel) -> Option<Self> {
        NonNull::new(pointer).map(|pointer| Self { pointer, api })
    }
    fn new(target: &Strong) -> Result<Self, Error> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: target owns a live associated remote Binder; output slot is writable.
        let code = unsafe { (target.api.prepare)(target.pointer.as_ptr(), &mut pointer) };
        let owned = Self::from_owned(target.api, pointer);
        status("prepare", code)?;
        owned.ok_or(Error::Runtime("prepare_returned_null"))
    }
    fn int(&mut self, value: i32) -> Result<(), Error> {
        // SAFETY: unique owned parcel, NDK validates available allocation/offsets.
        status("write_int", unsafe {
            (self.api.write_int)(self.pointer.as_ptr(), value)
        })
    }
    fn binder(&mut self, value: &Strong) -> Result<(), Error> {
        // SAFETY: both owned objects are live for the call; NDK retains refs as needed.
        status("write_binder", unsafe {
            (self.api.write_binder)(self.pointer.as_ptr(), value.pointer.as_ptr())
        })
    }
    fn long(&mut self, value: i64) -> Result<(), Error> {
        // SAFETY: live uniquely owned parcel; fixed C ABI from the pinned NDK header.
        status("write_long", unsafe {
            (self.api.write_long)(self.pointer.as_ptr(), value)
        })
    }
    fn info(&mut self) -> Result<(), Error> {
        self.int(1)?; // typed-object presence flag, before service info data
        for field in contract::SERVICE_INFO {
            match field {
                contract::Field::Int(v) => self.int(v)?,
                contract::Field::Long(v) => {
                    // SAFETY: unique owned writable parcel; signature pinned in binder_parcel.h.
                    status("write_long", unsafe {
                        (self.api.write_long)(self.pointer.as_ptr(), v)
                    })?;
                }
                contract::Field::NullString => {
                    // SAFETY: null pointer with length -1 is the NDK nullable String contract.
                    status("write_null_string", unsafe {
                        (self.api.write_string)(self.pointer.as_ptr(), std::ptr::null(), -1)
                    })?;
                }
            }
        }
        Ok(())
    }
    fn call(self, target: &Strong, code: u32, operation: &'static str) -> Result<(), Error> {
        self.reply(target, code, operation).map(|_| ())
    }
    fn reply(self, target: &Strong, code: u32, operation: &'static str) -> Result<Self, Error> {
        let api = self.api;
        let mut input = self.pointer.as_ptr();
        std::mem::forget(self); // transact always consumes and nulls the input slot
        let mut output = std::ptr::null_mut();
        // SAFETY: same target that prepared the parcel; valid unique pointer slots.
        // Pinned AIBinder_transact consumes input even when returning an error.
        let result =
            unsafe { (api.transact)(target.pointer.as_ptr(), code, &mut input, &mut output, 0) };
        let reply = Self::from_owned(api, output);
        status(operation, result)?;
        let reply = reply.ok_or(Error::Runtime("transaction_returned_null"))?;
        // SAFETY: reply is live and owned; size query does not change ownership.
        let size = unsafe { (api.data_size)(reply.pointer.as_ptr()) };
        if !(4..=contract::MAX_CALLBACK_BYTES).contains(&size) {
            return Err(Error::Runtime("invalid_reply_size"));
        }
        let mut raw_status = std::ptr::null_mut();
        // SAFETY: reply cursor is at the Java exception/status header, writable status output.
        let result = unsafe { (api.read_status)(reply.pointer.as_ptr(), &mut raw_status) };
        struct OwnedStatus(*mut Status, &'static Api);
        impl Drop for OwnedStatus {
            fn drop(&mut self) {
                // SAFETY: non-null status allocation from readStatusHeader, one owner.
                unsafe { (self.1.delete_status)(self.0) };
            }
        }
        let owned = NonNull::new(raw_status).map(|p| OwnedStatus(p.as_ptr(), api));
        status("read_exception", result)?;
        let owned = owned.ok_or(Error::Runtime("status_returned_null"))?;
        // SAFETY: owned AStatus is alive; reading its code does not transfer it.
        let exception = unsafe { (api.exception)(owned.0) };
        if exception == 0 {
            Ok(reply)
        } else if operation == "register" && exception == -5 {
            Err(Error::Conflict)
        } else {
            Err(Error::RemoteException {
                operation,
                code: exception,
            })
        }
    }
}

struct Shared {
    slot: Mutex<Slot<Strong>>,
    changed: Condvar,
}
impl Shared {
    fn new() -> Self {
        Self {
            slot: Mutex::new(Slot::default()),
            changed: Condvar::new(),
        }
    }
    fn closing(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            let connection = slot.close();
            drop(slot);
            drop(connection);
            self.changed.notify_all();
        }
    }
    fn wait_init(&self) -> Result<i32, Error> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut slot = self
            .slot
            .lock()
            .map_err(|_| Error::Runtime("callback_state_poisoned"))?;
        loop {
            if STOP.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if let Some(error) = &slot.error {
                return Err(error.clone());
            }
            if let Some((id, _)) = &slot.connection {
                return Ok(*id);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::InitializationTimeout);
            }
            (slot, _) = self
                .changed
                .wait_timeout(slot, left.min(Duration::from_millis(20)))
                .map_err(|_| Error::Runtime("callback_state_poisoned"))?;
        }
    }
}

unsafe extern "C" fn create(data: *mut c_void) -> *mut c_void {
    data
}
unsafe extern "C" fn destroy(data: *mut c_void) {
    // SAFETY: local_binder transferred exactly one Box<Arc<Shared>> to the NDK
    // on successful new; onDestroy runs once after all Binder refs/dispatches end.
    let _ = std::panic::catch_unwind(|| unsafe { drop(Box::from_raw(data.cast::<Arc<Shared>>())) });
}
unsafe extern "C" fn ignore(_: *mut Binder, _: u32, _: *const Parcel, _: *mut Parcel) -> i32 {
    -74
}

struct Reader {
    parcel: NonNull<Parcel>,
    api: &'static Api,
}
impl Reader {
    fn bounded(api: &'static Api, parcel: NonNull<Parcel>) -> Result<Self, Error> {
        // SAFETY: caller owns/borrows a live NDK parcel for this reader's duration.
        let size = unsafe { (api.data_size)(parcel.as_ptr()) };
        if !(0..=contract::MAX_CALLBACK_BYTES).contains(&size) {
            return Err(Error::InvalidCallback);
        }
        Ok(Self { parcel, api })
    }
}
impl InitReader for Reader {
    type Binder = Strong;
    fn binder(&mut self) -> Result<Option<Strong>, Error> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: borrowed parcel is valid for this dispatch, writable output slot.
        let result = unsafe { (self.api.read_binder)(self.parcel.as_ptr(), &mut pointer) };
        let owned = Strong::from_owned(self.api, pointer);
        status("read_binder", result)?;
        Ok(owned)
    }
    fn int(&mut self) -> Result<i32, Error> {
        let mut value = 0;
        // SAFETY: live input parcel and writable int; NDK enforces bounds.
        status("read_int", unsafe {
            (self.api.read_int)(self.parcel.as_ptr(), &mut value)
        })?;
        Ok(value)
    }
    fn finish(&self) -> Result<(), Error> {
        // SAFETY: immutable parcel metadata from the live dispatch.
        let (position, size) = unsafe {
            (
                (self.api.data_position)(self.parcel.as_ptr()),
                (self.api.data_size)(self.parcel.as_ptr()),
            )
        };
        if position >= 0 && position == size {
            Ok(())
        } else {
            Err(Error::InvalidCallback)
        }
    }
}

unsafe extern "C" fn callback(
    binder: *mut Binder,
    code: u32,
    parcel: *const Parcel,
    _: *mut Parcel,
) -> i32 {
    std::panic::catch_unwind(|| dispatch(binder, code, parcel)).unwrap_or(-22)
}
fn dispatch(binder: *mut Binder, code: u32, parcel: *const Parcel) -> i32 {
    let Ok(api) = Api::global() else {
        return -22;
    };
    if binder.is_null() || parcel.is_null() {
        return -22;
    }
    // SAFETY: Binder/Parcel are valid for onTransact; userData came from local_binder.
    let (uid, data) = unsafe { ((api.caller_uid)(), (api.user_data)(binder)) };
    if uid != 1000 {
        return -1;
    }
    if data.is_null() {
        return -22;
    }
    // SAFETY: onDestroy cannot run while this Binder dispatch has a strong ref.
    let shared = unsafe { &*data.cast::<Arc<Shared>>() };
    let Ok(mut slot) = shared.slot.lock() else {
        return -22;
    };
    if slot.closing {
        slot.counters.late = slot.counters.late.saturating_add(1);
        return 0;
    }
    let mut reader = match Reader::bounded(
        api,
        NonNull::new(parcel.cast_mut()).expect("checked non-null"),
    ) {
        Ok(reader) => reader,
        Err(error) => {
            slot.error.get_or_insert(error);
            slot.counters.invalid = slot.counters.invalid.saturating_add(1);
            shared.changed.notify_all();
            return -22;
        }
    };
    let invalid_before = slot.counters.invalid;
    let result = match code {
        contract::INIT => {
            decode_init(&mut reader).and_then(|(id, connection)| slot.accept(id, connection))
        }
        contract::INTERRUPT | contract::CLEAR_CACHE => reader.finish(),
        _ => {
            slot.counters.ignored = slot.counters.ignored.saturating_add(1);
            return -74;
        }
    };
    if let Err(error) = result {
        slot.error.get_or_insert(error);
        if slot.counters.invalid == invalid_before {
            slot.counters.invalid = slot.counters.invalid.saturating_add(1);
        }
        shared.changed.notify_all();
        return -22;
    }
    shared.changed.notify_all();
    0
}

struct Classes {
    manager: NonNull<Class>,
    owner: NonNull<Class>,
    client: NonNull<Class>,
    connection: NonNull<Class>,
    result: NonNull<Class>,
}
// SAFETY: NDK class descriptors/callbacks are immutable metadata, valid until
// process exit. The API is static and class pointers are never freed or mutated.
unsafe impl Send for Classes {}
// SAFETY: same invariant; concurrent new/associate use immutable class metadata.
unsafe impl Sync for Classes {}
impl Classes {
    fn global(api: &'static Api) -> Result<&'static Self, Error> {
        static CLASSES: OnceLock<Result<Classes, Error>> = OnceLock::new();
        CLASSES
            .get_or_init(|| {
                let define = |name: &std::ffi::CStr, handler: OnTransact| {
                    // SAFETY: static terminated descriptor and static ABI-safe callbacks;
                    // NDK owns class metadata for process lifetime (no delete API).
                    NonNull::new(unsafe { (api.define)(name.as_ptr(), create, destroy, handler) })
                        .ok_or(Error::Runtime("define_class_returned_null"))
                };
                Ok(Self {
                    connection: define(c"android.accessibilityservice.IAccessibilityServiceConnection", ignore)?,
                    result: {
                        // SAFETY: static descriptor and ABI callbacks, process-lived metadata.
                        NonNull::new(unsafe { (api.define)(
                            c"android.view.accessibility.IAccessibilityInteractionConnectionCallback".as_ptr(),
                            create, query::destroy, query::callback,
                        ) }).ok_or(Error::Runtime("define_query_class_returned_null"))?
                    },
                    manager: define(c"android.view.accessibility.IAccessibilityManager", ignore)?,
                    owner: define(c"org.geppetto.carlo.Owner", ignore)?,
                    client: define(
                        c"android.accessibilityservice.IAccessibilityServiceClient",
                        callback,
                    )?,
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}
fn local_binder(
    api: &'static Api,
    class: NonNull<Class>,
    shared: &Arc<Shared>,
) -> Result<Strong, Error> {
    let data = Box::into_raw(Box::new(shared.clone()));
    // SAFETY: class/callbacks are process-lived; Box<Arc<Shared>> remains valid until
    // onDestroy and gives independent lifetime to callbacks after session close.
    let pointer = unsafe { (api.new)(class.as_ptr(), data.cast()) };
    if let Some(binder) = Strong::from_owned(api, pointer) {
        Ok(binder)
    } else {
        // SAFETY: onCreate only returns data; a null new result transfers no ownership.
        unsafe { drop(Box::from_raw(data)) };
        Err(Error::Runtime("new_binder_returned_null"))
    }
}
fn start_pool(api: &'static Api) -> Result<(), Error> {
    static POOL: OnceLock<Result<(), Error>> = OnceLock::new();
    POOL.get_or_init(|| {
        // SAFETY: executable owns the process pool, configured once before callbacks.
        // One kernel-spawned worker plus startThreadPool's worker; main never joins.
        let ok = unsafe { !(api.pool_started)() && (api.pool_max)(1) };
        if !ok {
            return Err(Error::Runtime("pool_unavailable_or_already_started"));
        }
        // SAFETY: one-shot start after setting finite maximum, table/classes never unload.
        unsafe { (api.pool_start)() };
        Ok(())
    })
    .clone()
}

struct Session {
    manager: Strong,
    _owner: Strong,
    client: Strong,
    shared: Arc<Shared>,
    cleanup_due: bool,
    query_ids: super::query::Ids,
}
impl Session {
    fn open() -> Result<Self, Error> {
        let api = Api::global()?;
        let classes = Classes::global(api)?;
        start_pool(api)?;
        // SAFETY: static terminated service name; returns null or one owned strong ref.
        let pointer = unsafe { (api.check)(c"accessibility".as_ptr()) };
        let manager = Strong::from_owned(api, pointer)
            .ok_or(Error::Runtime("accessibility_service_unavailable"))?;
        // SAFETY: live remote object, immutable matching descriptor class. NDK checks descriptor.
        if !unsafe { (api.associate)(manager.pointer.as_ptr(), classes.manager.as_ptr()) } {
            return Err(Error::Runtime("manager_descriptor_mismatch"));
        }
        let shared = Arc::new(Shared::new());
        let owner = local_binder(api, classes.owner, &shared)?;
        let client = local_binder(api, classes.client, &shared)?;
        Ok(Self {
            manager,
            _owner: owner,
            client,
            shared,
            cleanup_due: false,
            query_ids: super::query::Ids::default(),
        })
    }
    fn register(&mut self) -> Result<(), Error> {
        let mut request = OwnedParcel::new(&self.manager)?;
        request.binder(&self._owner)?;
        request.binder(&self.client)?;
        request.info()?;
        request.int(0)?; // initial reference emulator, user 0 only
        request.int(contract::DONT_SUPPRESS)?;
        self.cleanup_due = true; // uncertain transport failure may still have registered
        let result = request.call(&self.manager, contract::REGISTER, "register");
        if matches!(
            result,
            Err(Error::Conflict | Error::RemoteException { code: -1 | -2, .. })
        ) {
            self.cleanup_due = false; // known rejection; never unregister another session
        }
        result
    }
    fn close(&mut self) -> Result<(), Error> {
        self.shared.closing();
        if !std::mem::replace(&mut self.cleanup_due, false) {
            return Ok(());
        }
        let mut request = OwnedParcel::new(&self.manager)?;
        request.binder(&self.client)?;
        request.call(&self.manager, contract::UNREGISTER, "unregister")
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

static STOP: AtomicBool = AtomicBool::new(false);
unsafe extern "C" fn signal_handler(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
}
struct Signals {
    old_int: usize,
    old_term: usize,
}
impl Signals {
    fn install() -> Result<Self, Error> {
        STOP.store(false, Ordering::Relaxed);
        // SAFETY: signal handler only does a lock-free atomic store. The CLI has
        // one active proof; handlers are restored after explicit unregister.
        let old_int = unsafe { signal(2, signal_handler as *const () as usize) };
        if old_int == usize::MAX {
            return Err(Error::Runtime("sigint_handler_install_failed"));
        }
        // SAFETY: same static handler; all dispositions are saved/restored.
        let old_term = unsafe { signal(15, signal_handler as *const () as usize) };
        if old_term == usize::MAX {
            // SAFETY: restore exactly the prior disposition returned by signal.
            unsafe { signal(2, old_int) };
            return Err(Error::Runtime("sigterm_handler_install_failed"));
        }
        Ok(Self { old_int, old_term })
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        // SAFETY: restore the original dispositions, no pointers to Rust stack data.
        unsafe {
            signal(2, self.old_int);
            signal(15, self.old_term);
        }
    }
}

#[cfg(test)]
pub(super) fn probe(hold_ms: u32) -> Outcome {
    probe_mode(hold_ms, false)
}

pub(super) fn probe_mode(hold_ms: u32, query_node: bool) -> Outcome {
    static EXCLUSIVE: Mutex<()> = Mutex::new(());
    let Ok(_exclusive) = EXCLUSIVE.try_lock() else {
        return Outcome {
            error: Some(Error::Conflict),
            ..Outcome::default()
        };
    };
    let mut outcome = Outcome::default();
    let signals = match Signals::install() {
        Ok(v) => v,
        Err(e) => {
            return Outcome {
                error: Some(e),
                ..outcome
            };
        }
    };
    let mut session = match Session::open() {
        Ok(v) => v,
        Err(e) => {
            return Outcome {
                error: Some(e),
                ..outcome
            };
        }
    };
    let result = session.register().and_then(|()| session.shared.wait_init());
    match result {
        Ok(id) => {
            outcome.initialized = true;
            outcome.connection_id = Some(id);
            if query_node {
                match session.query_node(
                    contract::ACTIVE_WINDOW,
                    contract::ROOT_NODE,
                    Duration::from_secs(5),
                ) {
                    Ok(envelope) => outcome.query = Some(envelope),
                    Err(error) => outcome.error = Some(error),
                }
            }
            let deadline = Instant::now() + Duration::from_millis(u64::from(hold_ms));
            while !STOP.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20)),
                );
            }
        }
        Err(e) => outcome.error = Some(e),
    }
    let had_registration = session.cleanup_due;
    match session.close() {
        Ok(()) => outcome.unregistered = had_registration,
        Err(e) => outcome.cleanup_error = Some(e),
    }
    outcome.interrupted = STOP.load(Ordering::Relaxed);
    if let Ok(slot) = session.shared.slot.lock() {
        outcome.callbacks = slot.counters;
        if outcome.error.is_none() {
            outcome.error = slot.error.clone();
        }
    } else if outcome.error.is_none() {
        outcome.error = Some(Error::Runtime("callback_state_poisoned"));
    }
    drop(session);
    drop(signals);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn android_sessions_release_resources_and_refuse_competing_registration() {
        let fingerprint = crate::accessibility::fingerprint().unwrap();
        let environment = crate::diagnostics::Diagnostics::collect().unwrap();
        assert!(contract::supported(
            environment.api34_shell(),
            std::env::consts::ARCH,
            fingerprint.as_deref()
        ));
        fn successful(outcome: Outcome) {
            assert!(outcome.initialized && outcome.unregistered, "{outcome:?}");
            assert!(
                outcome.error.is_none() && outcome.cleanup_error.is_none(),
                "{outcome:?}"
            );
            assert_eq!(outcome.callbacks.init, 1);
            assert_eq!(outcome.callbacks.invalid, 0);
        }
        fn resources() -> (usize, usize) {
            (
                std::fs::read_dir("/proc/self/fd").unwrap().count(),
                std::fs::read_dir("/proc/self/task").unwrap().count(),
            )
        }
        successful(probe(0));
        successful(probe(0));
        let baseline = resources();
        let mut samples = Vec::new();
        for _ in 0..20 {
            successful(probe(0));
            samples.push(resources());
        }
        assert!(
            samples.iter().all(|(fds, _)| *fds == baseline.0),
            "{baseline:?} {samples:?}"
        );
        assert!(
            samples
                .iter()
                .all(|(_, threads)| *threads <= baseline.1 + 1),
            "{baseline:?} {samples:?}"
        );
        println!(
            "CARLO_RESOURCE_CYCLES {}",
            serde_json::to_string(&samples).unwrap()
        );
        let mut owner = Session::open().unwrap();
        owner.register().unwrap();
        owner.shared.wait_init().unwrap();
        for _ in 0..2 {
            let mut contender = Session::open().unwrap();
            assert_eq!(contender.register(), Err(Error::Conflict));
            assert!(!contender.cleanup_due);
            contender.close().unwrap();
        }
        owner.close().unwrap();
        owner.close().unwrap(); // second close performs no transaction
        successful(probe(0));
    }
}

#[cfg(test)]
mod parcel_tests {
    use super::*;
    #[test]
    fn ndk_init_reader_rejects_truncation_null_negative_id_and_trailing_data() {
        let api = Api::global().unwrap();
        // These public NDK primitives are resolved only by this test, not shipped
        // as additional session bindings. Signatures pinned in binder_parcel.h.
        let create = api._library.symbol(c"AParcel_create").unwrap();
        let seek = api._library.symbol(c"AParcel_setDataPosition").unwrap();
        // SAFETY: exact exported C signatures, table/library remain process-lived.
        let (create, seek) = unsafe {
            (
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *mut Parcel>(
                    create.as_ptr(),
                ),
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*const Parcel, i32) -> i32>(
                    seek.as_ptr(),
                ),
            )
        };
        let classes = Classes::global(api).unwrap();
        let shared = Arc::new(Shared::new());
        let connection = local_binder(api, classes.owner, &shared).unwrap();
        for case in 0..6 {
            // SAFETY: AParcel_create returns one new standalone parcel allocation.
            let mut parcel = OwnedParcel::from_owned(api, unsafe { create() }).unwrap();
            if case != 0 {
                if case == 1 {
                    // SAFETY: owned parcel; NDK permits a null strong Binder value.
                    status("test_null", unsafe {
                        (api.write_binder)(parcel.pointer.as_ptr(), std::ptr::null_mut())
                    })
                    .unwrap();
                } else {
                    parcel.binder(&connection).unwrap();
                }
                parcel.int(if case == 2 { -1 } else { 9 }).unwrap();
                // SAFETY: same nullable Binder write, this time window token.
                status("test_window", unsafe {
                    (api.write_binder)(parcel.pointer.as_ptr(), std::ptr::null_mut())
                })
                .unwrap();
                if case == 3 {
                    parcel.int(42).unwrap();
                }
                if case == 5 {
                    for _ in 0..(contract::MAX_CALLBACK_BYTES / 4) {
                        parcel.int(0).unwrap();
                    }
                }
            }
            // SAFETY: seeking to offset zero is within every newly written parcel.
            status("test_seek", unsafe { seek(parcel.pointer.as_ptr(), 0) }).unwrap();
            if case == 5 {
                assert!(Reader::bounded(api, parcel.pointer).is_err());
                continue;
            }
            let mut reader = Reader::bounded(api, parcel.pointer).unwrap();
            let result = decode_init(&mut reader);
            if case == 4 {
                assert_eq!(result.unwrap().0, 9);
            } else {
                assert!(result.is_err());
            }
        }
    }
}
