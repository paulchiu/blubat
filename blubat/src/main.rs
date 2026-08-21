//! The blubat binary: the dashboard and the one-shot CLI over `blubat-core`.
//!
//! A bare `blubat` opens the live dashboard, or prints what it can do when
//! there is no terminal to draw one on. Every other command reads one
//! snapshot, prints it in the form the caller asked for, and exits with a code
//! a script can branch on: 0 for a usable reading, 3 when no matching device
//! has a battery, 1 for anything else.

mod config;
mod daemon;
mod effects;
mod hooks;
mod lock;
mod notify;
mod report;
#[cfg(test)]
mod scratch;
mod tui;
mod wait;

use std::fmt;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use blubat_core::{Paths, Snapshot};
use clap::{CommandFactory, Parser, Subcommand};

use report::Format;

/// Bluetooth battery monitor for macOS.
#[derive(Debug, Parser)]
#[command(name = "blubat", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Read configuration from this file instead of the resolved one.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Keep blubat's own state in this directory instead of the resolved one.
    ///
    /// The installed agent is pinned to the directory that installed it named,
    /// since launchd hands the daemon almost none of the environment the
    /// resolved one is worked out from.
    #[arg(long, global = true, value_name = "PATH")]
    state_dir: Option<PathBuf>,
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
    ///
    /// With a daemon running the wait is handed to it: blubat registers a
    /// one-shot watch, prints where it registered it and returns at once, and
    /// the daemon posts the banner when the level arrives. With no daemon the
    /// wait polls here until then and holds the terminal while it does.
    Wait(wait::Args),
    /// Show, open or check the configuration file.
    Config {
        #[command(subcommand)]
        command: config::Command,
    },
    /// Run blubat in the background, or install the agent that does.
    Daemon {
        #[command(subcommand)]
        command: daemon::Command,
    },
    /// Send a test banner and report the identity it was delivered under.
    ///
    /// blubat has no notification identity of its own, so macOS attributes its
    /// banners to another app: Terminal, or Script Editor on the fallback path.
    /// A silent success usually means that borrowed identity is muted, either
    /// by a Focus mode or in the notification settings for that app.
    NotifyTest,
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
        Ok(cli) => run(cli).map_or_else(|failure| fail(&failure), |()| ExitCode::SUCCESS),
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
    let files = || paths(cli.config.clone(), cli.state_dir.clone());

    match cli.command {
        Some(Command::List { json, all }) => report::list(&reading(&files()?), json, all),
        Some(Command::Status {
            device,
            json,
            number,
        }) => report::status(
            &reading(&files()?),
            device.as_deref(),
            Format::of(json, number),
        ),
        Some(Command::Wait(args)) => wait::run(&args, &files()?),
        Some(Command::Config { command }) => config::run(&command, &files()?),
        Some(Command::Daemon { command }) => daemon::run(&command, &files()?),
        Some(Command::NotifyTest) => notify::run(&files()?),
        None if io::stdout().is_terminal() => tui::run(&files()?),
        None => offer_the_commands(),
    }
}

/// Where blubat's files are, with each flag replacing the resolved location.
fn paths(config: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<Paths, Failure> {
    let mut paths = Paths::resolve()?;

    if let Some(path) = config {
        paths = paths.with_config_file(path);
    }
    if let Some(dir) = state_dir {
        paths = paths.with_state_dir(dir);
    }

    Ok(paths)
}

/// What a bare `blubat` says when there is no screen to draw a dashboard on.
///
/// Piped into a script or a test there is nowhere to put a full screen view, so
/// blubat prints what it can do instead. A successful request for text, so it
/// exits 0 the way `--help` does rather than failing a first run.
fn offer_the_commands() -> Result<(), Failure> {
    Cli::command().print_help()?;
    println!("\nRun `blubat list` for a reading, or `blubat` in a terminal for the dashboard.");

    Ok(())
}

/// One reading, with whatever the core could not use reported on stderr.
///
/// The core hands its warnings back rather than printing them, so a frontend
/// that owns the screen can place them. This one only owes stdout a clean value.
/// A degraded reading is said there too rather than in `--json`, which is a
/// compatibility surface carrying devices rather than anything about the read.
/// `paths` is only ever read from here: a Bose battery level reaches this
/// reading through `readings_file()`, a file, never a Bluetooth call of its own.
fn reading(paths: &Paths) -> Snapshot {
    let snapshot = blubat_core::snapshot(&paths.readings_file());

    if snapshot.degraded {
        eprintln!("blubat: warning: a source could not be read, so this is its last good answer");
    }
    for warning in &snapshot.warnings {
        eprintln!("blubat: warning: {warning}");
    }

    snapshot
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Error(error.to_string())
    }
}

fn fail(failure: &Failure) -> ExitCode {
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
