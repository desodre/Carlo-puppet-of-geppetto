//! The Android side of a hierarchy snapshot: one registered session of the
//! pinned API 36 profile exposed as a [`Transport`]. Registration, the callback
//! state, the exclusive guard and the unregister on every exit path are the same
//! objects the session proofs use.
//!
//! [`run`] is the one-shot command (it owns the process signals for its session).
//! [`PersistentSession`] is `carlo serve`'s: registered lazily on the first dump,
//! reused by later dumps, cancelled by the caller's own flag and unregistered on
//! close or drop.
use super::*;
use crate::accessibility::{
    hierarchy::{Run, Sequence, SessionOutcome, Transport, snapshot, stale},
    node::Node,
};
use std::sync::MutexGuard;

struct Remote<'a> {
    session: &'a mut Session,
    /// The owner's cancellation flag: [`STOP`] for one-shot commands.
    stop: &'a AtomicBool,
}

impl Transport for Remote<'_> {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn pause(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while !self.stop.load(Ordering::Relaxed) {
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
        if self.stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let connection = self.session.connection()?;
        let request = OwnedParcel::new(&connection)?;
        let reply = request.reply(&connection, contract::GET_WINDOWS, "get_windows")?;
        if self.stop.load(Ordering::Relaxed) {
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
        self.session
            .query_batch(window, node, prefetch, deadline, self.stop)
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
                stop: &STOP,
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

/// A registration kept across snapshots of one long-lived owner (`carlo serve`).
///
/// Nothing is opened until the first [`take`](Self::take). While open it holds
/// the process [`EXCLUSIVE`] guard and one registered session (flags exactly 1).
/// A session the system revoked, or whose Binder peer died, is closed and
/// re-opened at most once per snapshot. [`close`](Self::close) unregisters and
/// then releases the guard; `Drop` closes on every other exit path. It never
/// installs signal handlers: the owner's flag cancels every wait.
pub(in crate::accessibility) struct PersistentSession {
    profile: Profile,
    /// The guard is released only after the session is closed, by `discard`.
    open: Option<(MutexGuard<'static, ()>, Session)>,
}

impl PersistentSession {
    pub(in crate::accessibility) fn new(profile: Profile) -> Self {
        Self {
            profile,
            open: None,
        }
    }

    /// One complete document, or a typed failure with no document.
    pub(in crate::accessibility) fn take(
        &mut self,
        stop: &AtomicBool,
        sequence: &mut Sequence,
    ) -> Result<Vec<u8>, Error> {
        let mut reopened = false;
        loop {
            // Revoked between snapshots: never reuse the dead connection. This
            // is not a failed call, so it does not consume the one retry.
            if self
                .open
                .as_ref()
                .is_some_and(|(_, session)| revoked(session))
            {
                self.discard()?;
            }
            let session = self.ensure(stop)?;
            let mut remote = Remote {
                session: &mut *session,
                stop,
            };
            let (_, result) = snapshot(&mut remote, sequence);
            let callback_error = slot_error(session);
            let dead = revoked(session) || result.as_ref().err().is_some_and(stale);
            match (result, callback_error) {
                // A complete document stands even if the session was revoked
                // afterwards; the next take re-opens.
                (Ok(document), None) => {
                    if dead {
                        self.discard()?;
                    }
                    return Ok(document);
                }
                // A malformed callback leaves the session unusable.
                (_, Some(error)) => {
                    self.discard()?;
                    return Err(error);
                }
                (Err(error), None) => {
                    if !dead {
                        return Err(error);
                    }
                    self.discard()?;
                    if reopened || stop.load(Ordering::Relaxed) {
                        return Err(error);
                    }
                    reopened = true;
                }
            }
        }
    }

    /// Unregister and release the exclusive guard. Idempotent; a later
    /// [`take`](Self::take) registers again.
    pub(in crate::accessibility) fn close(&mut self) -> Result<(), Error> {
        self.discard()
    }

    /// The registered session, opening it (exclusive guard, register with
    /// flags 1, bounded `init` wait) when there is none. On any failure the
    /// partly opened session unregisters by its own guard before `EXCLUSIVE`
    /// is released, and nothing is kept.
    fn ensure(&mut self, stop: &AtomicBool) -> Result<&mut Session, Error> {
        if self.open.is_none() {
            let exclusive = EXCLUSIVE.try_lock().map_err(|_| Error::Conflict)?;
            // Declared after the guard, so dropped (closed) before it.
            let mut session = Session::open_profile(self.profile)?;
            session.register()?;
            session.shared.wait_init_with_stop(stop)?;
            self.open = Some((exclusive, session));
        }
        match self.open.as_mut() {
            Some((_, session)) => Ok(session),
            None => Err(Error::Runtime("session_not_open")),
        }
    }

    /// Close and drop the current session, then release `EXCLUSIVE`.
    fn discard(&mut self) -> Result<(), Error> {
        let Some((exclusive, mut session)) = self.open.take() else {
            return Ok(());
        };
        let result = session.close();
        drop(session);
        drop(exclusive);
        result
    }
}

impl Drop for PersistentSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// The system revoked this session (or its callback state is unusable).
fn revoked(session: &Session) -> bool {
    match session.shared.slot.lock() {
        Ok(slot) => slot.revoked,
        Err(_) => true,
    }
}

/// A malformed callback recorded by the session, which makes it unusable.
fn slot_error(session: &Session) -> Option<Error> {
    session.shared.slot.lock().map_or_else(
        |_| Some(Error::Runtime("callback_state_poisoned")),
        |slot| slot.error.clone(),
    )
}
