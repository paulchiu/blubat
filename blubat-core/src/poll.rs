use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crate::snapshot::{Snapshot, merge};
use crate::timestamp::Timestamp;
use crate::{iokit, profiler, warn};

/// Takes one merged reading from both sources.
///
/// A `system_profiler` failure degrades the reading to the IOKit devices
/// rather than failing it: the fast source alone still answers the question
/// the POC could answer, and a warning on stderr says what was lost.
pub fn snapshot() -> Snapshot {
    let read_at = Timestamp::now();
    let profiler = profiler::read(read_at).unwrap_or_else(|error| {
        warn(&error.to_string());
        Vec::new()
    });

    merge(iokit::read(read_at), profiler, read_at)
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

    fn counted(reads: Arc<AtomicI64>) -> impl Fn() -> Snapshot + Send + 'static {
        move || Snapshot {
            read_at: Timestamp::from_unix(reads.fetch_add(1, Ordering::SeqCst)),
            devices: Vec::new(),
        }
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
