//! `blubat daemon`: resident mode and the LaunchAgent that runs it.
//!
//! Nothing here starts by itself. Installing the agent is a command the user
//! runs, and until they do, blubat is a program that exits when it is finished.

mod bluetoothd;
mod bmap;
mod gatt;
mod launchd;
mod run;
mod sweep;
mod watches;

use std::io;

use blubat_core::Paths;

use crate::Failure;

/// What `blubat daemon` was asked to do.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Poll, notify and run hooks until stopped, which is what launchd calls.
    ///
    /// Runs in the foreground: launchd owns the process, and running it in a
    /// terminal is how to watch what the installed agent would be doing.
    Run,
    /// Write the LaunchAgent for this blubat and start it.
    Install,
    /// Stop the LaunchAgent and remove it.
    Uninstall,
    /// Re-register and restart the LaunchAgent, which an upgraded binary needs.
    Restart,
    /// Report whether the LaunchAgent is installed, loaded and running.
    Status,
    /// Print what macOS has cached for every paired device, as JSON.
    ///
    /// The sweep's own helper rather than anything to run by hand: from a
    /// terminal this may abort under TCC, since the terminal is then the
    /// process macOS holds responsible for the Bluetooth access, while the
    /// daemon's children read under the daemon's own grant.
    #[command(hide = true)]
    CachedLevels,
}

/// Runs one `blubat daemon` subcommand.
pub fn run(command: &Command, paths: &Paths) -> Result<(), Failure> {
    let mut out = io::stdout();

    match command {
        Command::Run => run::serve(paths),
        Command::Install => {
            launchd::install(&launchd::Cli, paths, &launchd::plist_file()?, &mut out)
        }
        Command::Uninstall => launchd::uninstall(&launchd::Cli, &launchd::plist_file()?, &mut out),
        Command::Restart => launchd::restart(&launchd::Cli, &launchd::plist_file()?, &mut out),
        Command::Status => launchd::status(&launchd::Cli, &launchd::plist_file()?, &mut out),
        Command::CachedLevels => bluetoothd::print_cache(&mut out),
    }
}
