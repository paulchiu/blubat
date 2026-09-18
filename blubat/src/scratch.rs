//! A throwaway directory for tests, so nothing here reaches a real config file,
//! state file or launch agent.
//!
//! One of these per crate rather than one per module: every test that touches a
//! path wants the same thing, which is somewhere private that removes itself
//! however the test ends.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use blubat_core::Paths;

/// What distinguishes one scratch directory from the next, so tests running
/// beside each other never share one.
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A directory that removes itself when the test holding it ends.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "blubat-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a scratch directory");

        Self(path)
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    /// A path inside it, which nothing has written to yet.
    pub fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// A path inside it that nothing can open, whatever it is run as.
    ///
    /// Descriptor exhaustion is the shape the callers guard against, which a
    /// test cannot arrange. A symlink pointing at itself reaches the same
    /// branch: every open of it fails, and unlike a file with its permissions
    /// taken away, root is refused too.
    pub fn unopenable(&self, path: &Path) -> PathBuf {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("a parent directory");
        }
        let name = path.file_name().expect("a named file");
        std::os::unix::fs::symlink(name, path).expect("a link to nowhere but itself");

        path.to_path_buf()
    }

    /// A path inside it that opens but that the kernel will not lock.
    ///
    /// A lock table that is full is the shape the callers guard against, which
    /// a test cannot arrange. `flock` refuses a FIFO outright, which reaches
    /// the same branch. The returned handle keeps a writer open, without which
    /// opening the FIFO to read would block for one.
    #[expect(unsafe_code)]
    pub fn unlockable(&self, path: &Path) -> (PathBuf, File) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("a parent directory");
        }
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .expect("a path with no interior nul");

        // SAFETY: `mkfifo` is given a nul-terminated path this process owns and
        // writes nothing back through it.
        let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(
            made,
            0,
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );

        let open = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("a fifo held open at both ends");

        (path.to_path_buf(), open)
    }

    /// blubat's whole layout under this directory.
    pub fn paths(&self) -> Paths {
        Paths::rooted(&self.0)
    }

    /// The config file these paths resolve to, written or not.
    pub fn config_file(&self) -> PathBuf {
        self.paths().config_file().to_path_buf()
    }

    /// The same file, holding this.
    pub fn write_config(&self, contents: &str) -> PathBuf {
        let path = self.config_file();
        fs::write(&path, contents).expect("a written config");

        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
