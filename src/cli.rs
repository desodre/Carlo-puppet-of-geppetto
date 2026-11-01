//! Small, deliberately bounded command surface for the native foundation.

use crate::error::CarloError;

/// Usage information for the implemented CLI.
pub const HELP: &str = "Carlo — Puppet of Geppetto

Usage: carlo <command>

Commands:
  doctor [--json]  Report environment and implemented capabilities
  serve            Reserved for P3; currently returns unavailable (69)
  --version, -V    Show the package version
  --help, -h       Show this help

Doctor checks the native foundation, not Binder or video compatibility.
Exit codes: 0 success, 64 invalid arguments, 69 unavailable, 74 I/O or serialization.
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
    /// Reserved entry point for the future session protocol.
    Serve,
}

/// Parse arguments excluding the executable name.
///
/// # Errors
/// Returns [`CarloError::InvalidArguments`] for unknown or excessive arguments.
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Command, CarloError> {
    let mut arguments = arguments.into_iter();
    let first = arguments.next();
    let second = arguments.next();
    if arguments.next().is_some() {
        return Err(CarloError::InvalidArguments("too many arguments"));
    }
    match (first.as_deref(), second.as_deref()) {
        (None | Some("--help" | "-h"), None) => Ok(Command::Help),
        (Some("--version" | "-V"), None) => Ok(Command::Version),
        (Some("doctor"), None) => Ok(Command::Doctor { json: false }),
        (Some("doctor"), Some("--json")) => Ok(Command::Doctor { json: true }),
        (Some("serve"), None) => Ok(Command::Serve),
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
            vec!["serve", "--json"],
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
    }
}
