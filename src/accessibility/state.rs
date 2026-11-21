//! One connection per session, no unbounded callback queues.

use super::Error;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(super) struct Counters {
    pub(super) init: u32,
    pub(super) late: u32,
    pub(super) ignored: u32,
    pub(super) invalid: u32,
}

pub(super) struct Slot<T> {
    pub(super) closing: bool,
    pub(super) connection: Option<(i32, T)>,
    pub(super) error: Option<Error>,
    pub(super) counters: Counters,
    /// `onInterrupt`/`clearAccessibilityCache` deliveries; never serialized by the
    /// session reports, read by a hierarchy snapshot.
    pub(super) invalidations: u32,
    /// `init` deliveries with a null connection (the system revoking the session).
    pub(super) revocations: u32,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            closing: false,
            connection: None,
            error: None,
            counters: Counters::default(),
            invalidations: 0,
            revocations: 0,
        }
    }
}

impl<T> Slot<T> {
    pub(super) fn accept(&mut self, id: i32, value: T) -> Result<(), Error> {
        if self.closing {
            self.counters.late = self.counters.late.saturating_add(1);
            return Ok(());
        }
        if id < 0 || self.connection.is_some() {
            self.error = Some(Error::InvalidCallback);
            self.counters.invalid = self.counters.invalid.saturating_add(1);
            return Err(Error::InvalidCallback);
        }
        self.connection = Some((id, value));
        self.counters.init = self.counters.init.saturating_add(1);
        Ok(())
    }

    pub(super) fn close(&mut self) -> Option<(i32, T)> {
        self.closing = true;
        self.connection.take()
    }
}

pub(super) trait InitReader {
    type Binder;
    fn binder(&mut self) -> Result<Option<Self::Binder>, Error>;
    fn int(&mut self) -> Result<i32, Error>;
    fn finish(&self) -> Result<(), Error>;
}

pub(super) fn decode_init<R: InitReader>(reader: &mut R) -> Result<(i32, R::Binder), Error> {
    let connection = reader.binder()?.ok_or(Error::InvalidCallback)?;
    let id = reader.int()?;
    let _window_token = reader.binder()?;
    reader.finish()?;
    if id < 0 {
        return Err(Error::InvalidCallback);
    }
    Ok((id, connection))
}

/// API 36 `init`: the system calls it again with a null connection while it
/// resets the session. `None` is that revocation, accepted without an error.
pub(super) fn decode_init_nullable<R: InitReader>(
    reader: &mut R,
) -> Result<Option<(i32, R::Binder)>, Error> {
    let connection = reader.binder()?;
    let id = reader.int()?;
    let _window_token = reader.binder()?;
    reader.finish()?;
    match connection {
        None => Ok(None),
        Some(_) if id < 0 => Err(Error::InvalidCallback),
        Some(connection) => Ok(Some((id, connection))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Ref(Rc<Cell<u32>>);
    impl Drop for Ref {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    #[test]
    fn close_is_idempotent_and_late_duplicate_connections_are_released() {
        let drops = Rc::new(Cell::new(0));
        let mut slot = Slot::default();
        slot.accept(7, Ref(drops.clone())).unwrap();
        assert_eq!(
            slot.accept(8, Ref(drops.clone())),
            Err(Error::InvalidCallback)
        );
        drop(slot.close());
        assert!(slot.close().is_none());
        slot.accept(9, Ref(drops.clone())).unwrap();
        assert!(slot.connection.is_none());
        assert_eq!(drops.get(), 3);
        assert_eq!(slot.counters.init, 1);
        assert_eq!(slot.counters.late, 1);
    }
    struct Reader {
        binders: Vec<Option<Ref>>,
        id: Result<i32, Error>,
        extra: bool,
    }
    impl InitReader for Reader {
        type Binder = Ref;
        fn binder(&mut self) -> Result<Option<Ref>, Error> {
            if self.binders.is_empty() {
                Err(Error::InvalidCallback)
            } else {
                Ok(self.binders.remove(0))
            }
        }
        fn int(&mut self) -> Result<i32, Error> {
            self.id.clone()
        }
        fn finish(&self) -> Result<(), Error> {
            if self.extra {
                Err(Error::InvalidCallback)
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn nullable_init_accepts_a_revocation_and_still_rejects_malformed_parcels() {
        let drops = Rc::new(Cell::new(0));
        let reader = |connection: bool, id: i32, extra: bool| Reader {
            binders: vec![connection.then(|| Ref(drops.clone())), None],
            id: Ok(id),
            extra,
        };
        assert!(matches!(
            decode_init_nullable(&mut reader(true, 4, false)),
            Ok(Some((4, _)))
        ));
        for id in [-1, 0, 9] {
            assert!(matches!(
                decode_init_nullable(&mut reader(false, id, false)),
                Ok(None)
            ));
        }
        assert!(decode_init_nullable(&mut reader(true, -1, false)).is_err());
        assert!(decode_init_nullable(&mut reader(false, 0, true)).is_err());
        assert!(decode_init_nullable(&mut reader(true, 1, true)).is_err());
        let mut short = reader(true, 1, false);
        short.binders.pop();
        assert!(decode_init_nullable(&mut short).is_err());
        drop(short);
        assert_eq!(drops.get(), 4);
    }
    #[test]
    fn malformed_init_releases_all_read_references_and_never_becomes_ready() {
        for case in 0..5 {
            let drops = Rc::new(Cell::new(0));
            let mut reader = Reader {
                binders: vec![Some(Ref(drops.clone())), Some(Ref(drops.clone()))],
                id: Ok(8),
                extra: false,
            };
            match case {
                0 => reader.id = Err(Error::InvalidCallback),
                1 => reader.id = Ok(-1),
                2 => reader.extra = true,
                3 => {
                    reader.binders.pop();
                }
                _ => {
                    reader.binders[0] = None;
                }
            }
            assert!(decode_init(&mut reader).is_err());
            drop(reader);
            assert_eq!(drops.get(), 2);
        }
    }
}
