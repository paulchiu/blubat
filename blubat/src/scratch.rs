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
