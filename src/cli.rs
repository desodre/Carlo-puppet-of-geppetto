//! Small, deliberately bounded command surface for the native foundation.

use crate::error::CarloError;

/// Usage information for the implemented CLI.
pub const HELP: &str = "Carlo — Puppet of Geppetto

Usage: carlo <command>

Commands:
  doctor [--json]  Report environment and implemented capabilities
  accessibility-probe [--hold-ms N]  Register/wait/unregister (0..10000 ms hold)
  node-query-probe  Decode active root; report metadata only (JSON)
  node-read-probe   Emit decoded root as command data (includes UI text)
  binder-probe     API 34 shell Binder/ServiceManager proof (JSON)
  video-encode-probe  Encode synthetic GLES frames to H.264; report metadata (JSON)
  video-encode-dump   Emit that synthetic H.264 Annex B stream as command data
  surface-parcel-probe  Flatten the frameless encoder input surface; report metadata (JSON)
  display-mirror-probe  Mirror the default display into the H.264 encoder (pinned build only; JSON)
  display-mirror-dump   Emit that mirrored H.264 Annex B stream as command data
  display-fault-probe   Provoke the classified mirror failures (pinned build only; JSON)
  serve --session-dir DIR  One client, control and video channels, on DIR/control.sock
  --version, -V    Show the package version
  --help, -h       Show this help

Doctor checks the native foundation, not Binder or video compatibility.
Video encode probes use generated frames only; they never capture a display.
Display mirror commands send private display-service transactions and are
refused outside the single pinned build; the dump contains screen contents.
The fault probe provokes permission_denied, display_absent and unsupported_format
in three isolated sessions and passes only if no display was created.
The surface parcel stays in this process: nothing is transacted or mirrored.
Serve needs an absolute DIR owned by the caller with mode 0700. It prints one
JSON ready line with the transport and session identifier and accepts one
same-UID client: its control connection (hello, ping, capabilities, channels,
video.start, video.stop, shutdown) and, within 10 s of hello, one video
connection attached with the same session identifier. Video streams only on
the pinned display-mirror build and is reported unavailable elsewhere. If the
socket file cannot be created (permission denied) it uses a random abstract
socket instead and says so on the ready line (docs/protocol.md).
Exit codes: 0 success, 64 invalid arguments, 69 unavailable or refused,
74 I/O, serialization or a failed serve session.
";

/// A validated CLI command.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Display help.
    Help,
    /// Display version.
    Version,
    /// Inspect the runtime environment.
    Doctor {
        /// Emit a single JSON document on stdout.
        json: bool,
    },
    /// Exercise a reference-profile session; the executable owns the Binder pool.
    AccessibilityProbe {
        /// Bounded post-initialization hold before cleanup (milliseconds).
        hold_ms: u32,
    },
    /// Run the API 34 shell Binder proof, emitting JSON even on unavailability.
    BinderProbe,
    /// Decode one root and emit metadata without UI contents.
    NodeQueryProbe,
    /// Emit decoded root data on stdout, never in diagnostics.
    NodeReadProbe,
    /// Encode synthetic frames; emit stream metadata, or the raw stream on stdout.
    VideoEncodeProbe {
        /// Write Annex B bytes instead of the JSON report.
        dump: bool,
    },
    /// Flatten the frameless encoder input surface into a local parcel; emit metadata.
    SurfaceParcelProbe,
    /// Mirror the default display into the encoder on the pinned build; emit
    /// metadata, or the raw stream (screen contents) on stdout.
    DisplayMirrorProbe {
        /// Write Annex B bytes instead of the JSON report.
        dump: bool,
    },
    /// Provoke the three classified mirror failures on the pinned build; emit metadata.
    DisplayFaultProbe,
    /// Serve the one-client control channel from a private session directory.
    Serve {
        /// Absolute directory, owned by the caller with mode 0700, for the socket.
        session_dir: String,
    },
}

