//! The `system_profiler` source: everything that is not an Apple HID peripheral.
//!
//! The schema here is undocumented and has changed across releases, so every
//! field is optional and anything unrecognised is collected into `warnings` and
//! skipped rather than treated as fatal.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::address::Address;
use crate::device::{ChargeState, Device, Levels, Source};
use crate::error::{Error, Result};
use crate::timestamp::Timestamp;

/// Runs `system_profiler SPBluetoothDataType -json` and parses what comes back.
pub(crate) fn read(
    read_at: Timestamp,
    timeout: Duration,
    warnings: &mut Vec<String>,
) -> Result<Vec<Device>> {
    let mut command = Command::new("system_profiler");
    command.args(["SPBluetoothDataType", "-json"]);
    let output = run(command, timeout)?;

    parse(&String::from_utf8_lossy(&output), read_at, warnings)
}

/// Runs `command` and hands back its stdout, giving up after `timeout`.
///
/// Both pipes are drained on their own threads as the child writes them, so a
/// long reading cannot fill a pipe buffer and stall the process being timed. A
/// child still running at the deadline is killed rather than waited out, which
/// is what keeps a wedged call from holding the slow tier open forever.
fn run(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::Command(format!("system_profiler could not be run: {error}")))?;

    let stdout = drain(child.stdout.take().expect("stdout was piped"));
    let stderr = drain(child.stderr.take().expect("stderr was piped"));

    let Ok(output) = stdout.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();

        return Err(Error::Command(format!(
            "system_profiler took longer than {}s and was stopped",
            timeout.as_secs()
        )));
    };

    let status = child.wait().map_err(|error| {
        Error::Command(format!("system_profiler could not be waited on: {error}"))
    })?;
    if !status.success() {
        return Err(Error::Command(format!(
            "system_profiler exited with {status}: {}",
            String::from_utf8_lossy(&stderr.recv().unwrap_or_default()).trim()
        )));
    }

    Ok(output)
}

/// Reads one child pipe to its end on a thread of its own.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (read, drained) = mpsc::channel();

    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        let _ = read.send(buffer);
    });

    drained
}

/// Parses one `SPBluetoothDataType` document.
fn parse(json: &str, read_at: Timestamp, warnings: &mut Vec<String>) -> Result<Vec<Device>> {
    let root: Value = serde_json::from_str(json)
        .map_err(|error| Error::Format(format!("system_profiler JSON is unreadable: {error}")))?;

    let Some(sections) = root.get("SPBluetoothDataType").and_then(Value::as_array) else {
        warnings.push("system_profiler returned no SPBluetoothDataType section".to_string());
        return Ok(Vec::new());
    };

    Ok(sections
        .iter()
        .flat_map(|section| section_devices(section, read_at, warnings))
        .collect())
}

/// Reads both device arrays of one section, which is where connectedness comes from.
fn section_devices(section: &Value, read_at: Timestamp, warnings: &mut Vec<String>) -> Vec<Device> {
    let mut devices = Vec::new();

    for (key, connected) in [("device_connected", true), ("device_not_connected", false)] {
        let entries = section.get(key).and_then(Value::as_array);

        for entry in entries.into_iter().flatten() {
            devices.extend(entry_devices(entry, connected, read_at, warnings));
        }
    }

    devices
}

/// Converts one `{ "Device Name": { ... } }` entry.
fn entry_devices(
    entry: &Value,
    connected: bool,
    read_at: Timestamp,
    warnings: &mut Vec<String>,
) -> Vec<Device> {
    let Some(fields) = entry.as_object() else {
        warnings.push("skipping a system_profiler entry that is not an object".to_string());
        return Vec::new();
    };

    fields
        .iter()
        .filter_map(|(name, properties)| device(name, properties, connected, read_at, warnings))
        .collect()
}

fn device(
    name: &str,
    properties: &Value,
    connected: bool,
    read_at: Timestamp,
    warnings: &mut Vec<String>,
) -> Option<Device> {
    let address = properties
        .get("device_address")
        .and_then(Value::as_str)
        .and_then(Address::parse);
    let Some(address) = address else {
        warnings.push(format!("skipping `{name}`: no usable device_address"));
        return None;
    };

    Some(Device {
        address,
        name: name.to_string(),
        kind: properties
            .get("device_minorType")
            .and_then(Value::as_str)
            .map(str::to_string),
        transport: None,
        levels: Levels {
            main: level(properties, name, "device_batteryLevelMain", warnings),
            left: level(properties, name, "device_batteryLevelLeft", warnings),
            right: level(properties, name, "device_batteryLevelRight", warnings),
            case: level(properties, name, "device_batteryLevelCase", warnings),
        },
        // No charge state exists in this source for any device.
        charge: ChargeState::Unknown,
        source: Source::SystemProfiler,
        connected,
        read_at,
    })
}

