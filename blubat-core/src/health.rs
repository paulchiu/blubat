//! What the daemon leaves behind about itself, and what anything else makes
//! of it.
//!
//! A process existing is not the same as a process working. A daemon whose
//! sweeps were all failing once kept its loop turning for 28 hours while
//! `readings.toml` was never written again, and every surface blubat had said
//! it was running. So the two questions distributed systems already keep apart
//! are kept apart here: **liveness** is the poll loop still coming round, and
//! **readiness** is its last sweep having actually landed.
//!
//! Both answers come from [`Heartbeat`], which the daemon itself rewrites on
//! every pass. That is the point of putting them in a file rather than
//! deriving them from a pid: a daemon wedged badly enough to stop sweeping is
//! wedged badly enough to stop writing here, so the record goes stale on its
//! own and nothing has to notice on its behalf.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::config::Poll;
use crate::error::{Error, Result};
use crate::timestamp::Timestamp;

/// Intervals a daemon may miss before it counts as gone.
///
/// Three rather than one: a pass runs late whenever the machine sleeps or a
/// source is slow, and a health line that cries wolf on every laptop lid is
/// one nobody reads.
const MISSED: u32 = 3;

/// The daemon's own account of its last pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    /// When the poll loop last came round.
    pub beat_at: Timestamp,
    /// When a sweep last saved its readings, absent until one ever has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swept_at: Option<Timestamp>,
    /// Descriptors the process held when it came round, absent where the
    /// count could not be taken or the record predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_files: Option<usize>,
}

/// How long each answer stays good for, in the absence of a fresher one.
///
/// Derived from the daemon's own cadence rather than fixed, so a config that
/// slows the daemon down does not turn it permanently unhealthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Windows {
    /// Silence past this and the loop has stopped coming round.
    pub liveness: Duration,
    /// No sweep inside this and nothing the daemon has left is worth trusting.
    pub readiness: Duration,
}

impl Windows {
    /// The windows a daemon polling on this `[poll]` section is judged by.
    ///
    /// Saturating, since the file may name an interval too large to multiply
    /// and a window is not worth a panic in the dashboard's render path.
    pub fn of(poll: &Poll) -> Self {
        Self {
            liveness: poll.daemon_interval.saturating_mul(MISSED),
            readiness: poll.profiler_interval.saturating_mul(MISSED),
        }
    }
}

/// What the heartbeat file amounted to when it was read.
///
/// A file that is not there and a file that cannot be read are kept apart,
/// because they mean opposite things: the first is a machine with no daemon,
/// the second is a machine whose daemon cannot be asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Nothing has ever been written there.
    Never,
    /// What the daemon last wrote.
    Beat(Heartbeat),
    /// It is there, and could not be made sense of: it would not open, or
    /// it is not the TOML the daemon writes.
    Unreadable,
}

/// What the daemon's own record amounts to, judged against a clock.
///
/// Shaped so the impossible combinations cannot be built: a ready daemon
/// always has a sweep to name, and a down one always has a last beat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// No daemon has ever written here, which is what a machine that has never
    /// run `blubat daemon install` looks like. Not a fault.
    Absent,
    /// No beat inside the liveness window: loaded, perhaps, but not polling.
    Down { last_beat: Timestamp },
    /// Beating, but no sweep has landed inside the readiness window.
    NotReady {
        last_beat: Timestamp,
        last_sweep: Option<Timestamp>,
    },
    /// Beating, and sweeping.
    Ready {
        last_beat: Timestamp,
        last_sweep: Timestamp,
    },
    /// The daemon left a record that could not be read, so nothing here is
    /// known either way. Distinct from [`Health::Down`], which is a daemon
    /// this machine did manage to ask about.
    Unknown,
}

