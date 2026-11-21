//! Correlated query state. Node data is omitted from metadata serialization.
use super::Error;
use serde::Serialize;
use std::time::Instant;

#[derive(Default)]
pub(super) struct Ids(i32);
impl Ids {
    pub(super) fn next(&mut self) -> Result<i32, Error> {
        self.0 = self.0.checked_add(1).ok_or(Error::QueryIdExhausted)?;
        Ok(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Envelope {
    pub(super) interaction_id: i32,
    pub(super) node_present: bool,
    pub(super) opaque_node_bytes: u32,
    pub(super) fields_decoded: bool,
    #[serde(skip)]
    pub(super) node: Option<super::node::Node>,
}
impl Envelope {
    /// The pinned AIDL writes presence, opaque node, then interactionId. This only
    /// validates that outer envelope. Full parcelable validation is issue #23.
    #[cfg(test)]
    pub(super) fn checked(presence: i32, bytes: i32, id: i32) -> Result<Self, Error> {
        if id <= 0 || bytes < 0 || bytes % 4 != 0 || !matches!((presence, bytes), (0, 0) | (1, 8..))
        {
            return Err(Error::InvalidCallback);
        }
        Ok(Self {
            interaction_id: id,
            node_present: presence == 1,
            opaque_node_bytes: bytes as u32,
            fields_decoded: false,
            node: None,
        })
    }
}

/// A callback result that names the request it answers.
pub(super) trait Correlated {
    fn interaction_id(&self) -> i32;
}
impl Correlated for Envelope {
    fn interaction_id(&self) -> i32 {
        self.interaction_id
    }
}

/// What a per-request callback object can deliver: the API 34 single-node
/// envelope or an API 36 node list.
pub(super) enum Reply {
    Single(Box<Envelope>),
    Batch(super::node::Batch),
}
impl Correlated for Reply {
    fn interaction_id(&self) -> i32 {
        match self {
            Self::Single(envelope) => envelope.interaction_id,
            Self::Batch(batch) => batch.interaction_id,
        }
    }
}

/// The API 34 single-envelope state; `Awaiting` is the same machine for any reply.
#[cfg(test)]
pub(super) type Pending = Awaiting<Envelope>;

pub(super) struct Awaiting<T> {
    id: i32,
    deadline: Instant,
    closed: bool,
    result: Option<Result<T, Error>>,
}
impl<T: Correlated> Awaiting<T> {
    pub(super) fn new(id: i32, deadline: Instant) -> Self {
        Self {
            id,
            deadline,
            closed: false,
            result: None,
        }
    }
    pub(super) fn receive(&mut self, result: Result<T, Error>, now: Instant) {
        if self.closed || self.result.is_some() || now >= self.deadline {
            return;
        }
        if result
            .as_ref()
            .is_ok_and(|value| value.interaction_id() != self.id)
        {
            return;
        }
        self.result = Some(result);
    }
    pub(super) fn poll(&mut self, now: Instant, cancelled: bool) -> Option<Result<T, Error>> {
        if cancelled || self.closed {
            self.close();
            return Some(Err(Error::Cancelled));
        }
        // A synchronous transaction that exceeded its deadline cannot be saved by an
        // earlier callback. Host kill is still needed when dispatch never returns.
        if now >= self.deadline {
            self.close();
            return Some(Err(Error::QueryTimeout));
        }
        if let Some(result) = self.result.take() {
            self.close();
            return Some(result);
        }
        None
    }
    pub(super) fn close(&mut self) {
        self.closed = true;
        self.result = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn envelope(id: i32) -> Envelope {
        Envelope::checked(1, 16, id).unwrap()
    }
    #[test]
    fn correlation_rejects_stale_ids_and_duplicates() {
        let now = Instant::now();
        let mut pending = Pending::new(2, now + Duration::from_secs(1));
        pending.receive(Ok(envelope(1)), now);
        assert!(pending.poll(now, false).is_none());
        pending.receive(Ok(envelope(2)), now);
        pending.receive(Err(Error::InvalidCallback), now);
        assert_eq!(pending.poll(now, false), Some(Ok(envelope(2))));
        pending.receive(Ok(envelope(2)), now);
        assert_eq!(pending.poll(now, false), Some(Err(Error::Cancelled)));
    }
    #[test]
    fn deadline_cancel_and_late_callback_do_not_leak_to_new_request() {
        let now = Instant::now();
        let mut expired = Pending::new(1, now);
        expired.receive(Ok(envelope(1)), now);
        assert_eq!(expired.poll(now, false), Some(Err(Error::QueryTimeout)));
        let mut next = Pending::new(2, now + Duration::from_secs(1));
        expired.receive(Err(Error::InvalidCallback), now);
        assert!(next.poll(now, false).is_none());
        assert_eq!(next.poll(now, true), Some(Err(Error::Cancelled)));
        let mut blocked_dispatch = Pending::new(3, now + Duration::from_secs(1));
        blocked_dispatch.receive(Ok(envelope(3)), now);
        assert_eq!(
            blocked_dispatch.poll(now + Duration::from_secs(2), false),
            Some(Err(Error::QueryTimeout))
        );
    }
    #[test]
    fn null_result_is_not_timeout_and_invalid_envelopes_fail() {
        let now = Instant::now();
        let mut pending = Pending::new(1, now + Duration::from_secs(1));
        let null = Envelope::checked(0, 0, 1).unwrap();
        pending.receive(Ok(null.clone()), now);
        assert_eq!(pending.poll(now, false), Some(Ok(null)));
        for (presence, bytes, id) in [
            (0, 4, 1),
            (1, 0, 1),
            (1, 7, 1),
            (2, 8, 1),
            (1, -1, 1),
            (1, 8, 0),
        ] {
            assert_eq!(
                Envelope::checked(presence, bytes, id),
                Err(Error::InvalidCallback)
            );
        }
    }
    #[test]
    fn ids_never_wrap_or_reuse() {
        let mut ids = Ids::default();
        assert_eq!(ids.next(), Ok(1));
        assert_eq!(ids.next(), Ok(2));
        ids.0 = i32::MAX;
        assert_eq!(ids.next(), Err(Error::QueryIdExhausted));
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    use crate::accessibility::node::Batch;
    use std::time::Duration;

    fn batch(id: i32) -> Reply {
        Reply::Batch(Batch {
            interaction_id: id,
            nodes: Vec::new(),
        })
    }

    #[test]
    fn a_batch_for_another_request_never_satisfies_this_one() {
        let now = Instant::now();
        let mut pending = Awaiting::<Reply>::new(5, now + Duration::from_secs(1));
        pending.receive(Ok(batch(4)), now);
        assert!(pending.poll(now, false).is_none());
        pending.receive(Ok(batch(5)), now);
        pending.receive(Ok(batch(5)), now);
        assert!(matches!(
            pending.poll(now, false),
            Some(Ok(Reply::Batch(Batch {
                interaction_id: 5,
                ..
            })))
        ));
        assert!(matches!(
            pending.poll(now, false),
            Some(Err(Error::Cancelled))
        ));
    }

    #[test]
    fn the_window_gone_sentinel_ends_the_pending_request_and_late_answers_are_dropped() {
        let now = Instant::now();
        // The decoder turns (null, 0x7fffffff) into this error: it carries no
        // request id, and it must end the request instead of looking stale.
        let mut pending = Awaiting::<Reply>::new(9, now + Duration::from_secs(1));
        pending.receive(Err(Error::WindowGone), now);
        assert!(matches!(
            pending.poll(now, false),
            Some(Err(Error::WindowGone))
        ));
        let mut late = Awaiting::<Reply>::new(10, now + Duration::from_secs(1));
        late.receive(Ok(batch(10)), now + Duration::from_secs(1));
        assert!(late.poll(now, false).is_none());
        assert!(matches!(
            late.poll(now + Duration::from_secs(1), false),
            Some(Err(Error::QueryTimeout))
        ));
    }
}
