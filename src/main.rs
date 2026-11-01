//! Command-line entry point. Android features are reported explicitly by doctor.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let result = carlo::cli::parse(std::env::args().skip(1))
        .and_then(|command| carlo::run(command, &mut io::stdout().lock()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.is_broken_pipe() => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "carlo: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
