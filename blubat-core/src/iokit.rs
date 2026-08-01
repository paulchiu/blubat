//! The native IOKit source: Apple HID peripherals.
//!
//! Apple's own peripherals report a battery level here and nowhere else, so
//! this is both the only source for them and, at well under a millisecond, the
//! one cheap enough to sit on a poll tick.

use std::collections::HashMap;
use std::ffi::CString;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_io_kit::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices,
    IOServiceMatching, io_iterator_t, io_object_t, kIOMainPortDefault,
};

use crate::address::Address;
use crate::device::{ChargeState, Device, Levels, Source};
use crate::timestamp::Timestamp;
use crate::warn;

/// The class Apple's HID peripherals register under.
///
/// It bounds the iteration only. `BatteryPercent` is what decides whether an
/// entry counts as a reading, so a driver that carries the key but not the
/// class is a matter of widening this constant, not of reworking the parse.
const SERVICE_CLASS: &str = "AppleDeviceManagementHIDEventService";

const KEYS: [&str; 6] = [
    "Product",
    "DeviceAddress",
    "BatteryPercent",
    "BatteryStatusFlags",
    "HasBattery",
    "Transport",
];

/// Reads every Apple HID peripheral that reports a battery.
pub(crate) fn read(read_at: Timestamp) -> Vec<Device> {
    matching_entries()
        .into_iter()
        .filter_map(|properties| device(&properties, read_at))
        .collect()
}

/// One registry property, narrowed to the three types these keys use.
enum Property {
    Text(String),
    Number(i64),
    Flag(bool),
}

impl Property {
    fn text(&self) -> Option<&str> {
        match self {
            Property::Text(text) => Some(text),
            _ => None,
        }
    }

    fn number(&self) -> Option<i64> {
        match self {
            Property::Number(number) => Some(*number),
            _ => None,
        }
    }

    fn flag(&self) -> Option<bool> {
        match self {
            Property::Flag(flag) => Some(*flag),
            _ => None,
        }
    }
}

type Properties = HashMap<&'static str, Property>;

fn device(properties: &Properties, read_at: Timestamp) -> Option<Device> {
    let percent = properties
        .get("BatteryPercent")
        .and_then(Property::number)?;
    if properties.get("HasBattery").and_then(Property::flag) == Some(false) {
        return None;
    }

    let address = properties
        .get("DeviceAddress")
        .and_then(Property::text)
        .and_then(Address::parse)
        .or_else(|| {
            warn("skipping an IOKit battery reading with no usable DeviceAddress");
            None
        })?;

    let name = properties
        .get("Product")
        .and_then(Property::text)
        .map(str::to_string)
        .unwrap_or_else(|| address.to_string());

    Some(Device {
        address,
        name,
        // The device category comes from the other source, which names it better.
        kind: None,
        transport: properties
            .get("Transport")
            .and_then(Property::text)
            .map(str::to_string),
        levels: Levels {
            main: u8::try_from(percent).ok().filter(|&level| level <= 100),
            ..Levels::default()
        },
        charge: properties
            .get("BatteryStatusFlags")
            .and_then(Property::number)
            .map_or(ChargeState::Unknown, ChargeState::from_status_flags),
        source: Source::IoKit,
        // The registry lists a device only while it is present.
        connected: true,
        read_at,
    })
}

/// Runs one pass over the matching services, releasing every handle it takes.
fn matching_entries() -> Vec<Properties> {
    let class = CString::new(SERVICE_CLASS).expect("class name has no interior nul");
    let mut entries = Vec::new();

    unsafe {
        let Some(matching) = IOServiceMatching(class.as_ptr()) else {
            return entries;
        };
        // IOServiceMatching hands back the mutable subtype and the getter wants
        // the immutable one. Same object, so the reinterpret is sound.
        let matching: CFRetained<CFDictionary> = CFRetained::cast_unchecked(matching);

        let mut iterator: io_iterator_t = 0;
        let result =
            IOServiceGetMatchingServices(kIOMainPortDefault, Some(matching), &mut iterator);
        if result != 0 || iterator == 0 {
            return entries;
        }

        loop {
            let entry = IOIteratorNext(iterator);
            if entry == 0 {
                break;
            }
            entries.push(read_properties(entry));
            IOObjectRelease(entry);
        }

        IOObjectRelease(iterator);
    }

    entries
}

