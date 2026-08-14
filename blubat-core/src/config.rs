//! The TOML file at `~/.config/blubat/config.toml`.
//!
//! The file is user intent: blubat maintains `[dashboard] hidden` and
//! `[dashboard] hide_inactive` from the dashboard's own `h` and `i`, and
//! introduces a file that predates the self-documenting template to it once,
//! but everything else in the file stays exactly as the user left it. It is
//! also optional: everything here has a built-in default, so a machine with
//! no config file behaves exactly as one whose file repeats the defaults
//! back. What blubat knows about itself, the armed and fired flags and the
//! debounce clocks, is machine state and lives under the state directory
//! instead.
//!
//! Parsing is strict in the other direction: an unknown key, an unknown event
//! name or a duration that does not parse is an error carrying the line it is
//! on, because a typo that silently does nothing is worse than one that says so.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::address::Address;
use crate::device::Device;
use crate::duration::{Debounce, de_duration, de_optional_duration};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::poll::Tiers;
use crate::theme::Theme;

/// Everything the config file can say.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub poll: Poll,
    pub notifications: Notifications,
    pub defaults: Defaults,
    pub theme: Theme,
    pub dashboard: Dashboard,
    /// Per device overrides, in file order: the first block a device matches wins.
    #[serde(rename = "device")]
    pub devices: Vec<DeviceRule>,
    #[serde(rename = "hook")]
    pub hooks: Vec<Hook>,
}

impl Config {
    /// Parses config text, rejecting unknown keys and unparseable values.
    pub fn parse(contents: &str) -> Result<Self> {
        toml::from_str(contents).map_err(|error| Error::Format(error.to_string()))
    }

    /// Reads the config file. `Ok(None)` when there is none, which is not an error.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(contents) => Self::parse(&contents)
                .map(Some)
                .map_err(|error| Error::Format(format!("{}: {error}", path.display()))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// The config in force: the file's, or the built-in defaults without one.
    pub fn load(path: &Path) -> Result<Self> {
        Self::read(path).map(Option::unwrap_or_default)
    }

    /// The thresholds one device is judged by.
    ///
    /// Most specific first: the first `[[device]]` block the device matches,
    /// then `[defaults]`, then what the device itself advertises, then the
    /// built-in numbers. `advertised` is per key, so a device that publishes a
    /// low threshold but no critical one still takes the built-in critical.
    pub fn thresholds_for(&self, device: &Device, advertised: Advertised) -> Thresholds {
        self.resolve(self.rule_for(device), advertised)
    }

    /// The hooks that run for one event on one device, in file order.
    pub fn hooks_for<'a>(
        &'a self,
        event: Event,
        device: &'a Device,
    ) -> impl Iterator<Item = &'a Hook> {
        self.hooks
            .iter()
            .filter(move |hook| hook.event == event && hook.covers(device))
    }

    /// The `[[device]]` blocks that match nothing in a reading.
    ///
    /// A warning rather than an error: the device may simply be switched off.
    pub fn unmatched(&self, devices: &[Device]) -> Vec<&str> {
        self.devices
            .iter()
            .map(|rule| rule.pattern.as_str())
            .filter(|pattern| !devices.iter().any(|device| device.matches(pattern)))
            .collect()
    }

    /// What is wrong with the file that parsing alone cannot catch.
    ///
    /// Empty for a usable config, which is what `blubat config validate` exits
    /// on. Each entry names the table it came from, since a threshold is only
    /// nonsense in the company of the ones it is ordered against.
    pub fn problems(&self) -> Vec<String> {
        std::iter::once((
            String::from("[defaults]"),
            self.resolve(None, Advertised::NONE),
        ))
        .chain(self.devices.iter().map(|rule| {
            (
                format!("[[device]] match = \"{}\"", rule.pattern),
                self.resolve(Some(rule), Advertised::NONE),
            )
        }))
        .flat_map(|(table, thresholds)| {
            thresholds
                .problems()
                .into_iter()
                .map(move |problem| format!("{table}: {problem}"))
        })
        .chain(
            self.hooks
                .iter()
                .filter(|hook| hook.command.trim().is_empty())
                .map(|hook| format!("[[hook]] event = \"{}\": command is empty", hook.event)),
        )
        .collect()
    }

    /// The first block a device matches, which is the one that overrides.
    fn rule_for(&self, device: &Device) -> Option<&DeviceRule> {
        self.devices
            .iter()
            .find(|rule| device.matches(&rule.pattern))
    }

    fn resolve(&self, rule: Option<&DeviceRule>, advertised: Advertised) -> Thresholds {
        let built_in = Thresholds::BUILT_IN;

        Thresholds {
            low: rule
                .and_then(|rule| rule.low)
                .or(self.defaults.low)
                .or(advertised.low)
                .unwrap_or(built_in.low),
            critical: rule
                .and_then(|rule| rule.critical)
                .or(self.defaults.critical)
                .or(advertised.critical)
                .unwrap_or(built_in.critical),
            high: rule
                .and_then(|rule| rule.high)
                .or(self.defaults.high)
                .unwrap_or(built_in.high),
            rearm_margin: rule
                .and_then(|rule| rule.rearm_margin)
                .or(self.defaults.rearm_margin)
                .unwrap_or(built_in.rearm_margin),
        }
    }
}

