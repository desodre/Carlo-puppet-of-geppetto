//! The Android side of a hierarchy snapshot: one registered session of the
//! pinned API 36 profile exposed as a [`Transport`]. Registration, the callback
//! state, the exclusive guard, signal handling and the unregister on every exit
//! path are the same objects the session proofs use.
use super::*;
use crate::accessibility::{
    hierarchy::{Run, Sequence, SessionOutcome, Transport, snapshot},
    node::Node,
};

struct Remote<'a> {
    session: &'a mut Session,
}

impl Transport for Remote<'_> {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while !STOP.load(Ordering::Relaxed) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(20)));
        }
    }

    /// `getWindows`: no arguments; the system may hold the call for up to five
    /// seconds while it starts tracking windows. A synchronous transaction
    /// cannot be cancelled: the snapshot checks its deadline when this returns.
    fn windows(&mut self) -> Result<Vec<u8>, Error> {
        if STOP.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let connection = self.session.connection()?;
        let request = OwnedParcel::new(&connection)?;
        let reply = request.reply(&connection, contract::GET_WINDOWS, "get_windows")?;
        if STOP.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        // The reply was bounded to 64 KiB by `reply`; a window list carries no
        // Binder or FD objects, so one that does is not the pinned layout.
        query::copy_rest(connection.api, reply.pointer, contract::MAX_CALLBACK_BYTES).map_err(
            |error| {
                error.unwrap_or(Error::InvalidWindowList {
                    field: "reply",
                    reason: "binder_or_fd_objects",
                })
            },
        )
    }

    fn find(
        &mut self,
        window: i32,
        node: i64,
        prefetch: i32,
        deadline: Instant,
    ) -> Result<Vec<Node>, Error> {
        self.session.query_batch(window, node, prefetch, deadline)
    }

    fn invalidations(&mut self) -> u32 {
        self.session
            .shared
            .slot
            .lock()
            .map_or(0, |slot| slot.invalidations)
    }
}

/// Register, take one snapshot, unregister. The session is closed explicitly
/// and again by `Drop`, so an error or a panic in the walk still unregisters.
pub(in crate::accessibility) fn run(profile: Profile, sequence: &mut Sequence) -> Run {
    let mut outcome = SessionOutcome::default();
    let Ok(_exclusive) = EXCLUSIVE.try_lock() else {
        outcome.error = Some(Error::Conflict);
        return (outcome, None);
    };
    let signals = match Signals::install() {
        Ok(signals) => signals,
        Err(error) => {
            outcome.error = Some(error);
            return (outcome, None);
        }
    };
    let mut session = match Session::open_profile(profile) {
        Ok(session) => session,
        Err(error) => {
            outcome.error = Some(error);
            return (outcome, None);
        }
    };
    outcome.registration_flags = Some(contract::registration_flags());
    let mut result = None;
    match session.register().and_then(|()| session.shared.wait_init()) {
        Ok(id) => {
            outcome.initialized = true;
            outcome.connection_id = Some(id);
            let mut remote = Remote {
                session: &mut session,
            };
            result = Some(snapshot(&mut remote, sequence));
        }
        Err(error) => outcome.error = Some(error),
    }
    let had_registration = session.cleanup_due;
    match session.close() {
        Ok(()) => outcome.unregistered = had_registration,
        Err(error) => outcome.cleanup_error = Some(error),
    }
    outcome.interrupted = STOP.load(Ordering::Relaxed);
    if let Ok(slot) = session.shared.slot.lock() {
        outcome.callbacks = slot.counters;
        outcome.revocations = slot.revocations;
        if outcome.error.is_none() {
            outcome.error = slot.error.clone();
        }
    } else if outcome.error.is_none() {
        outcome.error = Some(Error::Runtime("callback_state_poisoned"));
    }
    drop(session);
    drop(signals);
    (outcome, result)
}
