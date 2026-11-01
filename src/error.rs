//! Errors exposed by Carlo's command-line foundation.

use std::fmt;
use std::io;

/// A recoverable foundation or command-line failure.
#[derive(Debug)]
pub enum CarloError {
    /// Command line did not match the supported grammar.
    InvalidArguments(&'static str),
    /// Requested feature has not been implemented.
    UnsupportedFeature(&'static str),
    /// Doctor collected a report, but its foundation checks did not pass.
    UnsuitableEnvironment,
    /// Runtime environment contained malformed data.
    InvalidEnvironment(&'static str),
    /// Output or environment I/O failed, preserving the original cause.
    Io {
        /// The operation that failed.
        operation: &'static str,
        /// The original I/O error.
        source: io::Error,
    },
    /// JSON serialization failed, preserving the original cause.
    Json(serde_json::Error),
}

impl CarloError {
    /// Wrap output I/O while preserving its cause.
    pub fn output(source: io::Error) -> Self {
        Self::Io {
            operation: "write output",
            source,
        }
    }

    /// Stable process exit code for this error category.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidArguments(_) => 64,
            Self::UnsupportedFeature(_)
            | Self::UnsuitableEnvironment
            | Self::InvalidEnvironment(_) => 69,
            Self::Io { .. } | Self::Json(_) => 74,
        }
    }

    /// Whether a reader closed stdout early.
    pub fn is_broken_pipe(&self) -> bool {
        match self {
            Self::Io { source, .. } => source.kind() == io::ErrorKind::BrokenPipe,
            Self::Json(source) => source.io_error_kind() == Some(io::ErrorKind::BrokenPipe),
            _ => false,
        }
    }
}

impl fmt::Display for CarloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(message)
            | Self::UnsupportedFeature(message)
            | Self::InvalidEnvironment(message) => f.write_str(message),
            Self::UnsuitableEnvironment => {
                f.write_str("native foundation checks failed; see doctor report")
            }
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Json(source) => write!(f, "serialize report: {source}"),
        }
    }
}

impl std::error::Error for CarloError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for CarloError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}
