//! `blubat wait`: hold the terminal until a device reaches a level, or hand
//! the wait to a running daemon and return immediately.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use blubat_core::{Device, Snapshot, Timestamp, Watch, watch_dir};

use crate::Failure;

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
    #[arg(long, short, default_value = "60s", value_parser = duration)]
    interval: Duration,
    /// Give up after this long instead of waiting indefinitely.
    #[arg(long, short, value_parser = duration)]
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
        wait_for_level(args, blubat_core::snapshot).map(|(device, level)| {
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
    let deadline = args.timeout.map(|timeout| {
        Timestamp::from_unix(
            Timestamp::now()
                .unix()
                .saturating_add(i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX)),
        )
    });

    Watch::new(&args.device, args.until, deadline)
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
fn wait_for_level(args: &Args, read: impl Fn() -> Snapshot) -> Result<(String, u8), Failure> {
    let started = Instant::now();

    loop {
        match progress(&read(), &args.device, args.until) {
            Progress::Reached { device, level } => return Ok((device, level)),
            Progress::Below => {}
            Progress::Absent => eprintln!(
                "blubat: no connected device matching `{}` reports a battery",
                args.device
            ),
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

/// Posts a desktop banner through osascript.
///
/// Deliberately minimal: the notifier that replaces this gains a configurable
/// sound and a delivery fallback. A wait that cannot notify still exits 0.
fn notify(body: &str) {
    let script = format!(
        "display notification {} with title \"blubat\" sound name \"Glass\"",
        applescript_string(body)
    );

    let _ = Command::new("osascript").arg("-e").arg(script).status();
}

/// An AppleScript string literal, with the two characters that can escape it.
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Parses a battery percentage, rejecting anything no device can report.
fn level(text: &str) -> Result<u8, String> {
    text.trim()
        .parse::<u8>()
        .ok()
        .filter(|level| *level <= 100)
        .ok_or_else(|| format!("`{text}` is not a percentage between 0 and 100"))
}

/// Parses a duration written as bare seconds or with an `s`, `m` or `h` suffix.
fn duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (digits, per_unit) = match text.chars().last() {
        Some('s') => (&text[..text.len() - 1], 1),
        Some('m') => (&text[..text.len() - 1], 60),
        Some('h') => (&text[..text.len() - 1], 3_600),
        _ => (text, 1),
    };

    digits
        .parse::<u64>()
        .ok()
        .and_then(|count| count.checked_mul(per_unit))
        .map(Duration::from_secs)
        .ok_or_else(|| format!("`{text}` is not a duration such as `90s`, `5m` or `2h`"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU8, Ordering};

    use blubat_core::{Address, ChargeState, Levels, Source};

    use super::*;

    const TRACKPAD: &str = "Paul\u{2019}s Magic Trackpad";

    fn args(device: &str, until: u8, timeout: Option<Duration>) -> Args {
        Args {
            device: device.to_string(),
            until,
            interval: Duration::ZERO,
            timeout,
        }
    }

    fn snapshot(level: Option<u8>, connected: bool) -> Snapshot {
        Snapshot {
            read_at: Timestamp::from_unix(1_785_643_199),
            devices: vec![Device {
                address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
                name: TRACKPAD.to_string(),
                kind: None,
                transport: None,
                levels: Levels {
                    main: level,
                    ..Levels::default()
                },
                charge: ChargeState::Charging,
                source: Source::IoKit,
                connected,
                read_at: Timestamp::from_unix(1_785_643_199),
            }],
        }
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
    fn a_duration_is_seconds_unless_it_names_its_unit() {
        assert_eq!(duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(duration("2h"), Ok(Duration::from_secs(7_200)));
        assert_eq!(duration(" 0s "), Ok(Duration::ZERO));
    }

    #[test]
    fn a_duration_rejects_what_it_cannot_measure() {
        for text in [
            "",
            "s",
            "m",
            "-5s",
            "5 m",
            "5d",
            "1.5h",
            "99999999999999999999h",
        ] {
            assert!(duration(text).is_err(), "{text} should be rejected");
        }
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
    fn a_notification_body_survives_the_quotes_a_device_name_may_carry() {
        assert_eq!(applescript_string("plain"), "\"plain\"");
        assert_eq!(
            applescript_string("a \"quoted\" back\\slash"),
            "\"a \\\"quoted\\\" back\\\\slash\""
        );
    }

    #[test]
    fn no_daemon_drains_watches_yet() {
        assert!(
            !daemon_is_running(),
            "wait polls in process until the daemon ships"
        );
    }
}