/// How often each tier reads, and how long a silence lasts before it is stale.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Poll {
    /// The tick while the dashboard or a foreground command runs.
    #[serde(deserialize_with = "de_duration")]
    pub foreground_interval: Duration,
    /// The tick under launchd, slower because nothing is watching.
    #[serde(deserialize_with = "de_duration")]
    pub daemon_interval: Duration,
    /// The slow tier's own interval, cached in between.
    #[serde(deserialize_with = "de_duration")]
    pub profiler_interval: Duration,
    /// How long `system_profiler` may take before blubat gives up on the call.
    ///
    /// Generous on purpose: the call costs about 150ms here and scales with how
    /// many devices have ever been paired, so this is a ceiling on a wedged
    /// call rather than a budget for a slow one.
    #[serde(deserialize_with = "de_duration")]
    pub profiler_timeout: Duration,
    /// A device silent for this long is stale.
    #[serde(deserialize_with = "de_duration")]
    pub stale_after: Duration,
}

impl Default for Poll {
    fn default() -> Self {
        Self {
            foreground_interval: Duration::from_secs(30),
            daemon_interval: Duration::from_secs(120),
            profiler_interval: Duration::from_secs(300),
            profiler_timeout: Tiers::default().timeout,
            stale_after: Duration::from_secs(600),
        }
    }
}

impl Poll {
    /// The two tier intervals for the daemon.
    ///
    /// The dashboard builds its own in `tui`, since it polls faster than any
    /// other caller while nothing in the file has asked otherwise.
    pub fn daemon_tiers(&self) -> Tiers {
        Tiers {
            fast: self.daemon_interval,
            slow: self.profiler_interval,
            timeout: self.profiler_timeout,
        }
    }
}

/// Which events raise a desktop banner, and what it sounds like.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    pub low: bool,
    pub critical: bool,
    /// The "safe to unplug" banner.
    pub charged: bool,
    /// Covers connect and disconnect together, which flap as a pair.
    pub connect: bool,
    pub stale: bool,
    /// A macOS sound name, as `osascript` names them.
    pub sound: String,
}

impl Default for Notifications {
    /// Battery events on, link events off: connect and disconnect are noisy.
    fn default() -> Self {
        Self {
            low: true,
            critical: true,
            charged: true,
            connect: false,
            stale: true,
            sound: "Glass".to_string(),
        }
    }
}

impl Notifications {
    /// Whether this event is one the user wants a banner for.
    pub fn enabled(&self, event: Event) -> bool {
        match event {
            Event::LowBattery => self.low,
            Event::CriticalBattery => self.critical,
            Event::Charged => self.charged,
            Event::Connected | Event::Disconnected => self.connect,
            Event::Stale => self.stale,
        }
    }
}

/// The `[defaults]` table: thresholds for every device that has no block.
///
/// Every key is optional so an unset one can fall through to what the device
/// advertises, which a filled-in default would silently shadow.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub low: Option<u8>,
    pub critical: Option<u8>,
    pub high: Option<u8>,
    pub rearm_margin: Option<u8>,
}

/// One `[[device]]` block: a match and the keys it overrides.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceRule {
    /// Case insensitive substring, tested against the name and the address.
    #[serde(rename = "match")]
    pub pattern: String,
    pub low: Option<u8>,
    pub critical: Option<u8>,
    pub high: Option<u8>,
    pub rearm_margin: Option<u8>,
}

