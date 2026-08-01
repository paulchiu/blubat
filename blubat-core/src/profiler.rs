//! The `system_profiler` source: everything that is not an Apple HID peripheral.
//!
//! The schema here is undocumented and has changed across releases, so every
//! field is optional and anything unrecognised is warned about and skipped
//! rather than treated as fatal.

use std::process::Command;

use serde_json::Value;

use crate::address::Address;
use crate::device::{ChargeState, Device, Levels, Source};
use crate::error::{Error, Result};
use crate::timestamp::Timestamp;
use crate::warn;

/// Runs `system_profiler SPBluetoothDataType -json` and parses what comes back.
pub(crate) fn read(read_at: Timestamp) -> Result<Vec<Device>> {
    let output = Command::new("system_profiler")
        .args(["SPBluetoothDataType", "-json"])
        .output()
        .map_err(|error| Error::Command(format!("system_profiler could not be run: {error}")))?;

    if !output.status.success() {
        return Err(Error::Command(format!(
            "system_profiler exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    parse(&String::from_utf8_lossy(&output.stdout), read_at)
}

/// Parses one `SPBluetoothDataType` document.
fn parse(json: &str, read_at: Timestamp) -> Result<Vec<Device>> {
    let root: Value = serde_json::from_str(json)
        .map_err(|error| Error::Parse(format!("system_profiler JSON is unreadable: {error}")))?;

    let Some(sections) = root.get("SPBluetoothDataType").and_then(Value::as_array) else {
        warn("system_profiler returned no SPBluetoothDataType section");
        return Ok(Vec::new());
    };

    Ok(sections
        .iter()
        .flat_map(|section| section_devices(section, read_at))
        .collect())
}

/// Reads both device arrays of one section, which is where connectedness comes from.
fn section_devices(section: &Value, read_at: Timestamp) -> Vec<Device> {
    [("device_connected", true), ("device_not_connected", false)]
        .into_iter()
        .flat_map(|(key, connected)| {
            section
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(move |entry| entry_devices(entry, connected, read_at))
        })
        .collect()
}

/// Converts one `{ "Device Name": { ... } }` entry.
fn entry_devices(entry: &Value, connected: bool, read_at: Timestamp) -> Vec<Device> {
    let Some(fields) = entry.as_object() else {
        warn("skipping a system_profiler entry that is not an object");
        return Vec::new();
    };

    fields
        .iter()
        .filter_map(|(name, properties)| device(name, properties, connected, read_at))
        .collect()
}

fn device(name: &str, properties: &Value, connected: bool, read_at: Timestamp) -> Option<Device> {
    let address = properties
        .get("device_address")
        .and_then(Value::as_str)
        .and_then(Address::parse)
        .or_else(|| {
            warn(&format!("skipping `{name}`: no usable device_address"));
            None
        })?;

    Some(Device {
        address,
        name: name.to_string(),
        kind: properties
            .get("device_minorType")
            .and_then(Value::as_str)
            .map(str::to_string),
        transport: None,
        levels: Levels {
            main: level(properties, name, "device_batteryLevelMain"),
            left: level(properties, name, "device_batteryLevelLeft"),
            right: level(properties, name, "device_batteryLevelRight"),
            case: level(properties, name, "device_batteryLevelCase"),
        },
        // No charge state exists in this source for any device.
        charge: ChargeState::Unknown,
        source: Source::SystemProfiler,
        connected,
        read_at,
    })
}

/// Reads one battery key, a percent suffixed string such as `"100%"`.
fn level(properties: &Value, name: &str, key: &str) -> Option<u8> {
    let raw = properties.get(key)?;

    raw.as_str()
        .map(|text| text.trim().trim_end_matches('%'))
        .and_then(|text| text.parse::<u8>().ok())
        .filter(|&level| level <= 100)
        .or_else(|| {
            warn(&format!(
                "ignoring {key} on `{name}`: {raw} is not a percentage"
            ));
            None
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    const REAL: &str = include_str!("../tests/fixtures/system_profiler.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/system_profiler_malformed.json");

    fn parsed(json: &str) -> Vec<Device> {
        parse(json, READ_AT).expect("fixture parses")
    }

    fn named(json: &str, name: &str) -> Device {
        parsed(json)
            .into_iter()
            .find(|device| device.name == name)
            .unwrap_or_else(|| panic!("no device named {name}"))
    }

    #[test]
    fn reads_every_device_from_both_arrays() {
        let devices = parsed(REAL);

        assert_eq!(devices.len(), 8);
        assert_eq!(devices.iter().filter(|device| device.connected).count(), 2);
        assert!(devices.iter().all(|device| device.read_at == READ_AT));
        assert!(
            devices
                .iter()
                .all(|device| device.source == Source::SystemProfiler)
        );
    }

    #[test]
    fn strips_the_percent_suffix_from_a_single_battery() {
        let keyboard = named(REAL, "MX Keys M Mac");

        assert_eq!(keyboard.levels.main, Some(100));
        assert_eq!(keyboard.levels.lowest(), Some(100));
        assert_eq!(keyboard.kind.as_deref(), Some("Keyboard"));
        assert!(keyboard.connected);
    }

    #[test]
    fn reads_left_right_and_case_for_airpods() {
        let airpods = named(REAL, "Paul\u{2019}s AirPods Pro");

        assert_eq!(
            airpods.levels,
            Levels {
                main: None,
                left: Some(100),
                right: Some(100),
                case: Some(68),
            }
        );
        assert_eq!(airpods.levels.lowest(), Some(68));
        assert!(!airpods.connected, "found in device_not_connected");
        assert_eq!(
            airpods.active_level(),
            None,
            "a last seen level never feeds a threshold"
        );
    }

    #[test]
    fn a_device_with_no_battery_keys_is_kept_without_a_level() {
        let trackpad = named(REAL, "Paul\u{2019}s Magic Trackpad");

        assert_eq!(trackpad.levels, Levels::default());
        assert!(!trackpad.has_battery(), "IOKit supplies this one");
        assert_eq!(trackpad.kind.as_deref(), Some("Magic Trackpad"));
    }

    #[test]
    fn charge_state_is_unknown_for_every_device_from_this_source() {
        assert!(
            parsed(REAL)
                .iter()
                .all(|device| device.charge == ChargeState::Unknown)
        );
    }

    #[test]
    fn addresses_are_normalised_to_lowercase_hyphens() {
        assert_eq!(
            named(REAL, "MX Keys M Mac").address.as_str(),
            "aa-bb-cc-00-00-02"
        );
    }

    #[test]
    fn malformed_devices_are_skipped_and_the_rest_survive() {
        let devices = parsed(MALFORMED);
        let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();

        assert_eq!(
            names,
            [
                "Good Device",
                "Numeric Battery",
                "Empty Battery",
                "Impossible Battery"
            ],
            "entries without a usable address are dropped, the rest are kept"
        );
        assert_eq!(named(MALFORMED, "Good Device").levels.main, Some(42));
    }

    #[test]
    fn an_unusable_battery_value_leaves_the_device_without_a_level() {
        for name in ["Numeric Battery", "Empty Battery", "Impossible Battery"] {
            assert_eq!(named(MALFORMED, name).levels, Levels::default(), "{name}");
        }
    }

    #[test]
    fn a_document_of_the_wrong_shape_yields_no_devices() {
        for json in ["{}", "[]", r#"{"SPBluetoothDataType": {}}"#, "null"] {
            assert_eq!(parsed(json), Vec::new(), "{json}");
        }
    }

    #[test]
    fn json_that_is_not_json_is_an_error_rather_than_a_panic() {
        assert!(matches!(
            parse("not json at all", READ_AT),
            Err(Error::Parse(_))
        ));
    }
}
