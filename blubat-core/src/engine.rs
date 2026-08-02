//! The event engine: threshold crossings, hysteresis and the state behind them.
//!
//! [`Engine::step`] is a pure function of the state, one reading, the config
//! and the clock. Nothing in it touches a device, a banner or a shell, which is
//! what makes the whole hysteresis table testable with no Bluetooth in the
//! room. The notifier and the hook runner read the events it returns and live
//! in the binary.
//!
//! Two ideas do all the work. Each device and event pair is a two state
//! machine, armed or fired: a level merely sitting past a threshold cannot
//! raise anything, because only a re-arm moves a fired pair back. Re-arming
//! takes recovery past the threshold by `rearm_margin`, so a level oscillating
//! around the boundary, which is what coarse readings do, raises one event
//! rather than forty.
//!
//! Debounce clocks live in this state rather than in the hook runner. They have
//! to survive a restart in the same file as the arm flags, and they are keyed
//! by device, event and hook command, which is finer than anything the runner
//! keeps. The decision stays out here: [`Engine::step`] never sees a hook, and
//! [`Engine::admits`] answers for one hook at dispatch time.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::atomic;
use crate::config::{Advertised, AdvertisedThresholds, Config, Hook, Thresholds};
use crate::device::{ChargeState, Device, Source};
use crate::duration::Debounce;
use crate::error::{Error, Result};
use crate::event::Event;
use crate::snapshot::Snapshot;
use crate::timestamp::Timestamp;

/// One raised event, with everything a banner or a hook needs to describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raised {
    pub event: Event,
    /// The friendly device name.
    pub device: String,
    pub address: Address,
    /// The live level, absent for a device with none to report.
    pub level: Option<u8>,
    /// The last live level before this one.
    pub previous: Option<u8>,
    pub charge: ChargeState,
    pub source: Source,
    /// The threshold that raised it, absent for the events that watch no level.
    pub threshold: Option<u8>,
    /// The re-arm cycle this firing belongs to, which is what `once` is once per.
    pub cycle: u64,
    pub at: Timestamp,
}

/// Everything blubat remembers about what it has already raised.
///
/// Serialised to `state.toml` under the state directory. It is machine state
/// rather than intent, so nothing here belongs in the config file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Engine {
    #[serde(rename = "device")]
    devices: BTreeMap<Address, DeviceState>,
}

impl Engine {
    /// How long a link change has to hold before it is announced.
    ///
    /// Bluetooth links flap, so a disconnect that a reconnect undoes within
    /// this window is coalesced away and raises nothing at all.
    pub const COALESCE_WINDOW: Duration = Duration::from_secs(30);

    /// Advances every device in `reading` and returns what that raised.
    ///
    /// Only devices that report a battery are judged: blubat is a battery
    /// monitor, and a device with no battery has nothing to say. A device the
    /// state has never seen is recorded rather than announced, so a machine
    /// starting with a flat trackpad stays quiet until it recovers and crosses
    /// back down. A device the reading omits keeps the state it had.
    pub fn step(
        mut self,
        reading: &Snapshot,
        config: &Config,
        advertised: &AdvertisedThresholds,
        now: Timestamp,
    ) -> (Self, Vec<Raised>) {
        let raised = reading
            .devices
            .iter()
            .filter(|device| device.has_battery())
            .flat_map(|device| {
                let thresholds = config.thresholds_for(
                    device,
                    advertised
                        .get(&device.address)
                        .copied()
                        .unwrap_or(Advertised::NONE),
                );

                self.judge(device, thresholds, config.poll.stale_after, now)
            })
            .collect();

        (self, raised)
    }

    /// Whether `hook` may run for `raised` now, recording the run when it may.
    ///
    /// The one impure corner of the engine, and deliberately not part of
    /// [`Engine::step`]: what happened is independent of what is configured to
    /// react to it, so the hook runner asks this per hook and the engine never
    /// has to know the hook list.
    pub fn admits(&mut self, raised: &Raised, hook: &Hook, now: Timestamp) -> bool {
        let runs = &mut self
            .devices
            .entry(raised.address.clone())
            .or_default()
            .events
            .entry(raised.event)
            .or_default()
            .hooks;
        let admitted = match (hook.debounce, runs.get(&hook.command)) {
            (Some(Debounce::Once), Some(run)) => run.cycle != raised.cycle,
            (Some(Debounce::Window(window)), Some(run)) => now >= run.at.plus(window),
            _ => true,
        };

        if admitted {
            runs.insert(
                hook.command.clone(),
                Run {
                    at: now,
                    cycle: raised.cycle,
                },
            );
        }

        admitted
    }

