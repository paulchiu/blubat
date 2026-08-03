//! The BMAP source: a Bose headset's battery level, read over Bluetooth
//! Classic RFCOMM.
//!
//! macOS exposes no public API for a Bluetooth Classic headphone's battery.
//! IOKit's registry covers Apple HID peripherals only, and `system_profiler`
//! carries a battery field for non-Apple headphones only intermittently, so
//! a Bose stays unreported through both of blubat's other sources most of
//! the time. BMAP is Bose's own protocol for a paired accessory to ask a
//! headset things over RFCOMM, undocumented by Bose but reverse engineered
//! by the community: this module implements only what the aaronsb/bosectl
//! project verified (building on the based-connect project before it), and
//! sends only the GET operator of the battery function block. A SET is
//! authenticated on newer firmware and blubat never attempts one.
//!
//! Actually opening a channel needs IOBluetooth, which only the daemon
//! process may touch: macOS attributes Bluetooth access through TCC to the
//! process responsible for it, and under a terminal that is the terminal
//! rather than blubat, so the daemon's own `bmap` module is the only place
//! in the workspace that links against it. This module owns everything
//! blubat-core can be about BMAP without that dependency: the wire format,
//! which product ids are supported, and the handoff file the daemon leaves
//! for every frontend to merge in as a data source of its own.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::atomic;
use crate::device::{ChargeState, Device, Levels, Source};
use crate::error::{Error, Result};
use crate::snapshot::Snapshot;
use crate::timestamp::Timestamp;

/// Bose's vendor id, as both `system_profiler` and IOKit report it.
const BOSE_VENDOR_ID: u16 = 0x009E;

/// Function block 2 (battery), function 2, operator GET (1), empty payload:
/// the only frame this source ever writes to a device.
pub const BATTERY_QUERY: [u8; 4] = [0x02, 0x02, 0x01, 0x00];

/// The RFCOMM channel a known Bose product id answers BMAP queries on.
///
/// Only these two product ids are community verified against real hardware;
/// anything else is skipped rather than guessed at, so a headset blubat has
/// never been confirmed against is left alone rather than probed.
fn channel_for(vendor_id: u16, product_id: u16) -> Option<u8> {
    if vendor_id != BOSE_VENDOR_ID {
        return None;
    }

    match product_id {
        0x4075 => Some(8), // QuietComfort Headphones (2023), "prince"
        0x4082 => Some(2), // QC Ultra, second generation, "wolverine"
        _ => None,
    }
}

/// The connected, BMAP capable devices in one reading: each one's address,
/// name and the RFCOMM channel to query it on.
///
/// Only a device with a known vendor and product id and a live link is a
/// candidate. blubat never probes a device it cannot identify by these ids
/// and never scans for one.
pub fn candidates(devices: &[Device]) -> Vec<(Address, String, u8)> {
    devices
        .iter()
        .filter(|device| device.connected)
        .filter_map(|device| {
            let channel = channel_for(device.vendor_id?, device.product_id?)?;

            Some((device.address.clone(), device.name.clone(), channel))
        })
        .collect()
}

/// Accumulates RFCOMM bytes across delegate callbacks and yields a battery
/// level once a complete matching STATUS frame has arrived.
///
/// A complete frame for a different function block is dropped rather than
/// blocking the one being waited for, since the query answers with exactly
/// one frame but nothing stops another from arriving first.
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds newly arrived bytes in, returning the battery level the moment
    /// a complete matching frame is in the buffer. Bytes belonging to a
    /// frame still in flight are held until the rest of it arrives.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<u8> {
        self.buffer.extend_from_slice(bytes);

        while self.buffer.len() >= 4 {
            let frame_len = 4 + self.buffer[3] as usize;
            if self.buffer.len() < frame_len {
                break;
            }

            let frame: Vec<u8> = self.buffer.drain(..frame_len).collect();
            if let Some(level) = battery_level(&frame) {
                return Some(level);
            }
        }

        None
    }
}