impl Health {
    /// What the daemon last wrote, against this clock and these windows.
    ///
    /// Liveness is answered first: a loop that has stopped coming round is
    /// down whatever its last sweep says, since nothing is left to refresh it.
    pub fn of(recorded: Recorded, now: Timestamp, windows: Windows) -> Self {
        let beat = match recorded {
            Recorded::Never => return Self::Absent,
            Recorded::Unreadable => return Self::Unknown,
            Recorded::Beat(beat) => beat,
        };

        if beat.beat_at.plus(windows.liveness) < now {
            return Self::Down {
                last_beat: beat.beat_at,
            };
        }

        match beat.swept_at {
            Some(swept_at) if swept_at.plus(windows.readiness) >= now => Self::Ready {
                last_beat: beat.beat_at,
                last_sweep: swept_at,
            },
            last_sweep => Self::NotReady {
                last_beat: beat.beat_at,
                last_sweep,
            },
        }
    }

    /// Whether this is a state worth saying something about.
    ///
    /// [`Health::Absent`] is not: a machine with no daemon installed is a
    /// documented way to run blubat, not a daemon that has gone wrong.
    pub fn alarming(self) -> bool {
        matches!(
            self,
            Self::Down { .. } | Self::NotReady { .. } | Self::Unknown
        )
    }

    /// The state in a word, for a surface that has room for one.
    pub fn label(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Down { .. } => "down",
            Self::NotReady { .. } => "not ready",
            Self::Ready { .. } => "ready",
            Self::Unknown => "unknown",
        }
    }
}

/// What this machine's record says about its daemon: the verdict, and the
/// measurement behind it that is a fact rather than a judgement.
///
/// The count sits beside [`Health`] rather than inside it because it says
/// nothing about whether the daemon is well: a climbing count is the thing a
/// reader judges for themselves, across passes, which no single reading can
/// answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reported {
    pub health: Health,
    /// Descriptors the daemon held when it last came round.
    pub open_files: Option<usize>,
}

impl Reported {
    /// What the daemon last wrote, judged against this clock and these windows.
    pub fn of(recorded: Recorded, now: Timestamp, windows: Windows) -> Self {
        Self {
            health: Health::of(recorded, now, windows),
            open_files: match recorded {
                Recorded::Beat(beat) => beat.open_files,
                Recorded::Never | Recorded::Unreadable => None,
            },
        }
    }
}

/// Where macOS lists the descriptors the calling process holds.
const DESCRIPTORS: &str = "/dev/fd";

/// How many descriptors this process is holding.
///
/// One of them is the read of `/dev/fd` itself, so the figure is a series to
/// watch rather than a number to read on its own: what a leak looks like is
/// this climbing pass after pass.
pub fn open_files() -> Option<usize> {
    counted(Path::new(DESCRIPTORS))
}

/// The count over whichever directory is handed in, so the answer for one that
/// cannot be read is exercised without making `/dev/fd` unreadable.
///
/// Absent rather than zero: a process holding no descriptors at all is not a
/// state that exists, so reporting one would be reporting a failed count as a
/// reading.
fn counted(directory: &Path) -> Option<usize> {
    fs::read_dir(directory).ok().map(Iterator::count)
}

/// Writes the heartbeat atomically, the same idiom every other state file uses.
///
/// # Errors
///
/// Returns [`Error::Format`] if it cannot be serialised to TOML, or
/// [`Error::Io`] if the file cannot be written.
pub fn save(path: &Path, beat: &Heartbeat) -> Result<()> {
    toml::to_string(beat)
        .map_err(|error| Error::Format(format!("heartbeat is unwritable: {error}")))
        .and_then(|contents| atomic::write(path, &contents))
}

