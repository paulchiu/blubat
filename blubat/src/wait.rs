//! `blubat wait`: hold the terminal until a device reaches a level, or hand
//! the wait to a running daemon and return immediately.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use blubat_core::{Config, Device, Paths, Snapshot, Watch, parse_duration};

use crate::notify::{Banner, Desktop, Notifier};
use crate::{Failure, lock, reading};

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
///
/// What ends a wait is the `--until` level and nothing else: the config's
/// thresholds are about the events blubat raises, and a wait is a question the
/// caller has already answered. The banner that ends one is the config's, since
/// nothing else decides what a blubat notification sounds like.
pub fn run(args: &Args, paths: &Paths) -> Result<(), Failure> {
    handled(args, paths, reading)
}

/// The same over whichever reader, which is the half a test drives without a
/// Bluetooth device.
fn handled(args: &Args, paths: &Paths, read: impl Fn() -> Snapshot) -> Result<(), Failure> {
    if daemon_is_running(paths) {
        register(args, &paths.watch_dir()).map(|path| {
            println!(
                "watching {} for {}%, registered as {}",
                args.device,
                args.until,
                path.display()
            );
        })
    } else {
        let sound = Config::load(paths.config_file())?.notifications.sound;

        wait_for_level(args, read).map(|(device, level)| {
            notify(&Desktop, &completed(&device, level), &sound);
            println!("{device} reached {level}%");
        })
    }
}

/// Whether a blubat daemon is running and will drain the watch directory.
///
/// The daemon holds a lock naming its process for as long as it runs, so this
/// is one file read rather than a `launchctl` call: an agent that is loaded but
/// between restarts is not one that will pick a watch up.
fn daemon_is_running(paths: &Paths) -> bool {
    lock::held(&paths.daemon_lock())
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

/// What the banner ending a wait says, whichever blubat ends it.
///
/// The daemon posts this too, so a wait it took over reads the same as one that
/// held the terminal.
pub fn completed(device: &str, level: u8) -> String {
    format!("{device} is at {level}%, safe to unplug.")
}

/// Posts the banner that ends a wait, which cannot fail the wait itself.
fn notify(notifier: &dyn Notifier, body: &str, sound: &str) {
    if let Err(problem) = notifier.post(&Banner::new("blubat", body, sound)) {
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

    use crate::scratch::Scratch;

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
            degraded: false,
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
    fn the_banner_ending_a_wait_carries_the_sound_the_config_asked_for() {
        let recorder = crate::notify::fake::Recorder::new();
        let (device, level) =
            wait_for_level(&args("trackpad", 100, None), || snapshot(Some(100), true))
                .expect("it reached the target");

        notify(&recorder, &completed(&device, level), "Ping");

        assert_eq!(recorder.posted().len(), 1);
        assert_eq!(recorder.posted()[0].title, "blubat");
        assert_eq!(
            recorder.posted()[0].body,
            format!("{TRACKPAD} is at 100%, safe to unplug.")
        );
        assert_eq!(
            recorder.posted()[0].sound.as_deref(),
            Some("Ping"),
            "the file's sound, not a hardcoded one"
        );
    }

    #[test]
    fn a_banner_that_cannot_be_delivered_does_not_fail_the_wait() {
        let recorder = crate::notify::fake::Recorder::failing("no notification centre");

        notify(&recorder, &completed(TRACKPAD, 100), "Glass");

        assert_eq!(recorder.posted().len(), 1, "it was attempted");
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
        let scratch = Scratch::new();

        let path = register(&args("trackpad", 100, None), scratch.dir()).expect("writes a watch");
        let watch = Watch::read(&path).expect("reads back");

        assert!(path.starts_with(scratch.dir()));
        assert_eq!(watch.device, "trackpad");
        assert_eq!(watch.target, 100);
        assert_eq!(watch.deadline, None);
    }

    #[test]
    fn a_timeout_becomes_the_watch_deadline() {
        let scratch = Scratch::new();
        let before = Timestamp::now().unix();

        let path = register(
            &args("trackpad", 100, Some(Duration::from_secs(600))),
            scratch.dir(),
        )
        .expect("writes a watch");

        let deadline = Watch::read(&path).expect("reads back").deadline;
        assert!(
            deadline.is_some_and(|deadline| deadline.unix() >= before + 600),
            "{deadline:?}"
        );
    }

    /// Which branch a wait takes is decided by one file, and getting that file
    /// wrong turns every wait into a no-op that prints success.
    #[test]
    fn a_running_daemon_is_handed_the_wait_and_the_terminal_comes_straight_back() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        let _daemon = lock::take(&paths.daemon_lock())
            .expect("a lock")
            .expect("nobody else holding it");

        handled(&args("trackpad", 100, None), &paths, || {
            panic!("a handed-over wait reads nothing here")
        })
        .expect("it registered and returned");

        let registered: Vec<_> = fs::read_dir(paths.watch_dir())
            .expect("a watch directory")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(registered.len(), 1);
        assert_eq!(
            Watch::read(&registered[0].path())
                .expect("reads back")
                .target,
            100
        );
    }

    #[test]
    fn a_wait_with_no_daemon_behind_it_polls_here_instead() {
        let scratch = Scratch::new();
        let paths = scratch.paths();
        fs::create_dir_all(paths.watch_dir()).expect("a state directory");
        fs::write(paths.daemon_lock(), "2147483646\n").expect("a lock file left behind");

        let failure = handled(&args("trackpad", 100, Some(Duration::ZERO)), &paths, || {
            snapshot(Some(85), true)
        })
        .expect_err("it timed out here rather than being handed over");

        assert!(failure.to_string().contains("gave up waiting"), "{failure}");
        assert_eq!(
            fs::read_dir(paths.watch_dir())
                .expect("a watch directory")
                .count(),
            0,
            "nothing was registered for a daemon that is not there"
        );
    }

    #[test]
    fn two_targets_for_one_device_register_as_two_watches() {
        let scratch = Scratch::new();

        let eighty = register(&args("trackpad", 80, None), scratch.dir()).expect("writes a watch");
        let full = register(&args("trackpad", 100, None), scratch.dir()).expect("writes a watch");

        assert_ne!(eighty, full, "the second must not overwrite the first");
        assert_eq!(Watch::read(&eighty).expect("reads back").target, 80);
        assert_eq!(Watch::read(&full).expect("reads back").target, 100);
    }
}
