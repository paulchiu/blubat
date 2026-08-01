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
}

impl Snapshot {
    /// Devices whose name or address contains `needle`, case insensitively.
    pub fn matching<'a>(&'a self, needle: &'a str) -> impl Iterator<Item = &'a Device> {
        self.devices
            .iter()
            .filter(move |device| device.matches(needle))
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
pub(crate) fn merge(iokit: Vec<Device>, profiler: Vec<Device>, read_at: Timestamp) -> Snapshot {
    let mut merged: BTreeMap<Address, Device> = profiler
        .into_iter()
        .map(|device| (device.address.clone(), device))
        .collect();

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

    Snapshot { read_at, devices }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ChargeState, Levels, Source};

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn device(name: &str, address: &str, source: Source) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
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
        let merged = merge(
            vec![trackpad_from_iokit()],
            vec![trackpad_from_profiler()],
            READ_AT,
        );

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
        let merged = merge(
            vec![trackpad_from_iokit()],
            vec![trackpad_from_profiler()],
            READ_AT,
        );

        assert_eq!(merged.devices[0].kind.as_deref(), Some("Magic Trackpad"));
        assert_eq!(merged.devices[0].transport.as_deref(), Some("Bluetooth"));
    }

    #[test]
    fn devices_unique_to_one_source_all_survive() {
        let merged = merge(
            vec![trackpad_from_iokit()],
            vec![
                trackpad_from_profiler(),
                device("MX Keys M Mac", "de:df:38:f0:46:9b", Source::SystemProfiler),
                device("Bedroom", "d0:03:4b:0b:e6:4e", Source::SystemProfiler),
            ],
            READ_AT,
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
        let merged = merge(vec![trackpad_from_iokit()], Vec::new(), READ_AT);

        assert_eq!(merged.devices.len(), 1);
        assert_eq!(merged.devices[0].kind, None);
        assert_eq!(merged.read_at, READ_AT);
    }

    #[test]
    fn selects_devices_by_match_and_by_battery() {
        let merged = merge(
            vec![trackpad_from_iokit()],
            vec![device(
                "MX Keys M Mac",
                "de:df:38:f0:46:9b",
                Source::SystemProfiler,
            )],
            READ_AT,
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
