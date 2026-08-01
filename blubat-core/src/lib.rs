//! Core of blubat, a Bluetooth battery monitor for macOS.
//!
//! This crate owns the device model, the macOS data sources that feed it, the
//! merge that reconciles them, the poller, the threshold event engine and the
//! configuration and JSON shapes. No single macOS source lists every device
//! battery, so readings come from IOKit and from `system_profiler` and are
//! merged into one view that keeps the source and freshness of each reading
//! visible. Nothing here depends on a terminal library, so the TUI is one
//! frontend over this crate rather than the program itself.
//!
//! ```no_run
//! let reading = blubat_core::snapshot();
//!
//! for device in reading.with_battery() {
//!     println!("{} {:?}", device.name, device.levels.lowest());
//! }
//! ```

mod address;
mod device;
mod error;
mod iokit;
mod poll;
mod profiler;
mod snapshot;
mod timestamp;
mod watch;

pub use address::Address;
pub use device::{ChargeState, Device, Levels, Source};
pub use error::{Error, Result};
pub use poll::{poll, snapshot};
pub use snapshot::Snapshot;
pub use timestamp::Timestamp;
pub use watch::{Watch, watch_dir};

/// Reports input blubat could not use and carried on past.
///
/// stderr, so a warning never contaminates the JSON or the bare number that a
/// script is reading from stdout.
pub(crate) fn warn(message: &str) {
    eprintln!("blubat: warning: {message}");
}