/// The battery level in one complete frame.
///
/// Matches only the STATUS response to a battery GET: block 2, function 2,
/// operator 3, and a payload of at least one byte whose first byte is a
/// percentage. The 2023 generation answers with a four byte payload and
/// older devices with one; both shapes are accepted since only the first
/// payload byte's meaning is verified.
fn battery_level(frame: &[u8]) -> Option<u8> {
    if frame.len() < 5 || frame[0..3] != [0x02, 0x02, 0x03] {
        return None;
    }

    let level = frame[4];
    (level <= 100).then_some(level)
}

/// One battery reading the daemon took over RFCOMM, as `readings.toml`
/// holds it.
///
/// `connected` is the device's own live state at `read_at`, not merely
/// whether this reading is fresh: a sweep that answers always carries
/// `true`, and one that carries a reading forward from an earlier sweep
/// (see [`carry_forward`]) copies whatever the daemon's own device list most
/// recently reported for that address, so a headset that has actually gone
/// away is shown as last seen rather than as connected with a stale level.
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
    /// A reading of `level` for the named device that answered this sweep,
    /// always attributed to this source and always connected: a device that
    /// did not answer never reaches this constructor.
    pub fn new(address: Address, name: impl Into<String>, level: u8, read_at: Timestamp) -> Self {
        Self {
            address,
            name: name.into(),
            level,
            connected: true,
            read_at,
            source: Source::Bmap,
        }
    }
}

/// Folds a sweep's fresh readings into what was already on disk.
///
/// A reading this sweep refreshed always wins. One it did not is carried
/// forward unchanged rather than dropped, the way [`crate::poll::read_slow`]
/// holds the last good `system_profiler` devices over a failed read: a
/// device merely missed this sweep (a transient RFCOMM failure, a timeout)
/// keeps aging in place toward `stale_after` instead of disappearing from
/// the file outright. Its `connected` flag is refreshed from `devices`
/// when that address is still known there, so a device `devices` reports as
/// actually gone is shown as last seen immediately rather than waiting out
/// the stale window; an address `devices` no longer mentions at all keeps
/// whatever `connected` it last carried.
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

/// The handoff file: every reading the daemon's last BMAP sweep produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Readings {
    #[serde(rename = "reading", default, skip_serializing_if = "Vec::is_empty")]
    readings: Vec<Reading>,
}

/// Writes the sweep atomically, the same idiom every other state file uses.
pub fn save(path: &Path, readings: &[Reading]) -> Result<()> {
    let document = Readings {
        readings: readings.to_vec(),
    };

    toml::to_string(&document)
        .map_err(|error| Error::Format(format!("readings file is unwritable: {error}")))
        .and_then(|contents| atomic::write(path, &contents))
}

/// Loads the handoff file, treating anything unusable as no BMAP data at
/// all rather than an error: a machine with no daemon running, or one under
/// an older blubat that never wrote this file, behaves exactly as if this
/// source did not exist.
pub fn load(path: &Path) -> Vec<Reading> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| toml::from_str::<Readings>(&contents).ok())
        .map(|document| document.readings)
        .unwrap_or_default()
}

