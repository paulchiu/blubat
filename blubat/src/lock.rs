//! The pid files blubat's two resident modes hold, and the handover between
//! them.
//!
//! Both the dashboard and the daemon evaluate the same events over the same
//! state, so exactly one of them may act on them. A lock file is what says
//! which: the dashboard takes one for as long as it is open, and the daemon
//! asks for it before every side effect. The file carries the pid of whoever
//! wrote it because the alternative is a dashboard that was killed silencing
//! the daemon forever, and a lock nobody can clear is worse than no lock.

use std::fs;
use std::path::{Path, PathBuf};

/// A lock file held for as long as this value lives.
///
/// Every way out of the scope that owns one removes the file: a normal return,
/// an error, a `?` and a panic all drop it. A process killed outright leaves it
/// behind, which is what the pid inside is for.
#[derive(Debug)]
pub struct Held {
    path: PathBuf,
    /// Which process the file names, so a lock another blubat has since taken
    /// over is left where it is rather than removed by this one on the way out.
    pid: u32,
}

impl Drop for Held {
    fn drop(&mut self) {
        if holder(&self.path) == Some(self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Claims `path` for this process, replacing whatever was there.
///
/// The last writer owns the lock, which is what two dashboards opened in two
/// panes come to: the newer one owns the side effects and the older one hands
/// them back rather than both fighting over the file.
pub fn take(path: PathBuf) -> Result<Held, String> {
    let pid = std::process::id();

    path.parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&path, format!("{pid}\n")))
        .map_err(|error| format!("{}: {error}", path.display()))
        .map(|()| Held { path, pid })
}

/// Whether a live process is holding `path`.
///
/// A file naming a pid that has gone is treated as no lock at all, so a
/// dashboard that was killed stops the daemon acting for exactly as long as it
/// takes the next reading to arrive.
pub fn held(path: &Path) -> bool {
    held_by(path, alive)
}

/// The same over whichever liveness test is given, which is the half a test
/// drives without a process to kill.
fn held_by(path: &Path, alive: impl Fn(u32) -> bool) -> bool {
    holder(path).is_some_and(alive)
}

/// The pid a lock file names, absent for a file that is missing or unreadable.
fn holder(path: &Path) -> Option<u32> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| contents.trim().parse().ok())
}

/// Whether a process with this id exists.
///
/// Signal 0 runs the existence and permission checks and delivers nothing. A
/// process owned by another user answers with a permission error, which is
/// still an answer that it is there.
fn alive(pid: u32) -> bool {
    // Zero and the negatives name process groups rather than one process, so a
    // lock file claiming one is a lock file naming nobody.
    let named = i32::try_from(pid).unwrap_or(0);

    if named <= 0 {
        return false;
    }

    // SAFETY: `kill` reads no memory blubat owns and, with signal 0, changes
    // nothing about the process it names.
    if unsafe { libc::kill(named, 0) } == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    /// A pid past anything macOS hands out, which is what a lock left behind by
    /// a process that has gone looks like.
    const STALE: u32 = 2_147_483_646;

    /// A directory that removes itself, so no test reaches a real state file.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "blubat-lock-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&path);

            Self(path)
        }

        fn lock(&self) -> PathBuf {
            self.0.join("state").join("tui.lock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dead(_: u32) -> bool {
        false
    }

    fn living(_: u32) -> bool {
        true
    }

    #[test]
    fn a_lock_lasts_as_long_as_the_value_holding_it() {
        let scratch = Scratch::new();
        let path = scratch.lock();

        let held = take(path.clone()).expect("a directory it can create");
        assert!(super::held(&path), "this process is alive and holding it");

        drop(held);
        assert!(!path.exists(), "and every exit path removes it");
    }

    #[test]
    fn a_lock_names_the_process_that_took_it() {
        let scratch = Scratch::new();
        let path = scratch.lock();
        let _held = take(path.clone()).expect("a lock");

        assert_eq!(holder(&path), Some(std::process::id()));
    }

    #[test]
    fn a_lock_left_by_a_process_that_has_gone_is_no_lock_at_all() {
        let scratch = Scratch::new();
        let path = scratch.lock();
        let _held = take(path.clone()).expect("a lock");

        assert!(held_by(&path, living));
        assert!(
            !held_by(&path, dead),
            "a killed dashboard must not silence a daemon forever"
        );
    }

    #[test]
    fn a_missing_or_unreadable_lock_is_absent_rather_than_an_error() {
        let scratch = Scratch::new();
        let path = scratch.lock();

        assert!(!held_by(&path, living), "nothing has ever been written");

        fs::create_dir_all(path.parent().expect("a parent")).expect("a scratch directory");
        for contents in ["", "not a pid", "-1", "12 34"] {
            fs::write(&path, contents).expect("a written lock");

            assert!(!held_by(&path, living), "{contents:?}");
        }
    }

    #[test]
    fn a_lock_another_blubat_has_taken_over_is_left_where_it_is() {
        let scratch = Scratch::new();
        let path = scratch.lock();
        let held = take(path.clone()).expect("a lock");
        fs::write(&path, "424242\n").expect("a second blubat claiming it");

        drop(held);

        assert_eq!(
            holder(&path),
            Some(424_242),
            "the owner it now names keeps it"
        );
    }

    #[test]
    fn this_process_is_alive_and_a_pid_nothing_can_hold_is_not() {
        assert!(alive(std::process::id()));
        assert!(!alive(STALE), "no process on a mac reaches this id");
        assert!(!alive(0), "and a group is not a process");
    }

    #[test]
    fn a_lock_that_cannot_be_written_comes_back_as_the_reason() {
        let scratch = Scratch::new();
        let path = scratch.lock();
        fs::create_dir_all(&path).expect("a directory where the file belongs");

        assert!(take(path).is_err());
    }
}
