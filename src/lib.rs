//! Native Android agent foundations: command parsing, diagnostics and typed errors.
//!
//! Binder lookup has an API 34 probe. Hierarchy, streaming and serve are not implemented.

mod accessibility;
pub mod binder;
pub mod cli;
pub mod diagnostics;
pub mod error;

use std::io::Write;

use cli::Command;
use diagnostics::Diagnostics;
use error::CarloError;

/// Execute a parsed command, writing user-facing data to `output`.
///
/// # Errors
/// Returns [`CarloError`] for I/O/serialization failures, an unsuitable foundation
/// environment, an unsuccessful Binder probe, or the unimplemented `serve` command.
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
        Command::Serve => Err(CarloError::UnsupportedFeature(
            "serve protocol is planned in P3",
        )),
    }
}
