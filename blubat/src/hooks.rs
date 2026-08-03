//! The hook runner: one detached child process per raised event.
//!
//! A hook is a shell command the user configured, so it can do anything,
//! including hang. Each one therefore runs on its own thread, is killed once it
//! outlives its timeout, and reports what it came to rather than being waited
//! on: nothing here can stall a poll, a keystroke or another hook.
//!
//! Those threads are detached, and the timeout is blubat's to enforce rather
//! than the shell's, so it lasts only as long as blubat does: a hook still
//! running when the dashboard is quit is reparented and runs to completion.
//! Quitting cannot be made to wait out whatever a hook is doing, and a hook
//! that has to be bounded past that has to bound itself.
//!
//! Whether a hook is due at all is the engine's decision, not this module's.
//! [`dispatch`] asks it, because recording the run is part of allowing it.

use std::fmt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use blubat_core::{ChargeState, Config, Engine, Event, Hook, Raised, Snapshot, Timestamp};

/// How often a waiting run looks to see whether its child has finished.
const TICK: Duration = Duration::from_millis(20);

/// What one hook run came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub command: String,
    pub event: Event,
    pub device: String,
    pub ran: Ran,
}

impl Outcome {
    /// Whether this is one the user needs telling about.
    pub fn went_wrong(&self) -> bool {
        !matches!(self.ran, Ran::Exited(0))
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} hook `{}` for {}: {}",
            self.event, self.command, self.device, self.ran
        )
    }
}

/// How a hook process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    Exited(i32),
    /// Ended by a signal, which carries no exit code.
    Signalled,
    /// Killed for outliving its timeout.
    TimedOut,
    /// Never started, or could not be waited for.
    Failed(String),
}

impl fmt::Display for Ran {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ran::Exited(code) => write!(f, "exit {code}"),
            Ran::Signalled => f.write_str("killed by a signal"),
            Ran::TimedOut => f.write_str("timed out and was killed"),
            Ran::Failed(problem) => write!(f, "could not run: {problem}"),
        }
    }
}

/// Somewhere a hook can be started, which a test fills with a recorder.
pub trait Hooks: Send + Sync {
    /// Starts one hook for one event and returns without waiting for it.
    fn start(&self, hook: &Hook, raised: &Raised);
}

/// Where a finished hook reports to.
type Report = Arc<dyn Fn(Outcome) + Send + Sync>;

/// The real runner: a shell child per hook, on a thread of its own.
#[derive(Clone)]
pub struct Runner {
    /// The timeout for a hook that sets none of its own.
    timeout: Duration,
    report: Report,
}

impl Runner {
    /// How long a hook that names no timeout of its own may run for.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// A runner that hands every outcome to `report`, which the dashboard uses
    /// to put a hook that went wrong on its status line rather than on top of
    /// the frame it is drawing.
    pub fn reporting(report: impl Fn(Outcome) + Send + Sync + 'static) -> Self {
        Self {
            timeout: Self::DEFAULT_TIMEOUT,
            report: Arc::new(report),
        }
    }
}

impl fmt::Debug for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runner")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Hooks for Runner {
    fn start(&self, hook: &Hook, raised: &Raised) {
        let timeout = hook.timeout.unwrap_or(self.timeout);
        let environment = environment(raised);
        let report = Arc::clone(&self.report);
        let command = hook.command.clone();
        let event = raised.event;
        let device = raised.device.clone();

        thread::spawn(move || {
            let ran = run(&command, &environment, timeout);

            report(Outcome {
                command,
                event,
                device,
                ran,
            });
        });
    }
}

/// Starts every hook configured for a raised event, subject to its debounce.
///
/// Takes the reading the event came from because a hook's own `match` selects a
/// device, which a raised event names but does not carry. An event whose device
/// that reading no longer holds runs nothing, which cannot happen for the events
/// the same reading raised.
pub fn dispatch(
    raised: &Raised,
    reading: &Snapshot,
    config: &Config,
    engine: &mut Engine,
    hooks: &dyn Hooks,
    now: Timestamp,
) {
    let Some(device) = reading
        .devices
        .iter()
        .find(|device| device.address == raised.address)
    else {
        return;
    };

    config
        .hooks_for(raised.event, device)
        .filter(|hook| engine.admits(raised, hook, now))
        .for_each(|hook| hooks.start(hook, raised));
}

