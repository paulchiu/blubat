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
        Heartbeat { beat_at, swept_at }
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
        };

        save(&scratch.health_file(), &beat).expect("writes");

        assert_eq!(load(&scratch.health_file()), Recorded::Beat(beat));
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