/// Loads the heartbeat, keeping a file that is not there apart from one that
/// cannot be made sense of.
///
/// Only the first means no daemon. Reading the second the same way reports a
/// daemon that may be running perfectly as one that has stopped, in the
/// surface that exists to answer exactly that question.
pub fn load(path: &Path) -> Recorded {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents).map_or(Recorded::Unreadable, Recorded::Beat),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Recorded::Never,
        Err(_) => Recorded::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const BEAT_AT: Timestamp = Timestamp::from_unix(1_785_643_199);
    /// The clock every judgement below is made against.
    const NOW: Timestamp = Timestamp::from_unix(1_785_646_800);

    fn ago(seconds: i64) -> Timestamp {
        Timestamp::from_unix(NOW.unix() - seconds)
    }

    /// The default daemon cadence: 120s passes and 300s sweeps, so the windows
    /// worked out by hand are 360s and 900s.
    fn windows() -> Windows {
        Windows::of(&Poll::default())
    }

    fn beating(beat_at: Timestamp, swept_at: Option<Timestamp>) -> Heartbeat {
        Heartbeat {
            beat_at,
            swept_at,
            open_files: None,
        }
    }

    fn judged(beat: Heartbeat) -> Health {
        Health::of(Recorded::Beat(beat), NOW, windows())
    }

    /// A directory that removes itself, so a failing test leaves nothing behind.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "blubat-health-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn health_file(&self) -> std::path::PathBuf {
            self.0.join("health.toml")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Descriptor exhaustion is the shape this guards, which a test cannot
    /// arrange; a file the process cannot open reaches the same branch.
    #[test]
    fn a_heartbeat_that_cannot_be_read_is_unknown_rather_than_a_daemon_that_is_down() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        let path = scratch.health_file();
        std::os::unix::fs::symlink("health.toml", &path).expect("a link to itself");

        let health = Health::of(load(&path), NOW, windows());

        assert_eq!(health, Health::Unknown);
        assert!(
            health.alarming(),
            "a record nobody can read is worth saying out loud"
        );
    }

    #[test]
    fn the_windows_are_three_of_the_daemons_own_intervals() {
        assert_eq!(windows().liveness, Duration::from_secs(360));
        assert_eq!(windows().readiness, Duration::from_secs(900));
    }

    /// `daemon_interval` is whatever the file says, and the file may say
    /// something absurd; a window is not worth a panic in the render path.
    #[test]
    fn an_interval_too_large_to_multiply_saturates_rather_than_overflowing() {
        let poll = Poll {
            daemon_interval: Duration::from_secs(u64::MAX),
            ..Poll::default()
        };

        assert_eq!(Windows::of(&poll).liveness, Duration::MAX);
    }

    #[test]
    fn nothing_ever_written_is_absent_rather_than_unhealthy() {
        let health = Health::of(Recorded::Never, NOW, windows());

        assert_eq!(health, Health::Absent);
        assert!(
            !health.alarming(),
            "no daemon installed is a way to run blubat"
        );
    }

    #[test]
    fn a_fresh_beat_over_a_fresh_sweep_is_ready() {
        let health = judged(beating(ago(60), Some(ago(300))));

        assert_eq!(
            health,
            Health::Ready {
                last_beat: ago(60),
                last_sweep: ago(300)
            }
        );
        assert!(!health.alarming());
    }

    #[test]
    fn a_daemon_still_beating_over_a_sweep_that_stopped_landing_is_live_and_not_ready() {
        let health = judged(beating(ago(60), Some(ago(901))));

        assert_eq!(
            health,
            Health::NotReady {
                last_beat: ago(60),
                last_sweep: Some(ago(901))
            }
        );
        assert!(health.alarming());
    }

    #[test]
    fn a_daemon_that_has_never_swept_is_not_ready() {
        assert_eq!(
            judged(beating(ago(60), None)),
            Health::NotReady {
                last_beat: ago(60),
                last_sweep: None
            }
        );
    }

    #[test]
    fn a_beat_older_than_the_liveness_window_is_down_however_its_sweeps_went() {
        assert_eq!(
            judged(beating(ago(361), Some(ago(60)))),
            Health::Down {
                last_beat: ago(361)
            },
            "a loop that has stopped coming round is down before it is anything else"
        );
    }

    #[test]
    fn a_beat_exactly_on_the_liveness_window_is_still_live() {
        assert_eq!(
            judged(beating(ago(360), Some(ago(60)))),
            Health::Ready {
                last_beat: ago(360),
                last_sweep: ago(60)
            }
        );
    }

    #[test]
    fn a_written_heartbeat_reads_back_unchanged() {
        let scratch = Scratch::new();
        let beat = Heartbeat {
            beat_at: BEAT_AT,
            swept_at: Some(Timestamp::from_unix(1_785_643_000)),
            open_files: Some(2536),
        };

        save(&scratch.health_file(), &beat).expect("writes");

        assert_eq!(load(&scratch.health_file()), Recorded::Beat(beat));
    }

    #[test]
    fn a_report_carries_the_count_the_record_kept() {
        let beat = Heartbeat {
            beat_at: ago(60),
            swept_at: Some(ago(60)),
            open_files: Some(2536),
        };

        let reported = Reported::of(Recorded::Beat(beat), NOW, windows());

        assert_eq!(reported.open_files, Some(2536));
    }

    #[test]
    fn a_report_over_a_record_that_could_not_be_read_carries_no_count() {
        for recorded in [Recorded::Never, Recorded::Unreadable] {
            assert_eq!(
                Reported::of(recorded, NOW, windows()).open_files,
                None,
                "{recorded:?}"
            );
        }
    }

    #[test]
    fn a_report_judges_the_record_the_same_way_health_does_on_its_own() {
        let beat = beating(ago(60), Some(ago(60)));

        let reported = Reported::of(Recorded::Beat(beat), NOW, windows());

        assert_eq!(
            reported.health,
            Health::Ready {
                last_beat: ago(60),
                last_sweep: ago(60)
            }
        );
    }

    #[test]
    fn a_heartbeat_with_no_count_to_record_is_still_written() {
        let scratch = Scratch::new();
        let beat = beating(BEAT_AT, Some(Timestamp::from_unix(1_785_643_000)));

        save(&scratch.health_file(), &beat).expect("writes");

        assert_eq!(load(&scratch.health_file()), Recorded::Beat(beat));
    }

    #[test]
    fn a_heartbeat_written_before_the_count_existed_still_reads_as_a_heartbeat() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        fs::write(
            scratch.health_file(),
            "beat_at = \"2026-08-02T03:59:59Z\"\nswept_at = \"2026-08-02T03:56:40Z\"\n",
        )
        .expect("a written file");

        assert_eq!(
            load(&scratch.health_file()),
            Recorded::Beat(Heartbeat {
                beat_at: BEAT_AT,
                swept_at: Some(Timestamp::from_unix(1_785_643_000)),
                open_files: None,
            })
        );
    }

    #[test]
    fn a_directory_of_descriptors_is_counted_by_how_many_it_lists() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        for name in ["0", "1", "2"] {
            fs::write(scratch.0.join(name), "").expect("a written entry");
        }

        assert_eq!(counted(&scratch.0), Some(3));
    }

    /// Descriptors, not one descriptor: the suite's other tests open and close
    /// files on their own threads while this one counts, so the tolerance sits
    /// well under what this test holds and well over that noise, the same way
    /// `crate::profiler`'s own leak tests are pitched.
    const HELD: usize = 20;
    const NOISE: usize = 8;

    #[test]
    fn the_count_follows_the_descriptors_this_process_actually_holds() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        let before = open_files().expect("a count of this process");

        let held: Vec<fs::File> = (0..HELD)
            .map(|_| fs::File::open(&scratch.0).expect("an opened directory"))
            .collect();
        let after = open_files().expect("a count of this process");
        drop(held);

        assert!(
            after >= before + HELD - NOISE,
            "holding {HELD} more descriptors took the count from {before} to {after}"
        );
    }

    #[test]
    fn a_count_that_could_not_be_taken_is_absent_rather_than_zero() {
        let scratch = Scratch::new();

        assert_eq!(counted(&scratch.0), None);
    }

    #[test]
    fn a_file_nothing_has_ever_written_is_no_daemon_rather_than_an_error() {
        let scratch = Scratch::new();

        assert_eq!(load(&scratch.health_file()), Recorded::Never);
    }

    #[test]
    fn a_file_that_is_there_but_makes_no_sense_is_unreadable_rather_than_absent() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("a scratch directory");
        fs::write(scratch.health_file(), "not toml at all {{").expect("a written file");

        assert_eq!(load(&scratch.health_file()), Recorded::Unreadable);
    }
}
