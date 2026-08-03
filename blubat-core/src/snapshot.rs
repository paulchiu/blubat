use std::collections::BTreeMap;

use serde::Serialize;

use crate::address::Address;
use crate::device::Device;
use crate::timestamp::Timestamp;

/// Every device macOS can see, from both sources, at one moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub read_at: Timestamp,
    pub devices: Vec<Device>,
    /// Whether the slow source is failing and its devices are held over from an
    /// earlier call. A frontend marks the reading rather than hiding it: the
    /// numbers are real, they have simply stopped being refreshed.
    pub degraded: bool,
    /// Input a source could not use and carried on past, for a frontend to place.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Snapshot {
    /// Devices whose name or address contains `needle`, case insensitively.
    pub fn matching<'a>(&'a self, needle: &str) -> impl Iterator<Item = &'a Device> + 'a {
        let needle = needle.to_string();

        self.devices
            .iter()
            .filter(move |device| device.matches(&needle))
    }

    /// Devices that reported a battery level, fresh or last seen.
    pub fn with_battery(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|device| device.has_battery())
    }
}

/// Merges both sources into one device list keyed on address.
///
/// Neither source is a superset of the other and the overlap is real: the
/// Magic Trackpad appears in both, with a battery level only in IOKit, so the
/// IOKit record wins. It keeps the device category from the other source,
/// which names it `Magic Trackpad` where IOKit only offers a transport.
///
/// `system_profiler` can also list one address twice within a section, once
/// connected and once not, and a live reading always beats a last seen one.
pub(crate) fn merge(
    iokit: Vec<Device>,
    profiler: Vec<Device>,
    read_at: Timestamp,
    warnings: Vec<String>,
) -> Snapshot {
    let mut merged: BTreeMap<Address, Device> = BTreeMap::new();

    for device in profiler {
        let held_is_live = merged
            .get(&device.address)
            .is_some_and(|held| held.connected);

        if device.connected || !held_is_live {
            merged.insert(device.address.clone(), device);
        }
    }

    for device in iokit {
        let kind = merged
            .get(&device.address)
            .and_then(|displaced| displaced.kind.clone());
        merged.insert(device.address.clone(), Device { kind, ..device });
    }

    let mut devices: Vec<Device> = merged.into_values().collect();
    devices.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.address.cmp(&b.address))
    });

    Snapshot {
        read_at,
        devices,
        degraded: false,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ChargeState, Levels, Source};

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn merged(iokit: Vec<Device>, profiler: Vec<Device>) -> Snapshot {
        merge(iokit, profiler, READ_AT, Vec::new())
    }

    fn device(name: &str, address: &str, source: Source) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
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

    fn trackpad_from_iokit() -> Device {
        Device {
            transport: Some("Bluetooth".to_string()),
            levels: Levels {
                main: Some(85),
                ..Levels::default()
            },
            charge: ChargeState::Discharging,
            ..device(
                "Paul\u{2019}s Magic Trackpad",
                "30-82-16-f2-24-90",
                Source::IoKit,
            )
        }
    }

    fn trackpad_from_profiler() -> Device {
        Device {
            kind: Some("Magic Trackpad".to_string()),
            connected: false,
            ..device(
                "Paul's Magic Trackpad",
                "30:82:16:F2:24:90",
                Source::SystemProfiler,
            )
        }
    }

    #[test]
    fn the_same_device_from_both_sources_collapses_to_the_iokit_reading() {
        let merged = merged(vec![trackpad_from_iokit()], vec![trackpad_from_profiler()]);

        let [trackpad] = &merged.devices[..] else {
            panic!("expected exactly one device, got {:?}", merged.devices);
        };
        assert_eq!(trackpad.source, Source::IoKit);
        assert_eq!(trackpad.levels.main, Some(85));
        assert_eq!(trackpad.charge, ChargeState::Discharging);
        assert!(trackpad.connected, "IOKit presence means connected");
        assert_eq!(trackpad.name, "Paul\u{2019}s Magic Trackpad");
    }

    #[test]
    fn the_displaced_reading_still_donates_the_device_category() {
        let merged = merged(vec![trackpad_from_iokit()], vec![trackpad_from_profiler()]);

        assert_eq!(merged.devices[0].kind.as_deref(), Some("Magic Trackpad"));
        assert_eq!(merged.devices[0].transport.as_deref(), Some("Bluetooth"));
    }

    #[test]
    fn devices_unique_to_one_source_all_survive() {
        let merged = merged(
            vec![trackpad_from_iokit()],
            vec![
                trackpad_from_profiler(),
                device("MX Keys M Mac", "de:df:38:f0:46:9b", Source::SystemProfiler),
                device("Bedroom", "d0:03:4b:0b:e6:4e", Source::SystemProfiler),
            ],
        );

        assert_eq!(
            merged
                .devices
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["Bedroom", "MX Keys M Mac", "Paul\u{2019}s Magic Trackpad"],
            "sorted by name, case insensitively"
        );
    }

    #[test]
    fn an_iokit_only_device_needs_no_counterpart() {
        let merged = merged(vec![trackpad_from_iokit()], Vec::new());

        assert_eq!(merged.devices.len(), 1);
        assert_eq!(merged.devices[0].kind, None);
        assert_eq!(merged.read_at, READ_AT);
    }

    #[test]
    fn one_address_listed_twice_keeps_the_connected_reading() {
        let live = Device {
            levels: Levels {
                main: Some(10),
                ..Levels::default()
            },
            ..device("Twice Listed", "11:22:33:44:55:66", Source::SystemProfiler)
        };
        let stale = Device {
            connected: false,
            levels: Levels {
                main: Some(90),
                ..Levels::default()
            },
            ..live.clone()
        };

        for profiler in [
            vec![live.clone(), stale.clone()],
            vec![stale.clone(), live.clone()],
        ] {
            let merged = merged(Vec::new(), profiler);

            let [device] = &merged.devices[..] else {
                panic!("expected exactly one device, got {:?}", merged.devices);
            };
            assert!(device.connected);
            assert_eq!(device.levels.main, Some(10));
            assert_eq!(device.active_level(), Some(10));
        }
    }

    #[test]
    fn devices_sharing_a_name_are_ordered_by_address() {
        let merged = merged(
            Vec::new(),
            vec![
                device("AirPods Pro", "bb:bb:bb:bb:bb:bb", Source::SystemProfiler),
                device("AirPods Pro", "aa:aa:aa:aa:aa:aa", Source::SystemProfiler),
            ],
        );

        assert_eq!(
            merged
                .devices
                .iter()
                .map(|device| device.address.as_str())
                .collect::<Vec<_>>(),
            ["aa-aa-aa-aa-aa-aa", "bb-bb-bb-bb-bb-bb"]
        );
    }

    #[test]
    fn selects_devices_by_match_and_by_battery() {
        let merged = merged(
            vec![trackpad_from_iokit()],
            vec![device(
                "MX Keys M Mac",
                "de:df:38:f0:46:9b",
                Source::SystemProfiler,
            )],
        );

        assert_eq!(merged.matching("keys").count(), 1);
        assert_eq!(merged.matching("de-df").count(), 1);
        assert_eq!(merged.matching("nothing here").count(), 0);
        assert_eq!(
            merged
                .with_battery()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>(),
            ["Paul\u{2019}s Magic Trackpad"]
        );
    }
}
