//! The bluetoothd source: the battery levels macOS itself already knows.
//!
//! macOS learns a level over channels no third party program can open. A
//! headset reports its battery in the hands free profile (`AT+IPHONEACCEV`)
//! and `bluetoothd` keeps what it hears, which is why System Settings shows a
//! Bose QC at 79% while neither IOKit nor `system_profiler` carries a number
//! for it. Since Monterey that cache is readable back off the paired device
//! itself, so the daemon can have the same level macOS has without speaking
//! any protocol of its own.
//!
//! Reading it needs IOBluetooth, which only the daemon process may touch, for
//! the same TCC reason [`crate::bmap`] may not be reached from a terminal.
//! This module owns the only part of the source that is not an Objective-C
//! call: what a device's cached percentages add up to. The properties
//! carrying them are private, so the sweep itself (`daemon::bluetoothd` in
//! the binary crate) checks for each one before reading it and degrades to no
//! readings at all when a macOS release takes them away.

/// The battery level in one device's cached percentages.
///
/// `bluetoothd` holds a percentage per battery and leaves the ones it has
/// learned nothing about at zero, so a value outside `1..=100` is no reading
/// rather than a flat battery. The level is the lowest of what remains,
/// because a multi battery device is as charged as its emptiest part, the
/// same rule [`crate::Device::active_level`] reads by. A device with nothing
/// cached at all is left unreported.
pub fn battery_level(percentages: &[u8]) -> Option<u8> {
    percentages
        .iter()
        .copied()
        .filter(|percentage| (1..=100).contains(percentage))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lowest_cached_percentage_stands_for_the_device() {
        assert_eq!(battery_level(&[96]), Some(96));
        assert_eq!(battery_level(&[79, 81]), Some(79));
        assert_eq!(battery_level(&[100, 1]), Some(1));
    }

    #[test]
    fn a_percentage_bluetoothd_never_populated_is_not_a_flat_battery() {
        assert_eq!(
            battery_level(&[0, 79, 0, 81]),
            Some(79),
            "zero is the unpopulated value, not the emptiest part"
        );
    }

    #[test]
    fn a_device_with_nothing_cached_is_no_reading_at_all() {
        assert_eq!(battery_level(&[]), None);
        assert_eq!(battery_level(&[0, 0, 0, 0]), None);
    }

    #[test]
    fn a_percentage_outside_the_range_is_ignored_rather_than_clamped() {
        assert_eq!(battery_level(&[101]), None);
        assert_eq!(battery_level(&[0xff, 42]), Some(42));
    }
}
