//! The one-shot watches `blubat wait` hands over, and what a reading makes of
//! them.
//!
//! A wait registered against a running daemon is a file in the watch directory
//! and nothing else, so there is no socket, no daemon-side queue to query, and
//! nothing left behind by a wait that was interrupted. The daemon takes each
//! file over on the poll after it appears, deleting it as it does: a watch is
//! one notification, and a file still on disk is one a second reader could
//! notify from again.

use std::fs;
use std::path::{Path, PathBuf};

use blubat_core::{Device, Snapshot, Timestamp, Watch};

use crate::notify::{Banner, Notifier};
use crate::wait::completed;

/// The watches this daemon has taken over, held between readings.
///
/// In memory rather than on disk, which is what makes each of them exactly one
/// notification. A daemon that is restarted forgets the watches it had adopted,
/// the same trade the in-memory history makes: launchd restarts it only when it
/// fails, and a wait outliving that is what `--timeout` is for.
#[derive(Debug, Default)]
pub struct Watches {
    pending: Vec<Watch>,
}

/// What one reading makes of one watch.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// The device reached the level the watch was registered for.
    Met {
        device: String,
        level: u8,
    },
    Waiting,
    /// Nothing paired matches the substring, so no reading will complete it.
    Unknown,
    /// The deadline passed with the level unmet.
    Expired,
}

impl Watches {
    /// Takes over every watch file in `directory`, deleting each as it goes.
    ///
    /// A file that cannot be read is deleted too and reported: leaving it would
    /// mean reporting it again on every poll for as long as the daemon runs.
    pub fn adopt(&mut self, directory: &Path) -> Vec<String> {
        files(directory)
            .into_iter()
            .map(|path| {
                let adopted = Watch::read(&path);
                let _ = fs::remove_file(&path);

                match adopted {
                    Ok(watch) => {
                        let line = format!("watching `{}` for {}%", watch.device, watch.target);
                        self.pending.push(watch);

                        line
                    }
                    Err(problem) => format!("{}: {problem}", path.display()),
                }
            })
            .collect()
    }

    /// Measures every held watch against one reading, notifying what it met.
    ///
    /// Each line it hands back is one watch that is now finished with, whether
    /// that is because it was met, because nothing matching it is paired, or
    /// because it ran out of time.
    pub fn settle(
        &mut self,
        reading: &Snapshot,
        sound: &str,
        now: Timestamp,
        notifier: &dyn Notifier,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut waiting = Vec::new();

        for watch in std::mem::take(&mut self.pending) {
            match verdict(&watch, reading, now) {
                Verdict::Waiting => waiting.push(watch),
                Verdict::Met { device, level } => {
                    lines.push(announce(&device, level, sound, notifier));
                }
                Verdict::Unknown => lines.push(format!(
                    "nothing paired matches `{}`, dropping the watch for {}%",
                    watch.device, watch.target
                )),
                Verdict::Expired => lines.push(format!(
                    "gave up watching `{}` for {}%",
                    watch.device, watch.target
                )),
            }
        }

        self.pending = waiting;

        lines
    }
}

/// The watch files in `directory`, in the order their names sort.
///
/// A directory that does not exist yet is simply empty: nothing under the state
/// directory exists until a wait first registers one.
fn files(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    paths.sort();

    paths
}

/// Measures one watch against one reading.
///
/// Being met is decided before the deadline, so a watch whose level arrives in
/// the same reading its time runs out completes rather than expiring. A device
/// that is merely disconnected is waited on rather than dropped, because that
/// is the usual reason to be waiting on one; only a substring nothing paired
/// matches is hopeless.
fn verdict(watch: &Watch, reading: &Snapshot, now: Timestamp) -> Verdict {
    let reached = reading
        .matching(&watch.device)
        .find_map(|device| level_at_or_above(device, watch.target));

    match reached {
        Some((device, level)) => Verdict::Met { device, level },
        None if watch.deadline.is_some_and(|deadline| now >= deadline) => Verdict::Expired,
        None if reading.matching(&watch.device).next().is_none() => Verdict::Unknown,
        None => Verdict::Waiting,
    }
}

