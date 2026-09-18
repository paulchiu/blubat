//! The `system_profiler` source: everything that is not an Apple HID peripheral.
//!
//! The schema here is undocumented and has changed across releases, so every
//! field is optional and anything unrecognised is collected into `warnings` and
//! skipped rather than treated as fatal.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

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

/// How often [`settle`] asks whether the child has exited yet.
const POLL: Duration = Duration::from_millis(10);

/// Runs `command` and hands back its stdout, giving up after `timeout`.
///
/// Each stream is captured into a file rather than a pipe, so nothing in this
/// process has to keep reading while the child runs: a file has no buffer to
/// fill and so cannot stall the process being timed, and the descriptors close
/// when this call returns however it returns. A child still running at the
/// deadline is killed rather than waited out, which is what keeps a wedged
/// call from holding the slow tier open forever.
fn run(mut command: Command, timeout: Duration) -> Result<Vec<u8>> {
    let captured = |error| Error::Command(format!("system_profiler could not be read: {error}"));
    let out = Capture::new("out").map_err(captured)?;
    let err = Capture::new("err").map_err(captured)?;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(out.stdio().map_err(captured)?)
        .stderr(err.stdio().map_err(captured)?)
        .spawn()
        .map_err(|error| Error::Command(format!("system_profiler could not be run: {error}")))?;

    let Some(status) = settle(&mut child, timeout) else {
        return Err(Error::Command(format!(
            "system_profiler took longer than {}s and was stopped",
            timeout.as_secs()
        )));
    };

    if !status.success() {
        return Err(Error::Command(format!(
            "system_profiler exited with {status}: {}",
            String::from_utf8_lossy(&err.written()).trim()
        )));
    }

    Ok(out.written())
}

/// Waits for `child` to exit, killing it once `timeout` has gone by.
///
/// The wait is on the child itself rather than on its output ending, so a
/// descendant that inherited the capture and outlives its parent cannot turn
/// a run that finished into one reported as a timeout.
fn settle(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();

                return None;
            }
        }
    }
}

/// One stream of a child's output, held in a file unlinked as soon as it is
/// open.
///
/// Nothing else can reach it, nothing is left behind to clean up, and the
/// descriptor belongs to this value rather than to a reader that may never
/// reach the end of what it is reading. `blubat`'s bluetoothd sweep captures
/// its helper the same way.
struct Capture(File);

impl Capture {
    /// Refusing an existing name rather than truncating it is what keeps this
    /// off a planted symlink where `TMPDIR` is unset and the temporary
    /// directory is the shared `/tmp`. The clock sits in the name so that a
    /// file orphaned between opening and unlinking cannot make every later run
    /// refuse for good.
    fn new(tag: &str) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);

        let started = SystemTime::UNIX_EPOCH
            .elapsed()
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "blubat-{}-{started}-{}-{tag}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        fs::remove_file(&path)?;

        Ok(Self(file))
    }

    /// The child's end of the capture, which `spawn` closes in this process.
    fn stdio(&self) -> std::io::Result<Stdio> {
        self.0.try_clone().map(Stdio::from)
    }

    /// Everything written to it, read back once the child has exited. A stream
    /// that cannot be read back is nothing written, the way an empty one is.
    fn written(mut self) -> Vec<u8> {
        let mut buffer = Vec::new();
        let _ = self.0.seek(SeekFrom::Start(0));
        let _ = self.0.read_to_end(&mut buffer);

        buffer
    }
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
        vendor_id: hex_u16(properties, "device_vendorID"),
        product_id: hex_u16(properties, "device_productID"),
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

