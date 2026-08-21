//! The daemon's handoff file: every battery level its own sweeps read, left
//! in `readings.toml` for every frontend to merge back in.
//!
//! Three sources write here. [`crate::bluetoothd`] reads back the levels
//! macOS itself has cached, [`crate::bmap`] asks a Bose headset over
//! Bluetooth Classic RFCOMM and [`crate::gatt`] reads a BLE peripheral's own
//! Battery Service, and each reading names which of the three took it. None
//! can be reached from this crate: every one of them needs a framework only
//! the daemon process may touch (see those modules for why), so this is the
//! whole of what `blubat-core` can be about a sweep. The record, the file,
//! and the precedence a reading merges back in under.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::atomic;
use crate::device::{ChargeState, Device, Levels, Source};
use crate::error::{Error, Result};
use crate::snapshot::Snapshot;
use crate::timestamp::Timestamp;

/// One battery reading a daemon sweep took, as `readings.toml` holds it.
///
/// `connected` is the device's own live state at `read_at`, not merely
/// whether this reading is fresh: a sweep that answers always carries
/// `true`, and one that carries a reading forward from an earlier sweep
/// (see [`carry_forward`]) copies whatever the daemon's own device list most
/// recently reported for that address, so a device that has actually gone
/// away is shown as last seen rather than as connected with a stale level.
/// This flag only stands where nothing fresher exists: when [`merge`] finds
/// the address still in the live scan, the scan's own `connected` wins,
/// since a reading may be days old and the scan is this poll's own answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    pub address: Address,
    pub name: String,
    pub level: u8,
    pub connected: bool,
    pub read_at: Timestamp,
    pub source: Source,
}

impl Reading {
    /// A level a Bose headset answered a BMAP query with.
    pub fn bmap(address: Address, name: impl Into<String>, level: u8, read_at: Timestamp) -> Self {
        Self::taken(Source::Bmap, address, name, level, read_at)
    }

    /// A level a BLE peripheral's Battery Service answered with.
    pub fn gatt(address: Address, name: impl Into<String>, level: u8, read_at: Timestamp) -> Self {
        Self::taken(Source::Gatt, address, name, level, read_at)
    }

    /// A level read back out of what macOS's own `bluetoothd` had cached.
    pub fn bluetoothd(
        address: Address,
        name: impl Into<String>,
        level: u8,
        read_at: Timestamp,
    ) -> Self {
        Self::taken(Source::Bluetoothd, address, name, level, read_at)
    }

    /// Always connected: a device that did not answer never reaches here.
    fn taken(
        source: Source,
        address: Address,
        name: impl Into<String>,
        level: u8,
        read_at: Timestamp,
    ) -> Self {
        Self {
            address,
            name: name.into(),
            level,
            connected: true,
            read_at,
            source,
        }
    }
}

/// Folds a sweep's fresh readings into what was already on disk.
///
/// A reading this sweep refreshed always wins. One it did not is carried
/// forward unchanged rather than dropped, the way [`crate::poll::read_slow`]
/// holds the last good `system_profiler` devices over a failed read: a
/// device merely missed this sweep (a transient failure, a timeout) keeps
/// aging in place toward `stale_after` instead of disappearing from the file
/// outright. That is also what lets both sources share one file, since each
/// carries the other's addresses forward untouched. Its `connected` flag is
/// refreshed from `devices` when that address is still known there, so a
/// device `devices` reports as actually gone is shown as last seen
/// immediately rather than waiting out the stale window; an address
/// `devices` no longer mentions at all keeps whatever `connected` it last
/// carried.
pub fn carry_forward(
    previous: Vec<Reading>,
    fresh: Vec<Reading>,
    devices: &[Device],
) -> Vec<Reading> {
    let mut merged: Vec<Reading> = previous
        .into_iter()
        .filter(|old| !fresh.iter().any(|new| new.address == old.address))
        .map(|old| {
            let connected = devices
                .iter()
                .find(|device| device.address == old.address)
                .map_or(old.connected, |device| device.connected);

            Reading { connected, ..old }
        })
        .collect();

    merged.extend(fresh);
    merged
}

