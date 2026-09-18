//! The resident loop: the same engine, banners and hooks as the dashboard,
//! with no terminal attached.
//!
//! Everything here is the dashboard minus its view. It polls on the daemon's
//! own slower tiers, hands each reading to the shared [`Effects`], and takes
//! over the one-shot watches `blubat wait` leaves behind. It keeps no history:
//! nothing resident draws a chart, so a run measured in weeks holds one engine,
//! whatever watches are outstanding, and nothing that grows with time.
//!
//! While a dashboard is open it owns the notifications, the hooks and the state
//! file, so this loop keeps polling and fires nothing, and resumes the moment
//! the dashboard's lock goes away. The one-shot watches are this loop's either
//! way: nothing else drains them, so a wait handed over is settled whether or
//! not a dashboard happens to be up.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use blubat_core::{Config, Heartbeat, Paths, Snapshot, Timestamp};

use crate::effects::Effects;
use crate::hooks::Outcome;
use crate::notify::{Desktop, Notifier};
use crate::{Failure, lock};

use super::bluetoothd::Bluetoothd;
use super::bmap::IoBluetooth;
use super::gatt::CoreBluetooth;
use super::sweep::{self, SweepRequest};
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
    dashboard: Dashboard,
}

/// The dashboard lock, re-read once a pass, and what is worth saying about it.
struct Dashboard {
    lock: PathBuf,
    /// What the last read concluded, which is what [`Effects`] defers to.
    owns: Arc<AtomicBool>,
    /// Whether the lock being unreadable has been said since it last read.
    said: bool,
}

impl Dashboard {
    fn new(lock: PathBuf) -> Self {
        Self {
            lock,
            owns: Arc::new(AtomicBool::new(false)),
            said: false,
        }
    }

    /// Re-reads the lock, answering with the one line worth logging.
    ///
    /// Doubt counts as a dashboard, which costs a banner the dashboard is
    /// posting anyway. Said once rather than every pass, because the daemon
    /// stays this way until something changes and a line a minute is a log
    /// nobody reads; said at all, because otherwise a lock that can never be
    /// opened disables the banners for good with nothing written down.
    fn asked(&mut self) -> Option<String> {
        let held = lock::held(&self.lock);
        self.owns.store(held.unwrap_or(true), Ordering::SeqCst);

        match (held, self.said) {
            (None, false) => {
                self.said = true;
                Some(format!(
                    "{}: cannot be read, so the side effects are left to the dashboard",
                    self.lock.display()
                ))
            }
            (None, true) => None,
            (Some(_), _) => {
                self.said = false;
                None
            }
        }
    }
}

/// `blubat daemon run`: poll and act until the process is stopped.
///
/// Refuses to start beside another daemon: two of them would evaluate the same
/// readings against the same state file and announce everything twice.
///
/// This process's own main thread is the only place the daemon's own
/// Bluetooth calls ever complete (see `super::bmap` for the live finding
/// behind that, which `super::gatt` inherits), so `serve` sets everything up
/// and then stays on this thread as the sweep executor for as long as the
/// daemon runs. The polling and every other side effect move onto a worker
/// thread, [`poll_loop`], started with the setup this function already did;
/// that is what keeps a wedged Bluetooth open from ever stalling a reading.
/// `poll_loop`'s `Result` is what this function's own caller sees, recovered
/// by joining the worker once the executor loop below ends, which happens the
/// moment `poll_loop` drops its sender, whether by returning or by
/// panicking.
pub fn serve(paths: &Paths) -> Result<(), Failure> {
    let Some(_running) = lock::take(&paths.daemon_lock()).map_err(Failure::Error)? else {
        return Err(Failure::Error(
            "a blubat daemon is already running".to_string(),
        ));
    };
    // Best effort: a file that predates the guide is introduced to it here,
    // and the load right after reports a real problem on its own either way.
    let _ = crate::config::annotate(paths.config_file());
    let (config, unreadable) = load(paths);
    let (effects, stale_state) = Effects::live(paths, report);
    let resident = resident(paths, effects, Box::new(Desktop));
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

    let (sweeps, requests) = mpsc::sync_channel(1);
    let (saved, landings) = mpsc::channel();

    thread::scope(|scope| {
        let worker =
            scope.spawn(move || poll_loop(resident, &config, paths, out, sweeps, landings));

        sweep::execute(&Bluetoothd, &IoBluetooth, &CoreBluetooth, requests, &saved);

        worker.join().expect("the poll loop thread panicked")
    })
}

