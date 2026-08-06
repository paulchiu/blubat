//! The daemon's own battery reads, scheduled as one errand: the BMAP query
//! [`super::bmap`] makes over RFCOMM and the Battery Service read
//! [`super::gatt`] makes over BLE, folded into the one `readings.toml` both
//! share (see `blubat_core::readings`).
//!
//! One request drives both, because both want the same thing at the same
//! time: the devices the poll loop just read, on the `system_profiler`
//! cadence, once. Splitting them would double the scheduling in
//! [`super::run`] to buy nothing, since neither can run while the other is
//! using the main thread anyway. What they must not do is interfere, so
//! [`swept`] runs the Bose query first and folds its answer in before the
//! GATT sweep is even started: a peripheral that will not connect costs the
//! headset nothing, and a headset that will not answer costs the peripherals
//! nothing.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

use blubat_core::{Device, SweepReading, Timestamp, carry_forward_readings};

use super::bmap::{self, Channel};
use super::gatt::{self, Peripherals};

/// One pass the poll loop wants run: the devices to check, the file to fold
/// the result into, and how long either source may wait for an answer.
///
/// Plain and owned rather than borrowed, since it crosses from the worker
/// thread that finds it due to the main thread that actually runs it.
pub(crate) struct SweepRequest {
    pub(crate) devices: Vec<Device>,
    pub(crate) readings_file: PathBuf,
    pub(crate) timeout: Duration,
}

/// Whether a pass is due, advancing the tracker when it fires.
///
/// Mirrors the slow tier's own cadence in [`blubat_core::poll`]: the first
/// reading always fires it, since there is nothing to compare against yet,
/// and every one after waits out a full `interval` from the last firing.
pub(crate) fn due(last: &mut Option<Timestamp>, now: Timestamp, interval: Duration) -> bool {
    let elapsed = i64::try_from(interval.as_secs()).unwrap_or(i64::MAX);
    let fire = last.is_none_or(|previous| now.unix() - previous.unix() >= elapsed);

    if fire {
        *last = Some(now);
    }

    fire
}

/// Offers one pass to [`execute`], dropping it silently rather than waiting
/// if the previous one has not been taken yet.
///
/// `sweeps` is a channel of capacity one, so a request still sitting in it
/// means the executor is still mid pass on the one before. That is the same
/// one-attempt-no-retry discipline every sweep failure keeps: this pass is
/// simply skipped rather than queued behind the last one, so the backlog can
/// never grow past a single pending request.
pub(crate) fn offer(sweeps: &SyncSender<SweepRequest>, request: SweepRequest) {
    let _ = sweeps.try_send(request);
}

/// Both sweeps, folded into whatever the previous pass left on disk.
///
/// Either sweep only ever answers for the devices it reached this time; one
/// it missed (a transient failure, a timeout, a disconnect) keeps its last
/// known reading rather than vanishing, via
/// [`blubat_core::carry_forward_readings`]. Folding once per sweep rather
/// than once per pass is what keeps the two out of each other's way: each
/// fold refreshes only the addresses that sweep answered for and carries
/// every other address, the other source's included, forward untouched.
/// Split out from [`execute`] so this is exercised directly against fakes,
/// without a thread or a real file.
pub(crate) fn swept(
    channel: &dyn Channel,
    peripherals: &dyn Peripherals,
    devices: &[Device],
    read_at: Timestamp,
    timeout: Duration,
    previous: Vec<SweepReading>,
) -> Vec<SweepReading> {
    let after_bmap = carry_forward_readings(
        previous,
        bmap::sweep(channel, devices, read_at, timeout),
        devices,
    );

    carry_forward_readings(
        after_bmap,
        gatt::sweep(peripherals, devices, read_at, timeout),
        devices,
    )
}

