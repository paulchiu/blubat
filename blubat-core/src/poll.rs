use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::device::Device;
use crate::error::Result;
use crate::snapshot::{Snapshot, merge};
use crate::timestamp::Timestamp;
use crate::{iokit, profiler};

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

/// Takes one merged reading from both sources.
///
/// The one-shot path, on the default timeout: there is no earlier reading for
/// a degraded one to fall back on here, so a failing slow source leaves the
/// IOKit devices and the warning that says why.
pub fn snapshot() -> Snapshot {
    let read_at = Timestamp::now();
    let timeout = Tiers::default().timeout;
    let cached = read_slow(&Cached::default(), read_at, |at, warnings| {
        profiler::read(at, timeout, warnings)
    });

    read_fast(read_at, iokit::read, &cached)
}

/// Polls both tiers on their own threads and delivers merged snapshots.
///
/// Each tier reads once before its first wait, and both threads end once the
/// returned receiver is dropped, so a caller that stops listening stops the
/// polling. The channel is unbounded and only ever sent on from the fast tier,
/// so a consumer that renders slowly is never made to wait on a reading, and a
/// `system_profiler` call that hangs holds up nothing but its own tier.
pub fn poll(tiers: Tiers) -> Receiver<Snapshot> {
    poll_with(
        tiers,
        iokit::read,
        move |at, warnings| profiler::read(at, tiers.timeout, warnings),
        Timestamp::now,
    )
}

fn poll_with<F, S, C>(tiers: Tiers, fast: F, slow: S, clock: C) -> Receiver<Snapshot>
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
    thread::spawn(move || fast_tier(tiers.fast, fast, clock, &snapshots, &cached, polling));

    readings
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

/// Reads the slow source on its own thread, publishing each result to the fast tier.
///
/// `wanted` carries nothing. Waiting on it is both how this tier sleeps and how
/// it learns the fast tier has ended, so a shutdown does not wait out an
/// interval measured in minutes.
fn slow_tier(
    interval: Duration,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Result<Vec<Device>>,
    clock: impl Fn() -> Timestamp,
    refreshed: &Sender<Cached>,
    wanted: &Receiver<()>,
) {
    let mut held = Cached::default();

    loop {
        held = read_slow(&held, clock(), &read);

        if refreshed.send(held.clone()).is_err() {
            break;
        }
        if !matches!(
            wanted.recv_timeout(interval),
            Err(RecvTimeoutError::Timeout)
        ) {
            break;
        }
    }
}

/// Reads the fast source on every tick and sends the merged snapshot on.
///
/// Takes whatever the slow tier has published without ever waiting for it, so
/// the first readings carry IOKit alone and fill in once a slow reading lands.
/// `_polling` is never sent on: dropping it as this loop ends is what stops the
/// slow tier.
fn fast_tier(
    interval: Duration,
    read: impl Fn(Timestamp, &mut Vec<String>) -> Vec<Device>,
    clock: impl Fn() -> Timestamp,
    snapshots: &Sender<Snapshot>,
    cached: &Receiver<Cached>,
    _polling: Sender<()>,
) {
    let mut latest = Cached::default();

    loop {
        latest = cached.try_iter().last().unwrap_or(latest);

        if snapshots.send(read_fast(clock(), &read, &latest)).is_err() {
            break;
        }
        thread::sleep(interval);
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