/// One `[[hook]]` block: a command, the event that runs it, and its limits.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub event: Event,
    /// A shell command line, run with the `BLUBAT_*` variables set.
    pub command: String,
    /// Optional device filter, matched as `--device` is.
    #[serde(rename = "match")]
    pub pattern: Option<String>,
    pub debounce: Option<Debounce>,
    #[serde(default, deserialize_with = "de_optional_duration")]
    pub timeout: Option<Duration>,
}

impl Hook {
    /// Whether this hook covers a device, which an unfiltered hook always does.
    pub fn covers(&self, device: &Device) -> bool {
        self.pattern
            .as_deref()
            .is_none_or(|pattern| device.matches(pattern))
    }
}

/// What the dashboard hides and how it sorts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Dashboard {
    /// Matches for devices the dashboard leaves out, which `h` maintains.
    pub hidden: Vec<String>,
    pub sort: Sort,
    /// Whether the dashboard opens with the disconnected section already
    /// left off. `i` maintains this the same way `h` maintains `hidden`: the
    /// only two fields blubat ever writes back into the file.
    pub hide_inactive: bool,
    /// A device silent for this long counts as inactive even while macOS
    /// still reports it connected.
    #[serde(deserialize_with = "de_duration")]
    pub inactive_after: Duration,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            hidden: Vec::new(),
            sort: Sort::default(),
            hide_inactive: false,
            inactive_after: Duration::from_secs(3_600),
        }
    }
}

/// The order the dashboard lists devices in.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Level,
    Name,
    LastSeen,
}

/// The thresholds a device's own IOKit node publishes, where it has them.
///
/// Apple's numbers sit between the config file and the built-in defaults: they
/// describe the device rather than the user, so anything written wins over them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Advertised {
    pub low: Option<u8>,
    pub critical: Option<u8>,
}

impl Advertised {
    /// A device that publishes no thresholds of its own.
    pub const NONE: Self = Self {
        low: None,
        critical: None,
    };
}

/// What every device advertises about itself, keyed by address.
///
/// Reading the registry is a separate pass from a poll, so the event engine
/// takes one of these as an input and a test hands one in without a registry.
pub type AdvertisedThresholds = BTreeMap<Address, Advertised>;

/// The thresholds in force for one device, with every key answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub low: u8,
    pub critical: u8,
    /// The level that raises `charged`.
    pub high: u8,
    /// Points of recovery required before a fired event re-arms.
    pub rearm_margin: u8,
}

impl Thresholds {
    /// What blubat uses when neither the config nor the device says otherwise.
    pub const BUILT_IN: Self = Self {
        low: 20,
        critical: 10,
        high: 100,
        rearm_margin: 1,
    };

    /// The orderings and the ranges a usable set of thresholds has to hold.
    ///
    /// A threshold above 100 parses but can never be crossed, since no battery
    /// reports more, so it silences its event rather than configuring it.
    fn problems(self) -> Vec<String> {
        [
            (self.low >= self.high)
                .then(|| format!("low ({}) must be below high ({})", self.low, self.high)),
            (self.critical > self.low).then(|| {
                format!(
                    "critical ({}) must not be above low ({})",
                    self.critical, self.low
                )
            }),
        ]
        .into_iter()
        .flatten()
        .chain(
            [
                ("low", self.low),
                ("critical", self.critical),
                ("high", self.high),
                ("rearm_margin", self.rearm_margin),
            ]
            .into_iter()
            .filter(|(_, value)| *value > 100)
            .map(|(key, value)| format!("{key} ({value}) must be a percentage of 100 or less")),
        )
        .collect()
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self::BUILT_IN
    }
}

#[cfg(test)]
mod tests {
    use crate::address::Address;
    use crate::device::{ChargeState, Levels, Source};
    use crate::timestamp::Timestamp;

    use super::*;

