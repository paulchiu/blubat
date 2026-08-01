//! The blubat binary: the one-shot CLI over `blubat-core`.
//!
//! Every command reads one snapshot, prints it in the form the caller asked
//! for, and exits with a code a script can branch on: 0 for a usable reading,
//! 3 when no matching device has a battery, 1 for anything else.

mod report;
mod wait;

use std::fmt;
use std::process::ExitCode;

use blubat_core::Snapshot;
use clap::{CommandFactory, Parser, Subcommand};

use report::Format;

/// Bluetooth battery monitor for macOS.
#[derive(Debug, Parser)]
#[command(name = "blubat", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a table of every device that reports a battery.
    List {
        /// Emit the same devices as a JSON array.
        #[arg(long)]
        json: bool,
        /// Include devices that report no battery at all.
        #[arg(long, short)]
        all: bool,
    },
    /// Print one device's reading.
    Status {
        /// Substring matched against device name and address.
        #[arg(long, short)]
        device: Option<String>,
        /// Emit the device as a JSON object.
        #[arg(long, conflicts_with = "number")]
        json: bool,
        /// Print the bare percentage and nothing else.
        #[arg(long, short)]
        number: bool,
    },
    /// Wait until a device reaches a level, then notify.
    Wait(wait::Args),
}

/// Why a command stopped, carrying the exit code it owes a script.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// Nothing blubat could report on, the shell POC's exit 3.
    NoDevice(String),
    /// Anything blubat could not do.
    Error(String),
}

impl Failure {
    /// Exit code for anything blubat could not do, including a usage error.
    const ERROR: u8 = 1;
    /// Exit code for having nothing to report on.
    const NO_DEVICE: u8 = 3;

    fn code(&self) -> u8 {
        match self {
            Failure::NoDevice(_) => Self::NO_DEVICE,
            Failure::Error(_) => Self::ERROR,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoDevice(message) | Failure::Error(message) => f.write_str(message),
        }
    }
}

impl From<blubat_core::Error> for Failure {
    fn from(error: blubat_core::Error) -> Self {
        Failure::Error(error.to_string())
    }
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => run(cli).map_or_else(fail, |()| ExitCode::SUCCESS),
        Err(usage) => {
            let _ = usage.print();

            // Help and version are a successful request for text; everything
            // else clap rejects is a usage error, which is the error exit.
            if usage.use_stderr() {
                ExitCode::from(Failure::ERROR)
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

fn run(cli: Cli) -> Result<(), Failure> {
    match cli.command {
        Some(Command::List { json, all }) => report::list(&reading(), json, all),
        Some(Command::Status {
            device,
            json,
            number,
        }) => report::status(&reading(), device.as_deref(), Format::of(json, number)),
        Some(Command::Wait(args)) => wait::run(&args),
        None => print_help(),
    }
}

/// One reading, with whatever the core could not use reported on stderr.
///
/// The core hands its warnings back rather than printing them, so a frontend
/// that owns the screen can place them. This one only owes stdout a clean value.
fn reading() -> Snapshot {
    let snapshot = blubat_core::snapshot();

    for warning in &snapshot.warnings {
        eprintln!("blubat: warning: {warning}");
    }

    snapshot
}

/// Prints the help a bare `blubat` has instead of the dashboard.
fn print_help() -> Result<(), Failure> {
    Cli::command()
        .print_help()
        .map_err(|error| Failure::Error(error.to_string()))?;
    println!("\nThe live TUI dashboard bare `blubat` opens arrives in a later release.");

    Ok(())
}

fn fail(failure: Failure) -> ExitCode {
    eprintln!("blubat: {failure}");

    ExitCode::from(failure.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_definition_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn exit_codes_match_the_documented_contract() {
        assert_eq!(Failure::NoDevice("nothing".to_string()).code(), 3);
        assert_eq!(Failure::Error("broke".to_string()).code(), 1);
    }

    #[test]
    fn a_core_error_is_an_error_exit_carrying_its_message() {
        let failure = Failure::from(blubat_core::Error::Command(
            "no system_profiler".to_string(),
        ));

        assert_eq!(failure.code(), 1);
        assert_eq!(failure.to_string(), "no system_profiler");
    }
}
