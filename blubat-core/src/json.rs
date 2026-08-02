//! The JSON shape `--json` promises, kept apart from the model it renders.
//!
//! Anything a script parses is a compatibility surface, so the shape is a type
//! of its own with a version on it rather than whatever [`Device`] happens to
//! derive today. A [`Reading`] borrows the device it describes, so rendering a
//! listing copies nothing.

use serde::Serialize;

use crate::device::Device;

/// One device as `--json` writes it.
///
/// `schema_version`, `level`, `charge`, `source`, `connected` and `read_at` are
/// always written, including as `null` where there is nothing to report, since
/// a script branching on them should not have to tell absent from missing. The
/// descriptive keys are omitted when they have no value.
#[derive(Clone, Debug, Serialize)]
pub struct Reading<'a> {
    schema_version: u32,
    /// The single level that stands for the device: the lowest sub level
    /// present, which for AirPods and similar is the emptiest of the three.
    level: Option<u8>,
    #[serde(flatten)]
    device: &'a Device,
}

impl<'a> Reading<'a> {
    /// The version of this shape, raised only by a change that breaks a reader.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Renders one device in the documented shape.
    pub fn of(device: &'a Device) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            level: device.levels.lowest(),
            device,
        }
    }

    /// Renders every device of a listing, in the order it was given them.
    pub fn all<I: IntoIterator<Item = &'a Device>>(devices: I) -> Vec<Self> {
        devices.into_iter().map(Self::of).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;
    use crate::device::{ChargeState, Levels, Source};
    use crate::timestamp::Timestamp;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn trackpad() -> Device {
        Device {
            address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
            name: "Paul\u{2019}s Magic Trackpad".to_string(),
            kind: Some("Magic Trackpad".to_string()),
            transport: Some("Bluetooth".to_string()),
            levels: Levels {
                main: Some(85),
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            source: Source::IoKit,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn airpods() -> Device {
        Device {
            name: "Paul\u{2019}s AirPods Pro".to_string(),
            kind: None,
            transport: None,
            levels: Levels {
                main: None,
                left: Some(100),
                right: Some(97),
                case: Some(68),
            },
            charge: ChargeState::Unknown,
            source: Source::SystemProfiler,
            connected: false,
            ..trackpad()
        }
    }

    /// The exact bytes a script reads, pinned so a patch release cannot move
    /// the shape without this test saying so.
    #[test]
    fn the_serialised_shape_is_the_documented_one() {
        assert_eq!(
            serde_json::to_string(&Reading::of(&trackpad())).expect("serialisable"),
            concat!(
                r#"{"schema_version":1,"level":85,"address":"30-82-16-f2-24-90","name":""#,
                "Paul\u{2019}s Magic Trackpad",
                r#"","kind":"Magic Trackpad","transport":"Bluetooth","levels":{"main":85},"#,
                r#""charge":"discharging","source":"iokit","connected":true,"#,
                r#""read_at":"2026-08-02T03:59:59Z"}"#,
            )
        );
    }

    #[test]
    fn a_multi_battery_device_carries_every_sub_level_and_the_lowest_of_them() {
        let json = serde_json::to_value(Reading::of(&airpods())).expect("serialisable");

        assert_eq!(json["level"], 68);
        assert_eq!(
            json["levels"],
            serde_json::json!({ "left": 100, "right": 97, "case": 68 }),
            "the parts stay available beside the one number"
        );
        assert_eq!(json["connected"], false);
    }

    #[test]
    fn a_key_with_nothing_to_say_is_omitted_and_a_contract_key_is_null() {
        let anonymous = Device {
            kind: None,
            transport: None,
            levels: Levels::default(),
            ..trackpad()
        };

        let json = serde_json::to_value(Reading::of(&anonymous)).expect("serialisable");

        assert_eq!(json["level"], serde_json::Value::Null);
        assert_eq!(json["levels"], serde_json::json!({}));
        assert_eq!(json.get("kind"), None);
        assert_eq!(json.get("transport"), None);
        assert_eq!(
            json["charge"], "discharging",
            "still answered for a device with no level to answer about"
        );
    }

    #[test]
    fn a_listing_is_the_same_objects_in_the_order_it_was_given() {
        let devices = [airpods(), trackpad()];

        let json = serde_json::to_value(Reading::all(&devices)).expect("serialisable");

        assert_eq!(json.as_array().map(Vec::len), Some(2));
        assert_eq!(json[0]["name"], "Paul\u{2019}s AirPods Pro");
        assert_eq!(json[1]["level"], 85);
        assert!(
            json.as_array().is_some_and(|readings| readings
                .iter()
                .all(|reading| reading["schema_version"] == Reading::SCHEMA_VERSION)),
            "every object names the version it is written in"
        );
    }
}
