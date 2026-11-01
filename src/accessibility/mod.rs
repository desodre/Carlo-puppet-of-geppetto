//! Reference-profile session and query-envelope proofs. No decoded UI payload logging.

#![cfg_attr(
    not(target_os = "android"),
    allow(
        dead_code,
        reason = "Android FFI constructors; host exercises pure model/codecs"
    )
)]

use crate::{diagnostics::Diagnostics, error::CarloError};
use serde::Serialize;

mod contract;
#[cfg(target_os = "android")]
mod native;
mod query;
mod state;

/// Failures retain status codes and fixed operation names, never remote message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub(crate) enum Error {
    UnsupportedProfile,
    LibraryUnavailable,
    MissingSymbol(&'static str),
    Binder {
        operation: &'static str,
        status: i32,
    },
    RemoteException {
        operation: &'static str,
        code: i32,
    },
    Conflict,
    InitializationTimeout,
    QueryTimeout,
    QueryRejected,
    QueryIdExhausted,
    InvalidCallback,
    Cancelled,
    Runtime(&'static str),
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Outcome {
    initialized: bool,
    connection_id: Option<i32>,
    unregistered: bool,
    interrupted: bool,
    callbacks: state::Counters,
    error: Option<Error>,
    cleanup_error: Option<Error>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<query::Envelope>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    schema_version: u32,
    environment: Diagnostics,
    runtime_fingerprint: Option<String>,
    outcome: Outcome,
    hierarchy_available: bool,
}

impl Report {
    pub(crate) fn passed(&self) -> bool {
        self.outcome.initialized
            && self.outcome.unregistered
            && self.outcome.error.is_none()
            && self.outcome.cleanup_error.is_none()
    }
}

pub(crate) fn collect(hold_ms: u32) -> Result<Report, CarloError> {
    if hold_ms > 10_000 {
        return Err(CarloError::InvalidArguments(
            "hold must be 0..10000 milliseconds",
        ));
    }
    collect_mode(hold_ms, false)
}

pub(crate) fn collect_node() -> Result<Report, CarloError> {
    collect_mode(0, true)
}

#[cfg_attr(not(target_os = "android"), allow(unused_variables))]
fn collect_mode(hold_ms: u32, query: bool) -> Result<Report, CarloError> {
    let environment = Diagnostics::collect()?;
    let fingerprint = fingerprint()?;
    let supported = contract::supported(
        environment.api34_shell(),
        std::env::consts::ARCH,
        fingerprint.as_deref(),
    );
    let outcome = if supported {
        #[cfg(target_os = "android")]
        {
            native::probe_mode(hold_ms, query)
        }
        #[cfg(not(target_os = "android"))]
        {
            unreachable!("profile requires Android")
        }
    } else {
        Outcome {
            error: Some(Error::UnsupportedProfile),
            ..Outcome::default()
        }
    };
    Ok(Report {
        schema_version: 1,
        environment,
        runtime_fingerprint: fingerprint,
        outcome,
        hierarchy_available: false,
    })
}

#[cfg(not(target_os = "android"))]
fn fingerprint() -> Result<Option<String>, CarloError> {
    Ok(None)
}

#[cfg(target_os = "android")]
fn fingerprint() -> Result<Option<String>, CarloError> {
    use std::ffi::{CStr, c_char, c_void};
    #[repr(C)]
    struct PropInfo {
        _opaque: [u8; 0],
    }
    unsafe extern "C" {
        fn __system_property_find(name: *const c_char) -> *const PropInfo;
        fn __system_property_read_callback(
            info: *const PropInfo,
            callback: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, u32),
            cookie: *mut c_void,
        );
    }
    unsafe extern "C" fn read(cookie: *mut c_void, _: *const c_char, value: *const c_char, _: u32) {
        let result = std::panic::catch_unwind(|| {
            // SAFETY: Bionic provides a non-null terminated value for this callback's duration.
            let bytes = unsafe { CStr::from_ptr(value) }.to_bytes();
            if bytes.len() > 512 {
                return Err(CarloError::InvalidEnvironment("fingerprint too long"));
            }
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| CarloError::InvalidEnvironment("fingerprint is not UTF-8"))
        })
        .unwrap_or(Err(CarloError::InvalidEnvironment(
            "property callback failed",
        )));
        // SAFETY: cookie points to our unique stack slot during this synchronous call.
        unsafe { *cookie.cast::<Option<Result<String, CarloError>>>() = Some(result) };
    }
    // SAFETY: static terminated key; returned read-only prop_info lives for the process.
    let info = unsafe { __system_property_find(c"ro.build.fingerprint".as_ptr()) };
    if info.is_null() {
        return Ok(None);
    }
    let mut value: Option<Result<String, CarloError>> = None;
    // SAFETY: info is a live Bionic property. The stack slot outlives the synchronous
    // callback. Unlike __system_property_get this supports long read-only values.
    unsafe { __system_property_read_callback(info, read, std::ptr::from_mut(&mut value).cast()) };
    value
        .ok_or(CarloError::InvalidEnvironment("property callback missing"))?
        .map(Some)
}
