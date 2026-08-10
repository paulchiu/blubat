//! The bluetoothd sweep: the battery levels macOS has already learned, read
//! back off the paired devices and left in `readings.toml` beside the other
//! two sweeps' readings (see `blubat_core::bluetoothd` for what a device's
//! cached percentages amount to, and `blubat_core::readings` for the file the
//! three share).
//!
//! Everything [`super::bmap`]'s module doc says about TCC and the main thread
//! holds here too: this is the same IOBluetooth the BMAP sweep opens its
//! channel through, so it is reachable only from [`super::sweep::execute`],
//! which is the daemon's own pumped main thread. Nothing is waited on, since
//! a cache read answers or does not answer immediately, and no link is opened
//! at all.
//!
//! The properties carrying the cache are private. Apple publishes no battery
//! API on `IOBluetoothDevice`, and `batteryPercentSingle` and its siblings
//! have simply been there since Monterey, so any macOS release may take them
//! away without notice. Every one of them is therefore read only where
//! `respondsToSelector` says this system still has it, and a system that has
//! none of them sweeps to nothing rather than failing: blubat loses a source
//! it never had a guarantee of and keeps every other reading it takes.
//!
//! `batteryPercentCase` is left unread. The other four agree with what System
//! Settings shows; the case value does not reliably, and a level is only
//! worth reporting where blubat can stand behind it.

use blubat_core::{Address, SweepReading, Timestamp, bluetoothd_battery_level};

use objc2::runtime::AnyClass;
use objc2::{msg_send, sel};
use objc2_foundation::NSObjectProtocol;
use objc2_io_bluetooth::IOBluetoothDevice;

/// One paired device as macOS's cache describes it: what names it, whether
/// it is here, and every battery percentage held against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cached {
    pub(crate) address: String,
    pub(crate) name: String,
    pub(crate) connected: bool,
    pub(crate) percentages: Vec<u8>,
}

/// Somewhere the cached percentages are read, which a test fills with a fake.
pub(crate) trait Cache {
    /// Every device paired with this Mac, with whatever percentages macOS
    /// holds for each. A system with no such cache to read has none.
    fn paired(&self) -> Vec<Cached>;
}

/// The real one: the private battery properties on `IOBluetoothDevice`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bluetoothd;

/// One private percentage property, read only where this macOS has it.
///
/// Guard and read sit in the one expression so a property can never be added
/// here without the check that makes reading it safe.
macro_rules! cached_percentage {
    ($device:expr, $property:ident) => {
        // SAFETY: the guard is `respondsToSelector` on the very selector
        // being sent, and every one of these properties is an `unsigned char`.
        $device
            .respondsToSelector(sel!($property))
            .then(|| unsafe { msg_send![$device, $property] })
    };
}

/// The percentages macOS holds for one device, in no particular order:
/// [`bluetoothd_battery_level`] takes the lowest of them whatever they mean.
fn percentages(device: &IOBluetoothDevice) -> Vec<u8> {
    let cached: [Option<u8>; 4] = [
        cached_percentage!(device, batteryPercentSingle),
        cached_percentage!(device, batteryPercentCombined),
        cached_percentage!(device, batteryPercentLeft),
        cached_percentage!(device, batteryPercentRight),
    ];

    cached.into_iter().flatten().collect()
}

/// What the cache holds for one paired device.
fn cached(device: &IOBluetoothDevice) -> Option<Cached> {
    // SAFETY: the device's own connection state, identity and display name,
    // the same plain properties `super::bmap` already reaches this class for.
    let (connected, address, name) = unsafe {
        (
            device.isConnected(),
            device.addressString(),
            device.nameOrAddress(),
        )
    };

    Some(Cached {
        address: address?.to_string(),
        name: name?.to_string(),
        connected,
        percentages: percentages(device),
    })
}