/// The `BLUBAT_*` variables one hook run sees.
///
/// Every variable is always set and empty where the reading has no answer, so a
/// script can read one without testing whether it is there.
fn environment(raised: &Raised) -> [(&'static str, String); 8] {
    let percent = |level: Option<u8>| level.map(|level| level.to_string()).unwrap_or_default();

    [
        ("BLUBAT_DEVICE", raised.device.clone()),
        ("BLUBAT_DEVICE_ADDRESS", raised.address.as_str().to_string()),
        ("BLUBAT_EVENT", raised.event.to_string()),
        ("BLUBAT_LEVEL", percent(raised.level)),
        ("BLUBAT_PREVIOUS_LEVEL", percent(raised.previous)),
        ("BLUBAT_CHARGING", charging(raised.charge).to_string()),
        ("BLUBAT_SOURCE", raised.source.to_string()),
        ("BLUBAT_THRESHOLD", percent(raised.threshold)),
    ]
}

/// `BLUBAT_CHARGING` as a shell test reads it, and as the documented contract
/// names it: `1`, `0`, or `unknown` where no source can say.
fn charging(charge: ChargeState) -> &'static str {
    match charge {
        ChargeState::Charging => "1",
        ChargeState::Discharging => "0",
        ChargeState::Unknown => "unknown",
    }
}

/// Runs one command under `sh -c`, killing it if it outlives `timeout`.
///
/// Output goes nowhere: a hook that printed would land on top of whatever the
/// dashboard is drawing, and its exit code is what blubat reports on anyway.
fn run(command: &str, environment: &[(&'static str, String)], timeout: Duration) -> Ran {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .envs(environment.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_or_else(
            |error| Ran::Failed(error.to_string()),
            |child| wait_out(child, timeout),
        )
}

/// Waits for a child, killing it once its deadline has passed.
fn wait_out(mut child: Child, timeout: Duration) -> Ran {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().map_or(Ran::Signalled, Ran::Exited),
            Err(error) => return Ran::Failed(error.to_string()),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();

                return Ran::TimedOut;
            }
            Ok(None) => thread::sleep(TICK),
        }
    }
}

/// A hook sink that records what would have run, for the modules that wire the
/// real runner up.
#[cfg(test)]
pub mod fake {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use blubat_core::{Address, Event};

    use super::{Hook, Hooks, Raised};

