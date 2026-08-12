//! The bluetoothd sweep: the battery levels macOS has already learned, read
//! back off the paired devices and left in `readings.toml` beside the other
//! two sweeps' readings (see `blubat_core::bluetoothd` for what a device's
//! cached percentages amount to, and `blubat_core::readings` for the file the
//! three share).
//!
//! The read happens in a child process, freshly spawned per sweep, because a
//! process only ever sees the cache as it stood when IOBluetooth initialised
//! in it. Twenty minute probes established that against a cache that was
//! visibly changing: neither sustained run loop pumping, nor registering for
//! connect notifications, nor the private `updateFromServer` refreshed a
//! single value, and a Bose QC's percentage cleared itself to 0 about thirty
//! seconds after launch. A daemon reading in process therefore re-emitted its
//! launch time snapshot for as long as it ran, a trackpad held at 96 for 26
//! hours while the real cache drained to 90. A fresh process always reads the
//! current values, so [`Bluetoothd`] spawns one per sweep and it exits on the
//! one read, the same shape the slow tier's `system_profiler` call has.
//!
//! Everything [`super::bmap`]'s module doc says about TCC holds here too: the
//! child opens the same IOBluetooth, and macOS holds the daemon that spawned
//! it responsible for that access, so the daemon's own grant is what the child
//! reads under. Nothing beyond the child itself is waited on, since a cache
//! read answers or does not answer immediately, and no link is opened at all.
//! A child that will not spawn, outlasts the sweep's timeout, exits badly or
//! writes something unreadable is no readings this pass, the same
//! one-attempt-no-retry discipline every other daemon sweep keeps.
//!
//! The properties carrying the cache are private. Apple publishes no battery
//! API on `IOBluetoothDevice`, and `batteryPercentSingle` and its siblings
//! have simply been there since Monterey, so any macOS release may take them
//! away without notice. Every one of them is therefore read only where
//! `respondsToSelector` says this system still has it, and a system that has
//! none of them sweeps to nothing rather than failing: blubat loses a source
//! it never had a guarantee of and keeps every other reading it takes.
//!
//! `batteryPercentCase` is left unread. The other four agree with what System
//! Settings shows; the case value does not reliably, and a level is only
//! worth reporting where blubat can stand behind it.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use blubat_core::{Address, SweepReading, Timestamp, bluetoothd_battery_level};

use objc2::runtime::AnyClass;
use objc2::{msg_send, sel};
use objc2_foundation::NSObjectProtocol;
use objc2_io_bluetooth::IOBluetoothDevice;
use serde::{Deserialize, Serialize};

use crate::Failure;

/// What the daemon runs to have one read done somewhere fresh.
const HELPER: [&str; 2] = ["daemon", "cached-levels"];

/// One paired device as macOS's cache describes it: what names it, whether
/// it is here, and every battery percentage held against it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cached {
    pub(crate) address: String,
    pub(crate) name: String,
    pub(crate) connected: bool,
    pub(crate) percentages: Vec<u8>,
}

/// Somewhere the cached percentages are read, which a test fills with a fake.
pub(crate) trait Cache {
    /// Every device paired with this Mac, with whatever percentages macOS
    /// holds for each. A system with no such cache to read, and a read that
    /// does not finish inside `timeout`, have none.
    fn paired(&self, timeout: Duration) -> Vec<Cached>;
}

/// The real one: one helper process per sweep, for the reason the module doc
/// gives.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bluetoothd;

/// One private percentage property, read only where this macOS has it.
///
/// Guard and read sit in the one expression so a property can never be added
/// here without the check that makes reading it safe.
macro_rules! cached_percentage {
    ($device:expr, $property:ident) => {
        // SAFETY: the guard is `respondsToSelector` on the very selector
        // being sent, and every one of these properties is an `unsigned char`.
        $device
            .respondsToSelector(sel!($property))
            .then(|| unsafe { msg_send![$device, $property] })
    };
}

/// The percentages macOS holds for one device, in no particular order:
/// [`bluetoothd_battery_level`] takes the lowest of them whatever they mean.
fn percentages(device: &IOBluetoothDevice) -> Vec<u8> {
    let cached: [Option<u8>; 4] = [
        cached_percentage!(device, batteryPercentSingle),
        cached_percentage!(device, batteryPercentCombined),
        cached_percentage!(device, batteryPercentLeft),
        cached_percentage!(device, batteryPercentRight),
    ];

    cached.into_iter().flatten().collect()
}

/// What the cache holds for one paired device.
fn cached(device: &IOBluetoothDevice) -> Option<Cached> {
    // SAFETY: the device's own connection state, identity and display name,
    // the same plain properties `super::bmap` already reaches this class for.
    let (connected, address, name) = unsafe {
        (
            device.isConnected(),
            device.addressString(),
            device.nameOrAddress(),
        )
    };

    Some(Cached {
        address: address?.to_string(),
        name: name?.to_string(),
        connected,
        percentages: percentages(device),
    })
}