/// The poll loop, on a worker thread of its own: every reading, the sweeps
/// it schedules, and everything a reading sets off in [`Resident`].
///
/// Only the offering of a sweep differs from what used to run inline on the
/// daemon's main thread; everything else here is unchanged, moved wholesale
/// so the main thread is free to be the sweep executor instead. A sweep
/// already offered and not yet taken means the executor is still busy with
/// an earlier one, so this loop drops the new one silently rather than
/// waiting for room, matching the one-attempt-no-retry discipline every
/// sweep failure keeps.
///
/// Every pass ends by writing the heartbeat this loop is the sole author of.
/// `landings` carries back the moment each sweep's readings actually reached
/// disk, so the two halves of the record are written by one thread and can
/// never race: liveness is this loop coming round, readiness is what the
/// executor last managed.
#[expect(
    clippy::needless_pass_by_value,
    reason = "ownership of `sweeps` is what makes returning end the executor loop, as the doc above describes"
)]
fn poll_loop(
    mut resident: Resident,
    config: &Config,
    paths: &Paths,
    mut out: impl Write,
    sweeps: mpsc::SyncSender<SweepRequest>,
    landings: mpsc::Receiver<Timestamp>,
) -> Result<(), Failure> {
    let health_file = paths.health_file();
    let mut swept_at = None;
    let mut landed = resumed(&health_file);

    for reading in blubat_core::poll(config.poll.daemon_tiers(), &paths.readings_file()) {
        if sweep::due(
            &mut swept_at,
            reading.read_at,
            config.poll.profiler_interval,
        ) {
            sweep::offer(
                &sweeps,
                SweepRequest {
                    devices: reading.devices.clone(),
                    readings_file: paths.readings_file(),
                    timeout: config.poll.profiler_timeout,
                },
            );
        }

        for line in resident.tick(&reading, config) {
            log(&mut out, &line);
        }

        landed = landings.try_iter().last().or(landed);

        if let Some(problem) = beat(&health_file, reading.read_at, landed) {
            log(&mut out, &problem);
        }
    }

    // Reaching here means every poll tier has stopped delivering, so this
    // process is loaded and monitoring nothing. Failing says so to launchd,
    // which restarts the agent only on an unsuccessful exit. Returning also
    // drops `sweeps`, which is what ends the main thread's executor loop.
    let stopped = "the poller stopped delivering readings";
    log(&mut out, stopped);

    Err(Failure::Error(stopped.to_string()))
}

/// The loop's state, wired to the files and sinks it acts through.
///
/// The one place that decides the daemon defers to `tui.lock` and drains the
/// watch directory, so `serve` and the tests exercise the same wiring rather
/// than each naming these paths for themselves.
fn resident(paths: &Paths, effects: Effects, notifier: Box<dyn Notifier>) -> Resident {
    let dashboard = Dashboard::new(paths.tui_lock());
    let owns = Arc::clone(&dashboard.owns);

    Resident {
        effects: effects.deferring_to(move || owns.load(Ordering::SeqCst)),
        watches: Watches::default(),
        notifier,
        directory: paths.watch_dir(),
        dashboard,
    }
}

impl Resident {
    /// One reading: the engine first, then the watches, and whatever both left
    /// worth writing down.
    ///
    /// The watches are settled whichever blubat owns the events. A dashboard
    /// announces nothing about them, so deferring these to one would park every
    /// handed-over wait for as long as it stays open.
    fn tick(&mut self, reading: &Snapshot, config: &Config) -> Vec<String> {
        let mut lines: Vec<String> = self.dashboard.asked().into_iter().collect();

        lines.extend(self.effects.observe(reading, config).problems);

        lines.extend(self.watches.adopt(&self.directory));
        lines.extend(self.watches.settle(
            reading,
            &config.notifications.sound,
            reading.read_at,
            self.notifier.as_ref(),
        ));

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
#[expect(
    clippy::needless_pass_by_value,
    reason = "matches the `Fn(Outcome) + Send + Sync` shape `hooks::Runner::reporting` fixes for every reporter, dashboard's included"
)]
fn report(outcome: Outcome) {
    if outcome.went_wrong() {
        eprintln!("{}", stamped(Timestamp::now(), &outcome.to_string()));
    } else {
        println!("{}", stamped(Timestamp::now(), &outcome.to_string()));
    }
}

