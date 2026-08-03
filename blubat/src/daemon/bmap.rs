//! The BMAP sweep: the daemon's own read of a Bose headset's battery level
//! over Bluetooth Classic RFCOMM, left in `readings.toml` for every
//! frontend to merge (see `blubat_core::bmap` for the wire format and the
//! handoff file this writes).
//!
//! Only `daemon run` may reach [`IoBluetooth`]. TCC attributes Bluetooth
//! access to the process responsible for it: under launchd the daemon is
//! responsible for itself, and the embedded usage description `build.rs`
//! writes lets TCC prompt (or silently allow, once granted) rather than
//! abort. Run the same code from a terminal instead and the terminal is the
//! responsible process, so TCC kills blubat with SIGABRT whatever its own
//! Info.plist says, which is why the TUI, `list`, `status` and `wait` must
//! never reach this module. Nothing outside `daemon` names [`IoBluetooth`]
//! or [`spawn_sweep`], and that privacy is the whole enforcement: there is
//! no flag that turns this source off, because there is no other path that
//! reaches it.
//!
//! The actual channel is reached through [`Channel`], the same seam
//! [`super::launchd::Launchctl`] and [`crate::tui::editor::Editor`] already
//! use, so [`sweep`] is exercised below with a fake and none of its tests
//! need a paired Bose headset or Bluetooth permission of their own.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use blubat_core::{Address, BmapFrameReader, BmapReading, Device, Timestamp, bmap_candidates};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use objc2_io_bluetooth::{
    IOBluetoothDevice, IOBluetoothRFCOMMChannel, IOBluetoothRFCOMMChannelDelegate,
};

/// Somewhere a BMAP battery query runs, which a test fills with a fake.
pub(crate) trait Channel {
    /// Opens `channel_id` on the device at `address`, writes the battery
    /// GET query, and waits up to `timeout` for a matching STATUS frame.
    /// Any failure along the way, including the timeout, is `None`: one
    /// attempt, no retry, matching the daemon's own failure discipline for
    /// every other BMAP problem.
    fn battery(&self, address: &Address, channel_id: u8, timeout: Duration) -> Option<u8>;
}

/// The real one: IOBluetooth over RFCOMM, exactly as `poc-bmap-battery`
/// proved it out end to end against Bose QC Headphones.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IoBluetooth;

/// State the delegate callbacks fill in, read back once the wait is over.
///
/// Plain interior mutability rather than a lock: everything here runs on
/// the one thread that owns this attempt and pumps the run loop that
/// delivers the callbacks, so nothing is ever contended.
struct Ivars {
    reader: RefCell<BmapFrameReader>,
    battery: Cell<Option<u8>>,
    done: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl IOBluetoothRFCOMMChannelDelegate for Delegate {
        #[unsafe(method(rfcommChannelData:data:length:))]
        fn rfcomm_channel_data_data_length(
            &self,
            _rfcomm_channel: Option<&IOBluetoothRFCOMMChannel>,
            data_pointer: *mut c_void,
            data_length: usize,
        ) {
            // SAFETY: IOBluetooth hands back a pointer to exactly
            // `data_length` bytes of its own buffer, valid for the length
            // of this callback.
            let bytes =
                unsafe { std::slice::from_raw_parts(data_pointer as *const u8, data_length) };

            if let Some(level) = self.ivars().reader.borrow_mut().feed(bytes) {
                self.ivars().battery.set(Some(level));
                self.ivars().done.set(true);
            }
        }

        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        fn rfcomm_channel_open_complete_status(
            &self,
            _rfcomm_channel: Option<&IOBluetoothRFCOMMChannel>,
            _status: i32,
        ) {
            // openRFCOMMChannelSync's own return value already answers
            // whether the channel opened, which is all this attempt reads;
            // the delegate's copy of the status needs no handling here.
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            reader: RefCell::new(BmapFrameReader::new()),
            battery: Cell::new(None),
            done: Cell::new(false),
        });

        // SAFETY: `init` on a freshly allocated instance of this class,
        // exactly as every other `define_class!` type in this workspace
        // constructs itself.
        unsafe { msg_send![super(this), init] }
    }
}

/// `deviceWithAddressString:` documents the address it wants in colon
/// form, and blubat's own [`Address`] normalises to hyphens for the merge.
fn colon_form(address: &Address) -> String {
    address.as_str().replace('-', ":")
}

impl Channel for IoBluetooth {
    fn battery(&self, address: &Address, channel_id: u8, timeout: Duration) -> Option<u8> {
        let delegate = Delegate::new();
        let delegate_obj: &AnyObject = &delegate;

        let ns_address = NSString::from_str(&colon_form(address));
        // SAFETY: a plain lookup by address string, as the POC's own call.
        let device = unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&ns_address)) }?;

        let mut channel: Option<Retained<IOBluetoothRFCOMMChannel>> = None;
        // SAFETY: `channel` is a valid out pointer for the duration of this
        // call, and `delegate_obj` outlives it.
        let open_status = unsafe {
            device.openRFCOMMChannelSync_withChannelID_delegate(
                Some(&mut channel),
                channel_id,
                Some(delegate_obj),
            )
        };
        let channel = (open_status == 0).then_some(channel).flatten()?;

        let mut query = blubat_core::BMAP_QUERY;
        // SAFETY: `query` outlives the call, and its length matches what is
        // handed over.
        let write_status = unsafe {
            channel.writeSync_length(query.as_mut_ptr() as *mut c_void, query.len() as u16)
        };
        if write_status != 0 {
            // SAFETY: closing a channel this attempt opened.
            let _ = unsafe { channel.closeChannel() };
            return None;
        }

        let deadline = Instant::now() + timeout;
        while !delegate.ivars().done.get() && Instant::now() < deadline {
            // SAFETY: pumping the calling thread's own run loop briefly so
            // the delegate callbacks IOBluetooth queues on it can run.
            unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, 0.2, false) };
        }

        // SAFETY: closing a channel this attempt opened, whether or not a
        // battery frame ever arrived. Never held open between sweeps.
        let _ = unsafe { channel.closeChannel() };

        delegate.ivars().battery.get()
    }
}

