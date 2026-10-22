//! wayhint: keyboard hints for everything clickable on a Wayland desktop.
//!
//! Phase 0 has no hints yet, only a command line that behaves like a real
//! one: `--help`, `--version`, and exit code 2 for anything it doesn't know.

use std::process::ExitCode;

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Command {
    Run,
    Version,
    Help,
}

/// Why the command line was rejected. It owns its `String`, so the error can
/// outlive the arguments it came from.
#[derive(Debug, PartialEq)]
enum ParseError {
    Unexpected(String),
}

const USAGE: &str = "Usage: wayhint [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version";

/// Reads the arguments, without the program name. It borrows `&str`s so a
/// test can write `parse(&["-V"])`.
fn parse(args: &[&str]) -> Result<Command, ParseError> {
    match args {
        [] => Ok(Command::Run),
        ["-V" | "--version"] => Ok(Command::Version),
        ["-h" | "--help"] => Ok(Command::Help),
        // One flag we know, then more: the first extra one is the problem.
        ["-V" | "--version" | "-h" | "--help", extra, ..] | [extra, ..] => {
            Err(ParseError::Unexpected((*extra).to_string()))
        }
    }
}

fn main() -> ExitCode {
    // `env::args()` hands out owned `String`s. The `Vec<String>` has to live
    // in a named variable, because the `Vec<&str>` below borrows from it.
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();

    match parse(&args) {
        Ok(Command::Run) => {
            println!("wayhint: nothing to do yet");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("wayhint {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Help) => {
            println!("{}\n\n{USAGE}", env!("CARGO_PKG_DESCRIPTION"));
            ExitCode::SUCCESS
        }
        Err(ParseError::Unexpected(arg)) => {
            eprintln!("error: unexpected argument '{arg}'\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_args_runs() {
        assert_eq!(parse(&[]), Ok(Command::Run));
    }

    #[test]
    fn version_long_and_short() {
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn help_long_and_short() {
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert_eq!(
            parse(&["--bogus"]),
            Err(ParseError::Unexpected("--bogus".into()))
        );
    }

    #[test]
    fn extra_argument_is_an_error() {
        assert_eq!(
            parse(&["-V", "extra"]),
            Err(ParseError::Unexpected("extra".into()))
        );
    }
}
