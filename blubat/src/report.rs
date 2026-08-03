//! Turning a snapshot into what `blubat list` and `blubat status` print.
//!
//! The renderers are pure functions over borrowed devices, so every shape a
//! script consumes is asserted from fixtures rather than from a real machine.

use blubat_core::{Device, Reading, Snapshot};
use serde::Serialize;

use crate::Failure;

/// Stands in for a value no source reported.
const NOTHING: &str = "--";

const NO_BATTERY: &str = "no device reported a battery (is one connected?)";

/// How a reading is printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
    /// A bare integer percentage, for direct substitution into scripts.
    ///
    /// A bare number has nowhere to carry the `last seen` label the other
    /// formats do, so it can be data of unknown age; a script that needs
    /// freshness reads `connected` from `--json` instead.
    Number,
}

impl Format {
    /// Resolves the output flags, which clap already forbids combining.
    pub fn of(json: bool, number: bool) -> Self {
        match (json, number) {
            (true, _) => Format::Json,
            (_, true) => Format::Number,
            _ => Format::Human,
        }
    }
}

/// Prints every device that reports a battery, or every device at all.
///
/// Whatever was asked for is printed either way, so `--all` still shows the
/// paired devices that report nothing; only the exit code turns on whether a
/// usable reading was among them.
pub fn list(snapshot: &Snapshot, json: bool, all: bool) -> Result<(), Failure> {
    let listing = listing(snapshot, json, all)?;
    if !listing.is_empty() {
        println!("{listing}");
    }

    snapshot
        .with_battery()
        .next()
        .map(|_| ())
        .ok_or_else(|| Failure::NoDevice(NO_BATTERY.to_string()))
}

/// What `list` prints, which is nothing at all only when there is no table.
///
/// A JSON listing is always an array, empty or not, so a script piping into
/// `jq` parses on a machine with nothing paired.
fn listing(snapshot: &Snapshot, json: bool, all: bool) -> Result<String, Failure> {
    let devices: Vec<&Device> = if all {
        snapshot.devices.iter().collect()
    } else {
        snapshot.with_battery().collect()
    };

    match (json, devices.is_empty()) {
        (true, _) => encode(&Reading::all(devices.iter().copied())),
        (_, true) => Ok(String::new()),
        _ => Ok(table(&devices)),
    }
}

/// Prints the one device `--device` selected, or the only one there is.
pub fn status(snapshot: &Snapshot, needle: Option<&str>, format: Format) -> Result<(), Failure> {
    select(snapshot, needle)
        .and_then(|device| render_status(device, format))
        .map(|text| println!("{text}"))
}

