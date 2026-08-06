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
//! blubat-core can be about BMAP without that dependency: the wire format
//! and which product ids are supported. The file the daemon leaves for every
//! frontend to merge back in is [`crate::readings`], shared with the GATT
//! sweep.

use crate::address::Address;
use crate::device::Device;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ChargeState, Levels, Source};
    use crate::timestamp::Timestamp;

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
}