    /// Reads the state file, degrading anything unreadable to a fresh engine.
    ///
    /// Never fails. State blubat wrote about itself is not worth refusing to
    /// run over: a fresh engine only means the next tick records the devices it
    /// finds instead of re-firing for them. A missing file is a first run and
    /// says nothing; anything else hands back a warning to place.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).map_or_else(
                |error| Self::fresh(format!("{}: {error}", path.display())),
                |engine| (engine, None),
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => (Self::default(), None),
            Err(error) => Self::fresh(format!("{}: {error}", path.display())),
        }
    }

    /// Writes the state file atomically, creating the state directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        toml::to_string(self)
            .map_err(|error| Error::Format(format!("state file is unwritable: {error}")))
            .and_then(|contents| atomic::write(path, &contents))
    }

    fn fresh(problem: String) -> (Self, Option<String>) {
        (
            Self::default(),
            Some(format!("{problem}, starting from fresh event state")),
        )
    }

    /// Advances one device and returns whatever that step raises.
    fn judge(
        &mut self,
        device: &Device,
        thresholds: Thresholds,
        stale_after: Duration,
        now: Timestamp,
    ) -> Vec<Raised> {
        let Some(state) = self.devices.get_mut(&device.address) else {
            self.devices.insert(
                device.address.clone(),
                DeviceState::seeded(device, thresholds, stale_after, now),
            );

            return Vec::new();
        };

        let level = device.active_level();
        let previous = state.level;
        let observed = Observed {
            level,
            charge: device.charge,
            settled: state.settle(device.connected, now),
            overdue: device.is_stale(stale_after, now),
        };

        let raised = Event::ALL
            .into_iter()
            .filter_map(|event| {
                state
                    .apply(event, signal(event, observed, thresholds), now)
                    .map(|cycle| Raised {
                        event,
                        device: device.name.clone(),
                        address: device.address.clone(),
                        level,
                        previous,
                        charge: device.charge,
                        source: device.source,
                        threshold: Trigger::of(event, thresholds).map(|trigger| trigger.threshold),
                        cycle,
                        at: now,
                    })
            })
            .collect();

        // A disconnected device has no live level, and the one macOS keeps
        // reporting for it is undated, so the last live one stands.
        state.level = level.or(state.level);

        raised
    }
}

/// What blubat remembers about one device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DeviceState {
    /// The last live level seen, which a raised event reports as previous.
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<u8>,
    /// The link state blubat has announced, which a flap does not move.
    connected: bool,
    /// When the link was first seen differing from the announced state.
    #[serde(skip_serializing_if = "Option::is_none")]
    unsettled: Option<Timestamp>,
    #[serde(rename = "event")]
    events: BTreeMap<Event, Pair>,
}

impl DeviceState {
    /// A device blubat has not seen before: recorded, not announced.
    ///
    /// Everything already true of it counts as fired, which is what keeps a
    /// device that is flat, disconnected or silent at startup quiet.
    fn seeded(
        device: &Device,
        thresholds: Thresholds,
        stale_after: Duration,
        now: Timestamp,
    ) -> Self {
        let level = device.active_level();
        let observed = Observed {
            level,
            charge: device.charge,
            settled: Some(device.connected),
            overdue: device.is_stale(stale_after, now),
        };
        let fired = |event| signal(event, observed, thresholds) == Signal::Fire;

        Self {
            level,
            connected: device.connected,
            unsettled: None,
            events: Event::ALL
                .into_iter()
                .map(|event| {
                    (
                        event,
                        Pair {
                            fired: fired(event),
                            ..Pair::default()
                        },
                    )
                })
                .collect(),
        }
    }

    /// Advances the link machine, yielding the state a change settles into.
    ///
    /// A change has to hold for [`Engine::COALESCE_WINDOW`] before it is announced, so
    /// a disconnect that a reconnect undoes inside that window leaves nothing
    /// behind but the pending stamp it clears.
    fn settle(&mut self, connected: bool, now: Timestamp) -> Option<bool> {
        if connected == self.connected {
            self.unsettled = None;

            return None;
        }

        let since = *self.unsettled.get_or_insert(now);

        (now >= since.plus(Engine::COALESCE_WINDOW)).then(|| {
            self.connected = connected;
            self.unsettled = None;

            connected
        })
    }

    /// Applies one signal to one pair, yielding the cycle a firing belongs to.
    fn apply(&mut self, event: Event, signal: Signal, now: Timestamp) -> Option<u64> {
        let pair = self.events.entry(event).or_default();

        match signal {
            Signal::Fire if !pair.fired => {
                pair.fired = true;
                pair.last_fired = Some(now);

                Some(pair.cycle)
            }
            Signal::Rearm if pair.fired => {
                pair.fired = false;
                pair.cycle += 1;

                None
            }
            _ => None,
        }
    }
}

