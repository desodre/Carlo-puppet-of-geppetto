//! A new callback capability for each request, never published in ServiceManager.
//! Android forwards it to the target app; unlike session init, caller is not
//! necessarily system UID. Diagnostics exclude payload text and raw parcels.
use super::*;
use crate::accessibility::query::{Envelope, Pending};

struct Query {
    pending: Mutex<Pending>,
    changed: Condvar,
}
struct Active(Arc<Query>);
impl Drop for Active {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.pending.lock() {
            state.close();
        }
        self.0.changed.notify_all();
    }
}
pub(super) unsafe extern "C" fn destroy(data: *mut c_void) {
    // SAFETY: exactly one Box<Arc<Query>> passed to successful AIBinder_new.
    let _ = std::panic::catch_unwind(|| unsafe { drop(Box::from_raw(data.cast::<Arc<Query>>())) });
}
pub(super) unsafe extern "C" fn callback(
    binder: *mut Binder,
    code: u32,
    parcel: *const Parcel,
    _: *mut Parcel,
) -> i32 {
    std::panic::catch_unwind(|| dispatch(binder, code, parcel)).unwrap_or(-22)
}
fn dispatch(binder: *mut Binder, code: u32, parcel: *const Parcel) -> i32 {
    if code != contract::NODE_RESULT {
        return -74;
    }
    let Ok(api) = Api::global() else {
        return -22;
    };
    if binder.is_null() || parcel.is_null() {
        return -22;
    }
    // SAFETY: NDK dispatch keeps binder/user data alive until callback returns.
    let data = unsafe { (api.user_data)(binder) };
    if data.is_null() {
        return -22;
    }
    // SAFETY: only our query class uses this handler and this exact Arc type.
    let shared = unsafe { &*data.cast::<Arc<Query>>() };
    let result = read_envelope(
        api,
        NonNull::new(parcel.cast_mut()).expect("non-null checked"),
    );
    let invalid = result.is_err();
    let Ok(mut pending) = shared.pending.lock() else {
        return -22;
    };
    pending.receive(result, Instant::now());
    shared.changed.notify_all();
    if invalid { -22 } else { 0 }
}

fn read_envelope(api: &'static Api, parcel: NonNull<Parcel>) -> Result<Envelope, Error> {
    let _reader = Reader::bounded(api, parcel)?;
    // SAFETY: borrowed live input; metadata queries preserve ownership and cursor.
    let (start, end) = unsafe {
        (
            (api.data_position)(parcel.as_ptr()),
            (api.data_size)(parcel.as_ptr()),
        )
    };
    let len = end
        .checked_sub(start)
        .filter(|len| start >= 0 && *len >= 8)
        .ok_or(Error::InvalidCallback)? as usize;
    // Total parcel size was bounded before allocating. No runtime bytes are logged/persisted.
    let mut bytes = vec![0_u8; len];
    // SAFETY: destination has len writable bytes; validated start/len are in this live parcel.
    // NDK additionally rejects all Binder/FD objects and checks actual dataBufferSize.
    let result = unsafe { (api.marshal)(parcel.as_ptr(), bytes.as_mut_ptr(), start as usize, len) };
    if result == -38 {
        return Err(Error::UnsupportedNodeLayout("binder_or_fd_objects"));
    }
    status("marshal_node", result)?;
    crate::accessibility::node::decode(&bytes)
}

