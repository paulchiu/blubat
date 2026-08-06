//! The GATT source: a BLE peripheral's own Battery Service level.
//!
//! A third party Bluetooth Low Energy peripheral, an MX Keys or a Keychron
//! among them, publishes its battery through the standard Battery Service
//! (`180F`) and nothing else. That is what macOS Settings shows for it, and
//! it is not what either of blubat's other sources read: IOKit's
//! `BatteryPercent` covers Apple HID peripherals only, and `system_profiler`
//! carries a battery field for AirPods and little else. Such a device is
//! therefore unreported through both while sitting a menu away at 95%, which
//! is the whole reason this source exists.
//!
//! Reading the service needs CoreBluetooth, which only the daemon process may
//! touch, for the same TCC reason [`crate::bmap`] may not be reached from a
//! terminal. This module owns everything `blubat-core` can be about GATT
//! without that dependency: which devices a sweep may read, how a peripheral
//! is matched back to one of them, and what a Battery Level value means.
//!
//! Matching is by name, exactly. CoreBluetooth identifies a peripheral by a
//! per host UUID rather than by its Bluetooth address, and the
//! `CoreBluetoothCache` mapping the two used to be read from is no longer
//! present in `/Library/Preferences/com.apple.Bluetooth.plist` on a current
//! macOS, so there is no supported way to ask a peripheral for the address
//! the daemon's own device list keys on. What both sides do carry is the
//! name macOS shows, so a peripheral whose name is exactly one the device
//! list already knows is recorded under that device's address, and one that
//! matches nothing is skipped in silence, the discipline every other daemon
//! sweep keeps.

use crate::address::Address;
use crate::device::{Device, Source};

/// The Bluetooth SIG Battery Service, as CoreBluetooth wants it spelled.
pub const BATTERY_SERVICE_UUID: &str = "180F";

/// The Battery Level characteristic inside it: one byte of whole percent.
pub const BATTERY_LEVEL_UUID: &str = "2A19";

/// Whether a sweep may read this device's Battery Service at all.
///
/// A device another source already has a level for is left alone, so a GATT
/// reading can never shadow a direct one (see [`crate::readings::merge`],
/// which holds the same line for a reading already on disk). The exception is
/// a device GATT itself last answered for: the level on it is this source's
/// own, so passing it over would freeze that device at its first reading
/// forever.
fn readable(device: &Device) -> bool {
    device.connected && (device.source == Source::Gatt || !device.has_battery())
}

/// The names this sweep's peripherals are matched against.
///
/// A peripheral named nothing on this list is never even connected to, which
/// is what keeps the sweep to the devices blubat has something to gain from
/// reading.
pub fn candidates(devices: &[Device]) -> Vec<String> {
    devices
        .iter()
        .filter(|device| readable(device))
        .map(|device| device.name.clone())
        .collect()
}

/// The address a peripheral of this name should be recorded under.
///
/// Two devices sharing a name is the one ambiguity here, and the first is
/// taken: a reading under the wrong one of two identically named devices is
/// no worse than the reading blubat would otherwise not have at all.
pub fn matched(devices: &[Device], peripheral_name: &str) -> Option<Address> {
    devices
        .iter()
        .find(|device| readable(device) && device.name == peripheral_name)
        .map(|device| device.address.clone())
}

/// The battery level in one Battery Level characteristic value.
///
/// The characteristic is defined as a single unsigned byte of whole percent.
/// An empty value, or one over 100, is no reading rather than a clamped one:
/// a peripheral answering something blubat does not understand is better left
/// unreported than reported wrongly.
pub fn battery_level(value: &[u8]) -> Option<u8> {
    value.first().copied().filter(|level| *level <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ChargeState, Levels};
    use crate::timestamp::Timestamp;

    const KEYS: &str = "de-df-38-f0-46-9b";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn device(name: &str, raw: &str, source: Source) -> Device {
        Device {
            address: address(raw),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn keys() -> Device {
        device("MX Keys M Mac", KEYS, Source::SystemProfiler)
    }

    fn levelled(level: u8, device: Device) -> Device {
        Device {
            levels: Levels {
                main: Some(level),
                ..Levels::default()
            },
            ..device
        }
    }

    #[test]
    fn the_service_and_characteristic_are_the_documented_sixteen_bit_uuids() {
        assert_eq!(BATTERY_SERVICE_UUID, "180F");
        assert_eq!(BATTERY_LEVEL_UUID, "2A19");
    }

    #[test]
    fn a_connected_device_no_source_has_a_level_for_is_a_candidate() {
        assert_eq!(candidates(&[keys()]), ["MX Keys M Mac"]);
        assert_eq!(matched(&[keys()], "MX Keys M Mac"), Some(address(KEYS)));
    }

    #[test]
    fn a_disconnected_device_is_never_a_candidate() {
        let gone = Device {
            connected: false,
            ..keys()
        };

        assert_eq!(candidates(std::slice::from_ref(&gone)), [] as [String; 0]);
        assert_eq!(matched(&[gone], "MX Keys M Mac"), None);
    }

    #[test]
    fn a_device_another_source_already_has_a_level_for_is_never_a_candidate() {
        for source in [Source::IoKit, Source::SystemProfiler, Source::Bmap] {
            let reported = levelled(40, device("MX Keys M Mac", KEYS, source));

            assert_eq!(
                candidates(std::slice::from_ref(&reported)),
                [] as [String; 0],
                "{source}"
            );
            assert_eq!(matched(&[reported], "MX Keys M Mac"), None, "{source}");
        }
    }

    #[test]
    fn a_device_gatt_itself_took_over_stays_a_candidate_so_its_level_keeps_moving() {
        let taken_over = levelled(40, device("MX Keys M Mac", KEYS, Source::Gatt));

        assert_eq!(
            candidates(std::slice::from_ref(&taken_over)),
            ["MX Keys M Mac"]
        );
        assert_eq!(matched(&[taken_over], "MX Keys M Mac"), Some(address(KEYS)));
    }

    #[test]
    fn a_peripheral_name_no_device_carries_matches_nothing() {
        assert_eq!(matched(&[keys()], "Keychron K3"), None);
        assert_eq!(
            matched(&[keys()], "mx keys m mac"),
            None,
            "the names macOS shows on both sides are compared exactly"
        );
        assert_eq!(matched(&[], "MX Keys M Mac"), None);
    }

    #[test]
    fn the_first_of_two_devices_sharing_a_name_takes_the_reading() {
        let second = device("MX Keys M Mac", "aa-bb-cc-dd-ee-ff", Source::SystemProfiler);

        assert_eq!(
            matched(&[keys(), second], "MX Keys M Mac"),
            Some(address(KEYS))
        );
    }

    #[test]
    fn a_single_byte_of_whole_percent_is_the_reading() {
        assert_eq!(battery_level(&[95]), Some(95));
        assert_eq!(battery_level(&[0]), Some(0), "empty is a level");
        assert_eq!(battery_level(&[100]), Some(100));
        assert_eq!(
            battery_level(&[42, 0xff]),
            Some(42),
            "only the first byte is defined, so the rest is ignored"
        );
    }

    #[test]
    fn an_empty_or_out_of_range_value_is_no_reading_at_all() {
        assert_eq!(battery_level(&[]), None);
        assert_eq!(battery_level(&[101]), None);
        assert_eq!(battery_level(&[0xff]), None);
    }
}
