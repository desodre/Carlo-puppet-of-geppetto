//! API 34 ServiceManager proof; this does not register automation or read hierarchy.
//!
//! Only the Android 14 shell profile is probed. Private service-manager exports are
//! resolved at runtime; their presence is not a compatibility promise for other builds.

use serde::Serialize;

use crate::{diagnostics::Diagnostics, error::CarloError};

#[cfg(any(target_os = "android", all(test, unix)))]
pub(crate) mod native;

/// Bounded JSON result of the read-only Binder proof.
#[derive(Debug, Serialize)]
pub struct BinderProbe {
    schema_version: u32,
    environment: Diagnostics,
    outcome: Outcome,
    hierarchy_available: bool,
}

/// Result category for the limited ServiceManager proof.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    /// Runtime is outside the API 34 shell reference profile.
    UnsupportedTarget,
    /// The system Binder NDK library could not be opened.
    LibraryUnavailable,
    /// At least one required function export could not be resolved.
    MissingSymbols,
    /// Accessibility lookup returned null (absence, denial or transaction failure).
    ServiceUnavailable,
    /// Accessibility Binder ping returned a nonzero status.
    PingFailed,
    /// The deliberately absent control service unexpectedly resolved.
    UnexpectedControlService,
    /// Accessibility lookup/ping succeeded and control lookup returned null.
    Passed,
}

#[derive(Debug, Serialize)]
struct Symbol {
    name: &'static str,
    available: bool,
}

#[derive(Debug, Serialize)]
struct Lookup {
    name: &'static str,
    found: bool,
    remote: Option<bool>,
    ping_status: Option<i32>,
}

#[derive(Debug, Serialize)]
struct Outcome {
    status: ProbeStatus,
    symbols: Vec<Symbol>,
    lookups: Vec<Lookup>,
}

impl BinderProbe {
    /// Probe library loading, required exports, accessibility lookup and Binder ping.
    ///
    /// Always produces an unavailable outcome outside Android API 34 under UID 2000.
    /// Does not start a Binder pool, register automation or change accessibility settings.
    /// Synchronous Binder calls cannot be cancelled here: execute with the bounded
    /// host session helper, which terminates the process on timeout.
    ///
    /// # Errors
    /// Returns [`CarloError::InvalidEnvironment`] for malformed Android property data.
    /// Library, symbol and service failures are represented in the JSON outcome.
    pub fn collect() -> Result<Self, CarloError> {
        let environment = Diagnostics::collect()?;
        let supported = environment.api34_shell();
        #[cfg(target_os = "android")]
        let outcome = if supported {
            native::probe()
        } else {
            Outcome::unsupported()
        };
        #[cfg(not(target_os = "android"))]
        let outcome = {
            let _ = supported;
            Outcome::unsupported()
        };
        Ok(Self {
            schema_version: 1,
            environment,
            outcome,
            hierarchy_available: false,
        })
    }

    /// Whether required exports resolved, accessibility ping succeeded and control was null.
    ///
    /// Success proves lookup/ping only, never hierarchy or callback compatibility.
    pub fn passed(&self) -> bool {
        self.outcome.status == ProbeStatus::Passed
    }
}

#[cfg(any(target_os = "android", test))]
fn classify(accessibility: &Lookup, control: &Lookup) -> ProbeStatus {
    if !accessibility.found {
        ProbeStatus::ServiceUnavailable
    } else if accessibility.ping_status != Some(0) {
        ProbeStatus::PingFailed
    } else if control.found {
        ProbeStatus::UnexpectedControlService
    } else {
        ProbeStatus::Passed
    }
}

impl Outcome {
    fn unsupported() -> Self {
        Self {
            status: ProbeStatus::UnsupportedTarget,
            symbols: Vec::new(),
            lookups: Vec::new(),
        }
    }
}