    /// The sample configuration the PRD documents, which is the shape blubat
    /// promises to read.
    const SAMPLE: &str = r##"
[poll]
foreground_interval = "30s"
daemon_interval     = "120s"
profiler_interval   = "5m"
profiler_timeout    = "15s"
stale_after         = "10m"

[notifications]
low      = true
critical = true
charged  = true
connect  = false
sound    = "Glass"

[defaults]
low        = 20
critical   = 10
high       = 100
rearm_margin = 1

[theme]
scheme = "dark"
accent   = "#39c5cf"
critical = "#f47067"
low      = "#c69026"
ok       = "#57ab5a"

[dashboard]
hidden        = ["MX Master"]
sort          = "level"
hide_inactive = true
inactive_after = "5m"

[[device]]
match = "trackpad"
low   = 20
high  = 100

[[device]]
match = "Soundcore"
low   = 25
high  = 90
rearm_margin = 5

[[device]]
match = "MX Keys"
low   = 15

[[hook]]
event    = "low_battery"
command  = "~/.config/blubat/hooks/nag.sh"
debounce = "30m"

[[hook]]
event    = "charged"
command  = "osascript -e 'display notification'"
debounce = "once"

[[hook]]
event    = "disconnected"
match    = "AirPods"
command  = "~/bin/pause-music"
timeout  = "10s"
"##;