/// One device and event pair: the state machine, its clock and its hooks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Pair {
    /// False while the pair may fire, true from firing until it re-arms.
    fired: bool,
    /// Counts re-arms, so a firing can name the cycle it belongs to.
    cycle: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_fired: Option<Timestamp>,
    #[serde(rename = "hook", skip_serializing_if = "BTreeMap::is_empty")]
    hooks: BTreeMap<String, Run>,
}

/// The last run of one hook command for one pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    at: Timestamp,
    cycle: u64,
}

/// What one event's own rule says about a reading, before the arm state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    Fire,
    Rearm,
    Hold,
}

impl Signal {
    /// The signal from a condition that is simply on or off.
    fn of(active: bool) -> Self {
        if active { Signal::Fire } else { Signal::Rearm }
    }
}

/// One reading reduced to what the six event rules read of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Observed {
    level: Option<u8>,
    charge: ChargeState,
    /// The link state a change has settled into, absent while none has.
    settled: Option<bool>,
    /// Whether the reading is older than the stale window. A timeout that
    /// clears itself: the next reading inside the window re-arms the pair
    /// silently, with no recovery event to go with it.
    overdue: bool,
}

/// The threshold one battery event watches, and which side of it fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Trigger {
    threshold: u8,
    /// Whether it fires below the threshold rather than at or above it.
    below: bool,
    /// Whether a device known to be draining is barred from firing it.
    while_charging: bool,
}

impl Trigger {
    /// The trigger for a battery event, absent for one that watches no level.
    fn of(event: Event, thresholds: Thresholds) -> Option<Self> {
        match event {
            Event::LowBattery => Some(Self {
                threshold: thresholds.low,
                below: true,
                while_charging: false,
            }),
            Event::CriticalBattery => Some(Self {
                threshold: thresholds.critical,
                below: true,
                while_charging: false,
            }),
            Event::Charged => Some(Self {
                threshold: thresholds.high,
                below: false,
                while_charging: true,
            }),
            _ => None,
        }
    }

    /// Whether the level is on the firing side, in a charge state that allows it.
    ///
    /// `charged` says a charge has finished, so a device reporting that it is
    /// draining cannot raise it however high it reads: a multi battery device
    /// rises again whenever its emptiest part is put back. A device that
    /// reports no charge state at all still can, which is every `system_profiler`
    /// device and so most of them.
    fn fires(self, level: u8, charge: ChargeState) -> bool {
        let crossed = if self.below {
            level < self.threshold
        } else {
            level >= self.threshold
        };

        crossed && !(self.while_charging && charge == ChargeState::Discharging)
    }

    /// Whether the level has recovered past the threshold by the margin.
    ///
    /// Mirrored for `charged`: a low threshold of 20 with a margin of 1 re-arms
    /// at 21 or above, and a high threshold of 100 with the same margin re-arms
    /// at 98 or below.
    fn rearms(self, level: u8, margin: u8) -> bool {
        if self.below {
            level >= self.threshold.saturating_add(margin)
        } else {
            level < self.threshold.saturating_sub(margin)
        }
    }
}

/// What one event makes of a reading, whatever the pair's arm state is.
fn signal(event: Event, observed: Observed, thresholds: Thresholds) -> Signal {
    match event {
        Event::Connected => link(observed.settled, true),
        Event::Disconnected => link(observed.settled, false),
        Event::Stale => Signal::of(observed.overdue),
        _ => battery(event, observed, thresholds),
    }
}

/// A settled link fires the event it settled into and re-arms the other one.
fn link(settled: Option<bool>, connected: bool) -> Signal {
    settled.map_or(Signal::Hold, |state| Signal::of(state == connected))
}

