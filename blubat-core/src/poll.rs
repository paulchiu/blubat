use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crate::device::Device;
use crate::error::Result;
use crate::snapshot::{Snapshot, merge};
use crate::timestamp::Timestamp;
use crate::{iokit, profiler};

/// Takes one merged reading from both sources.
pub fn snapshot() -> Snapshot {
    let read_at = Timestamp::now();
    let mut warnings = Vec::new();
    let iokit = iokit::read(read_at, &mut warnings);
    let profiler = profiler::read(read_at, &mut warnings);

    snapshot_from(iokit, profiler, read_at, warnings)
}

/// Reconciles what the two sources returned into one reading.
///
/// A `system_profiler` failure degrades the reading to the IOKit devices
/// rather than failing it: the fast source alone still answers the question
/// the POC could answer, and the failure leaves as a warning.
fn snapshot_from(
    iokit: Vec<Device>,
    profiler: Result<Vec<Device>>,
    read_at: Timestamp,
    mut warnings: Vec<String>,
) -> Snapshot {
    let profiler = profiler.unwrap_or_else(|error| {
        warnings.push(error.to_string());
        Vec::new()
    });

    merge(iokit, profiler, read_at, warnings)
}

/// Repeats [`snapshot`] every `interval`, reading once before the first wait.
///
/// The reader thread ends when the returned receiver is dropped, so a caller
/// that stops listening stops the polling.
pub fn poll(interval: Duration) -> Receiver<Snapshot> {
    poll_with(interval, snapshot)
}

fn poll_with(
    interval: Duration,
    read: impl Fn() -> Snapshot + Send + 'static,
) -> Receiver<Snapshot> {
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        while sender.send(read()).is_ok() {
            thread::sleep(interval);
        }
    });

    receiver
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

    fn counted(reads: Arc<AtomicI64>) -> impl Fn() -> Snapshot + Send + 'static {
        move || Snapshot {
            read_at: Timestamp::from_unix(reads.fetch_add(1, Ordering::SeqCst)),
            devices: Vec::new(),
            warnings: Vec::new(),
        }
    }

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

    #[test]
    fn both_sources_merge_into_one_reading() {
        let reading = snapshot_from(
            vec![device("Magic Trackpad", "30-82-16-f2-24-90", Source::IoKit)],
            Ok(vec![device(
                "MX Keys M Mac",
                "de-df-38-f0-46-9b",
                Source::SystemProfiler,
            )]),
            READ_AT,
            Vec::new(),
        );

        assert_eq!(reading.devices.len(), 2);
        assert!(reading.warnings.is_empty());
    }

    #[test]
    fn a_failed_system_profiler_degrades_the_reading_rather_than_failing_it() {
        let reading = snapshot_from(
            vec![device("Magic Trackpad", "30-82-16-f2-24-90", Source::IoKit)],
            Err(Error::Command("system_profiler exited with 1".to_string())),
            READ_AT,
            Vec::new(),
        );

        assert_eq!(reading.devices.len(), 1, "the fast source still answers");
        assert_eq!(reading.warnings, ["system_profiler exited with 1"]);
    }

    #[test]
    fn reads_immediately_and_then_on_every_interval() {
        let reads = Arc::new(AtomicI64::new(0));
        let receiver = poll_with(Duration::from_millis(1), counted(Arc::clone(&reads)));

        let seen: Vec<i64> = receiver
            .iter()
            .take(3)
            .map(|snapshot| snapshot.read_at.unix())
            .collect();

        assert_eq!(seen, [0, 1, 2]);
    }

    #[test]
    fn dropping_the_receiver_stops_the_reader() {
        let reads = Arc::new(AtomicI64::new(0));
        let receiver = poll_with(Duration::from_millis(1), counted(Arc::clone(&reads)));

        receiver.recv().expect("the first reading");
        drop(receiver);
        thread::sleep(Duration::from_millis(50));

        let after_drop = reads.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(50));

        assert_eq!(reads.load(Ordering::SeqCst), after_drop);
    }
}