/// Picks the device `status` reports on.
///
/// One policy whether or not `--device` was given: exactly one battery device
/// is the answer, and several is a usage error naming them rather than a silent
/// pick of whichever happens to sort first.
fn select<'a>(snapshot: &'a Snapshot, needle: Option<&str>) -> Result<&'a Device, Failure> {
    let matched: Vec<&Device> = snapshot
        .with_battery()
        .filter(|device| needle.is_none_or(|needle| device.matches(needle)))
        .collect();

    match matched.as_slice() {
        [only] => Ok(only),
        [] => Err(Failure::NoDevice(needle.map_or_else(
            || NO_BATTERY.to_string(),
            |needle| format!("no device matching `{needle}` has a battery (is it connected?)"),
        ))),
        several => Err(Failure::Error(format!(
            "{} devices have a battery ({}), name one with --device",
            several.len(),
            several
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn render_status(device: &Device, format: Format) -> Result<String, Failure> {
    match format {
        Format::Human => Ok(line(device)),
        Format::Json => encode(&Reading::of(device)),
        Format::Number => device
            .levels
            .lowest()
            .map(|level| level.to_string())
            .ok_or_else(|| Failure::NoDevice(NO_BATTERY.to_string())),
    }
}

fn encode<T: Serialize>(value: &T) -> Result<String, Failure> {
    serde_json::to_string_pretty(value).map_err(|error| Failure::Error(error.to_string()))
}

/// The one line human reading, laid out as the shell POC lays it out.
fn line(device: &Device) -> String {
    format!("{}  {}  {}", name(device), percent(device), state(device))
}

/// A device name flattened onto one line.
///
/// macOS lets a device be named almost anything, and a control character in a
/// name would break both this line and the table's column arithmetic. Only the
/// rendering is flattened, so JSON stays faithful to what macOS reported.
fn name(device: &Device) -> String {
    device
        .name
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// The plain text table `blubat list` prints.
///
/// Column widths come from the content, so the table lines up without the
/// terminal library the core crate deliberately does without.
fn table(devices: &[&Device]) -> String {
    let header = ["NAME", "ADDRESS", "LEVEL", "STATE", "SOURCE"].map(str::to_string);
    let rows: Vec<[String; 5]> = std::iter::once(header)
        .chain(devices.iter().map(|device| row(device)))
        .collect();

    let widths = rows.iter().fold([0; 5], |mut widths, row| {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
        widths
    });

    rows.iter()
        .map(|row| {
            row.iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:width$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn row(device: &Device) -> [String; 5] {
    [
        name(device),
        device.address.to_string(),
        percent(device),
        state(device),
        device.source.to_string(),
    ]
}

fn percent(device: &Device) -> String {
    device
        .levels
        .lowest()
        .map_or_else(|| NOTHING.to_string(), |level| format!("{level}%"))
}

/// The trailing field of a reading: charge state, or the last seen label a
/// disconnected level must carry wherever it is shown.
fn state(device: &Device) -> String {
    match (device.has_battery(), device.connected) {
        (false, _) => NOTHING.to_string(),
        (_, true) => device.charge.to_string(),
        (_, false) => "last seen".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use blubat_core::{Address, ChargeState, Levels, Source, Timestamp};

    use super::*;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn device(name: &str, address: &str, main: Option<u8>) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main,
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: Source::SystemProfiler,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn trackpad() -> Device {
        Device {
            charge: ChargeState::Charging,
            source: Source::IoKit,
            ..device(
                "Paul\u{2019}s Magic Trackpad",
                "30-82-16-f2-24-90",
                Some(85),
            )
        }
    }

    fn airpods() -> Device {
        Device {
            levels: Levels {
                left: Some(100),
                case: Some(68),
                ..Levels::default()
            },
            connected: false,
            ..device("Paul\u{2019}s AirPods Pro", "74-15-f5-02-8e-38", None)
        }
    }

    fn keyboard() -> Device {
        device("MX Keys M Mac", "de-df-38-f0-46-9b", None)
    }

    fn snapshot(devices: Vec<Device>) -> Snapshot {
        Snapshot {
            read_at: READ_AT,
            devices,
            degraded: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn output_flags_resolve_to_one_format() {
        assert_eq!(Format::of(false, false), Format::Human);
        assert_eq!(Format::of(true, false), Format::Json);
        assert_eq!(Format::of(false, true), Format::Number);
        assert_eq!(
            Format::of(true, true),
            Format::Json,
            "clap forbids the pair, and JSON is the safer resolution of it"
        );
    }

    #[test]
    fn listing_prints_only_battery_devices_unless_all_was_asked_for() {
        let reading = snapshot(vec![keyboard(), trackpad()]);

        let batteries = listing(&reading, false, false).expect("a table");
        assert!(batteries.contains("Magic Trackpad"), "{batteries}");
        assert!(!batteries.contains("MX Keys"), "{batteries}");

        let everything = listing(&reading, false, true).expect("a table");
        assert!(everything.contains("MX Keys"), "{everything}");
    }

    #[test]
    fn listing_json_is_an_array_even_when_there_is_nothing_to_list() {
        assert_eq!(
            listing(&snapshot(Vec::new()), true, false).expect("json"),
            "[]"
        );
        assert_eq!(
            listing(&snapshot(Vec::new()), false, false).expect("no table"),
            "",
            "the human listing has nothing to lay out"
        );
    }

    #[test]
    fn what_list_prints_is_decoupled_from_the_code_it_exits_with() {
        assert!(
            list(&snapshot(vec![trackpad()]), false, false).is_ok(),
            "a usable reading"
        );

        for (all, note) in [(true, "listed but unusable"), (false, "not even listed")] {
            let failure = list(&snapshot(vec![keyboard()]), false, all).expect_err(note);

            assert_eq!(failure.code(), 3, "{note}");
        }
    }

    #[test]
    fn no_device_flag_defaults_to_the_only_device_with_a_battery() {
        let reading = snapshot(vec![keyboard(), trackpad()]);

        assert_eq!(
            select(&reading, None).expect("the only battery"),
            &trackpad()
        );
    }

    #[test]
    fn no_device_flag_is_a_usage_error_when_several_have_batteries() {
        let reading = snapshot(vec![airpods(), trackpad()]);

        let failure = select(&reading, None).expect_err("ambiguous");
        assert_eq!(failure.code(), 1);
        assert!(failure.to_string().contains("--device"), "{failure}");
    }

    #[test]
    fn a_device_is_selected_by_a_substring_of_its_name_or_address() {
        let reading = snapshot(vec![airpods(), trackpad()]);

        assert_eq!(
            select(&reading, Some("TRACK")).expect("by name"),
            &trackpad()
        );
        assert_eq!(
            select(&reading, Some("74:15:f5")).expect("by address"),
            &airpods()
        );
    }

    #[test]
    fn a_needle_matching_several_batteries_is_the_same_usage_error() {
        let reading = snapshot(vec![airpods(), trackpad()]);

        let failure = select(&reading, Some("Paul")).expect_err("ambiguous");

        assert_eq!(failure.code(), 1);
        assert!(failure.to_string().contains("--device"), "{failure}");
        assert!(failure.to_string().contains("AirPods Pro"), "{failure}");
        assert!(failure.to_string().contains("Magic Trackpad"), "{failure}");
    }

    #[test]
    fn a_match_without_a_battery_is_the_same_as_no_match() {
        let reading = snapshot(vec![keyboard(), trackpad()]);

        for needle in ["MX Keys", "nothing here"] {
            let failure = select(&reading, Some(needle)).expect_err("no battery");

            assert_eq!(failure.code(), 3, "{needle}");
        }
    }

    #[test]
    fn an_empty_snapshot_reports_no_device_rather_than_an_error() {
        let failure = select(&snapshot(Vec::new()), None).expect_err("nothing to report");

        assert_eq!(failure.code(), 3);
        assert_eq!(failure.to_string(), NO_BATTERY);
    }

    #[test]
    fn the_human_line_is_the_name_the_level_and_the_state() {
        assert_eq!(
            line(&trackpad()),
            "Paul\u{2019}s Magic Trackpad  85%  charging"
        );
        assert_eq!(
            line(&Device {
                charge: ChargeState::Discharging,
                ..trackpad()
            }),
            "Paul\u{2019}s Magic Trackpad  85%  on battery",
            "the POC's wording for a device running down"
        );
        assert_eq!(
            line(&airpods()),
            "Paul\u{2019}s AirPods Pro  68%  last seen",
            "the lowest sub level stands for a multi battery device"
        );
        assert_eq!(
            line(&keyboard()),
            "MX Keys M Mac  --  --",
            "a device with no reading claims no state either"
        );
    }

    #[test]
    fn the_table_lines_its_columns_up_under_a_header() {
        let devices = [airpods(), trackpad()];

        assert_eq!(
            table(&devices.iter().collect::<Vec<_>>()),
            concat!(
                "NAME                   ADDRESS            LEVEL  STATE      SOURCE\n",
                "Paul\u{2019}s AirPods Pro     74-15-f5-02-8e-38  68%    last seen  system_profiler\n",
                "Paul\u{2019}s Magic Trackpad  30-82-16-f2-24-90  85%    charging   iokit",
            )
        );
    }

    #[test]
    fn the_table_shows_a_device_with_no_reading_rather_than_dropping_it() {
        let devices = [keyboard()];

        assert!(
            table(&devices.iter().collect::<Vec<_>>())
                .contains("MX Keys M Mac  de-df-38-f0-46-9b  --"),
            "a device with no battery is still a row"
        );
    }

    #[test]
    fn a_name_carrying_a_control_character_still_renders_on_one_line() {
        let awkward = Device {
            name: "Newline\nName".to_string(),
            ..trackpad()
        };

        assert_eq!(line(&awkward), "Newline Name  85%  charging");
        assert_eq!(
            table(&[&awkward]).lines().count(),
            2,
            "a header and one row, not three lines"
        );
    }

    #[test]
    fn number_output_is_the_lowest_level_and_nothing_else() {
        assert_eq!(
            render_status(&trackpad(), Format::Number).expect("a level"),
            "85"
        );
        assert_eq!(
            render_status(&airpods(), Format::Number).expect("a level"),
            "68",
            "a bare number cannot say `last seen`, so a disconnected level is still printed"
        );
        assert_eq!(
            render_status(
                &device("Flat Battery", "aa-bb-cc-00-00-0a", Some(0)),
                Format::Number
            )
            .expect("a level"),
            "0",
            "empty is a reading, not a missing one"
        );
    }

    #[test]
    fn number_output_has_nothing_to_print_without_a_level() {
        let failure = render_status(&keyboard(), Format::Number).expect_err("no level");

        assert_eq!(
            failure.code(),
            3,
            "unreachable through select, still honest"
        );
    }

    #[test]
    fn status_json_is_one_versioned_object_carrying_the_documented_keys() {
        let json: serde_json::Value =
            serde_json::from_str(&render_status(&airpods(), Format::Json).expect("json"))
                .expect("valid json");

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["name"], "Paul\u{2019}s AirPods Pro");
        assert_eq!(json["address"], "74-15-f5-02-8e-38");
        assert_eq!(json["level"], 68, "the lowest sub level present");
        assert_eq!(json["levels"], serde_json::json!({"left": 100, "case": 68}));
        assert_eq!(json["charge"], "unknown");
        assert_eq!(json["source"], "system_profiler");
        assert_eq!(json["connected"], false);
        assert_eq!(json["read_at"], "2026-08-02T03:59:59Z");
    }

    #[test]
    fn list_json_is_an_array_of_those_same_objects() {
        let reading = snapshot(vec![airpods(), trackpad()]);

        let json: serde_json::Value =
            serde_json::from_str(&listing(&reading, true, false).expect("json"))
                .expect("valid json");

        assert_eq!(json.as_array().map(Vec::len), Some(2));
        assert_eq!(json[0]["name"], "Paul\u{2019}s AirPods Pro");
        assert_eq!(json[1]["schema_version"], 1);
        assert_eq!(json[1]["level"], 85);
        assert_eq!(json[1]["levels"]["main"], 85);
    }
}