/// Where a live level sits: past the threshold, recovered past the margin, or
/// in between, which is the band hysteresis holds.
///
/// A device with no live level holds everything. macOS keeps reporting the last
/// level of a disconnected device with no timestamp, and a number that old
/// cannot be allowed to fire or to re-arm anything.
fn battery(event: Event, observed: Observed, thresholds: Thresholds) -> Signal {
    Trigger::of(event, thresholds)
        .zip(observed.level)
        .map_or(Signal::Hold, |(trigger, level)| {
            if trigger.fires(level, observed.charge) {
                Signal::Fire
            } else if trigger.rearms(level, thresholds.rearm_margin) {
                Signal::Rearm
            } else {
                Signal::Hold
            }
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::device::Levels;

    use super::*;

    const TRACKPAD: &str = "30-82-16-f2-24-90";
    const KEYS: &str = "de-df-38-f0-46-9b";
    /// Well past any stale window a test configures, so a reading is fresh
    /// unless a test deliberately dates it.
    const START: i64 = 1_785_600_000;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-engine-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn state_file(&self) -> PathBuf {
            self.0.join("state.toml")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn at(second: i64) -> Timestamp {
        Timestamp::from_unix(START + second)
    }

    /// A connected device reading `level`, taken at `second`.
    ///
    /// Its charge state is unknown, as every `system_profiler` device's is, so
    /// the level path alone decides what a test raises.
    fn device(level: Option<u8>, second: i64) -> Device {
        Device {
            address: address(TRACKPAD),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: level,
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: Source::IoKit,
            connected: true,
            read_at: at(second),
        }
    }

    fn reading(devices: Vec<Device>, second: i64) -> Snapshot {
        Snapshot {
            read_at: at(second),
            devices,
            degraded: false,
            warnings: Vec::new(),
        }
    }

    /// Steps one connected trackpad at `level`, with the reading and the clock
    /// both at `second`.
    fn step(engine: Engine, config: &Config, level: u8, second: i64) -> (Engine, Vec<Raised>) {
        engine.step(
            &reading(vec![device(Some(level), second)], second),
            config,
            &AdvertisedThresholds::new(),
            at(second),
        )
    }

    /// The levels a device reports over successive ticks, one second apart, and
    /// every event that raises after the first tick has recorded the device.
    fn levels(config: &Config, path: &[u8]) -> Vec<Raised> {
        levels_engine(config, path).1
    }

    fn kinds(raised: &[Raised]) -> Vec<Event> {
        raised.iter().map(|event| event.event).collect()
    }

    fn config(text: &str) -> Config {
        Config::parse(text).expect("the test config parses")
    }

    #[test]
    fn crossing_down_through_low_fires_once_and_sitting_below_fires_nothing() {
        let raised = levels(&Config::default(), &[50, 30, 19, 18, 17, 5]);

        assert_eq!(
            kinds(&raised),
            [Event::LowBattery, Event::CriticalBattery],
            "one crossing each, and neither repeats while the level sits below"
        );
        assert_eq!(raised[0].level, Some(19));
        assert_eq!(raised[0].previous, Some(30));
        assert_eq!(raised[0].threshold, Some(20));
        assert_eq!(
            raised[1].level,
            Some(5),
            "critical crossed three ticks later"
        );
    }

    #[test]
    fn a_device_already_below_its_threshold_at_startup_stays_quiet() {
        let quiet = levels(&Config::default(), &[12, 11, 10]);
        let crossed = levels(&Config::default(), &[12, 11, 10, 9]);

        assert!(
            quiet.is_empty(),
            "recorded as already fired rather than announced: {quiet:?}"
        );
        assert_eq!(
            kinds(&crossed),
            [Event::CriticalBattery],
            "a threshold it started above is still a crossing when it reaches it"
        );
    }

    #[test]
    fn recovery_inside_the_margin_does_not_re_arm_and_recovery_past_it_does() {
        let inside = levels(&Config::default(), &[50, 19, 20, 19]);
        let past = levels(&Config::default(), &[50, 19, 21, 19]);

        assert_eq!(
            kinds(&inside),
            [Event::LowBattery],
            "20 is the threshold, not the threshold plus the margin"
        );
        assert_eq!(kinds(&past), [Event::LowBattery, Event::LowBattery]);
    }

    #[test]
    fn oscillating_across_the_boundary_fires_once_however_long_it_wobbles() {
        let raised = levels(
            &Config::default(),
            &[50, 19, 20, 19, 20, 19, 20, 19, 20, 19],
        );

        assert_eq!(kinds(&raised), [Event::LowBattery]);
    }

    #[test]
    fn a_wider_margin_holds_a_coarse_reporter_through_a_wider_wobble() {
        let coarse = config("[defaults]\nlow = 25\nrearm_margin = 5\n");

        let inside = levels(&coarse, &[50, 24, 29, 24]);
        let past = levels(&coarse, &[50, 24, 30, 24]);

        assert_eq!(kinds(&inside), [Event::LowBattery]);
        assert_eq!(kinds(&past), [Event::LowBattery, Event::LowBattery]);
    }

    #[test]
    fn charged_fires_at_the_devices_own_high_and_mirrors_the_margin() {
        let optimised = config("[[device]]\nmatch = \"trackpad\"\nhigh = 90\n");

        let raised = levels(&optimised, &[50, 90, 91, 89, 88, 90]);

        assert_eq!(
            kinds(&raised),
            [Event::Charged, Event::Charged],
            "89 holds inside the margin, 88 re-arms"
        );
        assert_eq!(raised[0].threshold, Some(90));
        assert_eq!(raised[0].level, Some(90));
    }

    #[test]
    fn charged_at_the_default_high_needs_a_full_battery() {
        let raised = levels(&Config::default(), &[50, 99, 100, 98, 100]);

        assert_eq!(kinds(&raised), [Event::Charged, Event::Charged]);
    }

    #[test]
    fn a_device_that_says_it_is_draining_cannot_raise_charged_however_high_it_reads() {
        let config = Config::default();
        let draining = |level, second| Device {
            charge: ChargeState::Discharging,
            ..device(Some(level), second)
        };
        let advance = |engine: Engine, level, second| {
            engine.step(
                &reading(vec![draining(level, second)], second),
                &config,
                &AdvertisedThresholds::new(),
                at(second),
            )
        };

        let (engine, _) = advance(Engine::default(), 100, 0);
        let (engine, wobbling) = [97, 100, 97, 100].into_iter().enumerate().fold(
            (engine, Vec::new()),
            |(engine, mut raised), (tick, level)| {
                let (engine, step) = advance(engine, level, 1 + tick as i64);
                raised.extend(step);

                (engine, raised)
            },
        );
        let (_, plugged) = step(engine, &config, 100, 5);

        assert!(
            wobbling.is_empty(),
            "an earbud back in its case is not a finished charge: {wobbling:?}"
        );
        assert_eq!(
            kinds(&plugged),
            [Event::Charged],
            "and the same level does raise it once the device stops draining"
        );
    }

    #[test]
    fn a_margin_of_zero_re_arms_the_moment_the_level_is_back_at_the_threshold() {
        let unhysteretic = config("[defaults]\nrearm_margin = 0\n");

        let raised = levels(&unhysteretic, &[50, 19, 20, 19, 20, 19]);

        assert_eq!(
            kinds(&raised),
            [Event::LowBattery, Event::LowBattery, Event::LowBattery],
            "a margin of zero is hysteresis switched off, which is the caller's to ask for"
        );
    }

    #[test]
    fn a_margin_wider_than_the_scale_leaves_an_event_latched_once_it_has_fired() {
        let unreachable = config("[defaults]\nlow = 100\nrearm_margin = 100\n");

        let raised = levels(&unreachable, &[100, 99, 100, 99]);

        assert_eq!(
            kinds(&raised),
            [Event::LowBattery],
            "re-arming would take 200%, so the crossing is announced once and never again"
        );
    }

    #[test]
    fn a_disconnected_device_neither_fires_nor_re_arms_on_its_last_level() {
        let config = Config::default();
        let (engine, _) = step(Engine::default(), &config, 50, 0);
        let flat = Device {
            connected: false,
            ..device(Some(5), 1)
        };

        let (engine, away) = engine.step(
            &reading(vec![flat.clone()], 1),
            &config,
            &AdvertisedThresholds::new(),
            at(1),
        );
        let (_, back) = engine.step(
            &reading(vec![device(Some(5), 2)], 2),
            &config,
            &AdvertisedThresholds::new(),
            at(2),
        );

        assert!(
            !kinds(&away).contains(&Event::LowBattery),
            "an undated last seen level cannot fire: {away:?}"
        );
        assert_eq!(
            kinds(&back),
            [Event::LowBattery, Event::CriticalBattery],
            "the same level fires the moment it is live again"
        );
    }

    #[test]
    fn a_link_that_flaps_inside_the_window_raises_nothing() {
        let config = Config::default();
        let away = Device {
            connected: false,
            ..device(Some(50), 1)
        };

        let (engine, _) = step(Engine::default(), &config, 50, 0);
        let (engine, dropped) = engine.step(
            &reading(vec![away], 1),
            &config,
            &AdvertisedThresholds::new(),
            at(1),
        );
        let (engine, restored) = step(engine, &config, 50, 5);
        let (_, settled) = step(engine, &config, 50, 90);

        assert!(dropped.is_empty(), "a change is not announced on sight");
        assert!(restored.is_empty(), "the reconnect undid it");
        assert!(settled.is_empty(), "and nothing is owed later");
    }

    #[test]
    fn a_link_change_that_holds_past_the_window_is_announced_once() {
        let config = Config::default();
        let away = |second| Device {
            connected: false,
            ..device(Some(50), second)
        };
        let step_away = |engine: Engine, second| {
            engine.step(
                &reading(vec![away(second)], second),
                &config,
                &AdvertisedThresholds::new(),
                at(second),
            )
        };

        let (engine, _) = step(Engine::default(), &config, 50, 0);
        let (engine, pending) = step_away(engine, 1);
        let (engine, announced) = step_away(engine, 31);
        let (engine, held) = step_away(engine, 61);
        let (engine, returning) = step(engine, &config, 50, 200);
        let (_, back) = step(engine, &config, 50, 240);

        assert!(pending.is_empty());
        assert_eq!(kinds(&announced), [Event::Disconnected]);
        assert_eq!(announced[0].previous, Some(50), "the last live level");
        assert!(held.is_empty(), "a disconnect is announced once");
        assert!(returning.is_empty(), "a reconnect waits out the window too");
        assert_eq!(kinds(&back), [Event::Connected]);
    }

    #[test]
    fn a_device_that_stops_reporting_goes_stale_once_and_clears_silently() {
        let config = config("[poll]\nstale_after = \"10m\"\n");
        let held = |engine: Engine, now| {
            engine.step(
                &reading(vec![device(Some(50), 0)], now),
                &config,
                &AdvertisedThresholds::new(),
                at(now),
            )
        };

        let (engine, _) = step(Engine::default(), &config, 50, 0);
        let (engine, fresh) = held(engine, 599);
        let (engine, overdue) = held(engine, 600);
        let (engine, still) = held(engine, 1_200);
        let (_, recovered) = step(engine, &config, 50, 1_300);

        assert!(fresh.is_empty());
        assert_eq!(kinds(&overdue), [Event::Stale]);
        assert!(still.is_empty(), "stale is raised once");
        assert!(
            recovered.is_empty(),
            "the next reading clears it with no recovery event: {recovered:?}"
        );
    }

    #[test]
    fn a_device_already_silent_at_startup_is_recorded_rather_than_announced() {
        let config = config("[poll]\nstale_after = \"10m\"\n");

        let (_, seeded) = Engine::default().step(
            &reading(vec![device(Some(50), 0)], 900),
            &config,
            &AdvertisedThresholds::new(),
            at(900),
        );

        assert!(seeded.is_empty(), "{seeded:?}");
    }

    #[test]
    fn a_multi_battery_device_is_judged_on_its_emptiest_part() {
        let config = Config::default();
        let earbuds = |case, second| Device {
            levels: Levels {
                main: None,
                left: Some(100),
                right: Some(100),
                case: Some(case),
            },
            ..device(None, second)
        };
        let advance = |engine: Engine, case, second| {
            engine.step(
                &reading(vec![earbuds(case, second)], second),
                &config,
                &AdvertisedThresholds::new(),
                at(second),
            )
        };

        let (engine, _) = advance(Engine::default(), 50, 0);
        let (engine, raised) = advance(engine, 19, 1);
        let (_, full) = advance(engine, 100, 2);

        assert_eq!(
            kinds(&raised),
            [Event::LowBattery],
            "a full pair of buds in a flat case is a low battery"
        );
        assert_eq!(raised[0].level, Some(19), "the lowest present sub level");
        assert_eq!(raised[0].previous, Some(50));
        assert_eq!(
            kinds(&full),
            [Event::Charged],
            "and the whole device is charged only once its case is too"
        );
    }

    #[test]
    fn a_device_with_no_battery_at_all_is_not_judged() {
        let config = Config::default();
        let speaker = Device {
            connected: false,
            ..device(None, 0)
        };

        let (engine, _) = Engine::default().step(
            &reading(vec![device(None, 0)], 0),
            &config,
            &AdvertisedThresholds::new(),
            at(0),
        );
        let (engine, raised) = engine.step(
            &reading(vec![speaker], 600),
            &config,
            &AdvertisedThresholds::new(),
            at(600),
        );

        assert!(raised.is_empty(), "{raised:?}");
        assert_eq!(engine.devices.len(), 0, "and nothing about it is kept");
    }

    #[test]
    fn a_device_the_reading_omits_keeps_the_state_it_had() {
        let config = Config::default();
        let (engine, _) = levels_engine(&config, &[50, 19]);

        let (engine, empty) = engine.step(
            &reading(Vec::new(), 5),
            &config,
            &AdvertisedThresholds::new(),
            at(5),
        );
        let (_, returned) = step(engine, &config, 19, 6);

        assert!(empty.is_empty());
        assert!(
            returned.is_empty(),
            "still fired, so still quiet: {returned:?}"
        );
    }

    #[test]
    fn what_a_device_advertises_stands_in_where_the_config_says_nothing() {
        let config = Config::default();
        let advertised = AdvertisedThresholds::from([(
            address(TRACKPAD),
            Advertised {
                low: Some(30),
                critical: Some(15),
            },
        )]);
        let advance = |engine: Engine, level, second| {
            engine.step(
                &reading(vec![device(Some(level), second)], second),
                &config,
                &advertised,
                at(second),
            )
        };

        let (engine, _) = advance(Engine::default(), 50, 0);
        let (_, raised) = advance(engine, 29, 1);

        assert_eq!(kinds(&raised), [Event::LowBattery]);
        assert_eq!(raised[0].threshold, Some(30), "Apple's number, not 20");
    }

    #[test]
    fn the_config_wins_over_what_a_device_advertises() {
        let config = config("[defaults]\nlow = 20\n");
        let advertised = AdvertisedThresholds::from([(
            address(TRACKPAD),
            Advertised {
                low: Some(30),
                critical: None,
            },
        )]);
        let advance = |engine: Engine, level, second| {
            engine.step(
                &reading(vec![device(Some(level), second)], second),
                &config,
                &advertised,
                at(second),
            )
        };

        let (engine, _) = advance(Engine::default(), 50, 0);
        let (engine, quiet) = advance(engine, 29, 1);
        let (_, raised) = advance(engine, 19, 2);

        assert!(quiet.is_empty(), "29 is above the configured 20");
        assert_eq!(raised[0].threshold, Some(20));
    }

    #[test]
    fn a_raised_event_carries_what_a_hook_environment_needs() {
        let config = Config::default();
        let (engine, _) = step(Engine::default(), &config, 50, 0);

        let (_, raised) = engine.step(
            &reading(
                vec![Device {
                    charge: ChargeState::Charging,
                    ..device(Some(9), 1)
                }],
                1,
            ),
            &config,
            &AdvertisedThresholds::new(),
            at(1),
        );

        let [low, critical] = &raised[..] else {
            panic!("expected both crossings, got {raised:?}");
        };
        assert_eq!(low.event, Event::LowBattery);
        assert_eq!(critical.event, Event::CriticalBattery);
        assert_eq!(critical.device, "Paul\u{2019}s Magic Trackpad");
        assert_eq!(critical.address, address(TRACKPAD));
        assert_eq!(critical.level, Some(9));
        assert_eq!(critical.previous, Some(50));
        assert_eq!(critical.charge, ChargeState::Charging);
        assert_eq!(critical.source, Source::IoKit);
        assert_eq!(critical.threshold, Some(10));
        assert_eq!(critical.cycle, 0);
        assert_eq!(critical.at, at(1));
    }

    #[test]
    fn each_device_is_judged_against_its_own_block() {
        let config = config("[[device]]\nmatch = \"keys\"\nlow = 40\n");
        let keys = |level, second| Device {
            address: address(KEYS),
            name: "MX Keys M Mac".to_string(),
            ..device(Some(level), second)
        };
        let advance = |engine: Engine, level, second| {
            engine.step(
                &reading(
                    vec![device(Some(level), second), keys(level, second)],
                    second,
                ),
                &config,
                &AdvertisedThresholds::new(),
                at(second),
            )
        };

        let (engine, _) = advance(Engine::default(), 50, 0);
        let (_, raised) = advance(engine, 35, 1);

        assert_eq!(kinds(&raised), [Event::LowBattery]);
        assert_eq!(
            raised[0].address,
            address(KEYS),
            "the trackpad is still fine"
        );
    }

    #[test]
    fn a_hook_with_no_debounce_runs_every_time_its_event_fires() {
        let hook = hook("low_battery", "nag", None);
        let (mut engine, raised) = fired_low();

        assert!(engine.admits(&raised, &hook, at(1)));
        assert!(engine.admits(&raised, &hook, at(2)));
    }

    #[test]
    fn a_debounce_window_holds_a_hook_off_until_it_expires() {
        let hook = hook("low_battery", "nag", Some("30m"));
        let (mut engine, raised) = fired_low();

        assert!(engine.admits(&raised, &hook, at(1)));
        assert!(!engine.admits(&raised, &hook, at(1_800)));
        assert!(engine.admits(&raised, &hook, at(1_801)));
    }

    #[test]
    fn once_means_once_per_re_arm_cycle_rather_than_once_per_run() {
        let hook = hook("low_battery", "nag", Some("once"));
        let (mut engine, raised) = fired_low();
        let next_cycle = Raised {
            cycle: raised.cycle + 1,
            ..raised.clone()
        };

        assert!(engine.admits(&raised, &hook, at(1)));
        assert!(!engine.admits(&raised, &hook, at(9_000)));
        assert!(
            engine.admits(&next_cycle, &hook, at(9_001)),
            "a genuine second discharge is a second alert"
        );
    }

    #[test]
    fn one_hooks_debounce_says_nothing_about_another_hooks() {
        let nag = hook("low_battery", "nag", Some("30m"));
        let log = hook("low_battery", "log", Some("30m"));
        let (mut engine, raised) = fired_low();

        assert!(engine.admits(&raised, &nag, at(1)));
        assert!(engine.admits(&raised, &log, at(1)));
        assert!(!engine.admits(&raised, &nag, at(2)));
    }

    #[test]
    fn a_debounce_is_per_device_so_one_device_does_not_silence_another() {
        let hook = hook("low_battery", "nag", Some("30m"));
        let (mut engine, raised) = fired_low();
        let other = Raised {
            address: address(KEYS),
            device: "MX Keys M Mac".to_string(),
            ..raised.clone()
        };

        assert!(engine.admits(&raised, &hook, at(1)));
        assert!(engine.admits(&other, &hook, at(1)));
    }

    #[test]
    fn state_round_trips_mid_cycle_so_a_restart_does_not_re_fire() {
        let scratch = Scratch::new();
        let config = Config::default();
        let hook = hook("low_battery", "nag", Some("30m"));
        let (mut engine, raised) = fired_low();
        assert!(engine.admits(&raised, &hook, at(1)), "the hook ran once");

        engine.save(&scratch.state_file()).expect("writes");
        let (mut restored, warning) = Engine::load(&scratch.state_file());

        assert_eq!(warning, None);
        assert_eq!(restored, engine, "every flag and clock survived");
        assert_eq!(
            restored.devices[&address(TRACKPAD)].events[&Event::LowBattery].last_fired,
            Some(at(1)),
            "the moment it fired is part of that state"
        );

        let (stepped, quiet) = step(restored.clone(), &config, 19, 2);
        let (_, again) = levels_after(stepped, &config, &[21, 19], 3);

        assert!(quiet.is_empty(), "still fired, so still quiet: {quiet:?}");
        assert_eq!(
            kinds(&again),
            [Event::LowBattery],
            "and it fires again once it has genuinely recovered and crossed"
        );
        assert!(
            !restored.admits(&raised, &hook, at(2)),
            "the debounce clock survived the restart too"
        );
    }

    #[test]
    fn the_state_file_is_readable_toml_naming_what_it_holds() {
        let (engine, _) = levels_engine(&Config::default(), &[50, 19]);

        let text = toml::to_string(&engine).expect("serialisable");

        assert!(text.contains("[device.30-82-16-f2-24-90]"), "{text}");
        assert!(
            text.contains("[device.30-82-16-f2-24-90.event.low_battery]"),
            "{text}"
        );
        assert!(text.contains("fired = true"), "{text}");
    }

    #[test]
    fn a_missing_state_file_is_a_first_run_rather_than_a_problem() {
        let scratch = Scratch::new();

        let (engine, warning) = Engine::load(&scratch.state_file());

        assert_eq!(engine, Engine::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn a_corrupt_state_file_degrades_to_a_fresh_engine_with_a_warning() {
        let scratch = Scratch::new();

        for contents in [
            "not toml at all {{",
            "[device.\"nonsense\"]\nconnected = true\n",
            "[device.30-82-16-f2-24-90]\nconnected = \"yes\"\n",
            "[device.30-82-16-f2-24-90]\nhaunted = true\n",
            "[device.30-82-16-f2-24-90.event.exploded]\nfired = true\n",
        ] {
            fs::create_dir_all(&scratch.0).expect("a scratch directory");
            fs::write(scratch.state_file(), contents).expect("a written state file");

            let (engine, warning) = Engine::load(&scratch.state_file());

            assert_eq!(engine, Engine::default(), "{contents:?}");
            assert!(
                warning.is_some_and(|warning| warning.contains("fresh event state")),
                "{contents:?} should warn"
            );
        }
    }

    /// Runs `levels` and hands back the engine as well as what it raised.
    fn levels_engine(config: &Config, path: &[u8]) -> (Engine, Vec<Raised>) {
        let (engine, seeded) = step(Engine::default(), config, path[0], 0);
        assert!(seeded.is_empty(), "the first sighting announces nothing");

        levels_after(engine, config, &path[1..], 1)
    }

    /// Steps an engine that has already seen the device through more levels.
    fn levels_after(
        engine: Engine,
        config: &Config,
        path: &[u8],
        first: i64,
    ) -> (Engine, Vec<Raised>) {
        path.iter().enumerate().fold(
            (engine, Vec::new()),
            |(engine, mut raised), (tick, level)| {
                let (engine, step) = step(engine, config, *level, first + tick as i64);
                raised.extend(step);

                (engine, raised)
            },
        )
    }

    /// An engine that has just raised `low_battery`, and the event it raised.
    fn fired_low() -> (Engine, Raised) {
        let (engine, raised) = levels_engine(&Config::default(), &[50, 19]);
        let [low] = &raised[..] else {
            panic!("expected one crossing, got {raised:?}");
        };

        (engine, low.clone())
    }

    fn hook(event: &str, command: &str, debounce: Option<&str>) -> Hook {
        let debounce = debounce
            .map(|written| format!("debounce = \"{written}\"\n"))
            .unwrap_or_default();

        config(&format!(
            "[[hook]]\nevent = \"{event}\"\ncommand = \"{command}\"\n{debounce}"
        ))
        .hooks
        .remove(0)
    }
}
