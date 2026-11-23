//! `hierarchy.dump`: one complete snapshot per request, answered as raw UTF-8 XML
//! on a hierarchy frame or as a stable error code on a control frame.
//!
//! The document is real screen contents. It is handed to the connection owner
//! once and never logged, formatted or copied into an error; failures carry only
//! a fixed code. The gate is the exact hierarchy profile of
//! [`crate::accessibility::hierarchy`]: off it nothing is loaded and no Binder
//! call is made.

use std::sync::atomic::Ordering;

use super::socket::STOP;
use crate::accessibility::{self, Error, hierarchy::Snapshots};
use crate::diagnostics::Diagnostics;
use crate::error::CarloError;
use crate::protocol::Kind;

/// Where `hierarchy.dump` gets its document. Every call is bounded by the
/// implementation; the control loop waits for it.
pub(super) trait HierarchySource {
    /// One complete document, or a typed failure with no document at all.
    fn dump(&mut self) -> Result<Vec<u8>, Error>;
    /// Release whatever the source holds. Called once on every exit of the
    /// session; any later `dump` answers `unavailable`.
    fn teardown(&mut self);
}

/// This process's source. `None` off the pinned hierarchy profile.
///
/// Each snapshot registers its own automation session and unregisters it
/// before returning (on every exit path, by the session's own guard), so no
/// registration is held between requests; snapshot ids and the generation
/// continue across the whole serve session.
pub(super) struct Device {
    snapshots: Option<Snapshots>,
}

impl Device {
    /// Decide the gate from the same environment `doctor` reports and the build
    /// fingerprint (a property read). Never touches Binder or loads a library.
    ///
    /// # Errors
    /// Returns [`CarloError::InvalidEnvironment`] if the fingerprint property is
    /// invalid.
    pub(super) fn detect(diagnostics: &Diagnostics) -> Result<Self, CarloError> {
        let fingerprint = accessibility::fingerprint()?;
        Ok(Self {
            snapshots: Snapshots::detect(diagnostics, fingerprint.as_deref()),
        })
    }
}

impl HierarchySource for Device {
    fn dump(&mut self) -> Result<Vec<u8>, Error> {
        let Some(snapshots) = self.snapshots.as_mut() else {
            return Err(Error::UnsupportedProfile);
        };
        let (document, interrupted) = snapshots.take();
        // While the session ran, SIGINT/SIGTERM went to its own handlers; hand
        // the signal on so the serve session still ends as interrupted.
        if interrupted {
            STOP.store(true, Ordering::Relaxed);
        }
        document
    }

    fn teardown(&mut self) {
        self.snapshots = None;
    }
}

/// What the control channel sends for one `hierarchy.dump`.
pub(super) enum Answer {
    /// A complete document for a hierarchy frame.
    Document(Vec<u8>),
    /// A stable error code for a control frame.
    Failure(&'static str),
}

/// Validate a source result for the wire: UTF-8 and within the hierarchy frame
/// limit, else a stable code. The document is dropped, not reported, on refusal.
pub(super) fn answer(result: Result<Vec<u8>, Error>) -> Answer {
    match result {
        Ok(document) if document.len() > Kind::Hierarchy.max_payload() as usize => {
            Answer::Failure("truncated")
        }
        Ok(document) if std::str::from_utf8(&document).is_err() => Answer::Failure("internal"),
        Ok(document) => Answer::Document(document),
        Err(error) => Answer::Failure(code(&error)),
    }
}

/// Stable wire code for a snapshot failure. Only the variant is mapped; no
/// field (operation, status, reason) reaches the client.
pub(super) fn code(error: &Error) -> &'static str {
    match error {
        Error::UnsupportedProfile => "unavailable",
        Error::Conflict => "conflict",
        Error::WindowsUnavailable => "windows_unavailable",
        Error::InconsistentSnapshot { .. } => "inconsistent_snapshot",
        Error::Truncated { .. } => "truncated",
        Error::UnsupportedNode { .. } => "unsupported_node",
        Error::InitializationTimeout | Error::QueryTimeout => "timeout",
        _ => "internal",
    }
}