/// The daemon's main thread, for as long as it runs: takes one
/// [`SweepRequest`] at a time and saves whatever it and the previous pass
/// together account for.
///
/// This has to be the main thread and nothing else, because IOBluetooth and
/// CoreBluetooth both only complete their calls through that thread's own run
/// loop; see either module's doc for the live finding that established this.
/// The loop ends on its own once every [`SyncSender`] offering requests has
/// gone, which is the poll loop's worker thread finishing or dying, and
/// `serve` joins that thread immediately after to recover its result. A save
/// that fails is as silent as an empty sweep; the next pass folds in over
/// whatever is on disk either way.
pub(crate) fn execute(
    channel: &dyn Channel,
    peripherals: &dyn Peripherals,
    requests: Receiver<SweepRequest>,
) {
    for request in requests {
        let previous = blubat_core::load_readings(&request.readings_file);
        let readings = swept(
            channel,
            peripherals,
            &request.devices,
            Timestamp::now(),
            request.timeout,
            previous,
        );
        let _ = blubat_core::save_readings(&request.readings_file, &readings);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use blubat_core::{Address, ChargeState, Levels, Source};

    use super::*;

    const PRINCE: &str = "bc-87-fa-18-b0-b7";
    const KEYS: &str = "de-df-38-f0-46-9b";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn device(name: &str, raw: &str, vendor_id: Option<u16>, product_id: Option<u16>) -> Device {
        Device {
            address: address(raw),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id,
            product_id,
            levels: Levels::default(),
            charge: ChargeState::Unknown,
            source: Source::SystemProfiler,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn bose() -> Device {
        device("Bose QC Headphones", PRINCE, Some(0x009E), Some(0x4075))
    }

    fn keys() -> Device {
        device("MX Keys M Mac", KEYS, None, None)
    }

    fn bmap_reading(level: u8) -> SweepReading {
        SweepReading::bmap(address(PRINCE), "Bose QC Headphones", level, READ_AT)
    }

    fn gatt_reading(level: u8) -> SweepReading {
        SweepReading::gatt(address(KEYS), "MX Keys M Mac", level, READ_AT)
    }

    fn sweep_request(devices: Vec<Device>) -> SweepRequest {
        SweepRequest {
            devices,
            readings_file: PathBuf::from("/dev/null"),
            timeout: Duration::ZERO,
        }
    }

    /// A BMAP channel that answers with whatever was queued for it, in order.
    #[derive(Default)]
    struct FakeChannel {
        answers: Mutex<Vec<Option<u8>>>,
    }

    impl FakeChannel {
        fn answering(answers: Vec<Option<u8>>) -> Self {
            Self {
                answers: Mutex::new(answers),
            }
        }
    }

    impl Channel for FakeChannel {
        fn battery(&self, _address: &Address, _channel_id: u8, _timeout: Duration) -> Option<u8> {
            self.answers.lock().expect("an unpoisoned fake").remove(0)
        }
    }

    /// Peripherals that answer for whichever of their names were asked about.
    #[derive(Default)]
    struct FakePeripherals {
        answers: Vec<(String, u8)>,
    }

    impl FakePeripherals {
        fn answering(answers: Vec<(&str, u8)>) -> Self {
            Self {
                answers: answers
                    .into_iter()
                    .map(|(name, level)| (name.to_string(), level))
                    .collect(),
            }
        }
    }

    impl Peripherals for FakePeripherals {
        fn levels(&self, wanted: &[String], _timeout: Duration) -> Vec<(String, u8)> {
            self.answers
                .iter()
                .filter(|(name, _)| wanted.contains(name))
                .cloned()
                .collect()
        }
    }

    /// One pass over both sources, with nothing on disk from before it.
    fn pass(
        channel: &dyn Channel,
        peripherals: &dyn Peripherals,
        devices: &[Device],
        previous: Vec<SweepReading>,
    ) -> Vec<SweepReading> {
        swept(
            channel,
            peripherals,
            devices,
            READ_AT,
            Duration::ZERO,
            previous,
        )
    }

    #[test]
    fn one_pass_records_both_sources_in_the_one_set_of_readings() {
        let readings = pass(
            &FakeChannel::answering(vec![Some(76)]),
            &FakePeripherals::answering(vec![("MX Keys M Mac", 95)]),
            &[bose(), keys()],
            Vec::new(),
        );

        assert_eq!(readings, [bmap_reading(76), gatt_reading(95)]);
    }

    #[test]
    fn a_gatt_sweep_that_answers_nothing_leaves_the_bose_reading_alone() {
        let readings = pass(
            &FakeChannel::answering(vec![Some(76)]),
            &FakePeripherals::default(),
            &[bose(), keys()],
            Vec::new(),
        );

        assert_eq!(readings, [bmap_reading(76)]);
    }

    #[test]
    fn a_bose_that_would_not_answer_does_not_cost_the_peripherals_their_reading() {
        let readings = pass(
            &FakeChannel::answering(vec![None]),
            &FakePeripherals::answering(vec![("MX Keys M Mac", 95)]),
            &[bose(), keys()],
            Vec::new(),
        );

        assert_eq!(readings, [gatt_reading(95)]);
    }

    #[test]
    fn a_miss_on_the_second_pass_carries_the_first_passes_readings_forward() {
        let devices = [bose(), keys()];
        let first = pass(
            &FakeChannel::answering(vec![Some(76)]),
            &FakePeripherals::answering(vec![("MX Keys M Mac", 95)]),
            &devices,
            Vec::new(),
        );

        let second = swept(
            &FakeChannel::answering(vec![None]),
            &FakePeripherals::default(),
            &devices,
            Timestamp::from_unix(READ_AT.unix() + 300),
            Duration::ZERO,
            first.clone(),
        );

        assert_eq!(
            second, first,
            "both devices are still connected and merely missed this pass"
        );
    }

    #[test]
    fn a_device_that_actually_disconnected_between_passes_is_carried_forward_as_last_seen() {
        let first = pass(
            &FakeChannel::default(),
            &FakePeripherals::answering(vec![("MX Keys M Mac", 95)]),
            &[keys()],
            Vec::new(),
        );

        let gone = Device {
            connected: false,
            ..keys()
        };
        let second = swept(
            &FakeChannel::default(),
            &FakePeripherals::default(),
            &[gone],
            Timestamp::from_unix(READ_AT.unix() + 300),
            Duration::ZERO,
            first,
        );

        assert_eq!(second.len(), 1);
        assert_eq!(second[0].level, 95, "the last known level stays");
        assert!(
            !second[0].connected,
            "gone from the device list means last seen, not connected"
        );
    }

    #[test]
    fn the_first_reading_always_fires_the_pass() {
        let mut last = None;

        assert!(due(&mut last, READ_AT, Duration::from_secs(300)));
        assert_eq!(last, Some(READ_AT));
    }

    #[test]
    fn a_second_reading_waits_out_the_full_interval() {
        let mut last = Some(READ_AT);
        let interval = Duration::from_secs(300);

        assert!(!due(
            &mut last,
            Timestamp::from_unix(READ_AT.unix() + 299),
            interval
        ));
        assert!(due(
            &mut last,
            Timestamp::from_unix(READ_AT.unix() + 300),
            interval
        ));
        assert_eq!(last, Some(Timestamp::from_unix(READ_AT.unix() + 300)));
    }

    #[test]
    fn an_offer_made_while_the_previous_one_is_still_waiting_is_dropped_not_queued() {
        let (sweeps, requests) = std::sync::mpsc::sync_channel(1);

        offer(&sweeps, sweep_request(vec![bose()]));
        offer(&sweeps, sweep_request(Vec::new()));

        assert_eq!(
            requests.try_iter().count(),
            1,
            "the channel's capacity of one already held a request, so the second offer found no room"
        );
    }
}