/// Runs the sweep against every BMAP candidate `devices` names.
///
/// Every attempt is independent, and a failure on one device never stops
/// the rest: a disconnect, a refused channel, a timeout or a malformed
/// response are all silently no reading for that device this sweep, the
/// same discipline every other BMAP failure keeps.
pub(crate) fn sweep(
    channel: &dyn Channel,
    devices: &[Device],
    read_at: Timestamp,
    timeout: Duration,
) -> Vec<BmapReading> {
    bmap_candidates(devices)
        .into_iter()
        .filter_map(|(address, name, channel_id)| {
            channel
                .battery(&address, channel_id, timeout)
                .map(|level| BmapReading::new(address, name, level, read_at))
        })
        .collect()
}

/// Whether the sweep is due, advancing the tracker when it fires.
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

/// Runs one sweep on a thread of its own and saves whatever it finds.
///
/// Detached rather than awaited: the sync RFCOMM open this walks through
/// must run somewhere it cannot wedge the daemon's own tick loop, the same
/// isolation the slow tier already gives `system_profiler`. A save that
/// fails is as silent as an empty sweep; the next sweep's readings simply
/// replace whatever is on disk.
pub(crate) fn spawn_sweep(devices: Vec<Device>, readings_file: PathBuf, timeout: Duration) {
    thread::spawn(move || {
        let readings = sweep(&IoBluetooth, &devices, Timestamp::now(), timeout);
        let _ = blubat_core::save_bmap_readings(&readings_file, &readings);
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use blubat_core::{ChargeState, Levels, Source};

    use super::*;

    const PRINCE: &str = "bc-87-fa-18-b0-b7";
    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);

    fn address(raw: &str) -> Address {
        Address::parse(raw).expect("valid address")
    }

    fn bose(vendor_id: Option<u16>, product_id: Option<u16>) -> Device {
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
            connected: true,
            read_at: READ_AT,
        }
    }

    /// A channel that answers with whatever was queued for it, in order,
    /// and records every address and channel id it was asked to open.
    #[derive(Default)]
    struct Fake {
        answers: Mutex<Vec<Option<u8>>>,
        calls: Mutex<Vec<(Address, u8)>>,
    }

    impl Fake {
        fn answering(answers: Vec<Option<u8>>) -> Self {
            Self {
                answers: Mutex::new(answers),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Channel for Fake {
        fn battery(&self, address: &Address, channel_id: u8, _timeout: Duration) -> Option<u8> {
            self.calls
                .lock()
                .expect("an unpoisoned fake")
                .push((address.clone(), channel_id));

            self.answers.lock().expect("an unpoisoned fake").remove(0)
        }
    }

    #[test]
    fn a_candidate_that_answers_becomes_one_reading() {
        let fake = Fake::answering(vec![Some(76)]);

        let readings = sweep(
            &fake,
            &[bose(Some(0x009E), Some(0x4075))],
            READ_AT,
            Duration::ZERO,
        );

        assert_eq!(
            readings,
            [BmapReading::new(
                address(PRINCE),
                "Bose QC Headphones",
                76,
                READ_AT
            )]
        );
        assert_eq!(
            fake.calls.lock().unwrap().as_slice(),
            [(address(PRINCE), 8)]
        );
    }

    #[test]
    fn a_candidate_the_channel_could_not_answer_yields_no_reading_at_all() {
        let fake = Fake::answering(vec![None]);

        let readings = sweep(
            &fake,
            &[bose(Some(0x009E), Some(0x4075))],
            READ_AT,
            Duration::ZERO,
        );

        assert_eq!(readings, []);
    }

    #[test]
    fn a_device_with_no_known_channel_is_never_asked() {
        let fake = Fake::default();

        let readings = sweep(
            &fake,
            &[bose(Some(0x004C), Some(0x4075))],
            READ_AT,
            Duration::ZERO,
        );

        assert_eq!(readings, []);
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn one_failing_candidate_does_not_stop_the_rest_of_the_sweep() {
        let wolverine = Device {
            address: address("30-82-16-f2-24-90"),
            name: "Bose QC Ultra".to_string(),
            ..bose(Some(0x009E), Some(0x4082))
        };
        let fake = Fake::answering(vec![None, Some(50)]);

        let readings = sweep(
            &fake,
            &[bose(Some(0x009E), Some(0x4075)), wolverine],
            READ_AT,
            Duration::ZERO,
        );

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].name, "Bose QC Ultra");
    }

    #[test]
    fn hyphenated_addresses_are_offered_to_the_channel_in_colon_form() {
        assert_eq!(colon_form(&address(PRINCE)), "bc:87:fa:18:b0:b7");
    }

    #[test]
    fn the_first_reading_always_fires_the_sweep() {
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
}