    /// One start, in enough detail to show what travelled with the command.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Started {
        pub command: String,
        pub event: Event,
        pub device: String,
        pub address: Address,
        pub cycle: u64,
        pub timeout: Option<Duration>,
    }

    /// Records every hook it is handed instead of running it.
    #[derive(Debug, Default)]
    pub struct Recorder {
        started: Mutex<Vec<Started>>,
    }

    impl Recorder {
        pub fn new() -> Self {
            Self::default()
        }

        /// Everything started so far, in order.
        pub fn started(&self) -> Vec<Started> {
            self.started.lock().expect("an unpoisoned recorder").clone()
        }

        /// The commands alone, for a test the rest of the detail says nothing to.
        pub fn commands(&self) -> Vec<String> {
            self.started()
                .into_iter()
                .map(|started| started.command)
                .collect()
        }
    }

    impl Hooks for Recorder {
        fn start(&self, hook: &Hook, raised: &Raised) {
            self.started
                .lock()
                .expect("an unpoisoned recorder")
                .push(Started {
                    command: hook.command.clone(),
                    event: raised.event,
                    device: raised.device.clone(),
                    address: raised.address.clone(),
                    cycle: raised.cycle,
                    timeout: hook.timeout,
                });
        }
    }

    /// So a test can read what was started while the code under test owns the
    /// sink it started them through.
    impl Hooks for Arc<Recorder> {
        fn start(&self, hook: &Hook, raised: &Raised) {
            self.as_ref().start(hook, raised);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;

    use blubat_core::{Address, Device, Levels, Source};

    use crate::scratch::Scratch;

    use super::fake::{Recorder, Started};
    use super::*;

    const TRACKPAD: &str = "30-82-16-f2-24-90";
    const KEYS: &str = "de-df-38-f0-46-9b";

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn device(name: &str, raw: &str) -> Device {
        Device {
            address: address(raw),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main: Some(18),
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: Timestamp::from_unix(1_785_643_199),
        }
    }

    fn reading(devices: &[Device]) -> Snapshot {
        Snapshot {
            read_at: Timestamp::from_unix(1_785_643_199),
            devices: devices.to_vec(),
            degraded: false,
            warnings: Vec::new(),
        }
    }

    fn raised(device: &Device) -> Raised {
        Raised {
            event: Event::LowBattery,
            device: device.name.clone(),
            address: device.address.clone(),
            level: Some(18),
            previous: Some(30),
            charge: device.charge,
            source: device.source,
            threshold: Some(20),
            cycle: 0,
            at: Timestamp::from_unix(1_785_643_199),
        }
    }

    fn hook(command: &str, timeout: Option<Duration>) -> Hook {
        Hook {
            event: Event::LowBattery,
            command: command.to_string(),
            pattern: None,
            debounce: None,
            timeout,
        }
    }

    /// Runs a command through the real runner and waits for its outcome.
    fn outcome_of(command: &str, timeout: Option<Duration>) -> Outcome {
        let (sender, outcomes) = mpsc::channel();
        let runner = Runner::reporting(move |outcome| {
            let _ = sender.send(outcome);
        });

        runner.start(
            &hook(command, timeout),
            &raised(&device("Trackpad", TRACKPAD)),
        );

        outcomes
            .recv_timeout(Duration::from_secs(30))
            .expect("the hook reported")
    }

    #[test]
    fn the_environment_carries_every_documented_variable() {
        let names: Vec<&str> = environment(&raised(&device("Trackpad", TRACKPAD)))
            .iter()
            .map(|(name, _)| *name)
            .collect();

        assert_eq!(
            names,
            [
                "BLUBAT_DEVICE",
                "BLUBAT_DEVICE_ADDRESS",
                "BLUBAT_EVENT",
                "BLUBAT_LEVEL",
                "BLUBAT_PREVIOUS_LEVEL",
                "BLUBAT_CHARGING",
                "BLUBAT_SOURCE",
                "BLUBAT_THRESHOLD",
            ]
        );
    }

    #[test]
    fn what_the_reading_cannot_answer_is_set_and_empty() {
        let unknown = Raised {
            level: None,
            previous: None,
            charge: ChargeState::Unknown,
            threshold: None,
            ..raised(&device("Trackpad", TRACKPAD))
        };

        let values: Vec<String> = environment(&unknown)
            .into_iter()
            .map(|(_, value)| value)
            .collect();

        assert_eq!(values[3], "", "no level");
        assert_eq!(values[4], "", "no previous level");
        assert_eq!(values[5], "unknown", "no charge state");
        assert_eq!(values[7], "", "an event that watches no threshold");
    }

    #[test]
    fn a_charging_device_reads_as_a_shell_test_expects() {
        assert_eq!(charging(ChargeState::Charging), "1");
        assert_eq!(charging(ChargeState::Discharging), "0");
        assert_eq!(charging(ChargeState::Unknown), "unknown");
    }

    #[test]
    fn a_hook_that_succeeds_reports_exit_zero_and_nothing_is_wrong() {
        let outcome = outcome_of("true", None);

        assert_eq!(outcome.ran, Ran::Exited(0));
        assert!(!outcome.went_wrong());
        assert_eq!(outcome.event, Event::LowBattery);
        assert_eq!(outcome.command, "true");
    }

    #[test]
    fn a_hook_that_fails_reports_the_code_it_failed_with() {
        assert_eq!(outcome_of("false", None).ran, Ran::Exited(1));
        assert_eq!(outcome_of("exit 3", None).ran, Ran::Exited(3));
        assert_eq!(
            outcome_of("blubat-no-such-command", None).ran,
            Ran::Exited(127),
            "the shell reports a command it cannot find"
        );
        assert!(outcome_of("false", None).went_wrong());
    }

    #[test]
    fn a_hook_that_hangs_is_killed_at_its_own_timeout() {
        let started = Instant::now();

        let outcome = outcome_of("sleep 30", Some(Duration::from_millis(100)));

        assert_eq!(outcome.ran, Ran::TimedOut);
        assert!(outcome.went_wrong());
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "it waited it out"
        );
    }

    #[test]
    fn a_hook_sees_the_event_that_started_it_in_its_environment() {
        let scratch = Scratch::new();
        let path = scratch.join("environment");
        let command = format!(
            "printf '%s\\n' \"$BLUBAT_DEVICE\" \"$BLUBAT_DEVICE_ADDRESS\" \"$BLUBAT_EVENT\" \
             \"$BLUBAT_LEVEL\" \"$BLUBAT_PREVIOUS_LEVEL\" \"$BLUBAT_CHARGING\" \
             \"$BLUBAT_SOURCE\" \"$BLUBAT_THRESHOLD\" > {}",
            path.display()
        );

        assert_eq!(outcome_of(&command, None).ran, Ran::Exited(0));

        let written = fs::read_to_string(&path).expect("the hook wrote its environment");
        assert_eq!(
            written.lines().collect::<Vec<&str>>(),
            [
                "Trackpad",
                TRACKPAD,
                "low_battery",
                "18",
                "30",
                "0",
                "iokit",
                "20"
            ]
        );
    }

    #[test]
    fn starting_a_hook_hands_it_to_a_thread_rather_than_waiting_for_it() {
        let (sender, outcomes) = mpsc::channel();
        // Built here rather than through `reporting` so the runner's own
        // timeout is short enough for a test to wait out.
        let runner = Runner {
            timeout: Duration::from_millis(100),
            report: Arc::new(move |outcome| {
                let _ = sender.send(outcome);
            }),
        };
        let started = Instant::now();

        runner.start(
            &hook("sleep 30", None),
            &raised(&device("Trackpad", TRACKPAD)),
        );
        let returned = started.elapsed();

        assert!(
            returned < Duration::from_secs(5),
            "start blocked for {returned:?}"
        );
        assert_eq!(
            outcomes
                .recv_timeout(Duration::from_secs(30))
                .expect("the hook reported")
                .ran,
            Ran::TimedOut,
            "the runner's own timeout applies to a hook that names none"
        );
    }

    #[test]
    fn an_outcome_reads_as_the_event_log_line_it_becomes() {
        let outcome = Outcome {
            command: String::from("~/bin/nag"),
            event: Event::LowBattery,
            device: String::from("Trackpad"),
            ran: Ran::Exited(1),
        };

        assert_eq!(
            outcome.to_string(),
            "low_battery hook `~/bin/nag` for Trackpad: exit 1"
        );
        assert_eq!(
            Outcome {
                ran: Ran::TimedOut,
                ..outcome.clone()
            }
            .to_string(),
            "low_battery hook `~/bin/nag` for Trackpad: timed out and was killed"
        );
        assert!(
            Ran::Failed(String::from("no sh"))
                .to_string()
                .contains("no sh")
        );
        assert_eq!(Ran::Signalled.to_string(), "killed by a signal");
    }

    #[test]
    fn dispatch_starts_the_hooks_the_config_runs_for_that_event_and_device() {
        let config = Config::parse(
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\ntimeout = \"10s\"\n\n\
             [[hook]]\nevent = \"low_battery\"\nmatch = \"keys\"\ncommand = \"keys-only\"\n\n\
             [[hook]]\nevent = \"charged\"\ncommand = \"unplug\"\n",
        )
        .expect("parses");
        let trackpad = device("Trackpad", TRACKPAD);
        let recorder = Recorder::new();
        let mut engine = Engine::default();

        dispatch(
            &raised(&trackpad),
            &reading(&[trackpad.clone(), device("MX Keys", KEYS)]),
            &config,
            &mut engine,
            &recorder,
            Timestamp::from_unix(1_785_643_199),
        );

        assert_eq!(
            recorder.started(),
            [Started {
                command: String::from("nag"),
                event: Event::LowBattery,
                device: String::from("Trackpad"),
                address: address(TRACKPAD),
                cycle: 0,
                timeout: Some(Duration::from_secs(10)),
            }],
            "the event, the device it was raised for and the hook's own timeout \
             all travel with the command"
        );
    }

    #[test]
    fn a_hook_the_engine_debounces_is_not_started_again() {
        let config = Config::parse(
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\ndebounce = \"30m\"\n",
        )
        .expect("parses");
        let trackpad = device("Trackpad", TRACKPAD);
        let reading = reading(std::slice::from_ref(&trackpad));
        let recorder = Recorder::new();
        let mut engine = Engine::default();
        let at = |second: i64| Timestamp::from_unix(1_785_643_199 + second);

        for second in [0, 60, 1_800] {
            dispatch(
                &raised(&trackpad),
                &reading,
                &config,
                &mut engine,
                &recorder,
                at(second),
            );
        }

        assert_eq!(
            recorder.commands(),
            ["nag", "nag"],
            "the run a minute in is inside the window, the one half an hour in is not"
        );
    }

    #[test]
    fn an_event_for_a_device_the_reading_does_not_hold_starts_nothing() {
        let config = Config::parse("[[hook]]\nevent = \"low_battery\"\ncommand = \"nag\"\n")
            .expect("parses");
        let recorder = Recorder::new();
        let mut engine = Engine::default();

        dispatch(
            &raised(&device("Trackpad", TRACKPAD)),
            &reading(&[device("MX Keys", KEYS)]),
            &config,
            &mut engine,
            &recorder,
            Timestamp::from_unix(1_785_643_199),
        );

        assert!(recorder.started().is_empty());
    }
}