#[cfg(test)]
pub(super) mod fake {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::{Error, HierarchySource};

    /// Answers queued results in order, then `unsupported_profile`; counts calls.
    #[derive(Default)]
    pub(in crate::serve) struct Fake {
        pub results: VecDeque<Result<Vec<u8>, Error>>,
        pub dumps: Rc<Cell<u32>>,
        pub teardowns: Rc<Cell<u32>>,
    }

    impl HierarchySource for Fake {
        fn dump(&mut self) -> Result<Vec<u8>, Error> {
            self.dumps.set(self.dumps.get() + 1);
            self.results
                .pop_front()
                .unwrap_or(Err(Error::UnsupportedProfile))
        }

        fn teardown(&mut self) {
            self.teardowns.set(self.teardowns.get() + 1);
            self.results.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_variant_maps_to_a_stable_code() {
        let cases = [
            (Error::UnsupportedProfile, "unavailable"),
            (Error::Conflict, "conflict"),
            (Error::WindowsUnavailable, "windows_unavailable"),
            (
                Error::InconsistentSnapshot {
                    reason: "window_changed",
                    attempts: 3,
                },
                "inconsistent_snapshot",
            ),
            (Error::Truncated { limit: "nodes" }, "truncated"),
            (
                Error::UnsupportedNode {
                    class: "text",
                    count: 2,
                },
                "unsupported_node",
            ),
            (Error::InitializationTimeout, "timeout"),
            (Error::QueryTimeout, "timeout"),
            (Error::LibraryUnavailable, "internal"),
            (Error::MissingSymbol("AIBinder_new"), "internal"),
            (
                Error::Binder {
                    operation: "register",
                    status: -32,
                },
                "internal",
            ),
            (
                Error::RemoteException {
                    operation: "get_windows",
                    code: -1,
                },
                "internal",
            ),
            (Error::Cancelled, "internal"),
            (Error::Runtime("session_not_clean"), "internal"),
            (Error::WindowGone, "internal"),
            (
                Error::InvalidWindowList {
                    field: "reply",
                    reason: "short",
                },
                "internal",
            ),
            (Error::InvalidTree("cycle"), "internal"),
        ];
        for (error, expected) in cases {
            assert_eq!(code(&error), expected);
            match answer(Err(error)) {
                Answer::Failure(code) => assert_eq!(code, expected),
                Answer::Document(_) => panic!("an error produced a document"),
            }
        }
    }

    #[test]
    fn documents_are_passed_through_only_when_utf8_and_within_the_frame_limit() {
        let xml = "<hierarchy rotation=\"0\"><node text=\"café\"/></hierarchy>";
        match answer(Ok(xml.as_bytes().to_vec())) {
            Answer::Document(document) => assert_eq!(document, xml.as_bytes()),
            Answer::Failure(code) => panic!("refused a valid document: {code}"),
        }
        let limit = Kind::Hierarchy.max_payload() as usize;
        assert!(matches!(
            answer(Ok(vec![b'a'; limit])),
            Answer::Document(document) if document.len() == limit
        ));
        assert!(matches!(
            answer(Ok(vec![b'a'; limit + 1])),
            Answer::Failure("truncated")
        ));
        assert!(matches!(
            answer(Ok(b"<a>\xff</a>".to_vec())),
            Answer::Failure("internal")
        ));
    }

    #[test]
    fn off_the_pinned_profile_the_device_source_is_unavailable_without_binder() {
        // The host and the API 34 reference: the gate fails before anything opens.
        for diagnostics in [
            Diagnostics::from_environment("linux", "x86_64", None, None, Some(1000), None),
            Diagnostics::from_environment(
                "android",
                "x86_64",
                Some(34),
                Some("x86_64".into()),
                Some(2000),
                None,
            ),
        ] {
            let mut device = Device::detect(&diagnostics).unwrap();
            assert!(device.snapshots.is_none());
            assert_eq!(device.dump(), Err(Error::UnsupportedProfile));
            device.teardown();
            device.teardown();
            assert_eq!(device.dump(), Err(Error::UnsupportedProfile));
        }
    }
}
