use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::device::Device;
use crate::error::Result;
use crate::snapshot::{Snapshot, merge};
use crate::timestamp::Timestamp;
use crate::{bmap, iokit, presence, profiler};

/// How often each tier reads its source, and how long the slow one may take.
///
/// The fast tier is the whole hot path: an IOKit read costs single digit
/// milliseconds, so it can run on every tick without being noticeable.
/// `system_profiler` costs closer to 150ms and gets slower the more devices
/// have ever been paired, so it runs on the slow tier and its last reading is
/// reused in between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiers {
    pub fast: Duration,
    pub slow: Duration,
    /// The ceiling on one `system_profiler` call, past which it is given up on.
    pub timeout: Duration,
}

impl Default for Tiers {
    /// The foreground intervals, which configuration later overrides.
    fn default() -> Self {
        Self {
            fast: Duration::from_secs(30),
            slow: Duration::from_secs(300),
            timeout: Duration::from_secs(10),
        }
    }
}

/// Takes one merged reading from both sources, plus whatever the daemon's
/// BMAP sweep last left in `readings_file`.
///
/// The one-shot path, on the default timeout: there is no earlier reading for
/// a degraded one to fall back on here, so a failing slow source leaves the
/// IOKit devices and the warning that says why. `readings_file` need not
/// exist: a machine with no daemon running merges nothing and reads exactly
/// as it always has.
pub fn snapshot(readings_file: &Path) -> Snapshot {
    let read_at = Timestamp::now();
    let timeout = Tiers::default().timeout;
    let cached = read_slow(&Cached::default(), read_at, |at, warnings| {
        profiler::read(at, timeout, warnings)
    });
    let reading = read_fast(read_at, iokit::read, &cached);

    bmap::merge(reading, &bmap::load(readings_file))
}

/// Polls both tiers on their own threads and delivers merged snapshots.
///
/// Each tier reads once before its first wait, and both threads end once the
/// returned receiver is dropped, so a caller that stops listening stops the
/// polling. The channel is unbounded and only ever sent on from the fast tier,
/// so a consumer that renders slowly is never made to wait on a reading, and a
/// `system_profiler` call that hangs holds up nothing but its own tier.
///
/// A device arriving or going away cuts the wait short on both tiers, since
/// that is the moment a held reading is most misleading. The fast tier reads
/// on every nudge; the slow one reads once and then sits out [`EARLY_FLOOR`],
/// since a flapping link must not turn into a stream of expensive calls.
///
/// Every reading is also merged with whatever the daemon's BMAP sweep has
/// most recently left in `readings_file`, re-read on every fast tick so a
/// sweep landing between ticks is picked up on the very next one.
pub fn poll(tiers: Tiers, readings_file: &Path) -> Receiver<Snapshot> {
    poll_with(
        tiers,
        iokit::read,
        move |at, warnings| profiler::read(at, tiers.timeout, warnings),
        Timestamp::now,
        presence::watch(),
        readings_file.to_path_buf(),
    )
}

fn poll_with<F, S, C>(
    tiers: Tiers,
    fast: F,
    slow: S,
    clock: C,
    nudges: Receiver<()>,
    readings_file: PathBuf,
) -> Receiver<Snapshot>
where
    F: Fn(Timestamp, &mut Vec<String>) -> Vec<Device> + Send + 'static,
    S: Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>> + Send + 'static,
    C: Fn() -> Timestamp + Clone + Send + 'static,
{
    let (snapshots, readings) = mpsc::channel();
    let (refreshed, cached) = mpsc::channel();
    let (polling, wanted) = mpsc::channel();
    let slow_clock = clock.clone();

    thread::spawn(move || slow_tier(tiers.slow, slow, slow_clock, &refreshed, &wanted));
    thread::spawn(move || {
        let wires = FastWires {
            snapshots: &snapshots,
            cached: &cached,
            polling,
            nudges: &nudges,
        };

        fast_tier(tiers.fast, fast, clock, wires, &readings_file)
    });

    readings
}