impl Cache for Bluetoothd {
    fn paired(&self) -> Vec<Cached> {
        // A macOS without the class at all would abort the typed call below,
        // which is the one failure this source must survive in silence.
        if AnyClass::get(c"IOBluetoothDevice").is_none() {
            return Vec::new();
        }

        // SAFETY: a plain class method, nil when nothing is paired.
        let paired = unsafe { IOBluetoothDevice::pairedDevices() };

        paired
            .map(|devices| {
                devices
                    .to_vec()
                    .into_iter()
                    .filter_map(|device| device.downcast::<IOBluetoothDevice>().ok())
                    .filter_map(|device| cached(&device))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Runs the sweep across every connected device the cache has a level for.
///
/// A disconnected device is passed over: its cached percentages are whatever
/// they were when it last spoke, with nothing to date them by, so recording
/// them as a reading taken now would age a number that is already old. A
/// device whose percentages are all unpopulated, and one macOS names by an
/// address blubat cannot parse, are likewise silently no reading this sweep,
/// the discipline every other daemon sweep keeps.
pub(crate) fn sweep(cache: &dyn Cache, read_at: Timestamp) -> Vec<SweepReading> {
    cache
        .paired()
        .into_iter()
        .filter(|device| device.connected)
        .filter_map(|device| {
            let level = bluetoothd_battery_level(&device.percentages)?;
            let address = Address::parse(&device.address)?;

            Some(SweepReading::bluetoothd(
                address,
                device.name,
                level,
                read_at,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINCE: &str = "bc-87-fa-18-b0-b7";
    const AIRPODS: &str = "74-15-f5-02-8e-38";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    /// A cache holding exactly what it was built with.
    struct Fake(Vec<Cached>);

    impl Fake {
        fn holding(devices: Vec<(&str, &str, Vec<u8>)>) -> Self {
            Self(
                devices
                    .into_iter()
                    .map(|(address, name, percentages)| Cached {
                        address: address.to_string(),
                        name: name.to_string(),
                        connected: true,
                        percentages,
                    })
                    .collect(),
            )
        }

        fn disconnected(self) -> Self {
            Self(
                self.0
                    .into_iter()
                    .map(|device| Cached {
                        connected: false,
                        ..device
                    })
                    .collect(),
            )
        }
    }

    impl Cache for Fake {
        fn paired(&self) -> Vec<Cached> {
            self.0.clone()
        }
    }

    #[test]
    fn a_cached_percentage_becomes_one_reading_under_the_devices_own_address() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![79])]);

        assert_eq!(
            sweep(&fake, READ_AT),
            [SweepReading::bluetoothd(
                address(PRINCE),
                "Bose QC Headphones",
                79,
                READ_AT
            )]
        );
    }

    #[test]
    fn a_paired_device_that_is_not_here_is_never_read() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![79])]).disconnected();

        assert_eq!(sweep(&fake, READ_AT), []);
    }

    #[test]
    fn a_devices_emptiest_battery_is_the_level_it_reads_at() {
        let fake = Fake::holding(vec![(AIRPODS, "AirPods Pro", vec![0, 0, 79, 62])]);

        assert_eq!(sweep(&fake, READ_AT)[0].level, 62);
    }

    #[test]
    fn a_device_with_nothing_cached_yields_no_reading_at_all() {
        let fake = Fake::holding(vec![(PRINCE, "Bose QC Headphones", vec![0, 0, 0, 0])]);

        assert_eq!(sweep(&fake, READ_AT), []);
    }

    #[test]
    fn one_device_without_a_level_does_not_stop_the_rest_of_the_sweep() {
        let fake = Fake::holding(vec![
            (PRINCE, "Bose QC Headphones", Vec::new()),
            (AIRPODS, "AirPods Pro", vec![79]),
        ]);

        let readings = sweep(&fake, READ_AT);

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].name, "AirPods Pro");
    }

    #[test]
    fn an_address_blubat_cannot_parse_is_skipped_rather_than_guessed_at() {
        let fake = Fake::holding(vec![("not an address", "Somewhere", vec![79])]);

        assert_eq!(sweep(&fake, READ_AT), []);
    }

    #[test]
    fn macos_own_colon_form_addresses_read_back_as_blubats() {
        let fake = Fake::holding(vec![("BC:87:FA:18:B0:B7", "Bose QC Headphones", vec![79])]);

        assert_eq!(sweep(&fake, READ_AT)[0].address, address(PRINCE));
    }
}