fn read_properties(entry: io_object_t) -> Properties {
    KEYS.iter()
        .filter_map(|&key| read_property(entry, key).map(|value| (key, value)))
        .collect()
}

/// Copies one property off a registry entry.
///
/// `CFRetained` owns the create-rule reference, so the release is its `Drop`
/// and every type test is a checked downcast.
fn read_property(entry: io_object_t, key: &str) -> Option<Property> {
    let key = CFString::from_str(key);
    let value: CFRetained<CFType> =
        unsafe { IORegistryEntryCreateCFProperty(entry, Some(&key), None, 0) }?;

    value
        .downcast_ref::<CFString>()
        .map(|text| Property::Text(text.to_string()))
        .or_else(|| {
            value
                .downcast_ref::<CFNumber>()
                .and_then(CFNumber::as_i64)
                .map(Property::Number)
        })
        .or_else(|| {
            value
                .downcast_ref::<CFBoolean>()
                .map(|flag| Property::Flag(flag.value()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn properties(pairs: Vec<(&'static str, Property)>) -> Properties {
        pairs.into_iter().collect()
    }

    fn trackpad() -> Properties {
        properties(vec![
            (
                "Product",
                Property::Text("Paul\u{2019}s Magic Trackpad".to_string()),
            ),
            (
                "DeviceAddress",
                Property::Text("30-82-16-f2-24-90".to_string()),
            ),
            ("BatteryPercent", Property::Number(85)),
            ("BatteryStatusFlags", Property::Number(0)),
            ("HasBattery", Property::Flag(true)),
            ("Transport", Property::Text("Bluetooth".to_string())),
        ])
    }

    #[test]
    fn builds_a_device_from_every_key_it_reads() {
        let device = device(&trackpad(), Timestamp::from_unix(0)).expect("a battery reading");

        assert_eq!(device.name, "Paul\u{2019}s Magic Trackpad");
        assert_eq!(device.address.as_str(), "30-82-16-f2-24-90");
        assert_eq!(device.levels.main, Some(85));
        assert_eq!(device.charge, ChargeState::Discharging);
        assert_eq!(device.transport.as_deref(), Some("Bluetooth"));
        assert_eq!(device.source, Source::IoKit);
        assert!(device.connected);
    }

    #[test]
    fn a_plugged_in_device_reads_as_charging() {
        let mut charging = trackpad();
        charging.insert("BatteryStatusFlags", Property::Number(3));

        let device = device(&charging, Timestamp::from_unix(0)).expect("a battery reading");
        assert_eq!(device.charge, ChargeState::Charging);
    }

    #[test]
    fn charge_state_is_unknown_without_the_status_flags() {
        let mut no_flags = trackpad();
        no_flags.remove("BatteryStatusFlags");

        let device = device(&no_flags, Timestamp::from_unix(0)).expect("a battery reading");
        assert_eq!(device.charge, ChargeState::Unknown);
    }

    #[test]
    fn an_entry_without_a_battery_percent_is_not_a_reading() {
        let mut no_battery = trackpad();
        no_battery.remove("BatteryPercent");

        assert!(device(&no_battery, Timestamp::from_unix(0)).is_none());
    }

    #[test]
    fn has_battery_false_overrules_a_reported_percentage() {
        let mut denied = trackpad();
        denied.insert("HasBattery", Property::Flag(false));

        assert!(device(&denied, Timestamp::from_unix(0)).is_none());
    }

    #[test]
    fn an_unusable_address_drops_the_entry() {
        let mut bad_address = trackpad();
        bad_address.insert("DeviceAddress", Property::Text("nonsense".to_string()));

        assert!(device(&bad_address, Timestamp::from_unix(0)).is_none());
    }

    #[test]
    fn the_address_stands_in_for_a_missing_product_name() {
        let mut anonymous = trackpad();
        anonymous.remove("Product");

        let device = device(&anonymous, Timestamp::from_unix(0)).expect("a battery reading");
        assert_eq!(device.name, "30-82-16-f2-24-90");
    }

    #[test]
    fn an_out_of_range_percentage_leaves_the_level_absent() {
        for percent in [-1, 101, 255] {
            let mut odd = trackpad();
            odd.insert("BatteryPercent", Property::Number(percent));

            let device = device(&odd, Timestamp::from_unix(0)).expect("a battery reading");
            assert_eq!(device.levels.main, None, "at {percent}");
        }
    }
}