/// The channels one fast tier is wired to the rest of [`poll_with`] through.
///
/// Grouped into one value rather than four parameters, since they travel
/// everywhere [`fast_tier`] does and nothing in it treats one apart from the
/// other three.
struct FastWires<'a> {
    snapshots: &'a Sender<Snapshot>,
    cached: &'a Receiver<Cached>,
    polling: Sender<()>,
    nudges: &'a Receiver<()>,
}

/// The last `system_profiler` reading, reused until the slow tier replaces it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cached {
    devices: Vec<Device>,
    warnings: Vec<String>,
    /// Whether these devices are held over from a call that has since failed.
    degraded: bool,
}

/// Reads the slow source, keeping the last good devices when it fails.
///
/// A timeout or an unparseable document degrades the reading rather than
/// emptying it: the devices only that source can see stay in the merge,
/// carrying the timestamps that say how old they now are, and the failure
/// travels as a warning until a later call replaces it. A poll never fails.
fn read_slow(
    held: &Cached,
    read_at: Timestamp,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>>,
) -> Cached {
    let mut warnings = Vec::new();

    match read(read_at, &mut warnings) {
        Ok(devices) => Cached {
            devices,
            warnings,
            degraded: false,
        },
        Err(error) => Cached {
            devices: held.devices.clone(),
            warnings: vec![format!("{error}, keeping the last good reading")],
            degraded: true,
        },
    }
}

/// Reads the fast source and reconciles it with the cached slow one.
///
/// The cached warnings travel on every reading they apply to, so a degraded
/// merge stays visible for as long as it lasts rather than for one tick.
fn read_fast(
    read_at: Timestamp,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Vec<Device>,
    cached: &Cached,
) -> Snapshot {
    let mut warnings = cached.warnings.clone();
    let devices = read(read_at, &mut warnings);

    Snapshot {
        degraded: cached.degraded,
        ..merge(devices, cached.devices.clone(), read_at, warnings)
    }
}

/// The soonest a second early read may follow one a nudge already brought on.
///
/// A Bluetooth link can flap several times a second and this source costs about
/// 150ms a call, so a nudge buys one extra read rather than one per flap.
const EARLY_FLOOR: Duration = Duration::from_secs(5);

