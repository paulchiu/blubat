//! The resident loop: the same engine, banners and hooks as the dashboard,
//! with no terminal attached.
//!
//! Everything here is the dashboard minus its view. It polls on the daemon's
//! own slower tiers, hands each reading to the shared [`Effects`], and takes
//! over the one-shot watches `blubat wait` leaves behind. It keeps no history:
//! nothing resident draws a chart, so a run measured in weeks holds one engine,
//! whatever watches are outstanding, and nothing that grows with time.
//!
//! While a dashboard is open it owns the notifications and the hooks, so this
//! loop keeps polling and keeps writing the state file but fires nothing, and
//! resumes the moment the dashboard's lock goes away.

use std::io::{self, Write};
use std::path::PathBuf;

use blubat_core::{Config, Paths, Snapshot, Timestamp};

use crate::effects::Effects;
use crate::hooks::Outcome;
use crate::notify::{Desktop, Notifier};
use crate::{Failure, lock};

use super::watches::Watches;

/// Everything the loop keeps between readings.
struct Resident {
    effects: Effects,
    watches: Watches,
    /// The banners a met watch posts. Its own rather than the engine's, because
    /// a watch is an errand this daemon was handed rather than an event the
    /// engine raised.
    notifier: Box<dyn Notifier>,
    /// Where `blubat wait` leaves the watches it hands over.
    directory: PathBuf,
    /// The dashboard's lock: while a live process holds it, this loop records
    /// what it sees and acts on none of it.
    dashboard: PathBuf,
}

/// `blubat daemon run`: poll and act until the process is stopped.
///
/// Refuses to start beside another daemon: two of them would evaluate the same
/// readings against the same state file and announce everything twice.
pub fn serve(paths: &Paths) -> Result<(), Failure> {
    let lock = paths.daemon_lock();

    if lock::held(&lock) {
        return Err(Failure::Error(
            "a blubat daemon is already running".to_string(),
        ));
    }

    let _running = lock::take(lock).map_err(Failure::Error)?;
    let (config, unreadable) = load(paths);
    let (effects, stale_state) = Effects::live(paths, report);
    let dashboard = paths.tui_lock();
    let deferring = dashboard.clone();
    let mut resident = Resident {
        effects: effects.deferring_to(move || lock::held(&deferring)),
        watches: Watches::default(),
        notifier: Box::new(Desktop),
        directory: paths.watch_dir(),
        dashboard,
    };
    let mut out = io::stdout();

    for problem in [unreadable, stale_state].into_iter().flatten() {
        log(&mut out, &problem);
    }
    log(
        &mut out,
        &format!(
            "watching every {:?}, logging to {}",
            config.poll.daemon_interval,
            paths.log_file().display()
        ),
    );

    for reading in blubat_core::poll(config.poll.daemon_tiers()) {
        for line in resident.tick(&reading, &config) {
            log(&mut out, &line);
        }
    }

    Ok(())
}

impl Resident {
    /// One reading: the engine first, then the watches, and whatever both left
    /// worth writing down.
    fn tick(&mut self, reading: &Snapshot, config: &Config) -> Vec<String> {
        let mut lines = self.effects.observe(reading, config).problems;

        // Watches wait for the dashboard to close rather than being consumed
        // while it owns the banners, which is what keeps each one exactly one
        // notification whichever blubat is up when its level arrives.
        if !lock::held(&self.dashboard) {
            lines.extend(self.watches.adopt(&self.directory));
            lines.extend(self.watches.settle(
                reading,
                &config.notifications.sound,
                reading.read_at,
                self.notifier.as_ref(),
            ));
        }

        lines
    }
}

/// The config in force at startup, with whatever was wrong with the file.
///
/// A file blubat cannot read is logged rather than fatal, for the reason the
/// dashboard has: a monitor that quits over a typo in a threshold stops
/// monitoring, and launchd would then restart it into the same file.
fn load(paths: &Paths) -> (Config, Option<String>) {
    Config::load(paths.config_file()).map_or_else(
        |error| (Config::default(), Some(error.to_string())),
        |config| (config, None),
    )
}

/// Where a finished hook reports to, which is the log this process writes.
///
/// A hook that went wrong goes to stderr and everything else to stdout, so the
/// error log is a short list of things to look at rather than a second copy of
/// the ordinary one.
fn report(outcome: Outcome) {
    if outcome.went_wrong() {
        eprintln!("{}", stamped(&outcome.to_string()));
    } else {
        println!("{}", stamped(&outcome.to_string()));
    }
}

/// One log line, stamped so a log read weeks later says when.
fn stamped(message: &str) -> String {
    format!("{} {message}", Timestamp::now())
}