/// Everything macOS has cached, read here in this process.
fn cache() -> Vec<Cached> {
    // A macOS without the class at all would abort the typed call below,
    // which is the one failure this source must survive in silence.
    if AnyClass::get(c"IOBluetoothDevice").is_none() {
        return Vec::new();
    }

    // SAFETY: a plain class method, nil when nothing is paired.
    let paired = unsafe { IOBluetoothDevice::pairedDevices() };

    paired
        .map(|devices| {
            devices
                .to_vec()
                .into_iter()
                .filter_map(|device| device.downcast::<IOBluetoothDevice>().ok())
                .filter_map(|device| cached(&device))
                .collect()
        })
        .unwrap_or_default()
}

/// `blubat daemon cached-levels`: the whole of the helper the sweep spawns.
pub(crate) fn print_cache(out: &mut impl Write) -> Result<(), Failure> {
    writeln!(out, "{}", printed(&cache()))?;

    Ok(())
}

/// What one helper run writes, and [`parse`] reads back.
///
/// A device carries only strings, a bool and percentages, so the one way this
/// can fail is a serde bug; the empty string it falls back to parses as no
/// devices, which is how every other failure in this source reads anyway.
fn printed(cache: &[Cached]) -> String {
    serde_json::to_string(cache).unwrap_or_default()
}

/// The devices one helper run described, none where it wrote anything else.
fn parse(printed: &str) -> Vec<Cached> {
    serde_json::from_str(printed).unwrap_or_default()
}

impl Cache for Bluetoothd {
    fn paired(&self, timeout: Duration) -> Vec<Cached> {
        let Ok(blubat) = std::env::current_exe() else {
            return Vec::new();
        };
        let mut helper = Command::new(blubat);
        helper.args(HELPER);

        run(helper, timeout)
            .as_deref()
            .map(parse)
            .unwrap_or_default()
    }
}

/// Runs `command` and hands back its stdout, giving up after `timeout`.
///
/// The same shape `blubat_core::profiler` runs `system_profiler` in: stdout is
/// drained on a thread of its own so a child cannot stall on a full pipe, and
/// one still running at the deadline is killed rather than waited out. What it
/// wrote on stderr is nobody's to report, since a failed sweep says nothing.
fn run(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let stdout = drain(child.stdout.take().expect("stdout was piped"));

    let Ok(output) = stdout.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();

        return None;
    };

    child.wait().ok().filter(ExitStatus::success)?;

    String::from_utf8(output).ok()
}

/// Reads one child pipe to its end on a thread of its own.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (read, drained) = mpsc::channel();

    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        let _ = read.send(buffer);
    });

    drained
}