/// Reads the slow source on its own thread, publishing each result to the fast tier.
///
/// Waiting on `wanted` is how this tier sleeps, how the fast tier asks it for
/// an early read, and how it learns the fast tier has ended, so a shutdown does
/// not wait out an interval measured in minutes.
fn slow_tier(
    interval: Duration,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>>,
    clock: impl Fn() -> Timestamp,
    refreshed: &Sender<Cached>,
    wanted: &Receiver<()>,
) {
    let mut held = Cached::default();
    let mut early = false;

    loop {
        held = read_slow(&held, clock(), &read);

        if refreshed.send(held.clone()).is_err() {
            break;
        }
        if early {
            thread::sleep(EARLY_FLOOR);
            wanted.try_iter().for_each(drop);
        }

        match wanted.recv_timeout(interval) {
            Ok(()) => early = true,
            Err(RecvTimeoutError::Timeout) => early = false,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Reads the fast source on every tick and sends the merged snapshot on.
///
/// Takes whatever the slow tier has published without ever waiting for it, so
/// the first readings carry IOKit alone and fill in once a slow reading lands.
/// A nudge cuts the tick short here and is passed on to the slow tier, whose
/// answer lands on the tick after it. Dropping `polling` as this loop ends is
/// what stops that tier. `readings_file` is re-read on every tick rather than
/// cached, since it is small and only the daemon's own BMAP sweep, on its own
/// much slower cadence, ever changes it.
fn fast_tier(
    interval: Duration,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Vec<Device>,
    clock: impl Fn() -> Timestamp,
    wires: FastWires<'_>,
    readings_file: &Path,
) {
    let mut latest = Cached::default();

    loop {
        latest = wires.cached.try_iter().last().unwrap_or(latest);

        let reading = read_fast(clock(), &read, &latest);
        let reading = bmap::merge(reading, &bmap::load(readings_file));
        if wires.snapshots.send(reading).is_err() {
            break;
        }
        if waited(wires.nudges, interval) == Wake::Nudged {
            let _ = wires.polling.send(());
        }
    }
}

/// Why a tier stopped waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    Tick,
    Nudged,
}

/// Waits out one tick, cut short by a device arriving or going away.
///
/// One nudge stands for however many arrived while the tier was reading, since
/// they all ask for the same thing. A nudge source that has gone away leaves
/// the tier on its plain interval rather than spinning on a dead channel.
fn waited(nudges: &Receiver<()>, interval: Duration) -> Wake {
    match nudges.recv_timeout(interval) {
        Ok(()) => {
            nudges.try_iter().for_each(drop);

            Wake::Nudged
        }
        Err(RecvTimeoutError::Timeout) => Wake::Tick,
        Err(RecvTimeoutError::Disconnected) => {
            thread::sleep(interval);

            Wake::Tick
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::address::Address;
    use crate::device::{ChargeState, Levels, Source};
    use crate::error::Error;

    const READ_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    const TRACKPAD: &str = "30-82-16-f2-24-90";
    const KEYBOARD: &str = "de-df-38-f0-46-9b";

    fn device(name: &str, address: &str, source: Source) -> Device {
        Device {
            address: Address::parse(address).expect("valid address"),
            name: name.to_string(),
            kind: None,
            transport: None,
            vendor_id: None,
            product_id: None,
            levels: Levels {
                main: Some(85),
                ..Levels::default()
            },
            charge: ChargeState::Unknown,
            source,
            connected: true,
            read_at: READ_AT,
        }
    }

    fn trackpad() -> Device {
        device("Magic Trackpad", TRACKPAD, Source::IoKit)
    }

    fn keyboard() -> Device {
        device("MX Keys M Mac", KEYBOARD, Source::SystemProfiler)
    }

    fn frozen() -> impl Fn() -> Timestamp + Clone + Send + 'static {
        || READ_AT
    }

    /// A nudge channel whose far end is already gone, which is a machine where
    /// IOKit refused a notification port.
    fn unnudged() -> Receiver<()> {
        mpsc::channel().1
    }

    /// A readings file that was never written, which is a machine with no
    /// BMAP daemon sweeping: nothing here should be merged in.
    fn no_readings() -> PathBuf {
        std::env::temp_dir().join(format!(
            "blubat-poll-tests-no-readings-{}.toml",
            std::process::id()
        ))
    }

    /// A fast source that stamps each device with the number of reads before it.
    fn counting_fast(
        reads: Arc<AtomicI64>,
    ) -> impl Fn(Timestamp, &mut Vec<String>) -> Vec<Device> + Send + 'static {
        move |_, _| {
            vec![Device {
                read_at: Timestamp::from_unix(reads.fetch_add(1, Ordering::SeqCst)),
                ..trackpad()
            }]
        }
    }

    /// A slow source that counts its reads, since reuse is the point of the tier.
    fn counting_slow(
        reads: Arc<AtomicI64>,
    ) -> impl Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>> + Send + 'static {
        move |_, _| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(vec![keyboard()])
        }
    }

    fn stamps(receiver: &Receiver<Snapshot>, count: usize) -> Vec<i64> {
        receiver
            .iter()
            .take(count)
            .map(|reading| reading.devices[0].read_at.unix())
            .collect()
    }

    /// A first slow reading, with nothing held over from before it.
    fn first(read: impl Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>>) -> Cached {
        read_slow(&Cached::default(), READ_AT, read)
    }

    fn failing(_: Timestamp, _: &mut Vec<String>) -> Result<Vec<Device>> {
        Err(Error::Command("system_profiler exited with 1".to_string()))
    }

    #[test]
    fn a_bmap_reading_on_disk_is_merged_into_a_tick_that_never_saw_it_come_from_a_source() {
        let readings_file = std::env::temp_dir().join(format!(
            "blubat-poll-tests-bmap-{}-{}.toml",
            std::process::id(),
            line!()
        ));
        let bose = crate::BmapReading::new(
            Address::parse("bc-87-fa-18-b0-b7").expect("valid address"),
            "Bose QC Headphones",
            77,
            READ_AT,
        );
        crate::save_bmap_readings(&readings_file, &[bose]).expect("writes");
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_secs(3_600),
                ..Tiers::default()
            },
            |_, _| Vec::new(),
            |_, _| Ok(Vec::new()),
            frozen(),
            unnudged(),
            readings_file.clone(),
        );

        let reading = receiver.recv().expect("the first reading");

        assert_eq!(reading.devices.len(), 1);
        assert_eq!(reading.devices[0].name, "Bose QC Headphones");
        assert_eq!(reading.devices[0].levels.main, Some(77));

        let _ = std::fs::remove_file(&readings_file);
    }

    #[test]
    fn both_sources_merge_into_one_reading() {
        let cached = first(|_, _| Ok(vec![keyboard()]));
        let reading = read_fast(READ_AT, |_, _| vec![trackpad()], &cached);

        assert_eq!(reading.devices.len(), 2);
        assert!(reading.warnings.is_empty());
        assert!(!reading.degraded);
    }

    #[test]
    fn a_failed_system_profiler_degrades_the_reading_rather_than_failing_it() {
        let cached = first(failing);
        let reading = read_fast(READ_AT, |_, _| vec![trackpad()], &cached);

        assert_eq!(reading.devices.len(), 1, "the fast source still answers");
        assert!(reading.degraded);
        assert_eq!(
            reading.warnings,
            ["system_profiler exited with 1, keeping the last good reading"]
        );
    }

    #[test]
    fn a_failure_keeps_the_last_good_slow_devices_rather_than_dropping_them() {
        let good = first(|_, _| Ok(vec![keyboard()]));
        let degraded = read_slow(&good, READ_AT, failing);
        let recovered = read_slow(&degraded, READ_AT, |_, _| Ok(vec![keyboard()]));

        let reading = read_fast(READ_AT, |_, _| vec![trackpad()], &degraded);

        assert_eq!(
            reading.devices.len(),
            2,
            "the device only the slow source can see is still listed"
        );
        assert!(reading.degraded);
        assert!(
            !read_fast(READ_AT, |_, _| vec![trackpad()], &recovered).degraded,
            "and the next good call clears it"
        );
    }

    #[test]
    fn a_cached_warning_travels_on_every_reading_it_applies_to() {
        let cached = first(|_, warnings| {
            warnings.push("skipped a malformed device".to_string());
            Ok(Vec::new())
        });

        for _ in 0..3 {
            let reading = read_fast(READ_AT, |_, _| vec![trackpad()], &cached);

            assert_eq!(reading.warnings, ["skipped a malformed device"]);
        }
    }

    #[test]
    fn what_a_source_reports_this_tick_is_added_to_the_cached_warnings() {
        let cached = first(|_, warnings| {
            warnings.push("from the slow source".to_string());
            Ok(Vec::new())
        });
        let reading = read_fast(
            READ_AT,
            |_, warnings| {
                warnings.push("from the fast source".to_string());
                Vec::new()
            },
            &cached,
        );

        assert_eq!(
            reading.warnings,
            ["from the slow source", "from the fast source"]
        );
    }

    #[test]
    fn the_fast_tier_reads_immediately_and_then_on_every_interval() {
        let reads = Arc::new(AtomicI64::new(0));
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_secs(60),
                ..Tiers::default()
            },
            counting_fast(Arc::clone(&reads)),
            |_, _| Ok(Vec::new()),
            frozen(),
            unnudged(),
            no_readings(),
        );

        assert_eq!(stamps(&receiver, 3), [0, 1, 2]);
    }

    #[test]
    fn the_slow_tier_is_read_once_and_reused_across_fast_ticks() {
        let slow_reads = Arc::new(AtomicI64::new(0));
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_secs(60),
                ..Tiers::default()
            },
            |_, _| vec![trackpad()],
            counting_slow(Arc::clone(&slow_reads)),
            frozen(),
            unnudged(),
            no_readings(),
        );

        let merged = receiver
            .iter()
            .take(500)
            .position(|reading| reading.devices.len() == 2);

        assert!(merged.is_some(), "the slow reading reaches a fast tick");
        assert!(
            receiver
                .iter()
                .take(5)
                .all(|reading| reading.devices.len() == 2),
            "and is reused on the ticks after it"
        );
        assert_eq!(slow_reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_nudge_reads_both_tiers_without_waiting_out_the_interval() {
        let fast_reads = Arc::new(AtomicI64::new(0));
        let slow_reads = Arc::new(AtomicI64::new(0));
        let (nudge, nudges) = mpsc::channel();
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_secs(3_600),
                slow: Duration::from_secs(3_600),
                ..Tiers::default()
            },
            counting_fast(Arc::clone(&fast_reads)),
            counting_slow(Arc::clone(&slow_reads)),
            frozen(),
            nudges,
            no_readings(),
        );

        receiver.recv().expect("the first reading");
        for _ in 0..3 {
            nudge.send(()).expect("the poller is listening");
        }

        assert_eq!(
            stamps(&receiver, 1),
            [1],
            "a second reading long before the hour is up"
        );
        assert!(
            (0..500).any(|_| {
                thread::sleep(Duration::from_millis(10));
                slow_reads.load(Ordering::SeqCst) > 1
            }),
            "and the slow tier was asked to read again too"
        );
    }

    #[test]
    fn a_flapping_link_does_not_turn_into_a_stream_of_slow_reads() {
        let slow_reads = Arc::new(AtomicI64::new(0));
        let (nudge, nudges) = mpsc::channel();
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_secs(3_600),
                slow: Duration::from_secs(3_600),
                ..Tiers::default()
            },
            |_, _| vec![trackpad()],
            counting_slow(Arc::clone(&slow_reads)),
            frozen(),
            nudges,
            no_readings(),
        );

        receiver.recv().expect("the first reading");
        for _ in 0..4 {
            nudge.send(()).expect("the poller is listening");
            thread::sleep(Duration::from_millis(50));
        }
        thread::sleep(Duration::from_millis(500));

        assert_eq!(
            slow_reads.load(Ordering::SeqCst),
            2,
            "one read on the first nudge, and the rest inside the floor"
        );
    }

    #[test]
    fn a_silent_nudge_source_leaves_the_tiers_on_their_intervals() {
        let reads = Arc::new(AtomicI64::new(0));
        let (_silent, nudges) = mpsc::channel();
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_secs(60),
                ..Tiers::default()
            },
            counting_fast(Arc::clone(&reads)),
            |_, _| Ok(Vec::new()),
            frozen(),
            nudges,
            no_readings(),
        );

        assert_eq!(stamps(&receiver, 3), [0, 1, 2]);
    }

    #[test]
    fn a_hung_slow_source_never_delays_a_fast_reading() {
        let (_blocked, never) = mpsc::channel::<()>();
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_millis(1),
                ..Tiers::default()
            },
            counting_fast(Arc::new(AtomicI64::new(0))),
            move |_, _| {
                let _ = never.recv();
                Ok(vec![keyboard()])
            },
            frozen(),
            unnudged(),
            no_readings(),
        );

        assert_eq!(stamps(&receiver, 3), [0, 1, 2]);
    }

    #[test]
    fn dropping_the_receiver_stops_both_tiers() {
        let fast_reads = Arc::new(AtomicI64::new(0));
        let slow_reads = Arc::new(AtomicI64::new(0));
        let receiver = poll_with(
            Tiers {
                fast: Duration::from_millis(1),
                slow: Duration::from_millis(1),
                ..Tiers::default()
            },
            counting_fast(Arc::clone(&fast_reads)),
            counting_slow(Arc::clone(&slow_reads)),
            frozen(),
            unnudged(),
            no_readings(),
        );

        receiver.recv().expect("the first reading");
        drop(receiver);

        let stopped = (settled(&fast_reads), settled(&slow_reads));
        thread::sleep(Duration::from_millis(50));

        assert_eq!(
            (
                fast_reads.load(Ordering::SeqCst),
                slow_reads.load(Ordering::SeqCst)
            ),
            stopped,
            "a stopped tier stays stopped"
        );
    }

    /// The count a tier stops on, waited for rather than timed, so a runner
    /// that has not scheduled the thread yet delays this rather than failing it.
    fn settled(reads: &AtomicI64) -> i64 {
        for _ in 0..500 {
            let before = reads.load(Ordering::SeqCst);
            thread::sleep(Duration::from_millis(10));

            if reads.load(Ordering::SeqCst) == before {
                return before;
            }
        }

        panic!("the tier never stopped reading");
    }
}