/// Parse arguments excluding the executable name.
///
/// # Errors
/// Returns [`CarloError::InvalidArguments`] for unknown or excessive arguments.
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Command, CarloError> {
    let mut arguments = arguments.into_iter();
    let first = arguments.next();
    let second = arguments.next();
    if first.as_deref() == Some("accessibility-probe") {
        let hold_ms = match second.as_deref() {
            None => 0,
            Some("--hold-ms") => arguments
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| *value <= 10_000)
                .ok_or(CarloError::InvalidArguments(
                    "hold must be 0..10000 milliseconds",
                ))?,
            _ => {
                return Err(CarloError::InvalidArguments(
                    "unknown accessibility probe option",
                ));
            }
        };
        if arguments.next().is_some() {
            return Err(CarloError::InvalidArguments("too many arguments"));
        }
        return Ok(Command::AccessibilityProbe { hold_ms });
    }
    if first.as_deref() == Some("serve") {
        let session_dir = match (second.as_deref(), arguments.next()) {
            (Some("--session-dir"), Some(directory)) => directory,
            _ => {
                return Err(CarloError::InvalidArguments(
                    "serve requires --session-dir <absolute directory>",
                ));
            }
        };
        if arguments.next().is_some() {
            return Err(CarloError::InvalidArguments("too many arguments"));
        }
        if !std::path::Path::new(&session_dir).is_absolute() {
            return Err(CarloError::InvalidArguments(
                "session directory must be an absolute path",
            ));
        }
        return Ok(Command::Serve { session_dir });
    }
    if arguments.next().is_some() {
        return Err(CarloError::InvalidArguments("too many arguments"));
    }
    match (first.as_deref(), second.as_deref()) {
        (None | Some("--help" | "-h"), None) => Ok(Command::Help),
        (Some("--version" | "-V"), None) => Ok(Command::Version),
        (Some("doctor"), None) => Ok(Command::Doctor { json: false }),
        (Some("doctor"), Some("--json")) => Ok(Command::Doctor { json: true }),
        (Some("node-read-probe"), None) => Ok(Command::NodeReadProbe),
        (Some("node-query-probe"), None) => Ok(Command::NodeQueryProbe),
        (Some("binder-probe"), None) => Ok(Command::BinderProbe),
        (Some("video-encode-probe"), None) => Ok(Command::VideoEncodeProbe { dump: false }),
        (Some("video-encode-dump"), None) => Ok(Command::VideoEncodeProbe { dump: true }),
        (Some("surface-parcel-probe"), None) => Ok(Command::SurfaceParcelProbe),
        (Some("display-mirror-probe"), None) => Ok(Command::DisplayMirrorProbe { dump: false }),
        (Some("display-mirror-dump"), None) => Ok(Command::DisplayMirrorProbe { dump: true }),
        (Some("display-fault-probe"), None) => Ok(Command::DisplayFaultProbe),
        _ => Err(CarloError::InvalidArguments(
            "unknown command or option; use --help",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn rejects_options_that_could_be_silently_ignored() {
        for values in [
            vec!["serve"],
            vec!["serve", "--json"],
            vec!["serve", "--session-dir"],
            vec!["serve", "--session-dir", "relative/dir"],
            vec!["serve", "--session-dir", "/tmp/a", "extra"],
            vec!["serve", "/tmp/a"],
            vec!["binder-probe", "--json"],
            vec!["video-encode-dump", "--output"],
            vec!["surface-parcel-probe", "--json"],
            vec!["display-mirror-probe", "--json"],
            vec!["display-mirror-dump", "--output"],
            vec!["display-fault-probe", "--json"],
            vec!["display-fault-probe", "--secure"],
            vec!["doctor", "--json", "extra"],
            vec!["--version", "doctor"],
        ] {
            assert!(matches!(
                parse(args(&values)),
                Err(CarloError::InvalidArguments(_))
            ));
        }
    }

    #[test]
    fn json_is_explicit_and_help_is_available_without_android() {
        assert_eq!(
            parse(args(&["doctor", "--json"])).unwrap(),
            Command::Doctor { json: true }
        );
        assert_eq!(parse(Vec::new()).unwrap(), Command::Help);
        assert_eq!(
            parse(args(&["surface-parcel-probe"])).unwrap(),
            Command::SurfaceParcelProbe
        );
        assert!(HELP.contains("surface-parcel-probe"));
        assert_eq!(
            parse(args(&["display-mirror-probe"])).unwrap(),
            Command::DisplayMirrorProbe { dump: false }
        );
        assert_eq!(
            parse(args(&["display-mirror-dump"])).unwrap(),
            Command::DisplayMirrorProbe { dump: true }
        );
        assert!(HELP.contains("display-mirror-probe") && HELP.contains("display-mirror-dump"));
        assert_eq!(
            parse(args(&["display-fault-probe"])).unwrap(),
            Command::DisplayFaultProbe
        );
        assert!(HELP.contains("display-fault-probe"));
        assert_eq!(
            parse(args(&["serve", "--session-dir", "/data/local/tmp/x"])).unwrap(),
            Command::Serve {
                session_dir: "/data/local/tmp/x".into()
            }
        );
        assert!(HELP.contains("serve --session-dir"));
    }
}
