use std::fmt;

use serde::Serialize;

use crate::address::Address;
use crate::timestamp::Timestamp;

/// Which macOS source produced a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Source {
    #[serde(rename = "iokit")]
    IoKit,
    #[serde(rename = "system_profiler")]
    SystemProfiler,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::IoKit => "iokit",
            Source::SystemProfiler => "system_profiler",
        })
    }
}

/// Whether a device is taking charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeState {
    Charging,
    Discharging,
    /// No source reports charge state for this device.
    Unknown,
}

impl ChargeState {
    /// Decodes Apple's `BatteryStatusFlags`, where bit `0x2` means charging.
    pub(crate) fn from_status_flags(flags: i64) -> Self {
        match flags & 0x2 {
            0 => ChargeState::Discharging,
            _ => ChargeState::Charging,
        }
    }
}

impl fmt::Display for ChargeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChargeState::Charging => "charging",
            ChargeState::Discharging => "discharging",
            ChargeState::Unknown => "unknown",
        })
    }
}

/// Battery levels in percent.
///
/// Single battery devices report `main`; AirPods and similar report the other
/// three and no main level at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Levels {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case: Option<u8>,
}

impl Levels {
    /// The single level that stands for the device: the lowest one present.
    ///
    /// A multi battery device is as charged as its emptiest part, so this is
    /// what thresholds and the one line CLI output both read.
    pub fn lowest(self) -> Option<u8> {
        [self.main, self.left, self.right, self.case]
            .into_iter()
            .flatten()
            .min()
    }
}

/// One device as blubat sees it after merging both sources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Device {
    pub address: Address,
    pub name: String,
    /// Device category as `system_profiler` names it, such as `Magic Trackpad`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Link the IOKit node reports, such as `Bluetooth`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    pub levels: Levels,
    pub charge: ChargeState,
    pub source: Source,
    pub connected: bool,
    pub read_at: Timestamp,
}

impl Device {
    /// Case insensitive substring match against the name and the address.
    ///
    /// The one matching rule in blubat, shared by `--device`, watch files and
    /// per-hook filters, so all three select the same device from one string.
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_lowercase();

        !needle.is_empty()
            && (self.name.to_lowercase().contains(&needle)
                || self.address.as_str().contains(&needle.replace(':', "-")))
    }

    /// True when either source reported any battery level for this device.
    pub fn has_battery(&self) -> bool {
        self.levels.lowest().is_some()
    }

    /// The level a threshold may act on, absent while the device is disconnected.
    ///
    /// macOS keeps reporting the last level of a disconnected device with no
    /// timestamp, so that number is last seen data and can be arbitrarily old.
    /// Frontends still show it, reading `connected` to label it; events do not.
    pub fn active_level(&self) -> Option<u8> {
        self.connected.then(|| self.levels.lowest()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, address: &str) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source: Source::SystemProfiler,
            connected: true,
            read_at: Timestamp::from_unix(0),
        }
    }

    #[test]
    fn charging_is_bit_two_of_the_status_flags() {
        assert_eq!(
            ChargeState::from_status_flags(0),
            ChargeState::Discharging,
            "on battery"
        );
        assert_eq!(
            ChargeState::from_status_flags(1),
            ChargeState::Discharging,
            "bit 0x1 is not charge state"
        );
        assert_eq!(ChargeState::from_status_flags(2), ChargeState::Charging);
        assert_eq!(
            ChargeState::from_status_flags(3),
            ChargeState::Charging,
            "plugged in Magic Trackpad"
        );
        assert_eq!(ChargeState::from_status_flags(0xff), ChargeState::Charging);
    }

    #[test]
    fn lowest_level_stands_for_a_multi_battery_device() {
        assert_eq!(Levels::default().lowest(), None);
        assert_eq!(
            Levels {
                main: Some(42),
                ..Levels::default()
            }
            .lowest(),
            Some(42)
        );
        assert_eq!(
            Levels {
                left: Some(100),
                right: Some(97),
                case: Some(68),
                ..Levels::default()
            }
            .lowest(),
            Some(68)
        );
    }

    #[test]
    fn matches_name_and_address_case_insensitively() {
        let trackpad = device("Paul\u{2019}s Magic Trackpad", "30:82:16:F2:24:90");

        assert!(trackpad.matches("trackpad"));
        assert!(trackpad.matches("MAGIC"));
        assert!(trackpad.matches("  Trackpad "));
        assert!(trackpad.matches("30-82-16"), "hyphenated address fragment");
        assert!(trackpad.matches("30:82:16"), "colon separated address");
        assert!(!trackpad.matches("keyboard"));
        assert!(!trackpad.matches(""), "an empty match selects nothing");
    }

    #[test]
    fn a_disconnected_level_is_visible_but_never_active() {
        let levels = Levels {
            main: Some(12),
            ..Levels::default()
        };
        let connected = Device {
            levels,
            ..device("MX Keys", "de:df:38:f0:46:9b")
        };
        let disconnected = Device {
            connected: false,
            ..connected.clone()
        };

        assert_eq!(connected.active_level(), Some(12));
        assert_eq!(disconnected.active_level(), None);
        assert!(
            disconnected.has_battery(),
            "the last seen level stays visible"
        );
        assert_eq!(disconnected.levels.lowest(), Some(12));
    }

    #[test]
    fn source_names_match_the_documented_json_values() {
        assert_eq!(Source::IoKit.to_string(), "iokit");
        assert_eq!(Source::SystemProfiler.to_string(), "system_profiler");
        assert_eq!(
            serde_json::to_string(&Source::SystemProfiler).expect("serialisable"),
            "\"system_profiler\""
        );
    }

    #[test]
    fn json_omits_absent_levels() {
        let airpods = Device {
            levels: Levels {
                left: Some(100),
                case: Some(68),
                ..Levels::default()
            },
            connected: false,
            ..device("Paul\u{2019}s AirPods Pro", "74:15:F5:02:8E:38")
        };

        let json = serde_json::to_value(&airpods).expect("serialisable");
        assert_eq!(
            json["levels"],
            serde_json::json!({ "left": 100, "case": 68 })
        );
        assert_eq!(json["read_at"], "1970-01-01T00:00:00Z");
        assert_eq!(json["connected"], false);
        assert_eq!(json["charge"], "unknown");
    }
}
