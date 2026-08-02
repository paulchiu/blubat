//! `blubat wait`: hold the terminal until a device reaches a level, or hand
//! the wait to a running daemon and return immediately.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use blubat_core::{Device, Notifications, Snapshot, Watch, parse_duration, watch_dir};

use crate::notify::{Banner, Desktop, Notifier};
use crate::{Failure, reading};

/// Arguments of `blubat wait`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Substring matched against device name and address.
    #[arg(long, short)]
    device: String,
    /// The level in percent that ends the wait.
    #[arg(long, short, value_parser = level)]
    until: u8,
    /// How long to leave between readings, such as `90s`, `5m` or `2h`.
    #[arg(long, short, default_value = "60s", value_parser = parse_duration)]
    interval: Duration,
    /// Give up after this long instead of waiting indefinitely.
    #[arg(long, short, value_parser = parse_duration)]
    timeout: Option<Duration>,
}

/// Runs `blubat wait` in whichever of its two modes applies.
pub fn run(args: &Args) -> Result<(), Failure> {
    if daemon_is_running() {
        watch_dir()
            .map_err(Failure::from)
            .and_then(|directory| register(args, &directory))
            .map(|path| {
                println!(
                    "watching {} for {}%, registered as {}",
                    args.device,
                    args.until,
                    path.display()
                );
            })
    } else {
        wait_for_level(args, reading).map(|(device, level)| {
            notify(&format!("{device} is at {level}%, safe to unplug."));
            println!("{device} reached {level}%");
        })
    }
}

/// Whether a blubat daemon is running and will drain the watch directory.
///
/// The daemon ships in a later release, so nothing drains watches yet and a
/// wait always polls in process.
fn daemon_is_running() -> bool {
    false
}

/// Drops a one-shot watch file for a running daemon to pick up.
fn register(args: &Args, directory: &Path) -> Result<PathBuf, Failure> {
    Watch::new(&args.device, args.until, args.timeout)
        .write(directory)
        .map_err(Failure::from)
}

/// What one reading of the wait loop concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Progress {
    /// The named device is at or above the target.
    Reached {
        device: String,
        level: u8,
    },
    Below,
    /// Nothing matching is reporting a live level.
    Absent,
}

/// Measures one snapshot against the target.
///
/// A disconnected device has no active level, so the wait keeps running rather
/// than completing on a level macOS persisted at an unknown time.
fn progress(snapshot: &Snapshot, device: &str, until: u8) -> Progress {
    snapshot
        .matching(device)
        .find_map(named_level)
        .map_or(Progress::Absent, |(device, level)| {
            if level >= until {
                Progress::Reached { device, level }
            } else {
                Progress::Below
            }
        })
}

fn named_level(device: &Device) -> Option<(String, u8)> {
    device
        .active_level()
        .map(|level| (device.name.clone(), level))
}

/// Polls until the device reaches the target, or the timeout expires.
///
/// Takes its reader so the loop is exercised without a Bluetooth device. A
/// device that is absent for a while is reported and waited on, matching the
/// shell POC, because the usual reason to wait is that it is still connecting.
/// That notice is said once per disappearance rather than once per tick, since
/// a wait can run for hours.
fn wait_for_level(args: &Args, read: impl Fn() -> Snapshot) -> Result<(String, u8), Failure> {
    let started = Instant::now();
    let mut said_absent = false;

    loop {
        match progress(&read(), &args.device, args.until) {
            Progress::Reached { device, level } => return Ok((device, level)),
            Progress::Below => said_absent = false,
            Progress::Absent if !said_absent => {
                eprintln!(
                    "blubat: no connected device matching `{}` reports a battery",
                    args.device
                );
                said_absent = true;
            }
            Progress::Absent => {}
        }

        if args
            .timeout
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            return Err(Failure::Error(format!(
                "gave up waiting for `{}` to reach {}%",
                args.device, args.until
            )));
        }

        thread::sleep(args.interval);
    }
}

/// Posts the banner that ends a wait.
///
/// Sounds as an unconfigured blubat does: a wait is a one-shot command that
/// resolves no config of its own. A wait that cannot notify still exits 0.
fn notify(body: &str) {
    let sound = Notifications::default().sound;

    if let Err(problem) = Desktop.post(&Banner::new("blubat", body, &sound)) {
        eprintln!("blubat: {problem}");
    }
}