/// The handoff file: every reading the daemon's last pass produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(rename = "reading", default, skip_serializing_if = "Vec::is_empty")]
    readings: Vec<Reading>,
}

/// Writes the sweep atomically, the same idiom every other state file uses.
///
/// # Errors
///
/// Returns [`Error::Format`] if the readings cannot be serialised to TOML, or
/// [`Error::Io`] if the file cannot be written.
pub fn save(path: &Path, readings: &[Reading]) -> Result<()> {
    let document = Document {
        readings: readings.to_vec(),
    };

    toml::to_string(&document)
        .map_err(|error| Error::Format(format!("readings file is unwritable: {error}")))
        .and_then(|contents| atomic::write(path, &contents))
}

/// Loads the handoff file, treating anything unusable as no sweep data at
/// all rather than an error: a machine with no daemon running, or one under
/// an older blubat that never wrote this file, behaves exactly as if these
/// sources did not exist.
pub fn load(path: &Path) -> Vec<Reading> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| toml::from_str::<Document>(&contents).ok())
        .map(|document| document.readings)
        .unwrap_or_default()
}

/// Whether a source may only ever fill a gap rather than displace a level.
///
/// GATT reads arbitrary third party peripherals and bluetoothd reads a cache
/// macOS filled from who knows when, so for both of them a level another
/// source already has is the better reading. Only a level the source itself
/// last left is refreshed, which is what keeps a device it has taken over
/// moving instead of frozen at its first reading.
fn fills_gaps(source: Source) -> bool {
    matches!(source, Source::Gatt | Source::Bluetoothd)
}

/// Whether the device already merged in outranks this reading.
///
/// IOKit is authoritative over every daemon sweep, the same way it already is
/// over `system_profiler` in [`crate::snapshot::merge`]. A BMAP reading beats
/// everything below that, since the only battery value `system_profiler` ever
/// carries for a supported Bose is a stale placeholder. The two gap filling
/// sources are the other way round, holding back wherever anything else has
/// already answered (see [`fills_gaps`], and [`crate::gatt::candidates`],
/// which will not offer a device up twice unless the level on it is GATT's
/// own).
fn outranks(device: &Device, reading: &Reading) -> bool {
    device.source == Source::IoKit
        || (fills_gaps(reading.source) && device.source != reading.source && device.has_battery())
}

