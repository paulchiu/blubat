//! Core of blubat, a Bluetooth battery monitor for macOS.
//!
//! This crate owns the device model, the macOS data sources that feed it, the
//! merge that reconciles them, the two tier poller, the rolling history behind
//! the trend and the rate, the threshold event engine and the configuration and
//! JSON shapes. No single macOS source lists every device
//! battery, so readings come from IOKit and from `system_profiler` and are
//! merged into one view that keeps the source and freshness of each reading
//! visible. Nothing here depends on a terminal library, so the TUI is one
//! frontend over this crate rather than the program itself. For the same
//! reason input a source could not use travels out on the snapshot rather than
//! being printed, leaving a frontend that owns the screen free to place it.
//!
//! ```no_run
//! let reading = blubat_core::snapshot();
//!
//! for device in reading.with_battery() {
//!     println!("{} {:?}", device.name, device.levels.lowest());
//! }
//! ```

mod address;
mod atomic;
mod config;
mod device;
mod duration;
mod engine;
mod error;
mod event;
mod history;
mod iokit;
mod json;
mod paths;
mod poll;
mod presence;
mod profiler;
mod snapshot;
mod theme;
mod timestamp;
mod watch;

pub use address::Address;
pub use config::{
    Advertised, AdvertisedThresholds, Config, Dashboard, Defaults, DeviceRule, Hook, Notifications,
    Poll, Sort, Thresholds,
};
pub use device::{ChargeState, Device, Levels, Part, Source};
pub use duration::{Debounce, parse_duration};
pub use engine::{Engine, Raised};
pub use error::{Error, Result};
pub use event::Event;
pub use history::{Direction, History, Sample, Trend};
pub use iokit::advertised;
pub use json::Reading;
pub use paths::Paths;
pub use poll::{Tiers, poll, snapshot};
pub use snapshot::Snapshot;
pub use theme::{Rgb, Scheme, Theme};
pub use timestamp::Timestamp;
pub use watch::Watch;