/// The device's name and live level, once that level has reached `target`.
fn level_at_or_above(device: &Device, target: u8) -> Option<(String, u8)> {
    device
        .active_level()
        .filter(|level| *level >= target)
        .map(|level| (device.name.clone(), level))
}

/// Posts the banner a met watch owes, and says what became of it.
///
/// The wording is the one a foreground wait ends with, so which path notified
/// is not something the banner makes the reader work out.
fn announce(device: &str, level: u8, sound: &str, notifier: &dyn Notifier) -> String {
    let posted = notifier.post(&Banner::new("blubat", completed(device, level), sound));

    match posted {
        Ok(_) => format!("{device} reached {level}%"),
        Err(problem) => {
            format!("{device} reached {level}%, but the banner was not posted: {problem}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use blubat_core::{Address, ChargeState, Levels, Source};

    use crate::notify::fake::Recorder as PostedBanners;

    use super::*;

    const TRACKPAD: &str = "Paul\u{2019}s Magic Trackpad";
    const READ_AT: i64 = 1_785_643_199;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A directory that removes itself, so no test reaches a real watch file.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-watches-tests-{}-{}",
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

    fn at(second: i64) -> Timestamp {
        Timestamp::from_unix(READ_AT + second)
    }

    fn reading(level: Option<u8>, connected: bool) -> Snapshot {
        Snapshot {
            read_at: at(0),
            devices: vec![Device {
                address: Address::parse("30-82-16-f2-24-90").expect("valid address"),
                name: TRACKPAD.to_string(),
                kind: None,
                transport: None,
                levels: Levels {
                    main: level,
                    ..Levels::default()
                },
                charge: ChargeState::Charging,
                source: Source::IoKit,
                connected,
                read_at: at(0),
            }],
            degraded: false,
            warnings: Vec::new(),
        }
    }

    fn watch(target: u8, deadline: Option<Timestamp>) -> Watch {
        Watch {
            device: "trackpad".to_string(),
            target,
            created: at(0),
            deadline,
        }
    }

    /// A watch directory holding the watches given, as `blubat wait` left them.
    fn registered(scratch: &Scratch, watches: &[Watch]) -> PathBuf {
        for watch in watches {
            watch.write(&scratch.0).expect("a written watch");
        }

        scratch.0.clone()
    }

    fn holding(watches: &[Watch]) -> Watches {
        Watches {
            pending: watches.to_vec(),
        }
    }

    #[test]
    fn a_watch_file_is_taken_over_and_removed_on_the_first_poll() {
        let scratch = Scratch::new();
        let directory = registered(&scratch, &[watch(100, None)]);
        let mut watches = Watches::default();

        let lines = watches.adopt(&directory);

        assert_eq!(lines, ["watching `trackpad` for 100%"]);
        assert_eq!(files(&directory), Vec::<PathBuf>::new(), "nothing is left");
        assert_eq!(watches.pending.len(), 1);
    }

    #[test]
    fn a_directory_no_wait_has_ever_written_to_is_simply_empty() {
        let scratch = Scratch::new();
        let mut watches = Watches::default();

        assert!(watches.adopt(&scratch.0).is_empty());
    }

    #[test]
    fn a_watch_file_that_cannot_be_read_is_reported_once_and_deleted() {
        let scratch = Scratch::new();
        let directory = registered(&scratch, &[]);
        fs::create_dir_all(&directory).expect("a scratch directory");
        fs::write(directory.join("broken.toml"), "not a watch at all {{").expect("a written file");
        let mut watches = Watches::default();

        let lines = watches.adopt(&directory);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("unreadable"), "{lines:?}");
        assert!(
            watches.adopt(&directory).is_empty(),
            "and is gone, so it is not reported on every poll after it"
        );
    }

    #[test]
    fn reaching_the_target_notifies_once_and_completes_the_watch() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[watch(100, None)]);

        let lines = watches.settle(&reading(Some(100), true), "Glass", at(0), &banners);

        assert_eq!(lines, [format!("{TRACKPAD} reached 100%")]);
        assert_eq!(banners.posted().len(), 1);
        assert_eq!(
            banners.posted()[0].body,
            format!("{TRACKPAD} is at 100%, safe to unplug.")
        );
        assert_eq!(banners.posted()[0].sound.as_deref(), Some("Glass"));
        assert!(
            watches
                .settle(&reading(Some(100), true), "Glass", at(0), &banners)
                .is_empty(),
            "a one-shot watch is finished with"
        );
        assert_eq!(banners.posted().len(), 1, "and notifies exactly once");
    }

    #[test]
    fn a_level_short_of_the_target_keeps_the_watch_waiting() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[watch(100, None)]);

        assert!(
            watches
                .settle(&reading(Some(85), true), "Glass", at(0), &banners)
                .is_empty()
        );
        assert!(banners.posted().is_empty());

        let lines = watches.settle(&reading(Some(100), true), "Glass", at(0), &banners);

        assert_eq!(lines.len(), 1, "the reading that reached it ends the watch");
    }

    #[test]
    fn a_disconnected_device_is_waited_on_rather_than_dropped() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[watch(100, None)]);

        assert!(
            watches
                .settle(&reading(Some(100), false), "Glass", at(0), &banners)
                .is_empty(),
            "a level nobody is reporting is not a crossing"
        );
        assert_eq!(watches.pending.len(), 1, "and the watch is still held");
    }

    #[test]
    fn a_watch_nothing_paired_matches_is_dropped_with_a_line_saying_so() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[Watch {
            device: "zzz-no-such-device".to_string(),
            ..watch(100, None)
        }]);

        let lines = watches.settle(&reading(Some(100), true), "Glass", at(0), &banners);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("zzz-no-such-device"), "{lines:?}");
        assert!(banners.posted().is_empty());
        assert!(watches.pending.is_empty());
    }

    #[test]
    fn a_watch_past_its_deadline_is_dropped_unmet() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[watch(100, Some(at(600)))]);

        assert!(
            watches
                .settle(&reading(Some(85), true), "Glass", at(599), &banners)
                .is_empty(),
            "there is still time"
        );

        let lines = watches.settle(&reading(Some(85), true), "Glass", at(600), &banners);

        assert_eq!(lines, ["gave up watching `trackpad` for 100%"]);
        assert!(banners.posted().is_empty());
    }

    #[test]
    fn a_level_arriving_as_the_deadline_does_completes_the_watch() {
        let banners = Arc::new(PostedBanners::new());
        let mut watches = holding(&[watch(100, Some(at(600)))]);

        let lines = watches.settle(&reading(Some(100), true), "Glass", at(600), &banners);

        assert_eq!(lines, [format!("{TRACKPAD} reached 100%")]);
        assert_eq!(banners.posted().len(), 1);
    }

    #[test]
    fn a_banner_that_could_not_be_posted_still_finishes_the_watch() {
        let banners = Arc::new(PostedBanners::failing("no notification centre"));
        let mut watches = holding(&[watch(100, None)]);

        let lines = watches.settle(&reading(Some(100), true), "Glass", at(0), &banners);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no notification centre"), "{lines:?}");
        assert!(watches.pending.is_empty());
    }

    #[test]
    fn two_watches_on_one_device_are_taken_over_and_met_in_turn() {
        let scratch = Scratch::new();
        let banners = Arc::new(PostedBanners::new());
        let directory = registered(&scratch, &[watch(80, None), watch(100, None)]);
        let mut watches = Watches::default();

        assert_eq!(watches.adopt(&directory).len(), 2);
        assert_eq!(
            watches
                .settle(&reading(Some(85), true), "Glass", at(0), &banners)
                .len(),
            1,
            "the 80% one is met and the 100% one waits"
        );
        assert_eq!(
            watches
                .settle(&reading(Some(100), true), "Glass", at(1), &banners)
                .len(),
            1
        );
        assert_eq!(banners.posted().len(), 2);
    }
}