/// Runs the sweep across every connected device the cache has a level for.
///
/// A disconnected device is passed over: its cached percentages are whatever
/// they were when it last spoke, with nothing to date them by, so recording
/// them as a reading taken now would age a number that is already old. A
/// device whose percentages are all unpopulated, and one macOS names by an
/// address blubat cannot parse, are likewise silently no reading this sweep,
/// the discipline every other daemon sweep keeps.
pub(crate) fn sweep(cache: &dyn Cache, read_at: Timestamp, timeout: Duration) -> Vec<SweepReading> {
    cache
        .paired(timeout)
        .into_iter()
        .filter(|device| device.connected)
        .filter_map(|device| {
            let level = bluetoothd_battery_level(&device.percentages)?;
            let address = Address::parse(&device.address)?;

            Some(SweepReading::bluetoothd(
                address,
                device.name,
                level,
                read_at,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINCE: &str = "bc-87-fa-18-b0-b7";
    const AIRPODS: &str = "74-15-f5-02-8e-38";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    /// Nothing a fake answers is timed, so every sweep below is given none.
    const TIMEOUT: Duration = Duration::ZERO;

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    /// A cache holding exactly what it was built with.
    struct Fake(Vec<Cached>);

    impl Fake {
        fn holding(devices: Vec<(&str, &str, Vec<u8>)>) -> Self {
            Self(
                devices
                    .into_iter()
                    .map(|(address, name, percentages)| Cached {
                        address: address.to_string(),
                        name: name.to_string(),
                        connected: true,
                        percentages,
                    })
                    .collect(),
            )
        }

        fn disconnected(self) -> Self {
            Self(
                self.0
                    .into_iter()
                    .map(|device| Cached {
                        connected: false,
                        ..device
                    })
                    .collect(),
            )
        }
    }

    impl Cache for Fake {
        fn paired(&self, _timeout: Duration) -> Vec<Cached> {
            self.0.clone()
        }
    }

    #[test]
    fn a_cached_percentage_becomes_one_reading_under_the_devices_own_address() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![79])]);

        assert_eq!(
            sweep(&fake, READ_AT, TIMEOUT),
            [SweepReading::bluetoothd(
                address(PRINCE),
                "Bose QC Headphones",
                79,
                READ_AT
            )]
        );
    }

    #[test]
    fn a_paired_device_that_is_not_here_is_never_read() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![79])]).disconnected();

        assert_eq!(sweep(&fake, READ_AT, TIMEOUT), []);
    }

    #[test]
    fn a_devices_emptiest_battery_is_the_level_it_reads_at() {
        let fake = Fake::holding(vec![(AIRPODS, "AirPods Pro", vec![0, 0, 79, 62])]);

        assert_eq!(sweep(&fake, READ_AT, TIMEOUT)[0].level, 62);
    }

    #[test]
    fn a_device_with_nothing_cached_yields_no_reading_at_all() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![0, 0, 0, 0])]);

        assert_eq!(sweep(&fake, READ_AT, TIMEOUT), []);
    }

    #[test]
    fn one_device_without_a_level_does_not_stop_the_rest_of_the_sweep() {
        let fake = Fake::holding(vec![
            (PRINCE, "Bose QC Headphones", Vec::new()),
            (AIRPODS, "AirPods Pro", vec![79]),
        ]);

        let readings = sweep(&fake, READ_AT, TIMEOUT);

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].name, "AirPods Pro");
    }

    #[test]
    fn an_address_blubat_cannot_parse_is_skipped_rather_than_guessed_at() {
        let fake = Fake::holding(vec![("not an address", "Somewhere", vec![79])]);

        assert_eq!(sweep(&fake, READ_AT, TIMEOUT), []);
    }

    #[test]
    fn macos_own_colon_form_addresses_read_back_as_blubats() {
        let fake = Fake::holding(vec![("BC:87:FA:18:B0:B7", "Bose QC Headphones", vec![79])]);

        assert_eq!(sweep(&fake, READ_AT, TIMEOUT)[0].address, address(PRINCE));
    }

    #[test]
    fn what_the_helper_writes_reads_back_as_the_devices_it_was_given() {
        let cache = vec![
            Cached {
                address: PRINCE.to_string(),
                name: "Bose QC Headphones".to_string(),
                connected: true,
                percentages: vec![79],
            },
            Cached {
                address: AIRPODS.to_string(),
                name: "AirPods Pro".to_string(),
                connected: false,
                percentages: Vec::new(),
            },
        ];

        assert_eq!(parse(&printed(&cache)), cache);
    }

    #[test]
    fn a_helper_that_wrote_something_else_entirely_yields_no_devices() {
        for written in [
            "",
            "\n",
            "not json at all",
            "{}",
            r#"[{"address": "bc-87-fa-18-b0-b7"}]"#,
            r#"[{"address": "bc-87-fa-18-b0-b7", "name": "Bose QC Headphones", "connected": true, "percentages": [79], "batteryPercentCase": 42}]"#,
        ] {
            assert_eq!(parse(written), Vec::new(), "{written}");
        }
    }

    /// A shell command, so a helper that hangs or fails is exercised without
    /// a Bluetooth read or the permission one needs.
    fn helper(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);

        command
    }

    #[test]
    fn a_helper_that_finishes_hands_back_everything_it_wrote() {
        let written = run(helper("printf '[]'"), Duration::from_secs(10));

        assert_eq!(written.as_deref(), Some("[]"));
    }

    #[test]
    fn a_helper_that_outlasts_the_sweeps_timeout_is_stopped_rather_than_waited_out() {
        let started = std::time::Instant::now();

        assert_eq!(run(helper("sleep 30"), Duration::from_millis(100)), None);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it gave up early"
        );
    }

    #[test]
    fn a_helper_that_exits_badly_is_disbelieved_whatever_it_printed() {
        let script = "printf '[{\"address\": \"bc-87-fa-18-b0-b7\"}]'; exit 1";

        assert_eq!(run(helper(script), Duration::from_secs(10)), None);
    }

    /// Nothing but this ties the arguments the sweep spawns to the subcommand
    /// clap derives, so renaming one without the other would be silent.
    #[test]
    fn what_the_sweep_spawns_is_a_command_this_blubat_still_answers_to() {
        use clap::Parser;

        let cli = crate::Cli::try_parse_from(["blubat"].into_iter().chain(HELPER))
            .expect("the helper's own arguments");

        assert!(matches!(
            cli.command,
            Some(crate::Command::Daemon {
                command: crate::daemon::Command::CachedLevels
            })
        ));
    }

    #[test]
    fn a_helper_that_cannot_even_be_started_is_no_reading_rather_than_a_panic() {
        let missing = Command::new("/nonexistent/blubat-not-a-command");

        assert_eq!(run(missing, Duration::from_secs(10)), None);
    }
}