/// Reads one battery key, a percent suffixed string such as `"100%"`.
///
/// Trimmed on both sides of the suffix, because the only guarantee this schema
/// offers is that it has changed shape before.
fn level(properties: &Value, name: &str, key: &str, warnings: &mut Vec<String>) -> Option<u8> {
    let raw = properties.get(key)?;
    let level = raw
        .as_str()
        .map(|text| text.trim().trim_end_matches('%').trim())
        .and_then(|text| text.parse::<u8>().ok())
        .filter(|&level| level <= 100);

    if level.is_none() {
        warnings.push(format!(
            "ignoring {key} on `{name}`: {raw} is not a percentage"
        ));
    }

    level
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    const REAL: &str = include_str!("../tests/fixtures/system_profiler.json");
    const MALFORMED: &str = include_str!("../tests/fixtures/system_profiler_malformed.json");

    fn parsed(json: &str) -> Vec<Device> {
        parse(json, READ_AT, &mut Vec::new()).expect("fixture parses")
    }

    fn skipped(json: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        parse(json, READ_AT, &mut warnings).expect("fixture parses");

        warnings
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

        assert_eq!(devices.len(), 10);
        assert_eq!(devices.iter().filter(|device| device.connected).count(), 4);
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
    fn a_connected_multi_battery_device_has_an_active_level() {
        let earbuds = named(REAL, "Soundcore Liberty 3 Pro");

        assert!(earbuds.connected);
        assert_eq!(earbuds.levels.lowest(), Some(72));
        assert_eq!(earbuds.active_level(), Some(72));
    }

    #[test]
    fn an_empty_battery_is_a_reading_rather_than_a_missing_one() {
        let mouse = named(REAL, "MX Master 3S");

        assert_eq!(mouse.levels.main, Some(0));
        assert!(mouse.has_battery(), "0% is a level, not the absence of one");
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
                "Impossible Battery",
                "Spaced Battery",
                "Bare Battery",
                "Twice Listed",
                "Twice Listed",
            ],
            "entries without a usable address are dropped, the rest are kept"
        );
        assert_eq!(named(MALFORMED, "Good Device").levels.main, Some(42));
    }

    #[test]
    fn what_was_skipped_is_returned_rather_than_printed() {
        let warnings = skipped(MALFORMED);

        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Bad Address")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("is not a percentage")),
            "{warnings:?}"
        );
        assert_eq!(
            skipped("{}"),
            ["system_profiler returned no SPBluetoothDataType section"]
        );
    }

    #[test]
    fn a_battery_value_survives_stray_space_and_a_missing_percent_sign() {
        assert_eq!(named(MALFORMED, "Spaced Battery").levels.main, Some(100));
        assert_eq!(named(MALFORMED, "Bare Battery").levels.main, Some(85));
    }

    #[test]
    fn one_address_in_both_arrays_yields_both_records_for_the_merge_to_settle() {
        let listed: Vec<Device> = parsed(MALFORMED)
            .into_iter()
            .filter(|device| device.name == "Twice Listed")
            .collect();

        let [live, stale] = &listed[..] else {
            panic!("expected the address twice, got {listed:?}");
        };
        assert_eq!(live.address, stale.address);
        assert!(live.connected && !stale.connected);
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
            parse("not json at all", READ_AT, &mut Vec::new()),
            Err(Error::Format(_))
        ));
    }

    /// A shell command, so the timing is exercised on something that behaves
    /// the way a wedged or failing `system_profiler` would without being one.
    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);

        command
    }

    #[test]
    fn a_command_that_finishes_hands_back_everything_it_wrote() {
        let output = run(shell("printf '{}'"), Duration::from_secs(10)).expect("it finishes");

        assert_eq!(String::from_utf8_lossy(&output), "{}");
    }

    #[test]
    fn a_command_that_outlasts_the_timeout_is_stopped_rather_than_waited_out() {
        let started = std::time::Instant::now();

        let error = run(shell("sleep 30"), Duration::from_millis(100))
            .expect_err("it never finishes on its own");

        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it gave up early"
        );
        assert!(error.to_string().contains("took longer than"), "{error}");
    }

    #[test]
    fn a_command_that_fails_reports_what_it_said_about_it() {
        let error = run(shell("echo trouble >&2; exit 3"), Duration::from_secs(10))
            .expect_err("a non-zero exit");

        assert!(error.to_string().contains("trouble"), "{error}");
        assert!(matches!(error, Error::Command(_)));
    }

    #[test]
    fn a_command_that_is_not_there_is_an_error_rather_than_a_panic() {
        let error = run(
            Command::new("/nonexistent/blubat-not-a-command"),
            Duration::from_secs(10),
        )
        .expect_err("nothing to run");

        assert!(error.to_string().contains("could not be run"), "{error}");
    }
}