fn log(out: &mut impl Write, message: &str) {
    let _ = writeln!(out, "{}", stamped(message));
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use blubat_core::{
        Address, AdvertisedThresholds, ChargeState, Device, Engine, Levels, Source, Watch,
    };

    use crate::hooks::fake::Recorder as StartedHooks;
    use crate::notify::fake::Recorder as PostedBanners;

    use super::*;

    const TRACKPAD: &str = "Paul\u{2019}s Magic Trackpad";
    const READ_AT: i64 = 1_785_643_199;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself, so no test reaches a real state file.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-daemon-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn paths(&self) -> Paths {
            Paths::rooted(&self.0)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reading(level: Option<u8>, second: i64) -> Snapshot {
        let read_at = Timestamp::from_unix(READ_AT + second);

        Snapshot {
            read_at,
            devices: vec![Device {
                address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
                name: TRACKPAD.to_string(),
                kind: None,
                transport: None,
                levels: Levels {
                    main: level,
                    ..Levels::default()
                },
                charge: ChargeState::Discharging,
                source: Source::IoKit,
                connected: true,
                read_at,
            }],
            degraded: false,
            warnings: Vec::new(),
        }
    }

    /// The loop's state with both sinks recording, over a scratch state file.
    fn resident(scratch: &Scratch) -> (Resident, Arc<PostedBanners>, Arc<StartedHooks>) {
        let paths = scratch.paths();
        let banners = Arc::new(PostedBanners::new());
        let hooks = Arc::new(StartedHooks::new());
        let dashboard = paths.tui_lock();
        let deferring = dashboard.clone();
        let effects = Effects::new(
            &paths,
            Engine::default(),
            AdvertisedThresholds::new(),
            Box::new(Arc::clone(&banners)),
            Box::new(Arc::clone(&hooks)),
        )
        .deferring_to(move || lock::held(&deferring));

        (
            Resident {
                effects,
                watches: Watches::default(),
                notifier: Box::new(Arc::clone(&banners)),
                directory: paths.watch_dir(),
                dashboard,
            },
            banners,
            hooks,
        )
    }

    fn config() -> Config {
        Config::parse("[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n").expect("parses")
    }

    /// A dashboard holding the lock, for as long as the returned value lives.
    fn dashboard(scratch: &Scratch) -> lock::Held {
        lock::take(scratch.paths().tui_lock()).expect("a lock")
    }

    #[test]
    fn a_crossing_reaches_the_banner_and_the_hook_with_no_terminal_attached() {
        let scratch = Scratch::new();
        let (mut resident, banners, hooks) = resident(&scratch);
        let config = config();

        resident.tick(&reading(Some(50), 0), &config);
        resident.tick(&reading(Some(19), 1), &config);

        assert_eq!(banners.posted().len(), 1);
        assert_eq!(banners.posted()[0].body, "Battery low at 19%");
        assert_eq!(hooks.commands(), ["nag"]);
    }

    #[test]
    fn a_dashboard_holding_the_lock_takes_the_banners_and_the_hooks() {
        let scratch = Scratch::new();
        let (mut resident, banners, hooks) = resident(&scratch);
        let config = config();
        let open = dashboard(&scratch);

        resident.tick(&reading(Some(50), 0), &config);
        resident.tick(&reading(Some(19), 1), &config);

        assert!(banners.posted().is_empty(), "{:?}", banners.posted());
        assert!(hooks.commands().is_empty(), "{:?}", hooks.commands());
        assert!(
            scratch.paths().state_file().exists(),
            "polling and the state file carry on regardless"
        );

        drop(open);
    }

    #[test]
    fn a_lock_left_by_a_dashboard_that_was_killed_does_not_silence_the_daemon() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = config();
        let lock = scratch.paths().tui_lock();
        fs::create_dir_all(lock.parent().expect("a parent")).expect("a state directory");
        fs::write(&lock, "2147483646\n").expect("a lock left by a process that has gone");

        resident.tick(&reading(Some(50), 0), &config);
        resident.tick(&reading(Some(19), 1), &config);

        assert_eq!(
            banners.posted().len(),
            1,
            "the pid in the lock names no process"
        );
    }

    #[test]
    fn the_dashboard_closing_hands_the_side_effects_back() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = config();

        let open = dashboard(&scratch);
        resident.tick(&reading(Some(50), 0), &config);
        drop(open);
        resident.tick(&reading(Some(19), 1), &config);

        assert_eq!(banners.posted().len(), 1);
        assert_eq!(banners.posted()[0].body, "Battery low at 19%");
    }

    #[test]
    fn a_watch_handed_over_is_taken_up_and_notified_on_the_poll_it_is_met() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = Config::default();
        Watch::new("trackpad", 90, None)
            .write(&scratch.paths().watch_dir())
            .expect("a registered watch");

        let adopted = resident.tick(&reading(Some(50), 0), &config);
        let met = resident.tick(&reading(Some(95), 1), &config);

        assert_eq!(adopted, ["watching `trackpad` for 90%"]);
        assert_eq!(met, [format!("{TRACKPAD} reached 95%")]);
        assert_eq!(banners.posted().len(), 1);
        assert_eq!(
            banners.posted()[0].body,
            format!("{TRACKPAD} is at 95%, safe to unplug.")
        );
    }

    #[test]
    fn a_watch_is_left_on_disk_while_a_dashboard_owns_the_banners() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = Config::default();
        let directory = scratch.paths().watch_dir();
        Watch::new("trackpad", 90, None)
            .write(&directory)
            .expect("a registered watch");
        let open = dashboard(&scratch);

        resident.tick(&reading(Some(95), 0), &config);

        assert!(banners.posted().is_empty());
        assert_eq!(
            fs::read_dir(&directory).expect("a watch directory").count(),
            1,
            "the file waits for the dashboard to close"
        );

        drop(open);
        let met = resident.tick(&reading(Some(95), 1), &config);

        assert_eq!(met.len(), 2, "taken over and met on the poll after it");
        assert_eq!(banners.posted().len(), 1);
    }

    #[test]
    fn a_stamped_line_carries_the_moment_it_was_written() {
        let stamped = stamped("watching every 120s");

        assert!(stamped.starts_with("20"), "{stamped}");
        assert!(stamped.ends_with(" watching every 120s"), "{stamped}");
    }
}
