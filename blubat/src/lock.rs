//! The lock files blubat's two resident modes hold, and the handover between
//! them.
//!
//! Both the dashboard and the daemon evaluate the same events over the same
//! state, so exactly one of them may act on them. A lock file is what says
//! which.
//!
//! The lock is the kernel's rather than the file's contents: it belongs to the
//! open file behind it, so it is released when the holder ends however it ends,
//! including being killed outright. That is the whole reason for taking one this
//! way. A pid written into a file cannot say whether the process it names is
//! still the one that wrote it, and a lock nobody can clear is worse than no
//! lock. The pid is written in anyway, for whoever reads the state directory.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

/// A lock held for as long as this value lives.
#[derive(Debug)]
pub struct Held {
    file: File,
}

impl Drop for Held {
    /// Gives the lock up rather than leaving that to the file being closed.
    ///
    /// A child spawned while this was held inherits a reference to the same
    /// lock, so closing alone would leave it held until the last such child has
    /// exec'd. Unlocking releases it there and then, whichever of them still
    /// have the descriptor.
    fn drop(&mut self) {
        release(&self.file);
    }
}

/// Claims `path` for this process, or `None` for one another blubat holds.
///
/// The first blubat to take it owns the side effects and every one after it
/// defers, which is what two dashboards opened in two panes come to: the second
/// draws the same devices and announces none of them.
pub fn take(path: &Path) -> Result<Option<Held>, String> {
    let file = opened(path).map_err(|error| format!("{}: {error}", path.display()))?;

    if !locked(&file, libc::LOCK_EX) {
        return Ok(None);
    }

    stamp(&file, std::process::id()).map_err(|error| format!("{}: {error}", path.display()))?;

    Ok(Some(Held { file }))
}

/// Whether a live blubat is holding `path`.
///
/// A shared lock is enough to answer, and is refused only by the exclusive lock
/// a holder took: two blubats asking at once do not refuse each other, and the
/// answer is given up again as soon as it has been read.
pub fn held(path: &Path) -> bool {
    File::open(path).is_ok_and(|file| !free(&file))
}

/// Whether nothing is holding `file`, asked by taking a shared lock and giving
/// it straight back.
fn free(file: &File) -> bool {
    let taken = locked(file, libc::LOCK_SH);

    if taken {
        release(file);
    }

    taken
}

/// The lock file, created where there is none and left as it is where there is.
fn opened(path: &Path) -> io::Result<File> {
    path.parent().map_or(Ok(()), std::fs::create_dir_all)?;

    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Writes the holder's pid over whatever the file had.
fn stamp(mut file: &File, pid: u32) -> io::Result<()> {
    file.set_len(0)?;
    file.write_all(format!("{pid}\n").as_bytes())
}

fn release(file: &File) {
    locked(file, libc::LOCK_UN);
}

/// Takes `operation` on `file` without waiting, answering whether it was given.
fn locked(file: &File, operation: i32) -> bool {
    // SAFETY: `flock` is given a descriptor this process owns and reads no
    // memory blubat owns. Non-blocking, so it cannot hold the caller up.
    unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) == 0 }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::scratch::Scratch;

    use super::*;

    /// A pid past anything macOS hands out, which is what a lock file left
    /// behind by a process that has gone names.
    const STALE: &str = "2147483646\n";

    fn lock(scratch: &Scratch) -> std::path::PathBuf {
        scratch.paths().tui_lock()
    }

    #[test]
    fn a_lock_lasts_as_long_as_the_value_holding_it() {
        let scratch = Scratch::new();
        let path = lock(&scratch);

        let held = take(&path).expect("a directory it can create");
        assert!(held.is_some());
        assert!(super::held(&path), "this process is holding it");

        drop(held);
        assert!(!super::held(&path), "and every exit path gives it up");
    }

    #[test]
    fn a_lock_names_the_process_that_took_it() {
        let scratch = Scratch::new();
        let path = lock(&scratch);
        let _held = take(&path).expect("a lock");

        assert_eq!(
            fs::read_to_string(&path).expect("a written lock").trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn a_lock_another_blubat_holds_is_not_handed_out_twice() {
        let scratch = Scratch::new();
        let path = lock(&scratch);
        let first = take(&path).expect("a lock").expect("nobody else has it");

        assert!(
            take(&path).expect("a readable file").is_none(),
            "the first one up owns it"
        );

        drop(first);
        assert!(
            take(&path).expect("a lock").is_some(),
            "and the next one up takes it once that is over"
        );
    }

    #[test]
    fn a_file_left_by_a_process_that_has_gone_is_no_lock_at_all() {
        let scratch = Scratch::new();
        let path = lock(&scratch);
        fs::create_dir_all(path.parent().expect("a parent")).expect("a state directory");
        fs::write(&path, STALE).expect("a lock left behind");

        assert!(!held(&path), "a killed dashboard must not silence a daemon");
        assert!(take(&path).expect("a lock").is_some());
    }

    #[test]
    fn a_lock_nothing_has_ever_written_is_absent_rather_than_an_error() {
        let scratch = Scratch::new();

        assert!(!held(&scratch.join("never-written.lock")));
    }

    #[test]
    fn a_lock_that_cannot_be_written_comes_back_as_the_reason() {
        let scratch = Scratch::new();
        let path = lock(&scratch);
        fs::create_dir_all(&path).expect("a directory where the file belongs");

        assert!(take(&path).is_err());
    }
}
