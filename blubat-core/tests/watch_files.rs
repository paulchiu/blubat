//! The one-shot watch handover, exercised through a real directory.
//!
//! The unit tests cover the file format; these cover the part that only shows
//! up on a filesystem: creating the directory, naming the file and reading a
//! whole directory of watches back the way a daemon drains it.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use blubat_core::{Timestamp, Watch};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A directory that removes itself, so a failing test leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "blubat-watch-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);

        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_written_watch_reads_back_unchanged() {
    let scratch = Scratch::new();
    let watch = Watch::new("trackpad", 100, Some(Timestamp::from_unix(1_785_646_799)));

    let path = watch
        .write(&scratch.0)
        .expect("writes into a new directory");

    assert!(path.starts_with(&scratch.0));
    assert_eq!(Watch::read(&path).expect("reads back"), watch);
}

#[test]
fn a_daemon_can_drain_a_directory_of_watches() {
    let scratch = Scratch::new();
    let watches = [
        Watch::new("trackpad", 100, None),
        Watch::new("MX Keys", 80, None),
        Watch::new("AirPods", 90, None),
    ];

    for watch in &watches {
        watch.write(&scratch.0).expect("writes");
    }

    let mut drained: Vec<Watch> = fs::read_dir(&scratch.0)
        .expect("the directory exists")
        .map(|entry| Watch::read(&entry.expect("a readable entry").path()).expect("parses"))
        .collect();
    drained.sort_by(|a, b| a.device.cmp(&b.device));

    let mut expected = watches.to_vec();
    expected.sort_by(|a, b| a.device.cmp(&b.device));

    assert_eq!(drained, expected);
}

#[test]
fn an_unreadable_path_is_an_error_rather_than_a_panic() {
    let scratch = Scratch::new();

    assert!(Watch::read(&scratch.0.join("nothing.toml")).is_err());
}
