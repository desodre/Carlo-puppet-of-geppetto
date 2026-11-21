//! Native Android agent foundations: command parsing, diagnostics and typed errors.
//!
//! Binder lookup and a synthetic H.264 encoder have probes. A display mirror proof
//! and its classified failures exist for one pinned build only. `serve` offers a
//! one-client control channel (handshake, ping, capabilities, shutdown) over a
//! Unix socket; hierarchy and streaming commands are not implemented.

mod accessibility;
pub mod binder;
pub mod cli;
pub mod diagnostics;
pub mod error;
pub mod protocol;
pub mod serve;
mod video;

use std::io::Write;

use cli::Command;
use diagnostics::Diagnostics;
use error::CarloError;

/// Execute a parsed command, writing user-facing data to `output`.
///
/// # Errors
/// Returns [`CarloError`] for I/O/serialization failures, an unsuitable foundation
/// environment, an unsuccessful Binder probe, or a refused or failed `serve` session.
/// Doctor and Binder probe write their reports before returning unavailable errors.
pub fn run(command: Command, output: &mut impl Write) -> Result<(), CarloError> {
    match command {
        Command::Help => output
            .write_all(cli::HELP.as_bytes())
            .map_err(CarloError::output),
        Command::Version => {
            writeln!(output, "carlo {}", env!("CARGO_PKG_VERSION")).map_err(CarloError::output)
        }
        Command::Doctor { json } => {
            let report = Diagnostics::collect()?;
            if json {
                serde_json::to_writer_pretty(&mut *output, &report)?;
                writeln!(output).map_err(CarloError::output)?;
            } else {
                report.write_text(output)?;
            }
            if report.foundation_ready {
                Ok(())
            } else {
                Err(CarloError::UnsuitableEnvironment)
            }
        }
        Command::AccessibilityProbe { hold_ms } => {
            let report = accessibility::collect(hold_ms)?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "accessibility session proof did not pass; see JSON report",
                ))
            }
        }
        Command::NodeQueryProbe | Command::NodeReadProbe => {
            let report = accessibility::collect_node(matches!(command, Command::NodeReadProbe))?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "node query proof did not pass; see JSON report",
                ))
            }
        }
        Command::BinderProbe => {
            let report = binder::BinderProbe::collect()?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "Binder probe did not pass; see JSON report",
                ))
            }
        }
        Command::VideoEncodeProbe { dump } => {
            let mut report = video::collect(dump)?;
            if dump {
                // Stream bytes are the only stdout data; a failed probe emits none.
                let data = report.take_data().ok_or(CarloError::UnsupportedFeature(
                    "synthetic encoder proof did not pass; run video-encode-probe",
                ))?;
                return output.write_all(&data).map_err(CarloError::output);
            }
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "synthetic encoder proof did not pass; see JSON report",
                ))
            }
        }
        Command::SurfaceParcelProbe => {
            let report = video::surface::collect()?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "surface parcel proof did not pass; see JSON report",
                ))
            }
        }
        Command::DisplayMirrorProbe { dump } => {
            let mut report = video::mirror::collect(dump)?;
            if dump {
                // Stream bytes are the only stdout data; a failed proof emits none.
                let data = report.take_data().ok_or(CarloError::UnsupportedFeature(
                    "display mirror proof did not pass; run display-mirror-probe",
                ))?;
                return output.write_all(&data).map_err(CarloError::output);
            }
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "display mirror proof did not pass; see JSON report",
                ))
            }
        }
        Command::DisplayFaultProbe => {
            let report = video::mirror::collect_faults()?;
            serde_json::to_writer_pretty(&mut *output, &report)?;
            writeln!(output).map_err(CarloError::output)?;
            if report.passed() {
                Ok(())
            } else {
                Err(CarloError::UnsupportedFeature(
                    "display fault proof did not pass; see JSON report",
                ))
            }
        }
        Command::Serve { session_dir } => serve::run(&session_dir, output),
    }
}