    fn device(name: &str, address: &str) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main: Some(50),
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: Source::IoKit,
            connected: true,
            read_at: Timestamp::from_unix(0),
        }
    }

    fn trackpad() -> Device {
        device("Paul\u{2019}s Magic Trackpad", "30-82-16-f2-24-90")
    }

    #[test]
    fn the_documented_sample_parses_into_every_table() {
        let config = Config::parse(SAMPLE).expect("the sample parses");

        assert_eq!(config.poll.profiler_interval, Duration::from_secs(300));
        assert_eq!(config.poll.profiler_timeout, Duration::from_secs(15));
        assert_eq!(config.poll.stale_after, Duration::from_secs(600));
        assert_eq!(config.notifications.sound, "Glass");
        assert!(!config.notifications.connect);
        assert_eq!(config.defaults.low, Some(20));
        assert_eq!(config.dashboard.hidden, ["MX Master"]);
        assert_eq!(config.dashboard.sort, Sort::Level);
        assert!(config.dashboard.hide_inactive);
        assert_eq!(config.dashboard.inactive_after, Duration::from_secs(300));
        assert_eq!(config.devices.len(), 3);
        assert_eq!(config.hooks.len(), 3);
        assert_eq!(config.hooks[0].event, Event::LowBattery);
        assert_eq!(
            config.hooks[0].debounce,
            Some(Debounce::Window(Duration::from_secs(1_800)))
        );
        assert_eq!(config.hooks[1].debounce, Some(Debounce::Once));
        assert_eq!(config.hooks[2].pattern.as_deref(), Some("AirPods"));
        assert_eq!(config.hooks[2].timeout, Some(Duration::from_secs(10)));
        assert!(config.problems().is_empty(), "{:?}", config.problems());
    }

    #[test]
    fn an_empty_file_is_the_built_in_defaults() {
        assert_eq!(Config::parse("").expect("parses"), Config::default());
        assert_eq!(
            Config::default().thresholds_for(&trackpad(), Advertised::NONE),
            Thresholds::BUILT_IN
        );
        assert!(
            !Config::default().dashboard.hide_inactive,
            "shown by default"
        );
        assert_eq!(
            Config::default().dashboard.inactive_after,
            Duration::from_secs(3_600),
            "an hour of silence before a connected device counts as inactive"
        );
    }

    #[test]
    fn inactive_after_parses_a_written_duration() {
        let config = Config::parse("[dashboard]\ninactive_after = \"5m\"\n").expect("parses");

        assert_eq!(config.dashboard.inactive_after, Duration::from_secs(300));
    }

    #[test]
    fn defaults_apply_to_a_device_with_no_block_of_its_own() {
        let config = Config::parse("[defaults]\nlow = 30\nrearm_margin = 4\n").expect("parses");

        let thresholds = config.thresholds_for(&trackpad(), Advertised::NONE);

        assert_eq!(thresholds.low, 30);
        assert_eq!(thresholds.rearm_margin, 4);
        assert_eq!(
            thresholds.critical,
            Thresholds::BUILT_IN.critical,
            "an unset key stays built in"
        );
        assert_eq!(thresholds.high, Thresholds::BUILT_IN.high);
    }

    #[test]
    fn a_block_matched_by_name_overrides_the_defaults() {
        let config = Config::parse(SAMPLE).expect("parses");

        let earbuds = config.thresholds_for(
            &device("Soundcore Liberty 3 Pro", "aa-bb-cc-00-00-0a"),
            Advertised::NONE,
        );

        assert_eq!(earbuds.low, 25);
        assert_eq!(earbuds.high, 90);
        assert_eq!(earbuds.rearm_margin, 5);
        assert_eq!(earbuds.critical, 10, "unset in the block, set in defaults");
    }

    #[test]
    fn a_block_matched_by_address_overrides_the_defaults() {
        let config =
            Config::parse("[defaults]\nlow = 20\n\n[[device]]\nmatch = \"de-df-38\"\nlow = 15\n")
                .expect("parses");
        let keys = device("MX Keys", "de-df-38-f0-46-9b");

        assert_eq!(config.thresholds_for(&keys, Advertised::NONE).low, 15);
        assert_eq!(
            config.thresholds_for(&trackpad(), Advertised::NONE).low,
            20,
            "another device keeps the defaults"
        );
    }

    #[test]
    fn the_first_matching_block_wins() {
        let config = Config::parse(
            "[[device]]\nmatch = \"trackpad\"\nlow = 25\n\n[[device]]\nmatch = \"magic\"\nlow = 35\n",
        )
        .expect("parses");

        assert_eq!(config.thresholds_for(&trackpad(), Advertised::NONE).low, 25);
    }

    #[test]
    fn what_a_device_advertises_sits_under_the_config_and_over_the_built_ins() {
        let advertised = Advertised {
            low: Some(18),
            critical: Some(6),
        };
        let silent = Config::default();
        let written = Config::parse("[defaults]\nlow = 30\n").expect("parses");

        assert_eq!(
            silent.thresholds_for(&trackpad(), advertised).low,
            18,
            "nothing written, so the device's own number stands"
        );
        assert_eq!(silent.thresholds_for(&trackpad(), advertised).critical, 6);
        assert_eq!(
            written.thresholds_for(&trackpad(), advertised).low,
            30,
            "the file wins over the device"
        );
        assert_eq!(
            written.thresholds_for(&trackpad(), advertised).critical,
            6,
            "per key, so an unwritten key still takes the device's"
        );
        assert_eq!(
            silent.thresholds_for(&trackpad(), Advertised::NONE).low,
            Thresholds::BUILT_IN.low
        );
    }

    #[test]
    fn a_missing_file_is_the_built_in_config_and_a_missing_directory_too() {
        let absent = std::env::temp_dir()
            .join(format!("blubat-absent-{}", std::process::id()))
            .join("config.toml");

        assert!(!absent.exists(), "{absent:?} was never written");
        assert_eq!(Config::read(&absent).expect("not an error"), None);
        assert_eq!(
            Config::load(&absent).expect("not an error"),
            Config::default()
        );
    }

    #[test]
    fn a_threshold_no_battery_could_reach_is_reported_rather_than_accepted() {
        let unreachable = Config::parse("[defaults]\nlow = 150\nhigh = 200\nrearm_margin = 120\n")
            .expect("parses");

        let problems = unreachable.problems();

        assert_eq!(
            problems,
            [
                "[defaults]: low (150) must be a percentage of 100 or less",
                "[defaults]: high (200) must be a percentage of 100 or less",
                "[defaults]: rearm_margin (120) must be a percentage of 100 or less",
            ],
            "{problems:?}"
        );
        assert!(
            Config::parse("[defaults]\nlow = 100\nhigh = 100\nrearm_margin = 100\n")
                .expect("parses")
                .problems()
                .iter()
                .all(|problem| !problem.contains("percentage of 100")),
            "100 is a level a battery reports"
        );
    }

    #[test]
    fn a_malformed_file_is_rejected_with_the_line_it_is_on() {
        let error = Config::parse("[defaults]\nlow = 20\ncritical = \"ten\"\n")
            .expect_err("a string threshold is not a number");

        let message = error.to_string();
        assert!(message.contains("line 3"), "{message}");
        assert!(matches!(error, Error::Format(_)));
    }

    #[test]
    fn every_kind_of_nonsense_in_the_file_is_an_error() {
        for contents in [
            "[defaults]\nlwo = 20\n",
            "[dashbord]\nsort = \"level\"\n",
            "[[device]]\nlow = 20\n",
            "[[hook]]\nevent = \"exploded\"\ncommand = \"true\"\n",
            "[[hook]]\nevent = \"low_battery\"\n",
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"true\"\ndebounce = \"soon\"\n",
            "[[hook]]\nevent = \"low_battery\"\ncommand = \"true\"\ntimeout = \"10 seconds\"\n",
            "[poll]\nstale_after = \"forever\"\n",
            "[poll]\nstale_after = 600\n",
            "[dashboard]\nsort = \"battery\"\n",
            "[theme]\naccent = \"#gggggg\"\n",
            "not toml at all {{",
        ] {
            assert!(
                Config::parse(contents).is_err(),
                "{contents:?} should be rejected"
            );
        }
    }

    #[test]
    fn thresholds_that_cannot_all_hold_are_reported_rather_than_parsed_away() {
        let inverted = Config::parse(
            "[defaults]\nlow = 20\nhigh = 15\n\n[[device]]\nmatch = \"keys\"\ncritical = 40\n",
        )
        .expect("it parses: the problem is what the numbers mean");

        let problems = inverted.problems();

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("[defaults]"), "{problems:?}");
        assert!(problems[0].contains("low (20) must be below high (15)"));
        assert!(
            problems.iter().any(|problem| problem
                .contains("[[device]] match = \"keys\": critical (40) must not be above low (20)")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_empty_hook_command_is_a_problem() {
        let config =
            Config::parse("[[hook]]\nevent = \"charged\"\ncommand = \"  \"\n").expect("parses");

        assert_eq!(
            config.problems(),
            ["[[hook]] event = \"charged\": command is empty"]
        );
    }

    #[test]
    fn hooks_are_selected_by_event_and_by_their_own_filter() {
        let config = Config::parse(SAMPLE).expect("parses");
        let airpods = device("Paul\u{2019}s AirPods Pro", "74-15-f5-02-8e-38");

        let run: Vec<&str> = config
            .hooks_for(Event::Disconnected, &airpods)
            .map(|hook| hook.command.as_str())
            .collect();
        assert_eq!(run, ["~/bin/pause-music"]);

        assert_eq!(
            config.hooks_for(Event::Disconnected, &trackpad()).count(),
            0,
            "the filter excludes every other device"
        );
        assert_eq!(
            config.hooks_for(Event::LowBattery, &trackpad()).count(),
            1,
            "an unfiltered hook covers every device"
        );
        assert_eq!(config.hooks_for(Event::Stale, &airpods).count(), 0);
    }

    #[test]
    fn a_block_matching_nothing_visible_is_named_so_a_typo_shows_up() {
        let config = Config::parse(SAMPLE).expect("parses");

        assert_eq!(
            config.unmatched(&[trackpad()]),
            ["Soundcore", "MX Keys"],
            "the trackpad block matched, the other two did not"
        );
        assert!(config.unmatched(&[]).len() == 3);
        assert!(Config::default().unmatched(&[]).is_empty());
    }

    #[test]
    fn notifications_answer_for_every_event() {
        let config = Config::default();

        assert!(config.notifications.enabled(Event::LowBattery));
        assert!(config.notifications.enabled(Event::CriticalBattery));
        assert!(config.notifications.enabled(Event::Charged));
        assert!(config.notifications.enabled(Event::Stale));
        assert!(
            !config.notifications.enabled(Event::Connected),
            "link events are off by default"
        );
        assert!(!config.notifications.enabled(Event::Disconnected));

        let noisy =
            Config::parse("[notifications]\nconnect = true\nlow = false\n").expect("parses");
        assert!(noisy.notifications.enabled(Event::Disconnected));
        assert!(!noisy.notifications.enabled(Event::LowBattery));
    }

    #[test]
    fn the_poll_table_hands_the_daemon_its_own_tiers() {
        let poll = Config::parse(SAMPLE).expect("parses").poll;

        assert_eq!(
            poll.daemon_tiers(),
            Tiers {
                fast: Duration::from_secs(120),
                slow: Duration::from_secs(300),
                timeout: Duration::from_secs(15),
            },
            "the slower tick, since nothing is watching the daemon"
        );
    }
}
