//! Native Android agent foundations: command parsing, diagnostics and typed errors.
//!
//! Hierarchy, Binder integration, streaming and the serve protocol are not yet implemented.

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
/// environment, or the currently unimplemented `serve` command. Doctor writes its
/// report before returning an unsuitable-environment error.
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
        Command::Serve => Err(CarloError::UnsupportedFeature(
            "serve protocol is planned in P3",
        )),
    }
}