/// Writes down what this pass amounts to: that the loop came round, and when
/// a sweep's readings last landed.
///
/// Hands back the line to log when the file could not be written. A heartbeat
/// nothing can write is the shape of the very failure this record exists to
/// expose, so it is said out loud rather than dropped the way a failed sweep
/// save is.
fn beat(path: &Path, beat_at: Timestamp, swept_at: Option<Timestamp>) -> Option<String> {
    blubat_core::save_heartbeat(path, &Heartbeat { beat_at, swept_at })
        .err()
        .map(|error| error.to_string())
}

/// The last sweep the previous run got to, read back at startup.
///
/// Readiness survives a restart on purpose: a daemon that had stopped
/// sweeping must not read as ready again merely because launchd started a
/// fresh process over it.
fn resumed(path: &Path) -> Option<Timestamp> {
    blubat_core::load_heartbeat(path)
        .beat()
        .and_then(|beat| beat.swept_at)
}

/// One log line, stamped so a log read weeks later says when.
fn stamped(now: Timestamp, message: &str) -> String {
    format!("{now} {message}")
}

fn log(out: &mut impl Write, message: &str) {
    let _ = writeln!(out, "{}", stamped(Timestamp::now(), message));
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use blubat_core::{
        Address, AdvertisedThresholds, ChargeState, Device, Engine, Levels, Source, Watch,
    };

    use crate::hooks::fake::Recorder as StartedHooks;
    use crate::notify::fake::Recorder as PostedBanners;
    use crate::scratch::Scratch;

    use super::*;

    const TRACKPAD: &str = "Paul\u{2019}s Magic Trackpad";
    const READ_AT: i64 = 1_785_643_199;

    fn reading(level: Option<u8>, second: i64) -> Snapshot {
        let read_at = Timestamp::from_unix(READ_AT + second);

        Snapshot {
            read_at,
            devices: vec![Device {
                address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
                name: TRACKPAD.to_string(),
                kind: None,
                transport: None,
                vendor_id: None,
                product_id: None,
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

    /// The loop `serve` runs, with both sinks recording rather than acting.
    ///
    /// Built by the same factory the daemon uses, so a test exercises the
    /// wiring instead of restating it.
    fn resident(scratch: &Scratch) -> (Resident, Arc<PostedBanners>, Arc<StartedHooks>) {
        let paths = scratch.paths();
        let banners = Arc::new(PostedBanners::new());
        let hooks = Arc::new(StartedHooks::new());
        let effects = Effects::new(
            &paths,
            Engine::default(),
            AdvertisedThresholds::new(),
            Box::new(Arc::clone(&banners)),
            Box::new(Arc::clone(&hooks)),
        );

        (
            super::resident(&paths, effects, Box::new(Arc::clone(&banners))),
            banners,
            hooks,
        )
    }

    fn config() -> Config {
        Config::parse("[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n").expect("parses")
    }

    /// A stand-in for the open dashboard: the same chain over the same state
    /// file, deferring to nobody, which is what the blubat holding the lock is.
    fn owner(scratch: &Scratch) -> Effects {
        Effects::new(
            &scratch.paths(),
            Engine::default(),
            AdvertisedThresholds::new(),
            Box::new(Arc::new(PostedBanners::new())),
            Box::new(Arc::new(StartedHooks::new())),
        )
    }

    /// A dashboard holding the lock, for as long as the returned value lives.
    fn dashboard(scratch: &Scratch) -> lock::Held {
        lock::take(&scratch.paths().tui_lock())
            .expect("a lock")
            .expect("nobody else holding it")
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
            !scratch.paths().state_file().exists(),
            "the dashboard owns the state file while it owns the events"
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
        fs::write(&lock, "2147483646\n").expect("a lock file left behind");

        resident.tick(&reading(Some(50), 0), &config);
        resident.tick(&reading(Some(19), 1), &config);

        assert_eq!(
            banners.posted().len(),
            1,
            "nothing is holding the lock the file was written for"
        );
    }

    /// The daemon's half of the same question the dashboard lock answers, at
    /// the loop that acts on it: a lock it cannot read must read as one a
    /// dashboard is holding, or a daemon short of descriptors announces over a
    /// dashboard that is still up.
    #[test]
    fn a_dashboard_lock_the_daemon_cannot_read_keeps_the_side_effects_off_it() {
        let scratch = Scratch::new();
        let (mut resident, banners, hooks) = resident(&scratch);
        let config = config();
        scratch.unopenable(&scratch.paths().tui_lock());

        resident.tick(&reading(Some(50), 0), &config);
        resident.tick(&reading(Some(19), 1), &config);

        assert!(banners.posted().is_empty(), "{:?}", banners.posted());
        assert!(hooks.commands().is_empty(), "{:?}", hooks.commands());
    }

    /// Going quiet is the safe default, but going quiet for good with nothing
    /// written down is how a daemon stops notifying and nobody finds out.
    #[test]
    fn a_dashboard_lock_the_daemon_cannot_read_is_said_once_rather_than_every_pass() {
        let scratch = Scratch::new();
        let (mut resident, _, _) = resident(&scratch);
        let config = config();
        scratch.unopenable(&scratch.paths().tui_lock());

        let first = resident.tick(&reading(Some(50), 0), &config);
        let second = resident.tick(&reading(Some(19), 1), &config);

        assert_eq!(
            first
                .iter()
                .filter(|line| line.contains("tui.lock"))
                .count(),
            1,
            "{first:?}"
        );
        assert!(
            second.iter().all(|line| !line.contains("tui.lock")),
            "a log line every pass is a log nobody reads: {second:?}"
        );
    }

    #[test]
    fn the_dashboard_closing_hands_the_side_effects_back() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = config();
        let mut open_elsewhere = owner(&scratch);

        let open = dashboard(&scratch);
        open_elsewhere.observe(&reading(Some(50), 0), &config);
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

    /// Nothing else drains the watch directory, so a wait handed over while a
    /// dashboard happens to be open would otherwise be parked until it closes.
    #[test]
    fn a_watch_is_settled_whether_or_not_a_dashboard_owns_the_banners() {
        let scratch = Scratch::new();
        let (mut resident, banners, _) = resident(&scratch);
        let config = Config::default();
        let directory = scratch.paths().watch_dir();
        Watch::new("trackpad", 90, None)
            .write(&directory)
            .expect("a registered watch");
        let open = dashboard(&scratch);

        let settled = resident.tick(&reading(Some(95), 0), &config);

        assert_eq!(
            settled,
            [
                "watching `trackpad` for 90%".to_string(),
                format!("{TRACKPAD} reached 95%")
            ]
        );
        assert_eq!(banners.posted().len(), 1);
        assert_eq!(
            fs::read_dir(&directory).expect("a watch directory").count(),
            0,
            "and the file is consumed rather than left behind"
        );

        drop(open);
    }

    #[test]
    fn a_pass_writes_down_that_it_came_round_and_when_its_last_sweep_landed() {
        let scratch = Scratch::new();
        let file = scratch.paths().health_file();
        let swept = Timestamp::from_unix(READ_AT - 60);

        let problem = beat(&file, Timestamp::from_unix(READ_AT), Some(swept));

        assert_eq!(problem, None);
        assert_eq!(
            blubat_core::load_heartbeat(&file),
            blubat_core::Recorded::Beat(Heartbeat {
                beat_at: Timestamp::from_unix(READ_AT),
                swept_at: Some(swept),
            })
        );
    }

    #[test]
    fn a_heartbeat_that_cannot_be_written_is_reported_rather_than_passing_silently() {
        let scratch = Scratch::new();
        let file = scratch.paths().health_file();
        fs::create_dir_all(&file).expect("a directory where the file should be");

        let problem = beat(&file, Timestamp::from_unix(READ_AT), None);

        assert!(
            problem.is_some_and(|line| line.contains("health.toml")),
            "the line has to name the file nobody could write"
        );
    }

    #[test]
    fn a_restarted_daemon_takes_up_the_previous_runs_last_sweep() {
        let scratch = Scratch::new();
        let file = scratch.paths().health_file();
        let swept = Timestamp::from_unix(READ_AT - 3600);
        beat(&file, Timestamp::from_unix(READ_AT), Some(swept));

        assert_eq!(
            resumed(&file),
            Some(swept),
            "a fresh process over a daemon that had stopped sweeping is not ready either"
        );
    }

    #[test]
    fn a_first_ever_run_has_no_sweep_to_take_up() {
        assert_eq!(resumed(&Scratch::new().paths().health_file()), None);
    }

    #[test]
    fn a_stamped_line_carries_the_moment_it_was_written() {
        assert_eq!(
            stamped(Timestamp::from_unix(READ_AT), "watching every 120s"),
            "2026-08-02T03:59:59Z watching every 120s"
        );
    }
}
