use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::timestamp::Timestamp;

/// Which macOS source produced a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    #[serde(rename = "iokit")]
    IoKit,
    #[serde(rename = "system_profiler")]
    SystemProfiler,
    /// The daemon's own RFCOMM read of a Bose headset's BMAP battery level.
    #[serde(rename = "bmap")]
    Bmap,
    /// The daemon's own read of a BLE peripheral's Battery Service level.
    #[serde(rename = "gatt")]
    Gatt,
    /// A level macOS's own `bluetoothd` cached, read back off the paired device.
    #[serde(rename = "bluetoothd")]
    Bluetoothd,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::IoKit => "iokit",
            Source::SystemProfiler => "system_profiler",
            Source::Bmap => "bmap",
            Source::Gatt => "gatt",
            Source::Bluetoothd => "bluetoothd",
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

/// Reads as the shell POC reads, where a discharging device is `on battery`.
///
/// Only the human surfaces use this. The JSON value stays `discharging`, which
/// is what the documented schema promises.
impl fmt::Display for ChargeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChargeState::Charging => "charging",
            ChargeState::Discharging => "on battery",
            ChargeState::Unknown => "unknown",
        })
    }
}

/// Which battery of a device one level belongs to.
///
/// `system_profiler` reports AirPods and similar as three separate keys, so a
/// frontend showing the parts needs a name for each of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    Main,
    Left,
    Right,
    Case,
}

impl Part {
    /// Every part in the order a frontend lists them.
    pub const ALL: [Self; 4] = [Part::Main, Part::Left, Part::Right, Part::Case];
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Part::Main => "main",
            Part::Left => "left",
            Part::Right => "right",
            Part::Case => "case",
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
    /// what thresholds and every one number surface both read.
    pub fn lowest(self) -> Option<u8> {
        self.present().map(|(_, level)| level).min()
    }

    /// The parts that reported a level, in [`Part::ALL`] order.
    ///
    /// The detail view lists these; everything judging a device reads
    /// [`Levels::lowest`] over them instead.
    pub fn present(self) -> impl Iterator<Item = (Part, u8)> {
        Part::ALL
            .into_iter()
            .zip([self.main, self.left, self.right, self.case])
            .filter_map(|(part, level)| level.map(|level| (part, level)))
    }

    /// Whether more than one battery reported, which is what AirPods do.
    pub fn multi_battery(self) -> bool {
        self.present().count() > 1
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
    /// The Bluetooth vendor id `system_profiler` reports, such as `0x009E`
    /// for Bose. IOKit's registry does not surface this, so it is present
    /// only for a device this source has read; the BMAP source keys its
    /// candidate selection off this and [`Device::product_id`] together.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    /// The Bluetooth product id alongside [`Device::vendor_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
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
    /// The lowest sub level present, because a multi battery device is as
    /// charged as its emptiest part. macOS keeps reporting the last level of a
    /// disconnected device with no timestamp, so that number is last seen data
    /// and can be arbitrarily old: frontends still show it, reading `connected`
    /// to label it, and events do not.
    pub fn active_level(&self) -> Option<u8> {
        self.connected.then(|| self.levels.lowest()).flatten()
    }

    /// Whether this device's newest reading is older than the stale window.
    ///
    /// The one staleness rule in blubat: the engine raises `stale` on it and a
    /// frontend marks a row with it, so the two cannot come to disagree about
    /// which devices have gone quiet.
    pub fn is_stale(&self, stale_after: Duration, now: Timestamp) -> bool {
        now >= self.read_at.plus(stale_after)
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
            vendor_id: None,
            product_id: None,
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
    fn a_flat_battery_is_a_reading_rather_than_a_missing_one() {
        let flat = Device {
            levels: Levels {
                main: Some(0),
                ..Levels::default()
            },
            ..device("MX Master 3S", "aa-bb-cc-00-00-0a")
        };

        assert_eq!(flat.levels.lowest(), Some(0));
        assert!(
            flat.has_battery(),
            "empty is a level, not the absence of one"
        );
        assert_eq!(flat.active_level(), Some(0));
    }

    #[test]
    fn the_parts_that_reported_are_listed_in_their_own_order() {
        let airpods = Levels {
            main: None,
            left: Some(100),
            right: Some(97),
            case: Some(68),
        };

        assert_eq!(
            airpods.present().collect::<Vec<_>>(),
            [(Part::Left, 100), (Part::Right, 97), (Part::Case, 68)]
        );
        assert!(airpods.multi_battery());
        assert_eq!(
            Levels {
                main: Some(42),
                ..Levels::default()
            }
            .present()
            .collect::<Vec<_>>(),
            [(Part::Main, 42)]
        );
        assert!(
            !Levels {
                main: Some(42),
                ..Levels::default()
            }
            .multi_battery()
        );
        assert_eq!(Levels::default().present().count(), 0);
        assert!(!Levels::default().multi_battery());
    }

    #[test]
    fn each_part_names_itself() {
        assert_eq!(
            Part::ALL.map(|part| part.to_string()),
            ["main", "left", "right", "case"]
        );
    }

    #[test]
    fn a_reading_is_stale_once_the_window_has_passed() {
        let window = Duration::from_secs(600);
        let trackpad = Device {
            read_at: Timestamp::from_unix(1_000),
            ..device("Paul\u{2019}s Magic Trackpad", "30-82-16-f2-24-90")
        };

        assert!(!trackpad.is_stale(window, Timestamp::from_unix(1_599)));
        assert!(trackpad.is_stale(window, Timestamp::from_unix(1_600)));
        assert!(trackpad.is_stale(window, Timestamp::from_unix(9_000)));
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
        assert_eq!(Source::Bmap.to_string(), "bmap");
        assert_eq!(Source::Gatt.to_string(), "gatt");
        assert_eq!(Source::Bluetoothd.to_string(), "bluetoothd");
        assert_eq!(
            serde_json::to_string(&Source::SystemProfiler).expect("serialisable"),
            "\"system_profiler\""
        );
        assert_eq!(
            serde_json::to_string(&Source::Bmap).expect("serialisable"),
            "\"bmap\""
        );
        assert_eq!(
            serde_json::to_string(&Source::Gatt).expect("serialisable"),
            "\"gatt\""
        );
        assert_eq!(
            serde_json::to_string(&Source::Bluetoothd).expect("serialisable"),
            "\"bluetoothd\""
        );
    }

    #[test]
    fn charge_state_reads_as_the_poc_and_serialises_as_documented() {
        assert_eq!(ChargeState::Charging.to_string(), "charging");
        assert_eq!(ChargeState::Discharging.to_string(), "on battery");
        assert_eq!(ChargeState::Unknown.to_string(), "unknown");
        assert_eq!(
            serde_json::to_string(&ChargeState::Discharging).expect("serialisable"),
            "\"discharging\"",
            "the JSON name is independent of the printed one"
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