/// Reads a `"0x…"` hex id such as `device_vendorID`, absent for anything
/// that is not one: a device with no such key, and one whose value this
/// schema changed under, are handled the same way as a missing key already
/// is elsewhere in this parse.
fn hex_u16(properties: &Value, key: &str) -> Option<u16> {
    properties
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .and_then(|text| text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")))
        .and_then(|digits| u16::from_str_radix(digits, 16).ok())
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
    fn a_hex_id_missing_its_prefix_or_otherwise_unusable_is_absent_rather_than_a_panic() {
        let value = |raw: serde_json::Value| serde_json::json!({ "device_vendorID": raw });

        for raw in ["009E", "0xGGGG", "", "0x"] {
            assert_eq!(
                hex_u16(&value(serde_json::json!(raw)), "device_vendorID"),
                None
            );
        }
        assert_eq!(
            hex_u16(&value(serde_json::json!(158)), "device_vendorID"),
            None
        );
        assert_eq!(hex_u16(&serde_json::json!({}), "device_vendorID"), None);
    }

    #[test]
    fn a_hex_id_reads_with_either_case_prefix_and_trims_stray_space() {
        let value = |raw: &str| serde_json::json!({ "device_vendorID": raw });

        assert_eq!(hex_u16(&value("0x009E"), "device_vendorID"), Some(0x009E));
        assert_eq!(hex_u16(&value("0X009e"), "device_vendorID"), Some(0x009E));
        assert_eq!(hex_u16(&value(" 0x4075 "), "device_vendorID"), Some(0x4075));
    }

    #[test]
    fn a_bose_headsets_vendor_and_product_id_are_retained() {
        let bose = named(REAL, "Bose QC Headphones");

        assert_eq!(bose.vendor_id, Some(0x009E));
        assert_eq!(bose.product_id, Some(0x4075));
        assert!(!bose.connected, "found in device_not_connected");
    }

    #[test]
    fn a_device_reporting_neither_id_has_neither() {
        let keyboard = named(REAL, "Keychron B1 Pro");

        assert_eq!(keyboard.vendor_id, None);
        assert_eq!(keyboard.product_id, None);
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

    /// This process's own open descriptors, which `/dev/fd` lists on macOS.
    fn open_files() -> usize {
        std::fs::read_dir("/dev/fd")
            .expect("/dev/fd is readable")
            .count()
    }

    /// Runs, not one run: a single leak is indistinguishable from a descriptor
    /// another test opened while this one was counting, so the tolerance sits
    /// well under one per run and well over the handful of those.
    const RUNS: usize = 20;
    const NOISE: usize = 8;

    #[test]
    fn a_timed_out_command_whose_descendant_still_holds_its_output_leaks_nothing() {
        let before = open_files();

        for _ in 0..RUNS {
            let error = run(shell("sleep 30 & sleep 30"), Duration::from_millis(100))
                .expect_err("it never finishes on its own");

            assert!(error.to_string().contains("took longer"), "{error}");
        }

        let after = open_files();

        assert!(
            after <= before + NOISE,
            "{RUNS} timed out runs took this process from {before} open files to {after}"
        );
    }

    #[test]
    fn a_command_that_finished_is_not_timed_out_by_a_descendant_still_holding_its_output() {
        let output = run(
            shell("sleep 30 & printf 'done'"),
            Duration::from_millis(100),
        )
        .expect("the command itself finished at once");

        assert_eq!(String::from_utf8_lossy(&output), "done");
    }

    /// Captures are named for this process, so nothing outside the suite can
    /// move this count.
    fn captures_left_in_the_temp_dir() -> usize {
        let mine = format!("blubat-{}-", std::process::id());

        std::fs::read_dir(std::env::temp_dir())
            .expect("the temp dir is readable")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&mine))
            .count()
    }

    /// Creating a capture and unlinking it is two calls, and the tests running
    /// alongside this one are making captures of their own, so a couple may be
    /// caught between the two. Three runs that failed to unlink would leave six.
    const IN_FLIGHT: usize = 2;

    #[test]
    fn a_run_leaves_no_capture_file_behind_whether_it_finishes_or_is_stopped() {
        let before = captures_left_in_the_temp_dir();

        let _ = run(shell("printf '{}'"), Duration::from_secs(10));
        let _ = run(shell("echo trouble >&2; exit 3"), Duration::from_secs(10));
        let _ = run(shell("sleep 30 & sleep 30"), Duration::from_millis(100));

        let after = captures_left_in_the_temp_dir();

        assert!(
            after <= before + IN_FLIGHT,
            "three runs took {:?} from {before} captures to {after}",
            std::env::temp_dir()
        );
    }

    #[test]
    fn a_reading_larger_than_a_pipe_buffer_comes_back_whole() {
        let output = run(
            shell("head -c 300000 /dev/zero | tr '\\0' 'a'"),
            Duration::from_secs(10),
        )
        .expect("it finishes");

        assert_eq!(output.len(), 300_000);
    }
}
