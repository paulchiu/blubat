//! Core of blubat, a Bluetooth battery monitor for macOS.
//!
//! This crate owns the device model, the macOS data sources that feed it, the
//! merge that reconciles them, the poller, the threshold event engine and the
//! configuration and JSON shapes. No single macOS source lists every device
//! battery, so readings come from IOKit and from `system_profiler` and are
//! merged into one view that keeps the source and freshness of each reading
//! visible. Nothing here depends on a terminal library, so the TUI is one
//! frontend over this crate rather than the program itself.