/// Overlays BMAP readings onto an already merged snapshot.
///
/// A fresh BMAP reading beats a `system_profiler` value for the same
/// address, which is a stale placeholder when `system_profiler` reports one
/// at all; IOKit stays authoritative over BMAP the same way it already is
/// over `system_profiler` in [`crate::snapshot::merge`]. The device category
/// and the vendor and product ids a displaced record carried are kept, the
/// way an IOKit reading keeps the category in that same merge: dropping the
/// ids here would make the device this sweep just read invisible to the
/// next one.
pub(crate) fn merge(mut snapshot: Snapshot, readings: &[Reading]) -> Snapshot {
    for reading in readings {
        let index = snapshot
            .devices
            .iter()
            .position(|device| device.address == reading.address);
        if index.is_some_and(|index| snapshot.devices[index].source == Source::IoKit) {
            continue;
        }

        let displaced = index.map(|index| &snapshot.devices[index]);
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
            source: Source::Bmap,
            connected: reading.connected,
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
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn frame(block_function_operator: [u8; 3], payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![
            block_function_operator[0],
            block_function_operator[1],
            block_function_operator[2],
            payload.len() as u8,
        ];
        frame.extend_from_slice(payload);

        frame
    }

    fn battery_frame(payload: &[u8]) -> Vec<u8> {
        frame([0x02, 0x02, 0x03], payload)
    }

    #[test]
    fn the_query_is_exactly_the_documented_four_bytes() {
        assert_eq!(BATTERY_QUERY, [0x02, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn a_one_byte_payload_is_accepted_as_the_older_devices_send_it() {
        let mut reader = FrameReader::new();

        assert_eq!(reader.feed(&battery_frame(&[42])), Some(42));
    }

    #[test]
    fn a_four_byte_payload_is_accepted_and_only_the_first_byte_is_read() {
        let mut reader = FrameReader::new();

        assert_eq!(
            reader.feed(&battery_frame(&[73, 0xff, 0xff, 0x00])),
            Some(73)
        );
    }

    #[test]
    fn a_frame_with_the_wrong_block_function_or_operator_is_rejected() {
        for header in [
            [0x01, 0x02, 0x03], // wrong block
            [0x02, 0x01, 0x03], // wrong function
            [0x02, 0x02, 0x01], // the query's own operator, not STATUS
        ] {
            let mut reader = FrameReader::new();

            assert_eq!(reader.feed(&frame(header, &[50])), None, "{header:?}");
        }
    }

    #[test]
    fn a_level_over_one_hundred_is_rejected() {
        let mut reader = FrameReader::new();

        assert_eq!(reader.feed(&battery_frame(&[101])), None);
    }

    #[test]
    fn a_frame_split_across_two_callbacks_is_still_read() {
        let whole = battery_frame(&[55, 0xff, 0xff, 0x00]);
        let (first, second) = whole.split_at(3);
        let mut reader = FrameReader::new();

        assert_eq!(reader.feed(first), None, "not a complete frame yet");
        assert_eq!(reader.feed(second), Some(55));
    }

    #[test]
    fn a_frame_from_a_different_function_block_does_not_block_the_one_waited_for() {
        let mut reader = FrameReader::new();
        let mut delivered = frame([0x03, 0x01, 0x03], &[0x00]);
        delivered.extend(battery_frame(&[64]));

        assert_eq!(reader.feed(&delivered), Some(64));
    }

    #[test]
    fn only_bose_vendor_ids_ever_resolve_a_channel() {
        assert_eq!(channel_for(0x009E, 0x4075), Some(8), "prince");
        assert_eq!(channel_for(0x009E, 0x4082), Some(2), "wolverine");
        assert_eq!(channel_for(0x009E, 0x1234), None, "unverified product id");
        assert_eq!(
            channel_for(0x004C, 0x4075),
            None,
            "the product id alone is not enough"
        );
    }

    fn candidate(vendor_id: Option<u16>, product_id: Option<u16>, connected: bool) -> Device {
        Device {
            address: address(PRINCE),
            name: "Bose QC Headphones".to_string(),
            kind: None,
            transport: None,
            vendor_id,
            product_id,
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source: Source::SystemProfiler,
            connected,
            read_at: READ_AT,
        }
    }

    #[test]
    fn a_connected_known_bose_device_is_the_one_candidate() {
        let found = candidates(&[candidate(Some(0x009E), Some(0x4075), true)]);

        assert_eq!(
            found,
            [(address(PRINCE), "Bose QC Headphones".to_string(), 8)]
        );
    }

    #[test]
    fn a_disconnected_device_is_never_a_candidate() {
        assert_eq!(
            candidates(&[candidate(Some(0x009E), Some(0x4075), false)]),
            []
        );
    }

    #[test]
    fn a_device_with_an_unverified_or_missing_id_is_never_a_candidate() {
        for device in [
            candidate(Some(0x009E), Some(0x9999), true),
            candidate(Some(0x009E), None, true),
            candidate(None, Some(0x4075), true),
            candidate(None, None, true),
        ] {
            assert_eq!(candidates(&[device]), []);
        }
    }

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "blubat-bmap-tests-{}-{}",
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

    fn reading(level: u8) -> Reading {
        Reading::new(address(PRINCE), "Bose QC Headphones", level, READ_AT)
    }

    #[test]
    fn a_written_sweep_reads_back_unchanged() {
        let scratch = Scratch::new();

        save(&scratch.readings_file(), &[reading(83)]).expect("writes");

        assert_eq!(load(&scratch.readings_file()), [reading(83)]);
    }

    #[test]
    fn a_missing_or_unparsable_file_is_no_bmap_data_rather_than_an_error() {
        let scratch = Scratch::new();

        assert_eq!(load(&scratch.readings_file()), []);

        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        fs::write(scratch.readings_file(), "not toml at all {{").expect("a written file");
        assert_eq!(load(&scratch.readings_file()), []);
    }

    #[test]
    fn a_device_missed_this_sweep_keeps_its_previous_reading_rather_than_vanishing() {
        let still_connected = candidate(Some(0x009E), Some(0x4075), true);

        let merged = carry_forward(vec![reading(76)], Vec::new(), &[still_connected]);

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
    fn a_device_devices_reports_as_actually_gone_is_carried_forward_as_disconnected() {
        let gone = candidate(Some(0x009E), Some(0x4075), false);

        let merged = carry_forward(vec![reading(76)], Vec::new(), &[gone]);

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
    fn every_reading_names_the_bmap_source() {
        let scratch = Scratch::new();
        save(&scratch.readings_file(), &[reading(50)]).expect("writes");

        let contents = fs::read_to_string(scratch.readings_file()).expect("a written file");

        assert!(contents.contains("source = \"bmap\""), "{contents}");
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
            source: Source::SystemProfiler,
            kind: Some("Headphones".to_string()),
            ..candidate(Some(0x009E), Some(0x4075), false)
        }
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
        assert!(device.connected);
        assert_eq!(
            device.kind.as_deref(),
            Some("Headphones"),
            "the device category the profiler record carried is kept"
        );
    }

    #[test]
    fn a_bmap_reading_preserves_the_ids_the_next_sweep_needs_to_find_it_again() {
        let merged = merge(snapshot(vec![profiler_placeholder()]), &[reading(76)]);

        assert_eq!(
            candidates(&merged.devices),
            [(address(PRINCE), "Bose QC Headphones".to_string(), 8)],
            "a device the merge just overwrote must still be a candidate next sweep"
        );
    }

    #[test]
    fn an_address_iokit_already_reported_is_left_untouched() {
        let iokit_device = Device {
            source: Source::IoKit,
            levels: Levels {
                main: Some(12),
                ..Levels::default()
            },
            ..candidate(Some(0x009E), Some(0x4075), true)
        };

        let merged = merge(snapshot(vec![iokit_device.clone()]), &[reading(76)]);

        assert_eq!(merged.devices, [iokit_device]);
    }

    #[test]
    fn a_bmap_reading_for_an_address_nothing_else_reported_is_added() {
        let merged = merge(snapshot(Vec::new()), &[reading(60)]);

        assert_eq!(merged.devices.len(), 1);
        assert_eq!(merged.devices[0].address, address(PRINCE));
        assert_eq!(merged.devices[0].levels.main, Some(60));
    }

    #[test]
    fn a_stale_bmap_reading_still_merges_and_carries_its_own_age() {
        let old = Timestamp::from_unix(READ_AT.unix() - 3_600);
        let aged = Reading::new(address(PRINCE), "Bose QC Headphones", 40, old);

        let merged = merge(snapshot(Vec::new()), std::slice::from_ref(&aged));

        assert_eq!(
            merged.devices[0].read_at, old,
            "merge does not filter by age; staleness is judged from read_at same as any other source"
        );
    }

    #[test]
    fn a_device_untouched_by_any_bmap_reading_is_unaffected() {
        let other = Device {
            source: Source::IoKit,
            ..candidate(None, None, true)
        };
        let other = Device {
            address: address("30-82-16-f2-24-90"),
            ..other
        };

        let merged = merge(snapshot(vec![other.clone()]), &[reading(76)]);

        assert!(merged.devices.contains(&other));
        assert_eq!(merged.devices.len(), 2);
    }
}