/// Overlays the daemon's sweep readings onto an already merged snapshot.
///
/// The device category and the vendor and product ids a displaced record
/// carried are kept, the way an IOKit reading keeps the category in
/// [`crate::snapshot::merge`]: dropping the ids here would make the device a
/// BMAP sweep just read invisible to the next one.
///
/// `connected` is kept from the displaced record too, not the reading: the
/// snapshot is this poll's own live scan, so it is always at least as fresh
/// as a reading, which may have been taken sweeps ago. Only when the address
/// is not in the snapshot at all does the reading's own `connected` stand,
/// since nothing fresher exists for it.
pub(crate) fn merge(mut snapshot: Snapshot, readings: &[Reading]) -> Snapshot {
    for reading in readings {
        let index = snapshot
            .devices
            .iter()
            .position(|device| device.address == reading.address);
        let displaced = index.map(|index| &snapshot.devices[index]);
        if displaced.is_some_and(|device| outranks(device, reading)) {
            continue;
        }

        let device = Device {
            address: reading.address.clone(),
            name: reading.name.clone(),
            kind: displaced.and_then(|device| device.kind.clone()),
            transport: None,
            vendor_id: displaced.and_then(|device| device.vendor_id),
            product_id: displaced.and_then(|device| device.product_id),
            levels: Levels {
                main: Some(reading.level),
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source: reading.source,
            connected: displaced.map_or(reading.connected, |device| device.connected),
            read_at: reading.read_at,
        };

        match index {
            Some(i) => snapshot.devices[i] = device,
            None => snapshot.devices.push(device),
        }
    }

    snapshot.devices.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.address.cmp(&b.address))
    });

    snapshot
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const PRINCE: &str = "bc-87-fa-18-b0-b7";
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
            vendor_id: Some(0x009E),
            product_id: Some(0x4075),
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn bose(connected: bool) -> Device {
        Device {
            connected,
            ..device("Bose QC Headphones", PRINCE, Source::SystemProfiler)
        }
    }

    fn reading(level: u8) -> Reading {
        Reading::bmap(address(PRINCE), "Bose QC Headphones", level, READ_AT)
    }

    fn keys_reading(level: u8) -> Reading {
        Reading::gatt(address(KEYS), "MX Keys M Mac", level, READ_AT)
    }

    fn cached_reading(level: u8) -> Reading {
        Reading::bluetoothd(address(PRINCE), "Bose QC Headphones", level, READ_AT)
    }

    fn snapshot(devices: Vec<Device>) -> Snapshot {
        Snapshot {
            read_at: READ_AT,
            devices,
            degraded: false,
            warnings: Vec::new(),
        }
    }

    fn profiler_placeholder() -> Device {
        Device {
            kind: Some("Headphones".to_string()),
            ..bose(false)
        }
    }

    fn connected_profiler_placeholder() -> Device {
        Device {
            connected: true,
            ..profiler_placeholder()
        }
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

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "blubat-readings-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn readings_file(&self) -> std::path::PathBuf {
            self.0.join("readings.toml")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_written_sweep_reads_back_unchanged() {
        let scratch = Scratch::new();

        save(&scratch.readings_file(), &[reading(83)]).expect("writes");

        assert_eq!(load(&scratch.readings_file()), [reading(83)]);
    }

    #[test]
    fn a_missing_or_unparsable_file_is_no_sweep_data_rather_than_an_error() {
        let scratch = Scratch::new();

        assert_eq!(load(&scratch.readings_file()), []);

        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        fs::write(scratch.readings_file(), "not toml at all {{").expect("a written file");
        assert_eq!(load(&scratch.readings_file()), []);
    }

    #[test]
    fn every_source_shares_the_one_file_and_reads_back_naming_itself() {
        let scratch = Scratch::new();
        let swept = [reading(50), keys_reading(95), cached_reading(79)];

        save(&scratch.readings_file(), &swept).expect("writes");

        let contents = fs::read_to_string(scratch.readings_file()).expect("a written file");
        assert!(contents.contains("source = \"bmap\""), "{contents}");
        assert!(contents.contains("source = \"gatt\""), "{contents}");
        assert!(contents.contains("source = \"bluetoothd\""), "{contents}");
        assert_eq!(load(&scratch.readings_file()), swept);
    }

    #[test]
    fn a_device_missed_this_sweep_keeps_its_previous_reading_rather_than_vanishing() {
        let merged = carry_forward(vec![reading(76)], Vec::new(), &[bose(true)]);

        assert_eq!(
            merged,
            [reading(76)],
            "a still connected device merely missed this sweep, not gone"
        );
    }

    #[test]
    fn a_fresh_reading_replaces_the_previous_one_for_the_same_address() {
        let merged = carry_forward(vec![reading(76)], vec![reading(80)], &[]);

        assert_eq!(merged, [reading(80)]);
    }

    #[test]
    fn one_sources_fold_carries_the_others_readings_forward_untouched() {
        let merged = carry_forward(
            vec![reading(76), keys_reading(95)],
            vec![reading(80)],
            &[bose(true)],
        );

        assert_eq!(
            merged,
            [keys_reading(95), reading(80)],
            "a BMAP fold refreshes only its own address and leaves GATT's alone"
        );
    }

    #[test]
    fn a_device_devices_reports_as_actually_gone_is_carried_forward_as_disconnected() {
        let merged = carry_forward(vec![reading(76)], Vec::new(), &[bose(false)]);

        assert_eq!(
            merged,
            [Reading {
                connected: false,
                ..reading(76)
            }],
            "the level and read_at stay, but connected reflects the live device list"
        );
    }

    #[test]
    fn an_address_devices_no_longer_mentions_at_all_keeps_its_last_known_connected_state() {
        let merged = carry_forward(vec![reading(76)], Vec::new(), &[]);

        assert_eq!(
            merged,
            [reading(76)],
            "no fresher information than what the reading already carried"
        );
    }

    #[test]
    fn a_reading_for_a_disconnected_device_merges_as_last_seen_rather_than_connected() {
        let last_seen = Reading {
            connected: false,
            ..reading(76)
        };

        let merged = merge(snapshot(Vec::new()), &[last_seen]);

        assert!(
            !merged.devices[0].connected,
            "a disconnected reading must not merge in as connected"
        );
    }

    #[test]
    fn a_fresh_bmap_reading_beats_a_system_profiler_value_for_the_same_address() {
        let merged = merge(snapshot(vec![profiler_placeholder()]), &[reading(76)]);

        let [device] = &merged.devices[..] else {
            panic!("expected exactly one device, got {:?}", merged.devices);
        };
        assert_eq!(device.source, Source::Bmap);
        assert_eq!(device.levels.main, Some(76));
        assert_eq!(device.charge, ChargeState::Unknown);
        assert!(
            !device.connected,
            "the profiler record is what the live scan just reported for this address, and it said not connected"
        );
        assert_eq!(
            device.kind.as_deref(),
            Some("Headphones"),
            "the device category the profiler record carried is kept"
        );
    }

    #[test]
    fn a_saved_readings_connected_flag_never_outlives_what_the_live_scan_just_reported() {
        let merged = merge(snapshot(vec![profiler_placeholder()]), &[reading(76)]);

        assert_eq!(
            merged.devices[0].levels.main,
            Some(76),
            "the level comes from the saved reading"
        );
        assert!(
            !merged.devices[0].connected,
            "connected comes from this poll's live scan, not a reading that may be days old"
        );
    }

    #[test]
    fn a_bmap_reading_preserves_the_ids_the_next_sweep_needs_to_find_it_again() {
        let merged = merge(
            snapshot(vec![connected_profiler_placeholder()]),
            &[reading(76)],
        );

        assert_eq!(
            crate::bmap::candidates(&merged.devices),
            [(address(PRINCE), "Bose QC Headphones".to_string(), 8)],
            "a device the merge just overwrote must still be a candidate next sweep"
        );
    }

    #[test]
    fn an_address_iokit_already_reported_is_left_untouched_by_any_source() {
        for reading in [reading(76), keys_reading(95), cached_reading(79)] {
            let held = levelled(12, device("Held", reading.address.as_str(), Source::IoKit));

            let merged = merge(snapshot(vec![held.clone()]), &[reading]);

            assert_eq!(merged.devices, [held]);
        }
    }

    #[test]
    fn a_gatt_reading_never_shadows_a_device_another_source_already_has_a_level_for() {
        for source in [Source::SystemProfiler, Source::Bmap, Source::Bluetoothd] {
            let reported = levelled(40, device("MX Keys M Mac", KEYS, source));

            let merged = merge(snapshot(vec![reported.clone()]), &[keys_reading(95)]);

            assert_eq!(
                merged.devices,
                [reported],
                "{source} already answered, and this sweep being fresher does not make it better"
            );
        }
    }

    #[test]
    fn a_gatt_reading_refreshes_the_level_gatt_itself_last_left() {
        let taken_over = levelled(40, device("MX Keys M Mac", KEYS, Source::Gatt));

        let merged = merge(snapshot(vec![taken_over]), &[keys_reading(95)]);

        assert_eq!(merged.devices[0].levels.main, Some(95));
        assert_eq!(merged.devices[0].source, Source::Gatt);
    }

    #[test]
    fn a_gatt_reading_fills_in_a_device_no_source_has_a_level_for() {
        let unreported = device("MX Keys M Mac", KEYS, Source::SystemProfiler);

        let merged = merge(snapshot(vec![unreported]), &[keys_reading(95)]);

        assert_eq!(merged.devices[0].source, Source::Gatt);
        assert_eq!(merged.devices[0].levels.main, Some(95));
    }

    #[test]
    fn a_bluetoothd_reading_never_shadows_a_device_another_source_already_has_a_level_for() {
        for source in [Source::SystemProfiler, Source::Bmap, Source::Gatt] {
            let reported = levelled(40, device("Bose QC Headphones", PRINCE, source));

            let merged = merge(snapshot(vec![reported.clone()]), &[cached_reading(79)]);

            assert_eq!(
                merged.devices,
                [reported],
                "{source} answered for itself, which beats anything macOS cached"
            );
        }
    }

    #[test]
    fn a_bluetoothd_reading_refreshes_the_level_bluetoothd_itself_last_left() {
        let cached = levelled(40, device("Bose QC Headphones", PRINCE, Source::Bluetoothd));

        let merged = merge(snapshot(vec![cached]), &[cached_reading(79)]);

        assert_eq!(merged.devices[0].levels.main, Some(79));
        assert_eq!(merged.devices[0].source, Source::Bluetoothd);
    }

    #[test]
    fn a_bluetoothd_reading_fills_in_a_device_no_source_has_a_level_for() {
        let merged = merge(
            snapshot(vec![profiler_placeholder()]),
            &[cached_reading(79)],
        );

        assert_eq!(merged.devices[0].source, Source::Bluetoothd);
        assert_eq!(merged.devices[0].levels.main, Some(79));
    }

    #[test]
    fn a_bmap_reading_displaces_a_level_the_cache_left() {
        let cached = levelled(40, device("Bose QC Headphones", PRINCE, Source::Bluetoothd));

        let merged = merge(snapshot(vec![cached]), &[reading(76)]);

        assert_eq!(merged.devices[0].levels.main, Some(76));
        assert_eq!(
            merged.devices[0].source,
            Source::Bmap,
            "the headset's own answer is better than what macOS had cached"
        );
    }

    #[test]
    fn a_reading_for_an_address_nothing_else_reported_is_added() {
        let merged = merge(snapshot(Vec::new()), &[reading(60)]);

        assert_eq!(merged.devices.len(), 1);
        assert_eq!(merged.devices[0].address, address(PRINCE));
        assert_eq!(merged.devices[0].levels.main, Some(60));
    }

    #[test]
    fn readings_from_both_sources_merge_side_by_side() {
        let merged = merge(snapshot(Vec::new()), &[reading(60), keys_reading(95)]);

        assert_eq!(
            merged
                .devices
                .iter()
                .map(|device| (device.name.as_str(), device.source))
                .collect::<Vec<_>>(),
            [
                ("Bose QC Headphones", Source::Bmap),
                ("MX Keys M Mac", Source::Gatt)
            ]
        );
    }

    #[test]
    fn a_stale_reading_still_merges_and_carries_its_own_age() {
        let old = Timestamp::from_unix(READ_AT.unix() - 3_600);
        let aged = Reading::bmap(address(PRINCE), "Bose QC Headphones", 40, old);

        let merged = merge(snapshot(Vec::new()), std::slice::from_ref(&aged));

        assert_eq!(
            merged.devices[0].read_at, old,
            "merge does not filter by age; staleness is judged from read_at same as any other source"
        );
    }

    #[test]
    fn a_device_untouched_by_any_reading_is_unaffected() {
        let other = device("Elsewhere", "30-82-16-f2-24-90", Source::IoKit);

        let merged = merge(snapshot(vec![other.clone()]), &[reading(76)]);

        assert!(merged.devices.contains(&other));
        assert_eq!(merged.devices.len(), 2);
    }
}
