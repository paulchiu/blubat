//! A throwaway directory for tests, so nothing here reaches a real config file,
//! state file or launch agent.
//!
//! One of these per crate rather than one per module: every test that touches a
//! path wants the same thing, which is somewhere private that removes itself
//! however the test ends.

use std::fs;
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