/// Parses a battery percentage, rejecting anything no device can report.
fn level(text: &str) -> Result<u8, String> {
    text.trim()
        .parse::<u8>()
        .ok()
        .filter(|level| *level <= 100)
        .ok_or_else(|| format!("`{text}` is not a percentage between 0 and 100"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU8, Ordering};

    use blubat_core::{Address, ChargeState, Levels, Source, Timestamp};

    use super::*;

    const TRACKPAD: &str = "Paul\u{2019}s Magic Trackpad";
    const EARBUDS: &str = "Soundcore Liberty 3 Pro";

    fn args(device: &str, until: u8, timeout: Option<Duration>) -> Args {
        Args {
            device: device.to_string(),
            until,
            interval: Duration::ZERO,
            timeout,
        }
    }

    fn one_device(name: &str, levels: Levels, connected: bool) -> Snapshot {
        Snapshot {
            read_at: Timestamp::from_unix(1_785_643_199),
            devices: vec![Device {
                address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
                name: name.to_string(),
                kind: None,
                transport: None,
                levels,
                charge: ChargeState::Charging,
                source: Source::IoKit,
                connected,
                read_at: Timestamp::from_unix(1_785_643_199),
            }],
            warnings: Vec::new(),
        }
    }

    fn snapshot(level: Option<u8>, connected: bool) -> Snapshot {
        one_device(
            TRACKPAD,
            Levels {
                main: level,
                ..Levels::default()
            },
            connected,
        )
    }

    fn earbuds(connected: bool) -> Snapshot {
        one_device(
            EARBUDS,
            Levels {
                left: Some(88),
                right: Some(91),
                case: Some(72),
                ..Levels::default()
            },
            connected,
        )
    }

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("blubat-cli-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_percentage_is_zero_to_one_hundred() {
        assert_eq!(level("0"), Ok(0));
        assert_eq!(level(" 100 "), Ok(100));
        assert!(level("101").is_err());
        assert!(level("-1").is_err());
        assert!(level("full").is_err());
        assert!(level("").is_err());
    }

    #[test]
    fn progress_completes_only_at_or_above_the_target() {
        assert_eq!(
            progress(&snapshot(Some(100), true), "trackpad", 100),
            Progress::Reached {
                device: TRACKPAD.to_string(),
                level: 100
            },
            "the real device name travels with the level, for the banner"
        );
        assert_eq!(
            progress(&snapshot(Some(85), true), "trackpad", 100),
            Progress::Below
        );
    }

    #[test]
    fn a_multi_battery_device_is_measured_by_its_emptiest_part() {
        assert_eq!(
            progress(&earbuds(true), "soundcore", 90),
            Progress::Below,
            "the case is at 72%, whatever the buds say"
        );
        assert_eq!(
            progress(&earbuds(true), "soundcore", 60),
            Progress::Reached {
                device: EARBUDS.to_string(),
                level: 72
            }
        );
        assert_eq!(progress(&earbuds(false), "soundcore", 60), Progress::Absent);
    }

    #[test]
    fn progress_ignores_a_level_no_longer_being_reported() {
        assert_eq!(
            progress(&snapshot(Some(100), false), "trackpad", 100),
            Progress::Absent,
            "a disconnected level is last seen, not a crossing"
        );
        assert_eq!(
            progress(&snapshot(None, true), "trackpad", 100),
            Progress::Absent
        );
        assert_eq!(
            progress(&snapshot(Some(100), true), "keyboard", 100),
            Progress::Absent
        );
    }

    #[test]
    fn a_wait_returns_the_level_that_ended_it() {
        let reads = AtomicU8::new(0);
        let climbing = || match reads.fetch_add(1, Ordering::SeqCst) {
            0 | 1 => snapshot(Some(98), true),
            _ => snapshot(Some(100), true),
        };

        assert_eq!(
            wait_for_level(&args("trackpad", 100, None), climbing),
            Ok((TRACKPAD.to_string(), 100))
        );
        assert_eq!(reads.load(Ordering::SeqCst), 3, "it read until it crossed");
    }

    #[test]
    fn a_wait_that_runs_out_of_time_is_an_error_exit() {
        let failure = wait_for_level(&args("trackpad", 100, Some(Duration::ZERO)), || {
            snapshot(Some(85), true)
        })
        .expect_err("timed out");

        assert_eq!(failure.code(), 1);
        assert!(failure.to_string().contains("gave up waiting"), "{failure}");
    }

    #[test]
    fn a_registered_watch_reads_back_as_the_wait_that_was_asked_for() {
        let scratch = Scratch::new("register");

        let path = register(&args("trackpad", 100, None), &scratch.0).expect("writes a watch");
        let watch = Watch::read(&path).expect("reads back");

        assert!(path.starts_with(&scratch.0));
        assert_eq!(watch.device, "trackpad");
        assert_eq!(watch.target, 100);
        assert_eq!(watch.deadline, None);
    }

    #[test]
    fn a_timeout_becomes_the_watch_deadline() {
        let scratch = Scratch::new("deadline");
        let before = Timestamp::now().unix();

        let path = register(
            &args("trackpad", 100, Some(Duration::from_secs(600))),
            &scratch.0,
        )
        .expect("writes a watch");

        let deadline = Watch::read(&path).expect("reads back").deadline;
        assert!(
            deadline.is_some_and(|deadline| deadline.unix() >= before + 600),
            "{deadline:?}"
        );
    }

    #[test]
    fn two_targets_for_one_device_register_as_two_watches() {
        let scratch = Scratch::new("targets");

        let eighty = register(&args("trackpad", 80, None), &scratch.0).expect("writes a watch");
        let full = register(&args("trackpad", 100, None), &scratch.0).expect("writes a watch");

        assert_ne!(eighty, full, "the second must not overwrite the first");
        assert_eq!(Watch::read(&eighty).expect("reads back").target, 80);
        assert_eq!(Watch::read(&full).expect("reads back").target, 100);
    }
}
