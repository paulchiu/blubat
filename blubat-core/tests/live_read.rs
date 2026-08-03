//! Smoke tests against the real machine.
//!
//! Ignored by default: they need paired Bluetooth devices, which CI runners do
//! not have. Run them with `cargo test -- --ignored` on a desk.

use blubat_core::{ChargeState, Source};

#[test]
#[ignore = "needs a real machine with paired Bluetooth devices"]
fn reads_a_device_with_a_battery_from_at_least_one_source() {
    let reading = blubat_core::snapshot(std::path::Path::new("/nonexistent/readings.toml"));

    let with_battery: Vec<_> = reading.with_battery().collect();
    assert!(
        !with_battery.is_empty(),
        "no device reported a battery: {:#?}",
        reading.devices
    );

    for device in with_battery {
        assert!(device.levels.lowest().is_some_and(|level| level <= 100));
        assert_eq!(device.read_at, reading.read_at);

        if device.source == Source::SystemProfiler {
            assert_eq!(
                device.charge,
                ChargeState::Unknown,
                "{} has no charge state in this source",
                device.name
            );
        }
    }
}

#[test]
#[ignore = "needs a real machine with paired Bluetooth devices"]
fn every_address_is_unique_after_the_merge() {
    let reading = blubat_core::snapshot(std::path::Path::new("/nonexistent/readings.toml"));

    let mut addresses: Vec<&str> = reading
        .devices
        .iter()
        .map(|device| device.address.as_str())
        .collect();
    let total = addresses.len();
    addresses.sort_unstable();
    addresses.dedup();

    assert_eq!(addresses.len(), total, "the merge left a duplicate address");
}