impl Session {
    /// &mut self serializes requests. No detached workers or unbounded result queue.
    pub(super) fn query_node(
        &mut self,
        window: i32,
        node: i64,
        timeout: Duration,
    ) -> Result<Envelope, Error> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(Error::Runtime("query_deadline_overflow"))?;
        let id = self.query_ids.next()?;
        let query = Arc::new(Query {
            pending: Mutex::new(Pending::new(id, deadline)),
            changed: Condvar::new(),
        });
        let _active = Active(query.clone()); // closes state on every early return, including transport failure
        if STOP.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if timeout.is_zero() {
            return Err(Error::QueryTimeout);
        }
        let api = self.manager.api;
        let classes = Classes::global(api)?;
        let connection = {
            let state = self
                .shared
                .slot
                .lock()
                .map_err(|_| Error::Runtime("callback_state_poisoned"))?;
            if state.closing {
                return Err(Error::Cancelled);
            }
            let (_, connection) = state
                .connection
                .as_ref()
                .ok_or(Error::Runtime("connection_not_initialized"))?;
            // SAFETY: mutex keeps original strong reference alive while taking our own.
            unsafe { (api.retain)(connection.pointer.as_ptr()) };
            Strong {
                pointer: connection.pointer,
                api,
            }
        };
        // SAFETY: live remote Binder and immutable class; NDK checks its descriptor.
        if !unsafe { (api.associate)(connection.pointer.as_ptr(), classes.connection.as_ptr()) } {
            return Err(Error::Runtime("connection_descriptor_mismatch"));
        }
        let data = Box::into_raw(Box::new(query.clone()));
        // SAFETY: callback class owns Box<Arc<Query>> on successful construction.
        let raw = unsafe { (api.new)(classes.result.as_ptr(), data.cast()) };
        let callback = match Strong::from_owned(api, raw) {
            Some(value) => value,
            None => {
                // SAFETY: null new result transfers no ownership; reclaim our box.
                unsafe { drop(Box::from_raw(data)) };
                return Err(Error::Runtime("new_query_callback_returned_null"));
            }
        };
        let mut request = OwnedParcel::new(&connection)?;
        request.int(window)?;
        request.long(node)?;
        request.int(id)?;
        request.binder(&callback)?;
        request.int(0)?; // no prefetch; single-node result callback, no cache in Carlo
        unsafe extern "C" {
            fn gettid() -> i32;
        }
        // SAFETY: Bionic gettid takes no pointers and returns the current native thread ID.
        request.long(i64::from(unsafe { gettid() }))?;
        request.int(0)?; // null typed Bundle
        let reply = request.reply(&connection, contract::FIND_NODE, "find_node")?;
        if STOP.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(Error::QueryTimeout);
        }
        let mut reader = Reader::bounded(api, reply.pointer)?;
        let packages = reader.int()?;
        if packages == -1 {
            return Err(Error::QueryRejected);
        }
        // Package names are unused in this envelope-only proof. Bound their count;
        // the reply is already byte-limited and dropped without allocating strings.
        if !(0..=128).contains(&packages) {
            return Err(Error::InvalidCallback);
        }
        let mut state = query
            .pending
            .lock()
            .map_err(|_| Error::Runtime("query_state_poisoned"))?;
        loop {
            let now = Instant::now();
            if let Some(result) = state.poll(now, STOP.load(Ordering::Relaxed)) {
                return result;
            }
            (state, _) = query
                .changed
                .wait_timeout(
                    state,
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(20)),
                )
                .map_err(|_| Error::Runtime("query_state_poisoned"))?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_query_parcels_reject_truncation_invalid_presence_and_size() {
        let api = Api::global().unwrap();
        let address = api._library.symbol(c"AParcel_create").unwrap();
        // SAFETY: pinned NDK AParcel_create C ABI; API table retains library forever.
        let create = unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *mut Parcel>(
                address.as_ptr(),
            )
        };
        for (fields, valid) in [
            (vec![], false),
            (vec![0], false),
            (vec![0, 1], true),
            (vec![0, 0, 1], false),
            (vec![1, 1], false),
            (vec![2, 0, 0, 1], false),
            (vec![1, 0, 0, 1], true),
            (vec![1, 0, 0, -1], false),
            (
                vec![0; (contract::MAX_CALLBACK_BYTES / 4 + 1) as usize],
                false,
            ),
        ] {
            // SAFETY: newly created standalone parcel returns one owned allocation.
            let mut parcel = OwnedParcel::from_owned(api, unsafe { create() }).unwrap();
            for value in fields {
                parcel.int(value).unwrap();
            }
            // SAFETY: offset zero is valid for any standalone parcel, including empty.
            assert_eq!(unsafe { (api.set_position)(parcel.pointer.as_ptr(), 0) }, 0);
            assert_eq!(read_envelope(api, parcel.pointer).is_ok(), valid);
        }
    }
    #[test]
    fn ndk_utf16_and_object_rejection_match_safe_decoder() {
        let api = Api::global().unwrap();
        let address = api._library.symbol(c"AParcel_create").unwrap();
        // SAFETY: exact public NDK signature, library retained by the process.
        let create = unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *mut Parcel>(
                address.as_ptr(),
            )
        };
        // SAFETY: constructor yields one uniquely owned standalone parcel.
        let mut parcel = OwnedParcel::from_owned(api, unsafe { create() }).unwrap();
        parcel.int(1).unwrap();
        parcel.long(1 << 28).unwrap();
        let value = c"fixture:id/ação_木🪵";
        assert_eq!(
            // SAFETY: valid UTF-8 C string with explicit byte length; NDK writes String16.
            unsafe {
                (api.write_string)(
                    parcel.pointer.as_ptr(),
                    value.as_ptr(),
                    value.to_bytes().len() as i32,
                )
            },
            0
        );
        parcel.int(3).unwrap();
        // SAFETY: offset zero is in bounds of our fully written owned parcel.
        assert_eq!(unsafe { (api.set_position)(parcel.pointer.as_ptr(), 0) }, 0);
        let envelope = read_envelope(api, parcel.pointer).unwrap();
        assert!(envelope.fields_decoded);
        assert!(envelope.node.unwrap().resource_id.as_deref() == Some("fixture:id/ação_木🪵"));
        // SAFETY: second independent standalone allocation, owned until end of test.
        let mut objects = OwnedParcel::from_owned(api, unsafe { create() }).unwrap();
        let classes = Classes::global(api).unwrap();
        let owner = local_binder(api, classes.owner, &Arc::new(Shared::new())).unwrap();
        objects.int(1).unwrap();
        objects.long(1 << 42).unwrap();
        objects.binder(&owner).unwrap();
        objects.int(4).unwrap();
        assert_eq!(
            // SAFETY: same bounded rewind of live owned parcel.
            unsafe { (api.set_position)(objects.pointer.as_ptr(), 0) },
            0
        );
        assert_eq!(
            read_envelope(api, objects.pointer),
            Err(Error::UnsupportedNodeLayout("binder_or_fd_objects"))
        );
    }
    #[test]
    fn real_queries_correlate_null_rejection_recovery_and_release_resources() {
        let environment = crate::diagnostics::Diagnostics::collect().unwrap();
        assert!(contract::supported(
            environment.api34_shell(),
            std::env::consts::ARCH,
            crate::accessibility::fingerprint().unwrap().as_deref()
        ));
        let _signals = Signals::install().unwrap();
        let mut session = Session::open().unwrap();
        session.register().unwrap();
        session.shared.wait_init().unwrap();
        let timeout = Duration::from_secs(5);
        // Enabling accessibility is asynchronous in the app. Only test setup
        // retries the ambiguous rejection while waiting for its initial window;
        // production query reports QueryRejected without guessing its cause.
        let ready_deadline = Instant::now() + timeout;
        let mut setup_rejections = 0;
        let first = loop {
            let left = ready_deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "active window did not become queryable");
            match session.query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, left) {
                Ok(value) => break value,
                Err(Error::QueryRejected) => {
                    setup_rejections += 1;
                    std::thread::sleep(Duration::from_millis(20).min(left));
                }
                Err(error) => panic!("initial query failed: {error:?}"),
            }
        };
        println!("CARLO_QUERY_SETUP_REJECTIONS {setup_rejections}");
        assert!(first.node_present && first.fields_decoded);
        let null = session
            .query_node(contract::ACTIVE_WINDOW, 0x7fffffff7fffffff, timeout)
            .unwrap();
        assert!(!null.node_present);
        assert_eq!(null.interaction_id, first.interaction_id + 1);
        assert_eq!(
            session.query_node(-12345, contract::ROOT_NODE, timeout),
            Err(Error::QueryRejected)
        );
        assert_eq!(
            session.query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, Duration::ZERO),
            Err(Error::QueryTimeout)
        );
        STOP.store(true, Ordering::Relaxed);
        assert_eq!(
            session.query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, timeout),
            Err(Error::Cancelled)
        );
        STOP.store(false, Ordering::Relaxed);
        fn resources() -> (usize, usize) {
            (
                std::fs::read_dir("/proc/self/fd").unwrap().count(),
                std::fs::read_dir("/proc/self/task").unwrap().count(),
            )
        }
        let warm = session
            .query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, timeout)
            .unwrap();
        let baseline = resources();
        let mut samples = Vec::new();
        let mut previous = warm.interaction_id;
        for _ in 0..20 {
            let envelope = session
                .query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, timeout)
                .unwrap();
            assert!(envelope.node_present && envelope.fields_decoded);
            assert_eq!(envelope.interaction_id, previous + 1);
            previous = envelope.interaction_id;
            samples.push(resources());
        }
        assert!(
            samples
                .iter()
                .all(|value| value.0 == baseline.0 && value.1 <= baseline.1 + 1)
        );
        println!(
            "CARLO_QUERY_CYCLES {}",
            serde_json::to_string(&samples).unwrap()
        );
        session.close().unwrap();
        assert_eq!(
            session.query_node(contract::ACTIVE_WINDOW, contract::ROOT_NODE, timeout),
            Err(Error::Cancelled)
        );
    }
}
